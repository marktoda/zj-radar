# Rail Line-as-Unit Deepening — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the rail's Lockstep invariant *structural* — every emitted line and its click target become one `Line` object, and `ansi`/`targets`/footprint all derive from a single `Vec<Line>`, eliminating the separate line-count predictor (`row_lines`) and the ~9 parallel `out.push_str`/`targets.push` pairs in `render_rail`.

**Architecture:** Render-first pipeline inside `src/render.rs`. (1) Each row renders to a full-fidelity `Vec<Line>` via `render_row`; header and strip get sibling producers. (2) The overflow planner reasons in pure `(Status, usize)` counts derived from `block.len()`. (3) An assembler builds one flat `Vec<Line>`, truncating each row with `.take(budget)` (proven equal to today's compression), tagging target + surface. (4) `RenderedRail::from_lines` derives `ansi` + `targets` in one place. The external rail seam (`RenderedRail { pub ansi, target_at_line, line_count }`) is **frozen**.

**Tech Stack:** Rust, `zellij-tile = "0.44"` (host target for unit tests), `insta` snapshots, `vt100` grid parser, `proptest`. Test runner: `cargo test --all-features`. NO rustfmt.

## Global Constraints

- **`ansi` byte-identical.** This is a behavior-preserving refactor. Every existing `insta` snapshot, `vt100` grid test, "color is purely additive" test, and `target_at_line` test MUST stay green **unchanged**. A moved snapshot = a refactor bug, never an accepted re-bless. Do NOT run `cargo insta accept`.
- **External seam frozen.** Do not change `RenderedRail`'s public fields/methods (`pub ansi: String`, `target_at_line(line: isize)`, `line_count()`, `empty()`). `Line` and `LineBg` stay private to `src/render.rs`.
- **No host calls.** Renderer is pure; no `get_pane_*` / blocking host calls anywhere (per `docs/smart-tabs-postmortem.md`).
- **Baseline:** 297 tests pass, 0 failures (3 e2e ignored) on branch `render-line-unit`. Keep it there after every task.
- **Line text convention:** every `Line.text` includes exactly one trailing `'\n'`. Spacer lines are `Line { text: "\n", .. }`.
- Run the full suite with `cargo test --all-features`. Commit after every task with the suite green.

---

## File Structure

- **Modify:** `src/render.rs` — the entire change lives here. New private `Line`/`LineBg` types, new `render_row`/`render_header`/`render_strip`/`assemble` shapes, deletions of `row_lines`, `render_row_buffer`, the duplicated `MAX_PANE_LINES`, and the parallel push pairs.
- **Modify:** `CONTEXT.md` — sharpen the "Lockstep" entry (Task 6) to record that lockstep now holds structurally.

### Key existing signatures the tasks reference (read these in `src/render.rs`)

```
const RESET: &str = "\x1b[0m";                                   // :10
struct CardSpacing { pad_x, pad_y, gap }                          // :167
fn card_spacing(d: Density) -> CardSpacing                        // :175
fn tc_bg(c: (u8,u8,u8)) -> String                                 // :832
fn card_tint(row: &TabRow, theme: &DerivedColors) -> String       // :844  (returns a bg ANSI escape)
fn paint_card_line(line: &str, width: usize, bg: &str) -> String  // :864  (returns text ending in '\n')
fn target_for_row(row: &TabRow) -> RailTarget                     // :886
struct RailTarget { pub tab_position: usize, pub pane_id: Option<u32> }  // :196
struct RenderedRail { pub ansi: String, targets: Vec<Option<RailTarget>> } // :202
fn render(rows, opts) -> String                                   // :1057 (test-only wrapper: returns rendered_rail.ansi)
crate::status::working_spin(frame: usize) -> char
```

---

## Task 1: Introduce `Line`, `LineBg`, and the single derive point `from_lines`

**Files:**
- Modify: `src/render.rs` (add types near `RenderedRail` ~:200; add `from_lines` to the `impl RenderedRail` block ~:207; add tests in the `mod tests` block)

**Interfaces:**
- Produces:
  - `enum LineBg { None, Rail, Card, ActiveChild }` (private, `#[derive(Clone, Copy, Debug, PartialEq, Eq)]`)
  - `struct Line { text: String, target: Option<RailTarget>, bg: LineBg }` (private, `#[derive(Clone, Debug)]`)
  - `impl RenderedRail { fn from_lines(lines: Vec<Line>) -> RenderedRail }` — joins `text` (already painted/final), collects `target` 1:1, pops one trailing `'\n'`. **Ignores `bg`** (bg is consumed during assembly before this point).

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `src/render.rs`:

```rust
#[test]
fn from_lines_derives_ansi_and_targets_in_lockstep() {
    let t = RailTarget { tab_position: 2, pane_id: None };
    let lines = vec![
        Line { text: "alpha\n".into(), target: Some(t), bg: LineBg::None },
        Line { text: "beta\n".into(),  target: None,    bg: LineBg::None },
        Line { text: "gamma\n".into(), target: Some(RailTarget { tab_position: 3, pane_id: Some(9) }), bg: LineBg::None },
    ];
    let rr = RenderedRail::from_lines(lines);
    // ansi: joined, trailing newline popped.
    assert_eq!(rr.ansi, "alpha\nbeta\ngamma");
    // targets: 1:1 with lines, never off-by-one.
    assert_eq!(rr.line_count(), 3);
    assert_eq!(rr.target_at_line(0), Some(t));
    assert_eq!(rr.target_at_line(1), None);
    assert_eq!(rr.target_at_line(2), Some(RailTarget { tab_position: 3, pane_id: Some(9) }));
    assert_eq!(rr.target_at_line(3), None);
    // Structural lockstep: every '\n'-terminated segment has a target slot.
    assert_eq!(rr.ansi.split('\n').count(), rr.line_count());
}

#[test]
fn from_lines_empty_is_empty() {
    let rr = RenderedRail::from_lines(vec![]);
    assert_eq!(rr.ansi, "");
    assert_eq!(rr.line_count(), 0);
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --all-features from_lines 2>&1 | tail -20`
Expected: FAIL — `cannot find type Line` / `LineBg` / `no function from_lines`.

- [ ] **Step 3: Add the types and `from_lines`**

Add the types just above `pub struct RenderedRail` (~:200):

```rust
/// Surface class a line sits on (Cards density only); resolved to a concrete
/// bg escape during assembly using the owning row. `None` = never painted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineBg {
    None,
    Rail,        // dark panel base (rail_bg): header, gaps, idle strip
    Card,        // this row's card surface (card_tint of the owning row)
    ActiveChild, // active multi-pane child line (surface_agent)
}

/// One physical rail line and the click target it resolves to. `text` always
/// ends in exactly one '\n'. The unit of rendering: ansi, targets, and
/// footprint all derive from a `Vec<Line>`, so they cannot drift.
#[derive(Clone, Debug)]
struct Line {
    text: String,
    target: Option<RailTarget>,
    bg: LineBg,
}
```

Add to the `impl RenderedRail` block (after `empty()` ~:211):

```rust
/// The single derive point: `ansi` and `targets` come from one `Vec<Line>`,
/// so they are always in 1:1 correspondence. `text` is already final
/// (painted during assembly); `bg` is ignored here. The trailing newline of
/// the last line is popped to prevent vt100 scroll in the test harness.
fn from_lines(lines: Vec<Line>) -> RenderedRail {
    let mut ansi = String::new();
    let mut targets = Vec::with_capacity(lines.len());
    for line in lines {
        ansi.push_str(&line.text);
        targets.push(line.target);
    }
    if ansi.ends_with('\n') {
        ansi.pop();
    }
    RenderedRail { ansi, targets }
}
```

- [ ] **Step 4: Run to verify pass + full suite still green**

Run: `cargo test --all-features 2>&1 | grep -E "test result:|error\[" | tail`
Expected: all `ok`, 299 passed (297 baseline + 2 new), 0 failed.

- [ ] **Step 5: Commit**

```bash
git add src/render.rs
git commit -m "feat(render): add Line/LineBg + RenderedRail::from_lines single derive point

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Convert `render_row` to return `Vec<Line>` (full fidelity, no budget/callback)

**Files:**
- Modify: `src/render.rs` — `render_row` (~:542), `emit_pane_line` (~:764), `render_row_buffer` (~:893, DELETE), the two call sites in `render_rail` (~:991, ~:1012).

**Interfaces:**
- Consumes: `Line`/`LineBg` from Task 1.
- Produces: `fn render_row(row: &TabRow, opts: &RenderOpts) -> Vec<Line>` — returns **raw (unpainted)** lines at full fidelity (no overflow truncation), each with its `target` and semantic `bg`:
  - single-pane line 1 (identity), multi-pane header line, `+N more` line, single-pane line 2 (detail) → `bg: LineBg::Card`, `target: target_for_row(row)`.
  - multi-pane child pane lines → `bg: LineBg::ActiveChild` **iff** `row.active` (matches today's `rows[i].active && is_multi_pane && line_idx > 0` rule), else `LineBg::Card`; `target: RailTarget { tab_position, pane_id: Some(pane.pane_id()) }`.

- [ ] **Step 1: Rewrite `render_row` signature + body**

Change `fn render_row<F>(out: &mut String, row, opts, max_lines, mut record_target: F)` to:

```rust
fn render_row(row: &TabRow, opts: &RenderOpts) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    // ... existing body, UNCHANGED string-building logic ...
}
```

Mechanical transformation rules (apply to the existing body verbatim — do not change any formatting/width/color math, so bytes stay identical):
1. Every `out.push_str(&format!("...\n", ...))` that builds a row line becomes:
   ```rust
   lines.push(Line { text: format!("...\n", ...), target: <the target>, bg: <Card|ActiveChild> });
   ```
   Pair each former `record_target(Some(X))` with the `target: Some(X)` on the same pushed `Line`. (Today the line is pushed then `record_target` is called next — fuse them.)
2. **Delete** all `max_lines` budget checks: `if max_lines <= 1 { return; }`, `if emitted >= max_lines`, `if emitted < max_lines`, and the `let mut emitted` counter. `render_row` now emits the FULL line set unconditionally; truncation moves to the assembler (Task 5). The `take(show)` over `MAX_PANE_LINES` in the multi-pane path STAYS (that is fidelity, not overflow).
3. The single-pane line-1 always pushes (`bg: Card`). Line-2 detail pushes only under its existing `if let Some(d)` + `!d.msg.trim().is_empty()` condition (drop the `emitted < max_lines` part of that condition), `bg: Card`.
4. `end with `lines` (return the Vec).

