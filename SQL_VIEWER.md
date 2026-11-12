# SQL result viewer

`sql-viewer` is a separate pager binary in this repository. It reuses ddb-tui's
Ratatui stack and themes; its table model is independent of DynamoDB.
It never connects to a database. The ggiho mycli and pgcli forks own the database
connection and supply typed result batches over a private local socket.

The macOS and Linux release archives include both `ddb-tui` and `sql-viewer`.
With the ggiho dotfiles, `chezmoi init --apply ggiho` (new Mac) or `chezmoi update`
(existing Mac) installs the binaries and configures the pager automatically; no
Rust toolchain is needed. See the dotfiles README for the full bootstrap command.

Build and install from source on macOS:


```sh
cargo build --release --locked --bin sql-viewer
install -m 755 target/release/sql-viewer ~/.local/bin/sql-viewer
install -m 755 scripts/sql-pager ~/.local/libexec/sql-pager/viewer
```

Install the optimized release binary, including for local UI/key-binding trials.
Debug builds make long-value parsing and wrapping noticeably slower.

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
| `g`, `G` | First / last loaded row |
| Page Up / Down | Move one page |
| `Ctrl+D`, `Ctrl+U` | Move grid down / up half a visible page (Vim style) |
| `v` | Copy displayed cell using `pbcopy` |
| `y` | Copy displayed row as TSV |
| `f` or `/`, Enter | Search loaded cells, case insensitive |
| `n`, `N` | Next / previous match |
| `s` | Sort loaded rows by selected column, toggle ascending / descending |
| `d` | Toggle record details; hide to give the grid the full width |
| `[`, `]` | Scroll right-hand record details |
| `T` | Cycle theme and save the selection |
| `?` | Help |
| `Ctrl+T` | Switch between query input and cached results (user-selected) |
| `q`, Escape | Close help or exit |

The user-selected **Ctrl+T** switches between query input and the last displayed
result. It replaces the query prompt's character-transposition command. Drafts
and cursor position remain intact; viewer selection, search, sorting and scroll
state are restored without executing SQL. Small results can also be reopened.
Alt+E and Alt+T remain unchanged. No terminal-global mapping is installed.
Restart the CLI after updating, then run a query to populate its session cache.

The cache is private to each CLI process (including concurrent connections),
stored in its mode-0700 temporary directory, and removed on normal process exit.
The typed preview keeps at most 64 MiB of estimated display data or 100,000 rows;
reaching that limit closes the cursor and clearly marks the unreceived tail. The
legacy text cache rejects results above 64 MiB. Restart both CLIs after updating
the forks. Existing mycli `\last` refers to the loaded preview for a paged query.

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

With compatible ggiho forks, ordinary single-table SELECTs use the existing
connection's streaming cursor. The first batch contains at most 500 rows, with one
lookahead row to detect more data; moving past the last loaded row requests the
next batch. Batches have a soft 4 MiB budget (one large row may exceed it). There is
no automatic LIMIT/OFFSET rewrite, COUNT query, or query rerun. Explicit LIMITs
remain unchanged. This reduces client buffering; database sorting and execution
can still take time before the first row arrives.

The footer distinguishes `DB batches` from results already buffered by the driver.
`500+ loaded` means the total is unknown, not that the query returned 500 rows.
Search text and current/total matching-cell counts stay visible independently of
copy feedback. Sorting is shown both in the column header (↑/↓) and footer. Search
and sorting cover loaded rows only; newly fetched rows join the active sort.
Actual SQL NULL flags travel separately from text: NULL stays last in either sort
direction, numeric values such as 2 and 10 sort numerically even with NULL, and the
literal string `NULL` remains ordinary text.

Ctrl+T retains the live cursor and loaded rows. Running another command closes the
old cursor before reusing its connection; its loaded rows can still be reopened,
with a notice that more rows are unavailable. MySQL cancels only its own active
query using a short-lived control connection, then drains/closes the cursor;
PostgreSQL cancels/drains its stream. The main connection is preserved. The pager
stays responsive while a batch is being fetched, including when closing it.

Streaming is deliberately conservative: a single SELECT from a verified permanent
base table, autocommit on, and no active transaction. Joins, functions, CTEs,
subqueries, locks, SELECT INTO, views, temporary tables and other complex queries
keep the existing buffered execution path. PostgreSQL RLS/rules also use that
path. Those SELECTs can still use typed preview/NULL flags, but the driver has
already received all rows. Writes, multi-statements, source/watch commands,
expanded output, export/redirection, tee/logging and other pagers keep their
existing behavior. Set `SQL_VIEWER_PAGING=0` before starting either CLI to use the
legacy pager path. Older viewer binaries retain Ctrl+T without enabling batches.

The legacy input contract remains **ascii_escaped display text**, not raw records.
Do not switch to an unescaped format: multiline values cannot reliably be
separated from rows. Direct invocation requires `--escaped`, e.g.
`sql-viewer --escaped < result.txt`. Alignment whitespace is trimmed, visible
escapes and the CLI NULL marker remain text. Legacy input cannot distinguish a
literal NULL marker from SQL NULL, so mixed columns keep text sorting. Typed input
preserves surrounding spaces and escapes terminal control characters. Copying
uses complete displayed values, not visual truncation; it does not promise an
exact round trip of original database types.

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
