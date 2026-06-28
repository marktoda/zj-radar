# Deepen the Rail Seam — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Collapse the renderer onto a single deep seam — `render_rail`/`onboarding` returning `RenderedRail` — so the test surface *is* the seam, click-targeting is verified at the width it's drawn, and the leaked layout/color internals stop being public.

**Architecture:** `render.rs` currently exposes 14 `pub fn`/`struct` beyond its real interface; the runtime resolves clicks against a cached `last_rendered` in production but tests re-render at a hardcoded width. We seal the interface to `render_rail`, `onboarding`, `RenderedRail` (+ accessors), `RenderOpts`, `TabRow`, `RailTarget`; delete the test-only `ColorMode` knob (preserving the color-is-additive invariant); promote lockstep to a proptest on `RenderedRail`; and migrate the click suite through the production `render → last_rendered → target_at_line` path. The 223-line `render_row` is **out of scope** (interface stays deep regardless of its internals).

**Tech Stack:** Rust, `zellij-tile = "0.44"`, `wasm32-wasip1` target, `insta` (snapshots), `proptest`, `vt100` (grid parsing). Host-tested modules carry no `zellij-tile` dep.

## Global Constraints

- **No rustfmt.** Match surrounding style by hand.
- **Two build targets must stay green:** host (`cargo test --all-features`) AND wasm (`cargo build --release --target wasm32-wasip1`). `lib.rs`'s `ZellijPlugin` impl is `#[cfg(target_arch = "wasm32")]`.
- **No new blocking host calls** (push-driven constraint; irrelevant to this refactor but never violate).
- **Snapshots are behavior:** production render output is unchanged by this work (color stays always-on at `Truecolor`), so `cargo insta` must report **no drift**. A snapshot change means a real regression — stop and investigate, do not `insta accept`.
- **Demotions are the point:** after Tasks 4–5, `grep -rE 'render::(plan_layout|plan_overflow|row_lines|card_block_lines|is_multi_pane|pane_tree_plan|header_lines|card_spacing|format_elapsed)' src/ | grep -v src/render.rs` must return **empty**, and `ColorMode` / `from_ansi_without_targets` must not appear in any non-`render.rs` file.

---

### Task 1: Lockstep proptest on `RenderedRail` (the safety net)

Add the seam-level lockstep property *before* any visibility/removal change, so every later task is guarded. This must pass against current code unchanged.

**Files:**
- Modify: `src/render.rs` (add tests inside the existing `#[cfg(test)] mod tests`, near the proptest at ~line 4334)

**Interfaces:**
- Consumes: `render_rail(rows: &[TabRow], opts: &RenderOpts) -> RenderedRail`, `RenderedRail::{line_count, target_at_line}`, existing test helper `arb_rows()` (proptest strategy, ~line 4322), existing `ro(width, now_tick) -> RenderOpts`.
- Produces: nothing new (test-only).

- [ ] **Step 1: Write the failing test** — add to `mod tests`:

```rust
proptest! {
    /// Lockstep: the emitted ANSI and the click-target map stay in exact
    /// 1:1 line correspondence, at every width the rail can be drawn at.
    #[test]
    fn render_rail_lockstep_lines_match_targets(
        rows in arb_rows(),
        width in 8usize..=120,
        height in 1usize..=60,
    ) {
        let mut opts = ro(width, 0);
        opts.height = height;
        let rail = render_rail(&rows, &opts);
        // 1:1 correspondence between physical lines and target slots.
        prop_assert_eq!(rail.line_count(), rail.ansi.matches('\n').count());
        // Every in-range line resolves without panic; out-of-range is None.
        for line in 0..rail.line_count() {
            let _ = rail.target_at_line(line as isize);
        }
        prop_assert_eq!(rail.target_at_line(-1), None);
        prop_assert_eq!(rail.target_at_line(rail.line_count() as isize), None);
    }
}
```

- [ ] **Step 2: Run to verify it passes against current behavior**