For the multi-pane child lines, `emit_pane_line` must now return a painted-free `String` and the caller tags `bg`. Change `emit_pane_line` from `(out: &mut String, ...)` to:
```rust
fn emit_pane_line(pane, opts, tab_active, idle_color, dim_strong) -> String { /* return the format! string ending in '\n' instead of out.push_str */ }
```
and in `render_row`:
```rust
let text = emit_pane_line(pane, opts, row.active, &idle_color, &dim_strong);
lines.push(Line {
    text,
    target: Some(RailTarget { tab_position: tab_target.tab_position, pane_id: Some(pane.pane_id()) }),
    bg: if row.active { LineBg::ActiveChild } else { LineBg::Card },
});
```

- [ ] **Step 2: Delete `render_row_buffer` and adapt the two `render_rail` call sites (temporary glue)**

Delete `render_row_buffer` (~:893) and its `debug_assert_eq!`. In `render_rail`, replace each `let (tab_buf, row_targets) = render_row_buffer(&rows[i], opts, max_lines);` + its `split_inclusive('\n')` loop with temporary glue that preserves today's behavior exactly:

Cards branch (~:991):
```rust
let bg = card_tint(&rows[i], &opts.theme);
let active_child_bg = tc_bg(opts.theme.surface_agent);
for (line_idx, line) in render_row(&rows[i], opts).into_iter().take(max_lines).enumerate() {
    let line_bg = match line.bg {
        LineBg::ActiveChild => &active_child_bg,
        _ => &bg,
    };
    out.push_str(&paint_card_line(&line.text, width, line_bg));
    targets.push(line.target);
    let _ = line_idx;
}
```
Non-cards branch (~:1012):
```rust
for line in render_row(&rows[i], opts).into_iter().take(max_lines) {
    out.push_str(&line.text);
    targets.push(line.target);
}
```

