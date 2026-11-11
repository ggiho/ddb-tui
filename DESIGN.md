# Design

## Source of truth
- Status: Active for the SQL viewer; existing DynamoDB screens retain their design.
- Last refreshed: 2026-11-11.
- Primary product surfaces: SQL grid and record inspector in `src/bin/sql-viewer.rs`.
- Evidence reviewed: `SQL_VIEWER.md`, `src/theme.rs`, `src/ui/mod.rs`, viewer code and PTY tests; user feedback in this session. No reference images or earlier design brief found.

## Brand
- Quiet, readable terminal workspace that belongs in the user's WezTerm.
- Preserve displayed values and make truncation explicit; avoid heavy black panels, decorative badges and dense boxes.

## Product goals
- Compare rows quickly; see all columns of the current row together.
- Non-goals: database editing, inferring SQL types from strings, exact original-value round trips.
- Success: column names and values are visually distinct, all fields retain their order, copy remains unchanged.

## Personas and jobs
- User: database practitioner using local mycli/pgcli forks on macOS.
- Jobs: scan results, read long values and JSON, copy a cell or row.

## Information architecture
- Grid is primary; right-hand inspector follows its selection. No result-toggle shortcut is assigned until the user chooses one.
- Record-panel hierarchy: record position → every column name and full value in query order → persistent actions and scroll position.

## Design principles
- Use hierarchy, padding and thin rules before additional background colors.
- Selection changes only the field marker; never reorder fields or replace the record with a selected-cell view.
- Formatting is presentation only: copy/search operate on the existing complete display text.
- Tradeoff: long values wrap and can require scrolling; do not shorten other fields because another cell is selected.

## Visual language
- Color: existing role-based themes; terminal default background remains visible. Unselected rows share one background; only selection is highlighted.
- Typography: terminal monospace, bold accent-colored column names with colons, normal indented values, a marker for the selected field.
- Layout: one-column inner padding, one blank line between groups, thin rules.
- Shape: inspector rounded outer frame; no nested boxes. No animation or images.

## Components
- Reuse `theme::Theme`, `ui::fit`, Ratatui Paragraph wrapping and line counts.
- Record panel: all full display values, explicit empty display marker, fixed query-column order; no selected-field hero or automatic JSON expansion.
- Tokens remain owned by `src/theme.rs`; no new dependencies or styling system.

## Accessibility
- Keyboard: hjkl selects cells; [ and ] scroll; d hides inspector; v/y copy.
- Selection is signaled by labels and emphasis, not color alone.
- Mouse wheel over inspector scrolls details, elsewhere scrolls grid; field clicks select that column without changing the scroll position.
- Shortcut ownership: ask the user to choose a key before assigning one. Inspect live mappings, but absence of a mapping is not permission to reserve it. Never add terminal-global shortcuts without explicit selection.
- No motion. Terminal screen-reader behavior is not certified.

## Responsive behavior
- Preserve 65/35 layout and existing automatic hide below 80 columns.
- Text wraps within inner padding; long field labels and values wrap.
- Clamp scrolling after resize; preserve scroll on column changes and reset on row changes; empty/tiny panels must not panic.

## Interaction states
- Loading: none, data is already buffered.
- Empty: zero rows stay inline; empty cell is labeled `(empty)` only in inspector.
- Error: all JSON remains original display text; pager fallback remains unchanged.
- Success: existing copy feedback. Disabled/offline: no network operations.

## Content voice
- Short English labels consistent with existing UI: Record; keep metadata minimal.
- Do not infer SQL types from display strings. Do not relabel textual NULL as a known SQL type.

## Implementation constraints
- Rust/Ratatui, existing theme and serde_json only.
- Keep source values unchanged. Do not unescape arbitrary backslash sequences.
- Test wrapping, selection changes, bounded scrolling, mouse selection, narrow layouts, and copy fidelity; inspect a synthetic result in tmux.

## Open questions
- None blocking this iteration. Exact original database types require a structured CLI-to-viewer protocol and remain out of scope.
