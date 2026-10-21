# ddb-tui

A fast terminal UI for Amazon DynamoDB.

- Browse tables and page through Scan/Query results; earlier pages come from a cache, and short
  pages (from filters or the 1 MB limit) are filled automatically.
- Scan/Query with filters, projections, consistent reads, sort order and secondary indexes,
  written in DynamoDB's own expression syntax.
- Edit items in `$EDITOR`, create, duplicate, delete (one or many) and run UpdateItem
  expressions, all behind a confirmation.
- PartiQL, table details (keys, indexes, capacity, deletion protection, encryption, TTL,
  point-in-time recovery, tags), consumed capacity per page and the requests behind it.
- Copy items as JSON, export a page to CSV or JSON, switch regions.
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
```

The connection list shows every profile plus a DynamoDB Local entry. Profiles are resolved by the
AWS SDK, so SSO, assume-role and `credential_process` profiles work; credential environment
variables are ignored in favor of the selected profile. A profile without a region starts in
`us-east-1`; press `R` to switch.

## Keys

Press `?` on any screen for its full key list. The most used ones:

| Screen | Keys |
|---|---|
| Tables | `Enter` open (scan) · `/` filter · `t` details · `x` PartiQL · `R` region · `o` saved operations |
| Items | `n`/`p` page · `Enter` view · `f` scan/query form · `e` edit · `u` update · `a` add · `c` copy as new · `space` mark · `d` delete · `y`/`Y` copy JSON · `E` export · `i` requests · `w` save read · `+`/`-` page size · `←`/`→` columns |
| Detail | `e` edit · `u` update · `c` copy as new · `y`/`Y` copy JSON |
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

- Every write asks for confirmation. Creating or duplicating an item, or editing its key, uses a
  condition so it never replaces an existing item.
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