> Note: this is throwaway glue — Task 5 deletes these loops. It exists only to keep the suite green between tasks. The `.take(max_lines)` here is the truncation that was formerly done inside `render_row`.

- [ ] **Step 3: Run the full suite**

Run: `cargo test --all-features 2>&1 | grep -E "test result:|error\[|FAILED" | tail -20`
Expected: all `ok`, 299 passed, 0 failed. **Snapshots unchanged** (byte-identical `ansi`). If any snapshot fails, the line/target fusion changed bytes — fix the transformation; do NOT accept the snapshot.

- [ ] **Step 4: Commit**

```bash
git add src/render.rs
git commit -m "refactor(render): render_row returns Vec<Line>; drop max_lines/callback + render_row_buffer

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Header and strip producers return `Vec<Line>`

**Files:**
- Modify: `src/render.rs` — extract the header block (~:936-974) and strip block (~:1036-1048) of `render_rail` into producers.

**Interfaces:**
- Consumes: `Line`/`LineBg`.
- Produces:
  - `fn render_header(rows: &[TabRow], opts: &RenderOpts, overflow: bool) -> Vec<Line>` — 0 lines if `!opts.header`; in Cards density 1 line (title only); else 2 lines (title + `═` rule). Each `Line { target: None, bg: LineBg::Rail }`, **raw text** (NOT painted — assembler paints). Same title/count/rule string-building as today (move it verbatim, returning `format!`ed strings instead of `out.push_str`).
  - `fn render_strip(strip_folded: usize, opts: &RenderOpts) -> Vec<Line>` — 0 lines if `strip_folded == 0`; else 1 line `Line { text: format!("{}{}{}\n", Role::Accent.ansi(), truncate(&format!("+{} idle ▾", strip_folded), opts.width), RESET), target: None, bg: LineBg::Rail }`.

- [ ] **Step 1: Add the two producers** (move the existing string-building out of `render_rail`; the `count`/`overflow` computation stays in `render_rail` and is passed in).

- [ ] **Step 2: Wire them into `render_rail` with temporary glue** (still painting inline so bytes stay identical):

Header (replace ~:936-974):
```rust
for line in render_header(rows, opts, overflow) {
    if cards {
        out.push_str(&paint_card_line(&line.text, width, &rail));
    } else {
        out.push_str(&line.text);
    }
    targets.push(None);
}
```
Strip (replace ~:1036-1048):
```rust
for line in render_strip(strip_folded, opts) {
    if cards {
        out.push_str(&paint_card_line(&line.text, width, &rail));
    } else {
        out.push_str(&line.text);
    }
    targets.push(None);
}
```

> Subtlety to preserve byte-for-byte: today's header line 1 in Cards uses `paint_card_line(&title_line, width, &rail)`; line 2 (the rule) only exists in non-Cards. `render_header` returns 1 line in Cards / 2 otherwise, so the loop above reproduces this exactly.

- [ ] **Step 3: Run the full suite**

Run: `cargo test --all-features 2>&1 | grep -E "test result:|error\[|FAILED" | tail`
Expected: 299 passed, 0 failed, snapshots unchanged.

- [ ] **Step 4: Commit**

```bash
git add src/render.rs
git commit -m "refactor(render): extract render_header/render_strip as Vec<Line> producers

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: Decouple the planner from `TabRow`; render blocks first; delete `row_lines`

