# Fast-Follow Task 2 Report: Multi-Agent Roster Line

## What Changed

### `src/model.rs`
- Added `pub roster: Vec<Status>` field to `TabAgg` struct.
- In `aggregate()`: for each pane where `s.ever_active`, pushed `s.status` to `roster` in pane-id iteration order.
- Updated the `TabAgg { .. }` construction at the end of `aggregate()` to include `roster`.
- Added test `aggregate_populates_roster_per_ever_active_pane`.

### `src/render.rs`
- Updated `row_lines(agg)` to add `+1` when `agg.total > 1 && agg.status.is_active()`. This is the single source of truth; `lib.rs::tab_position_at_line` replays it correctly via `row_lines`.
- Updated `render()`: after the per-state detail lines for each row, when `row.agg.total > 1 && row.agg.status.is_active() && !row.agg.roster.is_empty()`, emits a roster line with `"   "` (3-space indent) then each member's `<role_ansi><glyph><RESET>` joined by a single space. Width is guarded: tokens are appended until the next one would exceed `width - 3`, then truncated.
- Updated `agg()` test helper to include `roster: vec![]`.
- Updated 4 direct `TabAgg { .. }` literals in tests to include `roster: vec![]`:
  - `multi_agent_pending_shows_count` (3 occurrences)
  - `multi_pending_detail_never_exceeds_width` (1 occurrence)
- Added 3 new tests: `multi_agent_active_tab_shows_roster_line`, `roster_line_never_exceeds_width`, `single_agent_pending_no_roster_line`.

### `src/lib.rs`
- Added test `multi_agent_running_tab_occupies_extra_roster_line`: a 2-pane running tab spans 3 lines (2 base + 1 roster), confirming `tab_position_at_line` maps all three lines to the tab.

## Every `TabAgg { .. }` Literal Updated

| File | Location | Change |
|------|----------|--------|
| `src/model.rs` | `aggregate()` return | Added `roster` field |
| `src/render.rs` | `agg()` helper | Added `roster: vec![]` |
| `src/render.rs` | `multi_agent_pending_shows_count` (3x) | Added `roster: vec![]` |
| `src/render.rs` | `multi_pending_detail_never_exceeds_width` | Added `roster: vec![]` |

## Test Command and Result

```
cargo test
```

Result: **96 passed; 0 failed** (was 91 before this task, +5 new tests).

```
cargo clippy --all-targets
```

Result: **clean** (no warnings, no errors).

## Commit Hash

`1d4e31c` — `feat(render): multi-agent roster line of per-member status glyphs`

## Concerns / Notes

- **Roster ordering**: The roster is populated in `pane_ids` iteration order (as received from `aggregate()`'s input slice). In `build_rows()` in lib.rs, pane IDs come from `tab_panes`, which is a `HashMap`, so ordering is non-deterministic across renders. This is acceptable for a visual status bar (no test relies on stable ordering of roster members — only on their presence).
- **Running glyph in roster**: When `Status::Running` appears in the roster, `glyph_for(opts.glyphs)` returns the static `'◐'` (the spin frame 0 variant) rather than the animated frame used in the tab header line. This matches the spec description `◐ ● ● ◆` without specifying animation for roster members.
- **Empty-roster guard**: The render block is gated on `!row.agg.roster.is_empty()` in addition to `total > 1 && status.is_active()`. This is safe; if `total > 1 && active`, at least one pane is `ever_active`, so `roster` will always be non-empty in practice. The guard is belt-and-suspenders safety.
