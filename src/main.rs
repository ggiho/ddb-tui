use std::io::{Write, stdout};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use ddb_tui::app::{App, Msg};
use ddb_tui::ddb::Connection;
use ddb_tui::item::{self, Item};
use ddb_tui::{profiles, ui};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use tokio::sync::mpsc::UnboundedReceiver;

const DEFAULT_LOCAL_ENDPOINT: &str = "http://localhost:8000";

fn main() -> Result<()> {
    let endpoint = parse_args()?;
    let rt = tokio::runtime::Runtime::new()?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut connections: Vec<Connection> = profiles::profile_names()
        .into_iter()
        .map(Connection::Profile)
        .collect();
    connections.push(Connection::Local(endpoint));
    let mut app = App::new(connections, rt.handle().clone(), tx);

    let mut terminal = ratatui::init();
    // Pastes arrive as one event, so a newline in pasted text cannot press Enter.
    let _ = execute!(stdout(), EnableBracketedPaste);
    let result = run(&mut terminal, &mut app, &mut rx);
    let _ = execute!(stdout(), DisableBracketedPaste);
    ratatui::restore();
    result
}

fn parse_args() -> Result<String> {
    let mut args = std::env::args().skip(1);
    let mut endpoint = DEFAULT_LOCAL_ENDPOINT.to_string();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--endpoint" => endpoint = args.next().context("--endpoint needs a URL")?,
            "-h" | "--help" => {
                println!(
                    "usage: ddb-tui [--endpoint URL]\n\n  --endpoint URL  DynamoDB Local endpoint (default {DEFAULT_LOCAL_ENDPOINT})"
                );
                std::process::exit(0);
            }
            other => bail!("unknown argument: {other} (see --help)"),
        }
    }
    Ok(endpoint)
}

fn run(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    rx: &mut UnboundedReceiver<Msg>,
) -> Result<()> {
    while !app.quit {
        while let Ok(msg) = rx.try_recv() {
            app.on_msg(msg);
        }
        terminal.draw(|f| ui::draw(f, app))?;
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => app.on_key(key),
                Event::Paste(text) => app.on_paste(&text),
                _ => {}
            }
        }
        if let Some(item) = app.take_edit_request() {
            let _ = execute!(stdout(), DisableBracketedPaste);
            ratatui::restore();
            let result = edit_in_editor(&item);
            enable_raw_mode()?;
            execute!(stdout(), EnterAlternateScreen, EnableBracketedPaste)?;
            terminal.clear()?;
            app.on_edited(item, result);
        }
        if let Some(text) = app.take_clipboard() {
            app.on_copied(copy_to_clipboard(&text));
        }
    }
    Ok(())
}

/// Uses a clipboard command when one exists, otherwise asks the terminal through OSC 52
/// (supported by iTerm2, kitty, WezTerm, and tmux with `set-clipboard on`).
fn copy_to_clipboard(text: &str) -> Result<&'static str> {
    use base64::Engine;
    use std::process::Stdio;
    for (cmd, args) in [
        ("pbcopy", &[][..]),
        ("wl-copy", &[][..]),
        ("xclip", &["-selection", "clipboard"][..]),
    ] {
        let Ok(mut child) = Command::new(cmd).args(args).stdin(Stdio::piped()).spawn() else {
            continue;
        };
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes())?;
        }
        if child.wait()?.success() {
            return Ok(cmd);
        }
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    let mut out = stdout();
    write!(out, "\x1b]52;c;{encoded}\x07")?;
    out.flush()?;
    Ok("terminal OSC 52")
}

/// Opens the item as DynamoDB JSON in $VISUAL/$EDITOR. `Ok(None)` means nothing changed.
fn edit_in_editor(item: &Item) -> Result<Option<Item>> {
    let original = serde_json::to_string_pretty(&item::item_to_json(item))?;
    let path = std::env::temp_dir().join(format!("ddb-tui-{}.json", std::process::id()));
    write_private(&path, &original)?;
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    // Through sh so editors with arguments ("code -w") work.
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(&path)
        .status();
    let edited = std::fs::read_to_string(&path);
    let _ = std::fs::remove_file(&path);

    let status = status.context("could not start the editor")?;
    ensure!(status.success(), "editor exited with {status}");
    let value: serde_json::Value =
        serde_json::from_str(&edited?).context("edited item is not valid JSON")?;
    let edited = item::item_from_json(&value)?;
    Ok((edited != *item).then_some(edited))
}

/// The temp file holds table data, so only the current user may read it.
fn write_private(path: &Path, text: &str) -> Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(text.as_bytes())?;
    Ok(())
}