**Files:**
- Modify: `src/render.rs` — `plan_overflow` (~:317), `plan_layout` (~:436), `card_block_lines` (~:235), `render_rail` (reorder), DELETE `row_lines` (~:255).

**Interfaces:**
- Consumes: `render_row -> Vec<Line>` (Task 2), `Status`.
- Produces:
  - `fn plan_overflow(rows: &[RowMeta], body_budget: usize) -> (Vec<(usize, usize)>, usize)` where `struct RowMeta { status: Status, active: bool, full_lines: usize }`. Replace every `row_lines(&r.display, r.active)` with `r.full_lines`, every `rows[idx].display.status` with `rows[idx].status`, every `r.display.status` with `r.status`. Logic otherwise UNCHANGED (idle fold, calm-compress, urgent-compress, trim).
  - `fn plan_layout(metas: &[RowMeta], body_budget: usize, density: Density) -> (Vec<(usize, usize)>, usize, CardSpacing)` — same change; `card_block_lines` becomes `fn card_block_lines(full_lines: usize, spacing: CardSpacing) -> usize { spacing.pad_y + full_lines + spacing.gap }`.

- [ ] **Step 1: Add `RowMeta`, retype the planner functions, delete `row_lines`**

Add near `CardSpacing`:
```rust
struct RowMeta { status: Status, active: bool, full_lines: usize }
```
Apply the mechanical retype above. Delete `fn row_lines` and `fn is_multi_pane` **only if** no longer referenced (`is_multi_pane` is still used by the Cards active-child glue in Task 2 — keep it until Task 5; check with `grep -n is_multi_pane src/render.rs`). Keep `is_calm`.

