# Sidebar Fast-Follows Implementation Plan

> Follow-ups deferred from the gutter-rail build (`2026-06-26-gutter-rail-sidebar.md`). The collapsed mode and roster line are already specified in the approved mock ("Direction C"); the overflow refinement is spec §9; the clippy cleanup is hygiene.

**Goal:** Land four deferred items: clippy cleanup, the multi-agent per-agent roster line, progressive overflow compression (spec §9 pathological case), and collapsed mode with a keybind toggle.

**Architecture:** Same pure/host-testable split (`status.rs`, `model.rs`, `render.rs` pure; `lib.rs` wasm glue). Every change keeps `render()` ↔ `tab_position_at_line()` in lockstep via `row_lines`/`header_lines`/`plan_overflow`. TDD throughout; GPG signing blocked → commit with `git -c commit.gpgsign=false`.

## Global Constraints

- ANSI-16 role SGR codes only (no hex/truecolor). Colors via `Role::ansi()` / `Status::role().ansi()`.
- No emitted line (ANSI-stripped, unicode-width) may exceed `width`.
- `render()` and `tab_position_at_line()` must agree on every line's owner, in EVERY mode (expanded, folded, compressed, collapsed).
- Plain glyph default; Nerd opt-in. Tests assert against the Plain set.

---

### Task 1: Clippy `map_or` cleanup

**Files:** Modify `src/model.rs`, `src/naming.rs`.

`cargo clippy --all-targets` flags 3 pre-existing `unnecessary_map_or` warnings (model.rs:~17, naming.rs:~17, naming.rs:~76). Apply clippy's suggestion at each: `x.map_or(true, |v| pred)` → `x.is_none_or(|v| pred)`; `x.map_or(false, |v| pred)` → `x.is_some_and(|v| pred)`.

- [ ] Step 1: `cargo clippy --all-targets 2>&1 | grep -A3 map_or` to see each site + suggestion.
- [ ] Step 2: Apply the exact suggested rewrite at each of the 3 sites. No behavior change.
- [ ] Step 3: `cargo test` → green; `cargo clippy --all-targets 2>&1 | grep -c warning` → 0.
- [ ] Step 4: Commit `style: replace map_or with is_some_and/is_none_or (clippy)`.

---

### Task 2: Per-agent roster line (multi-agent tabs)

**Files:** Modify `src/model.rs` (TabAgg + aggregate), `src/render.rs` (roster line + `row_lines`), `src/lib.rs` (click-mapping tests).

Design (mock "Multi-agent tab"): a multi-agent **active** tab (`total > 1`, status active) gets ONE extra detail line — a roster strip of its members' status glyphs, each in its status role color, e.g. `   ◐ ● ● ◆`.

**Interfaces:**
- `TabAgg` gains `pub roster: Vec<Status>` — the status of each ever-active pane in the tab, in pane-id order. Populated by `aggregate()`. Empty for single-agent/plain tabs.
- `row_lines(agg)`: add `+1` when `agg.total > 1 && agg.status.is_active()` (the roster line). Otherwise unchanged.
- New render helper emits the roster line, width-bounded (each glyph = 1 col + 1 space; emit as many as fit in `width-3`; the strip never exceeds width).

- [ ] Step 1 (model test): in `model.rs`, assert `aggregate` populates `roster` with one entry per ever-active pane (e.g. 3 panes Running/Done/Pending → `roster.len()==3` containing those statuses); single idle pane → empty roster. Write failing test.
- [ ] Step 2: run → fails (no field `roster`).
- [ ] Step 3: add `pub roster: Vec<Status>` to `TabAgg`; in `aggregate()`, push each ever-active pane's `status` into `roster`; update every `TabAgg { .. }` literal (model.rs + render.rs test helper `agg(..)` sets `roster: vec![]`). Run model test → pass.
- [ ] Step 4 (render test): `multi_agent_active_tab_shows_roster_glyphs` — a TabAgg with `total: 4, status: Running, roster: vec![Running, Done, Done, Pending]` renders a line containing the colored glyphs; `row_lines` for it is base+1; and `roster_line_never_exceeds_width` across widths [16,20,24] with a long roster (e.g. 10 members). Write failing.
- [ ] Step 5: implement `row_lines` `+1` rule and the roster-line emission in `render()` (after the existing per-state detail lines, only when `total>1 && status.is_active()`). Build the strip: for each `roster` status, `format!("{}{}{}", st.role().ansi(), st.glyph_for(opts.glyphs), RESET)` joined by spaces, truncate the VISIBLE strip to `width-3` (indent 3) by dropping trailing members (compute via unicode-width; never split a glyph). Run → pass.
- [ ] Step 6 (click mapping): update `lib.rs` tests — a multi-agent active tab now spans one more line; add/adjust a `tab_position_at_line` test proving the extra roster line maps to that tab. Run full `cargo test` → green.
- [ ] Step 7: Commit `feat(render): multi-agent roster line of per-member status glyphs`.

---

### Task 3: Progressive overflow compression (spec §9 pathological)

**Files:** Modify `src/render.rs` (`plan_overflow` + render loop), `src/lib.rs` (`tab_position_at_line`).

Spec §9: when even after folding idle, kept non-idle rows still exceed `body_budget`, compress progressively: idle strip is dropped first (already), then **calm rows (Done/Running) compress to 1 line, lowest-position first; urgent rows (Pending/Error) lose detail last**.