Run: `cargo test --all-features render_rail_lockstep -- --nocapture`
Expected: PASS (current code already maintains the invariant; this locks it in). If it FAILS, a pre-existing lockstep bug exists — stop and report before continuing.

- [ ] **Step 3: Add the empty/onboarding-shape guard** — confirm the invariant also holds for an empty rail:

```rust
#[test]
fn render_rail_empty_has_zero_lines_and_no_targets() {
    let opts = ro(24, 0);
    let rail = render_rail(&[], &opts);
    assert_eq!(rail.line_count(), 0);
    assert_eq!(rail.ansi, "");
    assert_eq!(rail.target_at_line(0), None);
}
```

- [ ] **Step 4: Run the full render suite**

Run: `cargo test --all-features --lib render`
Expected: PASS (all existing render tests + the two new ones).

- [ ] **Step 5: Commit**

```bash
git add src/render.rs
git commit -m "test(render): lockstep proptest — ansi lines ⇄ targets across all widths

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: `onboarding` returns `RenderedRail`; privatize `from_ansi_without_targets`

Unify the onboarding entry on `RenderedRail` so the runtime stops bridging String→rail, and drop the bridging constructor from the public surface.

**Files:**
- Modify: `src/render.rs:507` (`onboarding` signature + body), `src/render.rs:150` (`from_ansi_without_targets` visibility), `src/render.rs:1209` (test using it)
- Modify: `src/runtime.rs:274` (drop the `from_ansi_without_targets` wrap)

**Interfaces:**
- Consumes: `RenderedRail` (struct), the private None-per-line construction logic currently in `from_ansi_without_targets`.
- Produces: `onboarding(opts: &RenderOpts) -> RenderedRail` (was `-> String`). All lines map to `None` targets; `line_count()` equals the emitted line count (invariant preserved).

- [ ] **Step 1: Write the failing test** — add to `mod tests`:

```rust
#[test]
fn onboarding_returns_rail_with_no_targets_but_matching_line_count() {
    let opts = ro(24, 0);
    let rail = onboarding(&opts);
    assert!(rail.line_count() > 0, "onboarding paints a panel");
    assert_eq!(rail.line_count(), rail.ansi.matches('\n').count());
    for line in 0..rail.line_count() {
        assert_eq!(rail.target_at_line(line as isize), None,
            "onboarding has no clickable rows");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --all-features onboarding_returns_rail -- --nocapture`
Expected: FAIL to compile — `onboarding` returns `String`, no `.line_count()`.

- [ ] **Step 3: Change `onboarding` to build a `RenderedRail`** — at `src/render.rs:507`, change the signature and wrap the final string. The body already builds `out: String`; replace the final `out` return with the None-per-line rail:

```rust
pub fn onboarding(opts: &RenderOpts) -> RenderedRail {
    let mut out = String::new();
    // ... existing body unchanged, building `out` ...
    RenderedRail::from_ansi_without_targets(out)
}
```

- [ ] **Step 4: Privatize the bridging constructor** — at `src/render.rs:150`, drop `pub` (keep the `#[cfg_attr]`):

```rust
    fn from_ansi_without_targets(ansi: String) -> Self {
        let targets = ansi.lines().map(|_| None).collect();
        RenderedRail { ansi, targets }
    }
```

- [ ] **Step 5: Update the runtime call site** — `src/runtime.rs:273-277`, drop the wrap:

```rust
        let rail = if !self.permission_granted || tabrows.is_empty() {
            render::onboarding(&opts)
        } else {
            render::render_rail(&tabrows, &opts)
        };
```

- [ ] **Step 6: Fix the in-file test at `render.rs:1209`** — it calls `from_ansi_without_targets` directly; since that's now private but the test is in the same module, it still compiles. Confirm it reads:

```rust
        let untargeted = RenderedRail::from_ansi_without_targets("a\nb\n".to_string());
```

(No change needed — same-module access. If the compiler complains about privacy, it does not: `mod tests` sees `super::*`.)

- [ ] **Step 7: Run host tests + wasm build**

Run: `cargo test --all-features --lib && cargo build --release --target wasm32-wasip1`
Expected: PASS + wasm compiles. `from_ansi_without_targets` now has no external caller.

- [ ] **Step 8: Commit**

```bash
git add src/render.rs src/runtime.rs
git commit -m "refactor(render): onboarding returns RenderedRail; privatize bridging ctor

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Remove the `ColorMode` knob; preserve color-is-additive

Color is always-on in production. Delete the test-only `ColorMode { Truecolor | None }` toggle and its 11 branches, collapse the mode-gated color helpers, and reframe the one suppression test into a single-render SGR-strip that still proves additivity.

**Files:**
- Modify: `src/render.rs` (delete `enum ColorMode` ~line 20; drop `color` field from `RenderOpts` ~line 31; collapse `tc_bg_mode`/`tc_fg_mode` ~lines 860/869 → unconditional; remove 11 `if color_mode == ColorMode::None` branches; update 21 test-opts constructors that set `color:`; reframe additivity test ~line 3903)
- Modify: `src/runtime.rs:271` (drop `color:` field from the one production `RenderOpts`)

**Interfaces:**
- Consumes: existing `tc_fg(c) -> String` (the unconditional truecolor fg, ~line 878), `strip_sgr(&str) -> String` test helper (used by additivity test), `grid(raw, width)` test helper.
- Produces: `RenderOpts` with 7 fields (no `color`). `tc_bg(c) -> String` / `tc_fg(c) -> String` unconditional helpers (rename `tc_bg_mode`→`tc_bg`, fold `tc_fg_mode`→ existing `tc_fg`).

- [ ] **Step 1: Reframe the additivity test FIRST (it defines the invariant we must keep)** — replace the body at `src/render.rs:~3870-3938` (`no_color` test). The new test renders once at `Truecolor` and asserts the *visible grid* (SGR stripped) matches the structural expectation, proving color is additive over a fixed layout:

```rust
#[test]
fn color_is_purely_additive_over_a_fixed_layout() {
    let rows = vec![
        TabRow {
            number: 1, name: "agent".into(), active: true, has_bell: false,
            agg: agg(Status::Pending, 0, 1, Some("needs you")),
        },
        TabRow {
            number: 2, name: "idle".into(), active: false, has_bell: false,
            agg: agg(Status::Idle, 0, 0, None),
        },
    ];
    let out = render(&rows, &ro(30, 0));
    // Stripping all SGR leaves a clean character grid (no escape residue),
    // i.e. color sits *on top of* layout and never alters the cell content.
    let stripped = strip_sgr(&out);
    assert!(!stripped.contains('\x1b'), "no escape residue after strip: {stripped:?}");
    assert!(stripped.contains("agent"));
    assert!(stripped.contains("idle"));
    // Visible width per line is unchanged by color (color adds zero columns).
    for line in stripped.lines() {
        assert!(visible_width(line) <= 30, "line exceeds width: {line:?}");
    }
}
```

- [ ] **Step 2: Run it (passes against current code, still has ColorMode)**

Run: `cargo test --all-features color_is_purely_additive -- --nocapture`
Expected: PASS (uses only `Truecolor` path + `strip_sgr`).

- [ ] **Step 3: Delete the `ColorMode` enum and the `color` field**

In `src/render.rs`: delete `pub enum ColorMode { Truecolor, None }` (~line 20) and remove the `pub color: ColorMode,` line from `RenderOpts` (~line 31).

- [ ] **Step 4: Collapse the color helpers** — at `src/render.rs:860-877`:

```rust
fn tc_bg(c: (u8, u8, u8)) -> String {
    format!("\x1b[48;2;{};{};{}m", c.0, c.1, c.2)
}
// tc_fg already exists unconditionally at ~line 878 — delete tc_fg_mode and
// point its callers at tc_fg.
```

- [ ] **Step 5: Remove the 11 `ColorMode::None` branches** — each `let x = if color_mode == ColorMode::None { "" } else { Role::Foo.ansi() };` collapses to `let x = Role::Foo.ansi();`. The mode-gated `tc_bg_mode(c, color_mode)` calls become `tc_bg(c)`; `tc_fg_mode(c, color_mode)` become `tc_fg(c)`. Delete now-unused `let color_mode = opts.color;` bindings. Use the compiler to find every site:

Run: `cargo build --all-features 2>&1 | grep -E 'ColorMode|color_mode|color:' | head -40`
Then fix each reported site.

- [ ] **Step 6: Update the production `RenderOpts`** — `src/runtime.rs:261-272`, delete the `color: render::ColorMode::Truecolor,` line (and its sibling at the soon-deleted helper ~line 344 — Task 5 removes that helper; for now drop the field there too so it compiles).

- [ ] **Step 7: Update the 21 test-opts constructors** — drop every `color: ColorMode::Truecolor,` line in `src/render.rs` tests (the `ro()` helper at ~line 1163 and inline opts). Compiler-driven:

Run: `cargo test --all-features --no-run 2>&1 | grep -E 'color|ColorMode' | head -40`
Fix each.

- [ ] **Step 8: Run full suite + snapshots + wasm**

Run: `cargo test --all-features && cargo insta test --review=no 2>/dev/null; cargo build --release --target wasm32-wasip1`
Expected: PASS, **zero snapshot drift** (production output unchanged), wasm compiles. If any `.snap` drifts, a real regression slipped in — investigate, do not accept.

- [ ] **Step 9: Commit**

```bash
git add src/render.rs src/runtime.rs
git commit -m "refactor(render): remove test-only ColorMode knob; keep color additive

Color is always-on Truecolor in production; the None toggle existed only for
one test. Reframe that test to strip SGR from a single render and assert the
visible grid, preserving the color-is-purely-additive invariant without a
render-time on/off mode.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Migrate the `row_lines` tests, then seal the layout surface

Move the only external consumers of layout internals (`lib.rs`'s 3 `row_lines` tests) into the renderer or onto `RenderedRail`, then demote every layout primitive + the `render` String wrapper to private. This is what makes the module actually deep.

**Files:**
- Modify: `src/lib.rs:722, 854, 1043` (the 3 tests calling `render::row_lines`)
- Modify: `src/render.rs` — drop `pub` from `format_elapsed`, `card_spacing`, `CardSpacing`, `card_block_lines`, `is_multi_pane`, `pane_tree_plan`, `PaneTreePlan`, `row_lines`, `header_lines`, `plan_overflow`, `plan_layout`; make `render` (~line 1133) `#[cfg(test)]`

**Interfaces:**
- Consumes: `RenderedRail::line_count`, the now-private layout fns (reachable from `render.rs`'s own `mod tests` via `super::*`).
- Produces: a sealed public surface — only `render_rail`, `onboarding`, `RenderedRail` (+ `empty`, `target_at_line`, `line_count`), `RenderOpts`, `TabRow`, `RailTarget`, `ColorMode`-free.

- [ ] **Step 1: Relocate the two unit assertions** — `lib.rs`'s `multi_pane_tree_click_mapping_lockstep` (722) and `click_mapping_cards_pad_y_and_post_content_row` (854) assert specific `row_lines` counts. These are render-layout facts; move the `row_lines`-asserting portion into `render.rs`'s `mod tests` as direct calls (private access works in-file). In `render.rs` add:

```rust
#[test]
fn multi_pane_collapsed_footprint_is_header_plus_expanded_plus_collapse() {
    // header(implicit) + 1 expanded pane + collapse line = 3 content lines
    let a = agg_multi(/* 1 active pane + N calm */);
    assert_eq!(row_lines(&a, true), 3);
}
```

(Use the existing multi-pane agg builder in `render.rs` tests; mirror the exact scenario from the old `lib.rs` test so coverage is preserved.) Then **delete** the `render::row_lines(...)` assertions from the two `lib.rs` tests, keeping their click-mapping assertions (which Task 5 re-points at `last_rendered`).

- [ ] **Step 2: Reframe the proptest** — `lib.rs`'s `click_round_trip_hits_drawn_target` (1043) sums `render::row_lines` to know body height. Replace the height derivation with the rendered rail's own `line_count()` (after Task 5 this renders through `last_rendered`; for now, render directly):

```rust
// was: let total_body: usize = rows.iter().map(|r| render::row_lines(...)).sum();
// now: drive the real render and walk its actual lines.
let mut st = /* state with rows + permission granted */;
let ansi = st.runtime.render(/*rows*/ height, /*cols*/ 80);
let rail_lines = ansi.matches('\n').count();
for line in 0..rail_lines as isize {
    // every drawn body line resolves to a real tab or a deliberate None
    let _ = st.runtime.target_at_line(line); // accessor added in Task 5
}
```

(If Task 5 isn't done yet, temporarily keep this using `render_rail(&rows, &opts).line_count()` so the task stays green standalone; Task 5 finalizes it.)

- [ ] **Step 3: Run to confirm the migrated tests pass**

Run: `cargo test --all-features multi_pane_collapsed_footprint click_round_trip click_mapping_cards multi_pane_tree_click`
Expected: PASS.

- [ ] **Step 4: Demote the layout surface** — in `src/render.rs`, remove `pub` from each of: `format_elapsed`, `card_spacing`, `CardSpacing` (struct), `card_block_lines`, `is_multi_pane`, `pane_tree_plan`, `PaneTreePlan` (struct), `row_lines`, `header_lines`, `plan_overflow`, `plan_layout`. Mark the `render` wrapper test-only:

```rust
#[cfg(test)]
fn render(rows: &[TabRow], opts: &RenderOpts) -> String {
    render_rail(rows, opts).ansi
}
```

- [ ] **Step 5: Prove the seal** — no external caller of any demoted item remains:

Run:
```bash
grep -rE 'render::(plan_layout|plan_overflow|row_lines|card_block_lines|is_multi_pane|pane_tree_plan|header_lines|card_spacing|format_elapsed|CardSpacing|PaneTreePlan)' src/ | grep -v 'src/render.rs'
```
Expected: **empty output**.

- [ ] **Step 6: Run full suite + wasm build**

Run: `cargo test --all-features && cargo build --release --target wasm32-wasip1`
Expected: PASS + wasm compiles. Any compile error names a missed external caller — fix it.

- [ ] **Step 7: Commit**

```bash
git add src/render.rs src/lib.rs
git commit -m "refactor(render): seal layout surface — only render_rail/onboarding are public

Relocate the row_lines height assertions into render.rs (private, in-file) and
reframe the click round-trip to walk the rendered rail. Demote plan_layout,
plan_overflow, row_lines, card_block_lines, is_multi_pane, pane_tree_plan,
header_lines, card_spacing and friends to private; render() is now test-only.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Fix the click-resolution test seam — resolve against the real `last_rendered`

Delete the re-rendering test helper (hardcoded `width: 80`, duplicate `RenderOpts`) and make click tests resolve against the rail the runtime actually drew, at an explicit width. Add width-generality and a `mouse_click → Effect` end-to-end check.

**Files:**
- Modify: `src/runtime.rs:310-348` (delete `rendered_target_for_current_rows`; re-point the two `#[cfg(test)]` accessors at `self.last_rendered`)
- Modify: `src/lib.rs:108-122` (the `State` test accessors) and the ~40 click assertions in `src/lib.rs:431-880`

**Interfaces:**
- Consumes: `self.last_rendered: RenderedRail` (already written by `render()` at `runtime.rs:279`), `RenderedRail::target_at_line`, `mouse_click(line) -> Outcome`, `Effect::{SwitchTab, ShowPane}`.
- Produces: `#[cfg(test)] pub(crate) fn target_at_line(&self, line) -> Option<RailTarget>` on `PluginRuntime`, delegating to `self.last_rendered` (no re-render). The `State` test harness renders at an explicit width before resolving.

- [ ] **Step 1: Add the real-rail accessor + delete the re-render helper** — in `src/runtime.rs`, replace `rendered_target_for_current_rows` and its two callers with delegators to the cached rail:

```rust
#[cfg(test)]
pub(crate) fn target_at_line(&self, line: isize) -> Option<(usize, Option<u32>)> {
    let t = self.last_rendered.target_at_line(line)?;
    Some((t.tab_position, t.pane_id))
}

#[cfg(test)]
pub(crate) fn tab_position_at_line(&self, line: isize) -> Option<usize> {
    self.target_at_line(line).map(|(pos, _)| pos)
}
```

Delete `rendered_target_for_current_rows` (the `width: 80` re-render), `target_at_line_for_current_rows`, and `tab_position_at_line_for_current_rows`.

- [ ] **Step 2: Make the `State` harness render before resolving** — in `src/lib.rs:108-122`, the test `State` accessors must drive a real render at an explicit width first. Update so callers render once, then resolve:

```rust
#[cfg(test)]
impl State {
    /// Render at an explicit width (sidebar default is 24; click tests
    /// historically asserted the width-80 layout — keep that explicit here).
    fn render_at(&mut self, width: usize) {
        self.runtime.permission_granted = true;
        let _ = self.runtime.render(/* rows/height */ 100, width);
    }
    fn tab_position_at_line(&mut self, line: isize) -> Option<usize> {
        self.render_at(80);
        self.runtime.tab_position_at_line(line)
    }
}
```

(If a test sets a non-default height, thread it through `render_at`. The key change: the layout being clicked is the layout that was drawn, at a width the test names.)

- [ ] **Step 3: Run the migrated click suite**

Run: `cargo test --all-features --lib tab_position_at_line`
Expected: PASS — the ~40 assertions still hold (same width 80, now via the production path).

- [ ] **Step 4: Add a `mouse_click → Effect` end-to-end test** — prove the genuine production seam (render → cache → click → effect), including the permission gate:

```rust
#[test]
fn mouse_click_on_tab_row_emits_switch_tab_effect() {
    let mut st = /* state: 2 tabs, agent on tab 0 */;
    st.runtime.permission_granted = true;
    let _ = st.runtime.render(100, 80);
    // line 2 is the first tab content row (lines 0-1 are the header)
    let outcome = st.runtime.mouse_click(2);
    assert_eq!(outcome.effects, vec![Effect::SwitchTab { position: 0 }]);
}

#[test]
fn mouse_click_without_permission_is_inert() {
    let mut st = /* state with tabs */;
    st.runtime.permission_granted = false;
    let _ = st.runtime.render(100, 80);
    assert!(st.runtime.mouse_click(2).effects.is_empty());
}
```

- [ ] **Step 5: Add width-generality to the click round-trip** — finalize `click_round_trip_hits_drawn_target` (from Task 4) to render through `last_rendered` across several widths including the real default **24**:

```rust
proptest! {
    #[test]
    fn click_round_trip_hits_drawn_target(
        rows in /* arb rows strategy */,
        width in prop::sample::select(vec![24usize, 40, 80]),
    ) {
        let mut st = /* state seeded from rows, permission granted */;
        let ansi = st.runtime.render(100, width);
        for line in 0..ansi.matches('\n').count() as isize {
            // a drawn line resolves to a real tab/pane or a deliberate None;
            // never an out-of-range tab position.
            if let Some((pos, _pane)) = st.runtime.target_at_line(line) {
                prop_assert!(pos < /* tab count */);
            }
        }
    }
}
```

- [ ] **Step 6: Prove the re-render helper is gone**

Run: `grep -rn 'rendered_target_for_current_rows\|width: 80\|_for_current_rows' src/`
Expected: **empty** (no re-render helper, no hardcoded width 80).

- [ ] **Step 7: Run full suite + wasm build**

Run: `cargo test --all-features && cargo build --release --target wasm32-wasip1`
Expected: PASS + wasm compiles.

- [ ] **Step 8: Commit**

```bash
git add src/runtime.rs src/lib.rs
git commit -m "refactor(runtime): resolve clicks against the real last_rendered rail

Delete the test-only re-render helper that scored clicks against a fresh rail
at a hardcoded width 80 — divergent from the width the user actually saw.
Click tests now render at an explicit width and resolve through the production
render → last_rendered → target_at_line path, with a mouse_click→Effect e2e
check and width-24 round-trip coverage.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: Cohesion sweep + RenderOpts assembled-once check

Final verification that the collapse is complete and the abstraction layers are clean.

**Files:** none (verification + any small fixes surfaced).

- [ ] **Step 1: One assembly site for `RenderOpts`** — production builds it in exactly one place now:

Run: `grep -rn 'RenderOpts {' src/ | grep -v 'src/render.rs'`
Expected: a single hit at `src/runtime.rs` (the production `render` path). If two appear, a stray assembly remains.

- [ ] **Step 2: Public surface audit** — confirm the sealed interface:

Run: `grep -nE '^\s*pub (fn|struct|enum) ' src/render.rs`
Expected only: `RenderOpts`, `TabRow`, `RenderedRail`, `RailTarget`, `render_rail`, `onboarding` (+ `RenderedRail`'s `pub fn` accessors `empty`/`target_at_line`/`line_count`). No `plan_*`, `row_lines`, `card_*`, `ColorMode`, `format_elapsed`, `header_lines`, `is_multi_pane`, `pane_tree_plan`.

- [ ] **Step 3: Dead-code check** — no `#[allow(dead_code)]` masking newly-orphaned helpers:

Run: `cargo build --all-features 2>&1 | grep -i 'warning' | head` and `cargo build --release --target wasm32-wasip1 2>&1 | grep -i warning | head`
Expected: no new dead-code warnings. Remove any helper the refactor orphaned.

- [ ] **Step 4: Full green across all surfaces**

Run: `just test && cargo build --release --target wasm32-wasip1`
Expected: PASS (host all-features) + wasm builds.

- [ ] **Step 5: Snapshot integrity**

Run: `cargo insta test --review=no 2>/dev/null || cargo test --all-features`
Expected: no `.snap` drift.

- [ ] **Step 6: Commit any sweep fixes**

```bash
git add -A
git commit -m "chore(render): cohesion sweep — single RenderOpts assembly, no orphans

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review (run after writing, before execution)

**1. Spec coverage** (the seven grilling decisions):
- Q1 seal interface → Task 4 (demotions) + Task 6 (audit). ✓
- Q2 `onboarding -> RenderedRail`, kill `from_ansi_without_targets` public → Task 2. ✓
- Q3 `render` → `#[cfg(test)]` → Task 4 Step 4. ✓
- Q4 remove `ColorMode`, preserve additivity → Task 3. ✓
- Q5 lockstep proptest on `RenderedRail`, migrate `row_lines` tests → Task 1 + Task 4. ✓
- Q6 delete re-render helper, click via `last_rendered` at explicit width → Task 5. ✓
- Q7 `render_row` out of scope → not touched. ✓
- Cohesion with gutter-rail spec / additivity invariant → Task 3 test + CONTEXT.md (already written). ✓

**2. Placeholder scan:** Task 4 Step 1/2 and Task 5 Step 4/5 use `/* ... */` for test-fixture setup (the exact agg/state builders live in the existing test modules and vary per scenario). These are deliberate fixture references, not logic placeholders — the executing subagent mirrors the existing neighbor tests' setup. Flagged so reviewers know to use real builders.

**3. Type consistency:** `target_at_line` accessor returns `Option<(usize, Option<u32>)>` (Task 5) consistently used by `tab_position_at_line`. `RenderedRail::target_at_line` returns `Option<RailTarget>` (unchanged). `onboarding -> RenderedRail` consumed at `runtime.rs:274`. `Effect::SwitchTab { position }` / `ShowPane { pane_id }` match the existing enum at `runtime.rs`.

**Ordering rationale:** Task 1 (safety net) → 2 (onboarding, smallest surface change) → 3 (ColorMode, wide but mechanical) → 4 (seal, guarded by 1) → 5 (click seam) → 6 (sweep). Each task is independently green and committed.