- [ ] **Step 2: Reorder `render_rail` to render blocks first, then plan from their lengths**

Near the top of `render_rail` (after computing `width`/`cards`/`rail`), before `plan_layout`:
```rust
let blocks: Vec<Vec<Line>> = rows.iter().map(|r| render_row(r, opts)).collect();
let metas: Vec<RowMeta> = rows.iter().zip(&blocks)
    .map(|(r, b)| RowMeta { status: r.display.status, active: r.active, full_lines: b.len() })
    .collect();
let body_budget = opts.height.saturating_sub(header_lines(rows, opts.header, opts.density));
let (plan, strip_folded, spacing) = plan_layout(&metas, body_budget, opts.density);
```
Then in the per-row loop, **stop calling `render_row` again** — index the pre-rendered `blocks`. Replace `render_row(&rows[i], opts).into_iter().take(max_lines)` (from Task 2 glue) with `blocks[i].iter().cloned().take(max_lines)` (or drain via index). Keep the rest of the Task 2/3 glue intact.

> This is the crux of the deepening: `full_lines = blocks[i].len()` is now the *same object* the lines come from, so the count and the content provably agree. `row_lines` (the separate predictor) is gone.

- [ ] **Step 3: Run the full suite**

Run: `cargo test --all-features 2>&1 | grep -E "test result:|error\[|FAILED" | tail -20`
Expected: 299 passed, 0 failed, snapshots unchanged. The planner unit tests (`test_overflow_*`, `row_lines_by_state`, `row_lines_multi_pane_*`) will need their call sites updated to build `RowMeta` instead of `TabRow`+`row_lines` — update those test bodies to construct `RowMeta { status, active, full_lines }` (compute `full_lines` via `render_row(&row, &opts).len()` so they still assert real counts). Keep their assertions identical.

- [ ] **Step 4: Commit**