**Interfaces:**
- Replace `plan_overflow(rows, body_budget) -> (Vec<usize>, usize)` with `plan_overflow(rows, body_budget) -> (Vec<(usize, usize)>, usize)` returning, per kept row index, the number of lines to render (its `row_lines` when it fits, or a compressed count down to 1), plus the folded-idle count.
- Compression order when over budget: (1) fold idle (as today); (2) compress calm non-idle rows (Done/Running) to 1 line, iterating lowest-position first until it fits; (3) if still over, compress urgent rows (Pending/Error) toward their 1-line form (drop msg, then drop branch/needs-you line). Never below 1 line/row. If still over after all compression, render what fits and stop (the lowest-priority overflow falls off the bottom) — `log`-equivalent: not applicable (pure), but ensure no panic and clamp to the visible budget.
- A row-render must accept a `max_lines` cap. Refactor the per-row body in `render()` into `render_row(out, row, opts, max_lines)` that emits at most `max_lines` (line 1 always; detail/roster lines only while under the cap, urgent detail prioritized over roster). `row_lines` remains the uncompressed truth.
- `tab_position_at_line` replays `plan_overflow` and uses each kept row's PLANNED line count (not `row_lines`) for the span.

- [ ] Step 1 (test): `overflow_compresses_calm_before_urgent` — many working+done rows + one pending, tiny height; assert the pending row keeps its detail (3 lines) while working/done rows are 1 line each, and total emitted body lines ≤ budget; `no_emitted_line_exceeds_width` analog still holds. `click_mapping_matches_compressed_layout` in lib.rs. Write failing.
- [ ] Step 2: run → fails.
- [ ] Step 3: implement the new `plan_overflow` (per-row line budget), `render_row(.., max_lines)`, render loop, and update `tab_position_at_line` to consume the planned per-row counts. Keep the existing fold-idle + `N ▲` header behavior. Run → pass.
- [ ] Step 4: full `cargo test` → green; verify the existing overflow tests (`overflow_folds_idle_into_strip_and_marks_header`, `overflow_keeps_non_idle_rows_visible`, `idle_strip_never_exceeds_width`) still pass.
- [ ] Step 5: Commit `feat(render): progressive overflow compression (calm before urgent, spec §9)`.

---

### Task 4: Collapsed mode + keybind toggle

**Files:** Modify `src/render.rs` (`render_collapsed` + collapsed `row_lines`/header), `src/lib.rs` (`collapsed` state, pipe-toggle, render branch, collapsed click mapping), `dev/dev.kdl` (document the keybind).

Design (mock "Collapsed ⇄ expanded"): a ~4-col mode — each tab is ONE line `▌◐1` (active bar + status glyph + tab number), no name/detail/header (or a 1-char accent marker). Toggled at runtime.

**Interfaces:**
- `RenderOpts` gains `pub collapsed: bool`.
- `pub fn render_collapsed(rows: &[TabRow], opts: &RenderOpts) -> String`: per row, `<bar|space><glyph><number>` (bar = active marker w/ attention tint like expanded; glyph = status glyph, Running spins; number with no space). 1 line per row, no header. Width is ~4; still width-safe via `truncate`.
- `render()` (or `lib.rs`) dispatches to `render_collapsed` when `opts.collapsed`.
- Collapsed click mapping: each row = 1 line, no header → `tab_position_at_line` branches on collapsed: `target` directly indexes the rows (0-based, position order), returns `rows[target].number-1`.
- Toggle: handle a plugin pipe message in `lib.rs::pipe()` — name `zj_radar.cmd` with payload `"toggle_collapsed"` (or a dedicated `zj_radar.toggle` name) flips `self.collapsed` and returns `true` (re-render). `State` gains `collapsed: bool`.
- `dev/dev.kdl`: add a commented keybind example, e.g. a `MessagePlugin`/`zellij pipe` binding that sends `zj_radar.toggle` to flip collapse (document, since the binding lives in the user's KDL).

- [ ] Step 1 (render test): `collapsed_renders_one_line_per_tab_glyph_and_number` — assert collapsed output has one line per tab, each containing the status glyph + number, no tab name, no AGENTS header; active row has the bar; `collapsed_never_exceeds_width` at width 4–6. Write failing.
- [ ] Step 2: run → fails (no `render_collapsed`/`collapsed` field).
- [ ] Step 3: add `collapsed: bool` to `RenderOpts`; implement `render_collapsed`; update the `ro(..)` test helper to set `collapsed: false`; update all `RenderOpts { .. }` literals. Run render tests → pass.
- [ ] Step 4 (click mapping test): in `lib.rs`, `click_mapping_collapsed_one_line_per_tab` — with `collapsed` set, line N maps to position N (no header offset). Write failing, then add the `collapsed` branch to `tab_position_at_line` and a `collapsed: bool` field on `State`. Run → pass.
- [ ] Step 5 (toggle): handle the `zj_radar.toggle` pipe message in `lib.rs::pipe()` to flip `self.collapsed`; wasm `render()` passes `collapsed: self.collapsed` into `RenderOpts` and dispatches. (Wasm-only; verify by reading.) Add the documented keybind to `dev/dev.kdl`.
- [ ] Step 6: full `cargo test` → green; `cargo clippy` clean; build wasm via `nix develop -c cargo build --target wasm32-wasip1`.
- [ ] Step 7: Commit `feat(sidebar): collapsed mode (~4 cols) with pipe-message toggle`.

---

## Self-Review

- T1 → clippy clean. T2 → roster (mock multi-agent). T3 → spec §9 progressive overflow. T4 → mock collapsed mode.
- Coupling handled by ordering: T2 (changes `row_lines`) → T3 (consumes per-row line budget incl. roster) → T4 (separate collapsed path). T1 independent first.
- Every task keeps render↔click lockstep and the no-line-exceeds-width invariant; each ends green + (T4) wasm-built.
