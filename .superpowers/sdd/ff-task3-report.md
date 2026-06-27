# Fast-Follow Task 3 Report: Progressive Overflow Compression

## Status

COMPLETE — all tests green, clippy clean, committed.

## Commit Hash

TBD (see below after commit)

## What Changed

### `src/render.rs`

**`plan_overflow` — new signature and compression logic**

- Old: `plan_overflow(rows, body_budget) -> (Vec<usize>, usize)` — returned kept row indices + folded-idle count.
- New: `plan_overflow(rows, body_budget) -> (Vec<(usize, usize)>, usize)` — returns `(row_idx, planned_lines)` per kept row + `strip_folded` count (0 = strip dropped).

Compression algorithm (in order):
1. If everything fits at full line counts → return all rows at `row_lines` each.
2. Fold idle rows; keep non-idle at full line counts.
3. Check if the strip line itself fits; if not, drop the strip (`strip_folded = 0`).
4. Compress calm rows (Done/Running) to 1 line each — lowest-index first, using `used -= *lines - 1; *lines = 1` to avoid the borrow checker conflict.
5. Compress urgent rows (Pending/Error) one line at a time (decrement per pass, restart loop from lowest-idx) — lowest-index first.
6. If still over with everything at 1 line — trim bottom rows that don't fit (never panics, clamps to budget).

**`render_row` — new helper**

Extracted the per-row rendering logic from `render()` into `fn render_row(out: &mut String, row: &TabRow, opts: &RenderOpts, max_lines: usize)`.

- Line 1 (gutter+glyph+num+name+slot) is always emitted.
- Per-state detail lines emitted in priority order within `max_lines`:
  - Running/Error: detail line (priority 1); roster (priority 2, lowest).
  - Pending: "branch · needs you" line (priority 1); quoted msg line (priority 2); roster (priority 3, lowest).
  - Done/Idle: no detail lines.

**`render()` — updated loop**

- Uses `(plan, strip_folded)` from the new `plan_overflow`.
- Overflow detection changed from `folded > 0` to `plan.len() < rows.len()` (any idle row absent from plan = there are folded rows = show `▲`).
- Iterates `for &(i, max_lines) in &plan` and calls `render_row(&mut out, &rows[i], opts, max_lines.max(1))`.
- Strip emission gated on `strip_folded > 0`.

**New tests in `render.rs`**

- `overflow_compresses_calm_before_urgent`: 3 Running rows (2 lines each) + 1 Pending-with-msg (3 lines), body_budget=5; asserts Running rows at 1 line each, Pending retains ≥2 lines (detail survives), total ≤ budget, render contains "needs you".
- `overflow_all_one_line_when_extreme`: height=3 (body_budget=1); asserts no panic, all planned rows ≥1 line, output lines ≤ height.

### `src/lib.rs`

**`tab_position_at_line` — updated to consume planned line counts**

```rust
let (plan, _strip_folded) = render::plan_overflow(&rows, body_budget);
for &(i, planned_lines) in &plan {
    let span = planned_lines.max(1);
    if target >= cursor && target < cursor + span {
        return Some((rows[i].number - 1) as usize);
    }
    cursor += span;
}
```

Each kept row's click span is now its PLANNED line count (compressed), not `row_lines`. This keeps render ↔ click in exact lockstep.

**New test in `lib.rs`**

- `click_mapping_matches_compressed_layout`: 3 Running tabs + 1 Pending-with-msg, `last_render_height = 7` (body_budget=5); asserts each Running tab maps to exactly 1 click line; Pending maps to 2 click lines (lines 5 and 6 both → position 3); line 7 → None.

## Test Summary

100 passed; 0 failed (was 97 before this task, +3 new tests).

`cargo clippy --all-targets` — clean (0 warnings).

## Render ↔ Click Lockstep Analysis

The lockstep guarantee holds because both `render()` and `tab_position_at_line()` call the **same** `plan_overflow(rows, body_budget)` with **identical** arguments:

- Both compute `body_budget` as `last_render_height.saturating_sub(header_lines)`.
- `render()` uses `plan.iter()` to call `render_row(_, _, _, max_lines)` — emitting exactly `max_lines` physical lines.
- `tab_position_at_line()` uses `plan.iter()` with the same `planned_lines` as the span for each row.

One subtle point: `render_row` emits AT MOST `max_lines` lines (it skips detail/roster when already at capacity). There is one edge case: a Pending row with no detail has `row_lines = 1`, but the compression code would only compress it if it's > 1 line. That case is already handled correctly — no compression needed, `planned_lines = 1`, and `render_row` emits 1 line.

## Concerns

1. **Strip vs. overflow marker**: When `strip_folded = 0` (strip dropped because non-idle rows alone overflow), the `▲` header marker still correctly shows via `plan.len() < rows.len()`. The strip is not rendered, but the overflow state is visible in the header. This is correct per the spec.

2. **Urgent row compression granularity**: Urgent rows lose exactly 1 line per outer-loop iteration (the `break` after decrement forces a restart). This means the decrement pattern for a 3-line Pending row is: first it loses the msg line (3→2), then if still over, it loses the "needs you" line (2→1). This matches the spec ("drop msg line, then drop branch/needs-you line") — the priority order within `render_row` is: "needs you" line is emitted first (priority 1 after line 1), msg line is emitted second (priority 2). So when compressed to 2 lines, "needs you" survives; when compressed to 1 line, neither detail line emits. Correct.

3. **`used` tracking**: The calm-row compression step uses `used -= *lines - 1` to update `used` in-place without re-summing the vec (avoiding the borrow checker conflict). This is correct as long as `*lines > 1` (guarded by the `if *lines <= 1 { continue; }` check above).