```bash
git add src/render.rs
git commit -m "refactor(render): planner reasons in RowMeta counts; render-first; delete row_lines predictor

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: Capstone — collapse `render_rail` to one flat `Vec<Line>` + `from_lines`

**Files:**
- Modify: `src/render.rs` — `render_rail` body (remove ALL `out.push_str`/`targets.push` pairs), `onboarding` (~:497, optional consistency), DELETE `is_multi_pane` if now unused.

**Interfaces:**
- Consumes: everything from Tasks 1-4.
- Produces: `render_rail` builds exactly one `Vec<Line>` and returns `RenderedRail::from_lines(flat)`. No `let mut out`, no `let mut targets`.

- [ ] **Step 1: Rewrite the `render_rail` body**

After computing `blocks`, `metas`, `plan`, `strip_folded`, `spacing`, `overflow`, `count`:

```rust
let cards = opts.density == Density::Cards;
let rail = tc_bg(opts.theme.rail_bg);
let mut flat: Vec<Line> = Vec::new();

// Header.
for mut line in render_header(rows, opts, overflow) {
    if cards { line.text = paint_card_line(&line.text, width, &rail); }
    flat.push(line);
}

// Body: one card block per kept row.
for &(i, budget) in &plan {
    let row_target = target_for_row(&rows[i]);
    let card_bg = card_tint(&rows[i], &opts.theme);
    let active_child_bg = tc_bg(opts.theme.surface_agent);

    // pad_y internal top padding — belongs to this card's click span.
    for _ in 0..spacing.pad_y {
        let text = if cards { paint_card_line("\n", width, &card_bg) } else { "\n".to_string() };
        flat.push(Line { text, target: Some(row_target), bg: LineBg::None });
    }

    // content (truncated to the planned budget == today's compression).
    for line in blocks[i].iter().cloned().take(budget) {
        let text = if cards {
            let bg = match line.bg { LineBg::ActiveChild => &active_child_bg, _ => &card_bg };
            paint_card_line(&line.text, width, bg)
        } else {
            line.text
        };
        flat.push(Line { text, target: line.target, bg: LineBg::None });
    }

    // gap external separation (dark panel base in Cards).
    for _ in 0..spacing.gap {
        let text = if cards { paint_card_line("\n", width, &rail) } else { "\n".to_string() };
        flat.push(Line { text, target: None, bg: LineBg::None });
    }
}

// Idle strip.
for mut line in render_strip(strip_folded, opts) {
    if cards { line.text = paint_card_line(&line.text, width, &rail); }
    flat.push(line);
}

RenderedRail::from_lines(flat)
```

Keep the early `if rows.is_empty() { return RenderedRail { ansi: String::new(), targets: vec![] }; }` guard (or `RenderedRail::from_lines(vec![])`).

> Verify against the old code: pad_y spacers carried `Some(row_target)`; gap spacers carried `None`; header/strip carried `None`; active multi-pane child content lines (idx>0) used `surface_agent`. All preserved above. `bg` is consumed here (text becomes final), so pushed lines carry `LineBg::None`.

- [ ] **Step 2: Delete now-dead helpers**

Run `grep -n "is_multi_pane\|fn render_row_buffer\|fn row_lines\|MAX_PANE_LINES" src/render.rs`. `render_row_buffer` and `row_lines` should already be gone (Tasks 2, 4). `MAX_PANE_LINES` should now appear exactly once (inside `render_row`). Delete `is_multi_pane` only if `grep` shows zero remaining callers.

- [ ] **Step 3: Run the full suite — this is the byte-identical gate**

Run: `cargo test --all-features 2>&1 | grep -E "test result:|error\[|FAILED" | tail -20`
Expected: 299 passed, 0 failed. **Every snapshot byte-identical.** If a snapshot moved, diff with `cargo insta pending-snapshots` / `git diff` and fix the assembler to match the old bytes — do NOT accept.

- [ ] **Step 4: Commit**

```bash
git add src/render.rs
git commit -m "refactor(render): collapse render_rail to one Vec<Line> + from_lines (Lockstep structural)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: Strengthen the lockstep guard + sharpen CONTEXT.md

