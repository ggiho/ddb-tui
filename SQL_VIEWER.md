# SQL result viewer

`sql-viewer` is a separate pager binary in this repository. It reuses ddb-tui's
Ratatui stack and themes; its table model is independent of DynamoDB.
It never connects to a database. The mycli and pgcli forks need no source changes.

The macOS and Linux release archives include both `ddb-tui` and `sql-viewer`.
With the ggiho dotfiles, `chezmoi init --apply ggiho` (new Mac) or `chezmoi update`
(existing Mac) installs the binaries and configures the pager automatically; no
Rust toolchain is needed. See the dotfiles README for the full bootstrap command.

Build and install from source on macOS:


```sh
cargo build --bin sql-viewer
install -m 755 target/debug/sql-viewer ~/.local/bin/sql-viewer
install -m 755 scripts/sql-pager ~/.local/libexec/sql-pager/viewer
```

In both `~/.myclirc` and `~/.config/pgcli/config`, set:

```ini
table_format = ascii_escaped
pager = /bin/sh -c 'exec "$HOME/.local/libexec/sql-pager/viewer"'
enable_pager = True
```

The explicit `/bin/sh` expands `$HOME`, including with Click versions that execute
the pager directly without a shell. Restart the CLI to
load these settings. Keep the existing `sql-pager/less` wrapper beside `viewer`.
A result fitting both screen dimensions prints directly; larger results open the
grid. Zero-row tables stay in the CLI screen regardless of width or
`SQL_VIEWER_ALWAYS`; their original output is printed without entering the viewer.
`SQL_VIEWER_ALWAYS=1` bypasses the wrapper's size check, provided the CLI
actually invokes its pager. Redirected output remains byte-for-byte unchanged.
Unsupported output, compact single-column layouts, multiple results, or a viewer
failure go to the existing less wrapper with the original bytes.

| Key | Action |
| --- | --- |
| `hjkl`, arrows | Move selected cell; automatically scroll |
| `g`, `G` | First / last row |
| Page Up / Down | Move one page |
| `Ctrl+D`, `Ctrl+U` | Move grid down / up half a visible page (Vim style) |
| `v` | Copy displayed cell using `pbcopy` |
| `y` | Copy displayed row as TSV |
| `/`, Enter | Search all cells, case insensitive |
| `n`, `N` | Next / previous match |
| `s` | Sort selected column, toggle ascending / descending |
| `d` | Toggle record details; hide to give the grid the full width |
| `[`, `]` | Scroll right-hand record details |
| `T` | Cycle theme and save the selection |
| `?` | Help |
| `q`, Escape | Close help or exit |

The default `terminal` theme uses the terminal's background, foreground, and ANSI
palette. In your WezTerm this follows the custom Catppuccin Mocha colors, including
the muted cyan accent; the default background also preserves terminal opacity.
Press uppercase `T` to cycle terminal → Catppuccin Mocha → Catppuccin Latte →
Tokyo Night. The current name appears in the top bar. Changes are saved in
`$XDG_CONFIG_HOME/sql-viewer/theme` (default `~/.config/sql-viewer/theme`) and loaded
for the next result, independently of ddb-tui's own theme setting. An absent or
unrecognized saved name selects `terminal`. Fixed themes paint their own background.

Columns have one space of padding on each side, thin vertical separators, and
one horizontal rule under the header. Long headers and values end in `…` only
when clipped; clipping respects display widths (including Korean text). Copy and
search still use the complete displayed value, without the visual ellipsis.
All unselected rows use the same background; thin column separators provide
boundaries. The selected row and cell remain highlighted. Press `d` to hide/show details without losing selection.
Details start enabled for each result and remain automatically hidden below 80
columns. The toggle is local to the current result.

The right-hand panel is a full record view: every column stays in query order,
regardless of the selected cell. Column names are bold and accent-colored, with a
colon; values use the normal text color and indentation. Blank lines separate
fields. The selected field has a `›` marker, without being moved or expanded.
All values wrap in full, including JSON in its original display form; no selected
field hero, character counts, shortened other-field previews, or JSON reformatting.
Empty display strings show `(empty)` only in this panel. Copying stays unchanged.
Changing columns or clicking a field preserves the detail scroll position; changing
rows starts at the top. Mouse wheel and [ / ] scrolling remain bounded.

Mouse clicks select grid cells or record fields. The wheel scrolls the
inspector when the pointer is over it; otherwise it moves grid rows. Terminals narrower than 80
columns hide the details pane. Quitting restores the previous terminal screen;
the grid is not reprinted into scrollback.

The input contract is **ascii_escaped display text**, not raw database records.
Do not switch the CLI to an unescaped format while using this pager: rendered
multiline values cannot reliably be distinguished from extra rows. `--escaped`
is required for direct invocation, e.g. `sql-viewer --escaped < result.txt`.
Line breaks retain their visible escapes, alignment whitespace is trimmed, and
NULL remains the CLI's display marker. Copying therefore does not promise an
exact round trip of original database values. Plain decimals sort exactly,
including large integer IDs; other columns (including scientific notation,
NULL markers, and mixed values) sort as text. All rows are held in memory.

The ggiho mycli fork can arrange a single column as an `ls`-style grid. One-column
rows containing two consecutive spaces fall back to less conservatively, because
those spaces may represent multiple original records. Unsupported Unicode or
other output formats also fall back. Key remapping and frozen columns are not
implemented in this version.

Verification:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --bin sql-viewer
python3 -m unittest discover -s tests -p 'test_sql*.py'
```

The PTY regression test supplies piped stdin, exercises selection, clipboard and
search with a fake `pbcopy`, and checks terminal restoration. It does not change
the real clipboard. Live database connections are not needed.

To revert, restore the backed-up `pager` and `table_format` settings together.
The previous pspg and less wrappers remain installed.
