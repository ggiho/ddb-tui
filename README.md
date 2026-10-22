# ddb-tui

A fast terminal UI for Amazon DynamoDB.

- One workspace: the table list, the item grid and the selected item side by side. Narrow
  terminals fold the outer panes away.
- Values read by type: numbers right-aligned, nested maps and lists folded into `{3}` and `[2]`,
  status-like columns with colored dots.
- A command palette (`Ctrl+P`) for every command and for jumping to any table.
- Themes: Tokyo Night, Catppuccin Mocha and Catppuccin Latte, in 24-bit color with a 256-color
  fallback.
- Browse tables and page through Scan/Query results; recent pages come from a cache (up to 10
  pages and 32 MB, so paging through a large table does not grow memory without bound; older
  pages are read again), and short pages (from filters or the 1 MB limit) are filled
  automatically.
- Scan/Query with filters, projections, consistent reads, sort order and secondary indexes,
  written in DynamoDB's own expression syntax.
- Edit items in `$EDITOR`, create, duplicate, delete (one or many) and run UpdateItem
  expressions, all behind a confirmation.
- PartiQL, table details (keys, the attributes seen on the loaded page, indexes, capacity,
  deletion protection, encryption, TTL, point-in-time recovery, tags), consumed capacity per page
  and the requests behind it.
- Search the table list or the loaded page as you type, with matches highlighted.
- View items as DynamoDB JSON or plain JSON, copy them, and export a page or the whole result
  to CSV, JSON or JSON Lines. A whole-result export streams to the file page by page, shows its
  progress and cost, and can be cancelled; switch regions.
- Reads and writes [NoSQL Workbench](https://docs.aws.amazon.com/amazondynamodb/latest/developerguide/workbench.html)
  saved operations, so the two tools share them.

## Install

Requires Rust (developed with 1.94) on macOS or Linux.

```sh
cargo install --git https://github.com/ggiho/ddb-tui
```

or from a checkout: `cargo build --release` and run `target/release/ddb-tui`.

## Run

```sh
ddb-tui                                     # AWS profiles from ~/.aws/config and ~/.aws/credentials
ddb-tui --endpoint http://localhost:8000    # endpoint for the DynamoDB Local entry
ddb-tui --theme catppuccin-latte            # tokyo-night (default), catppuccin-mocha, catppuccin-latte
ddb-tui --read-only                         # no writes on any connection
```

The connection list shows every profile with its region, account and credential type (read from
`~/.aws/config`; secrets are never read), plus a DynamoDB Local entry. `/` searches it by any of
these, and profiles whose name says production are flagged. Profiles are resolved by the
AWS SDK, so SSO, assume-role and `credential_process` profiles work; credential environment
variables are ignored in favor of the selected profile. A profile without a region starts in
`us-east-1`; press `R` to switch.

Production connections start read-only: editing, deleting, updating and PartiQL writes are
refused until `W` allows them for the session (after a confirmation); `W` again turns them off.
A connection counts as production when its profile name says so (`prod`, `prd`, `production`,
`live` as a word, not `nonprod` or `pre-prod`) or when the settings below list its account or
profile. SSO errors include the `aws sso login --profile …` command to run.

Settings live in `~/.config/ddb-tui/config` (`$XDG_CONFIG_HOME` when set):

```ini
production_accounts = 111122223333, 444455556666   # production whatever the profile is called
production_profiles = legacy-main
read_only = production                             # production (default), all or none
```

`T` switches the theme while running and remembers the choice in `~/.config/ddb-tui/theme`
(`$XDG_CONFIG_HOME` when set). Terminals that do not set `COLORTERM=truecolor` get the nearest
256-color palette.

## Keys

Press `?` on any screen for its full key list, or `Ctrl+P` to search the commands. The most used
ones:

| Screen | Keys |
|---|---|
| Anywhere | `Ctrl+P` command palette · `T` theme · `W` allow or block writes · `?` keys |
| Connections | `/` search profile, region or account · `Enter` connect |
| Tables | `Enter` open (scan) · `/` search · `t` details · `x` PartiQL · `R` region · `o` saved operations |
| Export (`E`) | `Tab` this page / all results · `Enter` write · `Esc` cancel (also stops a running export) |
| Items | `/` search this page · `f` scan/query form (server-side filter) · `n`/`p` page · `Enter` view · `e` edit · `u` update · `a` add · `c` copy as new · `space` mark · `d` delete · `y`/`Y` copy JSON · `E` export · `i` requests · `w` save read · `+`/`-` page size · `←`/`→` columns |
| Detail | `v` DynamoDB / plain JSON · `e` edit · `u` update · `d` delete · `c` copy as new · `y`/`Y` copy JSON |
| Forms | `Tab` complete attribute name or next field · `←`/`→` change choice · `Enter` run |

## Expressions

Filters, conditions and updates use DynamoDB syntax with literal names and values; ddb-tui turns
them into `#name`/`:value` placeholders, so reserved words such as `status` or `type` need no
escaping.

```text
status = "closed" AND size(tags) > 2 AND NOT attribute_exists(deletedAt)
amount BETWEEN 10 AND 50 OR region IN ("kr", "jp")
SET views = if_not_exists(views, 0) + 1, history = list_append(history, ["seen"]) REMOVE draft
ADD labels <<"new">>
```

Strings are quoted, numbers are bare, `true`, `false` and `null` are literals, lists are
`[...]` and sets `<<...>>`. Names that are not plain words go in backticks: `` `my key` ``.

## Safety

- Every write asks for confirmation, PartiQL `INSERT`/`UPDATE`/`DELETE` included; refreshing a
  write statement's result never runs it again, and saved write statements open in the editor
  instead of running. Creating or duplicating an item, or editing its key, uses a condition so
  it never replaces an existing item.
- Editing first re-reads the complete item, so rows from a projection or a partially projected
  index cannot drop attributes.
- Credential files are only read, never written. There is no telemetry.
- Exports and the editor's temporary file are readable only by you; exports never overwrite.

## NoSQL Workbench saved operations

Saved operations live in `~/.aws/workbench/saved-operations/<connection>/<region>/`, where
`<connection>` is the profile name (`local` for the DynamoDB Local entry). Scan, Query and
PartiQL statements without parameters load and save; other types are listed only. Filters that
Workbench cannot represent (nested attribute paths, comparing two attributes) are not saved.

## Tests

```sh
cargo test                                               # unit tests
DDB_LOCAL_ENDPOINT=http://localhost:8000 cargo test      # plus end-to-end tests on DynamoDB Local
```

The end-to-end tests create and delete their own table, and run only against the endpoint you
give them.

## License

MIT. ddb-tui is not affiliated with Amazon Web Services; NoSQL Workbench and DynamoDB are AWS
products.