**Files:**
- Modify: `src/render.rs` (add a property test), `CONTEXT.md` (Lockstep entry).

- [ ] **Step 1: Add a proptest that lockstep holds for arbitrary rails**

Add to the `mod tests` block (follow the existing `proptest!` usage in the file — `grep -n "proptest!" src/render.rs` for the pattern; reuse the existing row/opts generators if present, else build a small one):

```rust
proptest! {
    #[test]
    fn lockstep_holds_for_arbitrary_rails(
        rows in prop::collection::vec(arb_tab_row(), 0..8),
        width in 8usize..40,
        height in 1usize..30,
        density in prop_oneof![Just(Density::Compact), Just(Density::Comfortable), Just(Density::Cards)],
    ) {
        let opts = RenderOpts { width, height, density, ..ro() };
        let rr = render_rail(&rows, &opts);
        // Structural lockstep: ansi line slots == target slots, always.
        let ansi_lines = if rr.ansi.is_empty() { 0 } else { rr.ansi.split('\n').count() };
        prop_assert_eq!(ansi_lines, rr.line_count());
    }
}
```

> Do NOT add a `line_count() <= height` bound — the identity header emits unconditionally, so a tiny `height` legitimately exceeds the budget. The single invariant to assert is `ansi_lines == line_count()` across the input space — the structural lockstep Task 5 established. If no `arb_tab_row()` generator exists, write a minimal one producing varied `Status`, `active`, pane counts, and msgs. If a `proptest!`/`ro()` helper already exists, reuse it and adapt the names. `width`/`height` are still varied to exercise overflow and narrow-width paths.

- [ ] **Step 2: Run it**

Run: `cargo test --all-features lockstep 2>&1 | grep -E "test result:|FAILED" | tail`
Expected: PASS.

- [ ] **Step 3: Sharpen the CONTEXT.md Lockstep entry**

In `CONTEXT.md`, update the "## Lockstep" section's last lines so it records that lockstep is now **structural**. Replace the sentence beginning "It is verified as a property of `RenderedRail`…" with:

```
Lockstep is now structural, not discipline-held: `render_rail` builds a single
`Vec<Line>` where each line carries its own `RailTarget`, and `ansi`/`targets`/
line-count all derive from that one list via `RenderedRail::from_lines`. There is
no separate height predictor — a row's footprint is `block.len()` of the very
lines it renders — so the emitted ANSI and the click-target map cannot drift.
```

- [ ] **Step 4: Run the full suite + commit**

Run: `cargo test --all-features 2>&1 | grep -E "test result:" | tail`
Expected: 300 passed, 0 failed.

```bash
git add src/render.rs CONTEXT.md
git commit -m "test(render): proptest lockstep across densities; CONTEXT: lockstep now structural

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review Notes

- **Spec coverage:** render-first (T2,T4) · whole-rail Line scope (T3,T5) · semantic `LineBg` resolved at assembly (T5) · planner reasons in counts (T4) · `RenderedRail` frozen (T1, all) · byte-identical guard (every task) · structural lockstep test + CONTEXT (T6). All grilling decisions covered.
- **Type consistency:** `Line { text, target, bg }`, `LineBg { None, Rail, Card, ActiveChild }`, `RowMeta { status, active, full_lines }`, `render_row(&TabRow,&RenderOpts)->Vec<Line>`, `render_header(&[TabRow],&RenderOpts,bool)->Vec<Line>`, `render_strip(usize,&RenderOpts)->Vec<Line>`, `from_lines(Vec<Line>)->RenderedRail`, `card_block_lines(usize,CardSpacing)->usize` — consistent across tasks.
- **Risk:** the only behavior-bearing task is T5; T2-T4 carry throwaway glue specifically so the suite stays green between commits. The snapshot suite is the complete characterization guard.
```
