# Gutter Rail Sidebar Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Re-render the zj-radar sidebar as the finalized "gutter rail" — a 2-column status gutter (active/attention bar + status glyph), a two-line AGENTS header, per-state adaptive density with a right-aligned status slot, overflow folding, and an onboarding panel.

**Architecture:** All work lands in the existing pure modules (`status.rs`, `render.rs`) plus thin wasm glue (`lib.rs`); `model.rs`, `state.rs`, `payload.rs`, `naming.rs` are untouched. The pure/host-testable split is preserved, so every behavior is covered by `cargo test` on the host target — no WASM round-trip. New `status.rs` vocabulary is added *additively* (old `glyph()`/`ansi()` stay until callers migrate) so the suite stays green after each task.

**Tech Stack:** Rust (edition 2021), `zellij-tile = "0.44"`, `cargo test` (host target), wasm32 only for the live plugin.

## Global Constraints

- **No new host calls, no new permissions, no blocking queries.** Render/state logic only (see `docs/design.md` §11 risk 6).
- **Colors are ANSI-16 role SGR codes only** — never raw hex, never truecolor `38;2;…`. Roles: error `\x1b[31m`, attention `\x1b[91m`, working `\x1b[33m`, success `\x1b[32m`, muted `\x1b[90m`, accent `\x1b[35m`. Reset `\x1b[0m`, bold `\x1b[1m`.
- **Rows stay in tab-position order.** No priority re-sort. Tab number = `position + 1`.
- **Plain-glyph set in tests.** Unit tests assert against the Plain glyph set (`○ ◐ ◆ ● ✗`) for stable, font-independent assertions.
- **`row_lines()` and `header_lines()` are the single sources of truth** for vertical spans; `lib.rs::tab_position_at_line()` replays them, so any change to row/header height must keep click-mapping tests passing.
- Spec: `docs/superpowers/specs/2026-06-26-zj-agents-gutter-rail-design.md`.

---

### Task 1: Status vocabulary — roles, glyph sets, spinner

**Files:**
- Modify: `src/status.rs` (add to the `impl Status` block ~line 12, and new free items)
- Test: `src/status.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: existing `Status` enum (`Idle, Done, Running, Pending, Error`).
- Produces:
  - `pub enum Role { Error, Attention, Working, Success, Muted, Accent }` with `pub fn ansi(self) -> &'static str`.
  - `Status::role(self) -> Role`.
  - `pub enum GlyphSet { Nerd, Plain }` with `pub fn from_config(s: &str) -> GlyphSet`.
  - `Status::glyph_for(self, set: GlyphSet) -> char`.
  - `pub fn working_spin(frame: usize) -> char` and `pub fn msg_spin(frame: usize) -> char`.
  - Existing `glyph()` / `ansi()` are left intact (removed in Task 3 once `render.rs` migrates).

- [ ] **Step 1: Write the failing tests**

Add to `src/status.rs` `mod tests`:

```rust
#[test]
fn role_colors_match_spec() {
    assert_eq!(Role::Error.ansi(), "\x1b[31m");
    assert_eq!(Role::Attention.ansi(), "\x1b[91m");
    assert_eq!(Role::Working.ansi(), "\x1b[33m");
    assert_eq!(Role::Success.ansi(), "\x1b[32m");
    assert_eq!(Role::Muted.ansi(), "\x1b[90m");
    assert_eq!(Role::Accent.ansi(), "\x1b[35m");
}

#[test]
fn status_maps_to_role() {
    assert_eq!(Status::Error.role(), Role::Error);
    assert_eq!(Status::Pending.role(), Role::Attention); // waiting is the loud one
    assert_eq!(Status::Running.role(), Role::Working);
    assert_eq!(Status::Done.role(), Role::Success);
    assert_eq!(Status::Idle.role(), Role::Muted);
}

#[test]
fn plain_glyphs_use_geometric_shapes() {
    use GlyphSet::Plain;
    assert_eq!(Status::Idle.glyph_for(Plain), '○');
    assert_eq!(Status::Running.glyph_for(Plain), '◐');
    assert_eq!(Status::Pending.glyph_for(Plain), '◆'); // moved from ◑ to ◆
    assert_eq!(Status::Done.glyph_for(Plain), '●');
    assert_eq!(Status::Error.glyph_for(Plain), '✗');
}

#[test]
fn nerd_glyphs_use_private_use_codepoints() {
    use GlyphSet::Nerd;
    assert_eq!(Status::Pending.glyph_for(Nerd), '\u{f0f3}');
    assert_eq!(Status::Done.glyph_for(Nerd), '\u{f058}');
    assert_eq!(Status::Error.glyph_for(Nerd), '\u{f057}');
}

#[test]
fn glyph_set_from_config_defaults_to_nerd() {
    assert_eq!(GlyphSet::from_config("plain"), GlyphSet::Plain);
    assert_eq!(GlyphSet::from_config("nerd"), GlyphSet::Nerd);
    assert_eq!(GlyphSet::from_config("anything-else"), GlyphSet::Nerd);
}

#[test]
fn working_spinner_cycles_quarter_circles() {
    assert_eq!(working_spin(0), '◐');
    assert_eq!(working_spin(1), '◓');
    assert_eq!(working_spin(2), '◑');
    assert_eq!(working_spin(3), '◒');
    assert_eq!(working_spin(4), '◐'); // wraps
}

#[test]
fn msg_spinner_cycles_braille() {
    assert_eq!(msg_spin(0), '⠋');
    assert_eq!(msg_spin(10), '⠋'); // wraps at 10
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib status::`
Expected: FAIL — `cannot find type Role`, `no method named role`, `glyph_for`, etc.

- [ ] **Step 3: Implement the new vocabulary**

In `src/status.rs`, add after the `Status` enum / `impl Status` block:

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Error,
    Attention,
    Working,
    Success,
    Muted,
    Accent,
}

impl Role {
    pub fn ansi(self) -> &'static str {
        match self {
            Role::Error => "\x1b[31m",
            Role::Attention => "\x1b[91m",
            Role::Working => "\x1b[33m",
            Role::Success => "\x1b[32m",
            Role::Muted => "\x1b[90m",
            Role::Accent => "\x1b[35m",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GlyphSet {
    Nerd,
    Plain,
}

impl GlyphSet {
    pub fn from_config(s: &str) -> GlyphSet {
        match s {
            "plain" => GlyphSet::Plain,
            _ => GlyphSet::Nerd,
        }
    }
}

/// Working status glyph animation (both glyph sets): ◐ ◓ ◑ ◒.
pub fn working_spin(frame: usize) -> char {
    const FRAMES: [char; 4] = ['◐', '◓', '◑', '◒'];
    FRAMES[frame % FRAMES.len()]
}

/// In-message braille spinner.
pub fn msg_spin(frame: usize) -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    FRAMES[frame % FRAMES.len()]
}
```

Add these two methods inside the existing `impl Status { … }`:

```rust
    pub fn role(self) -> Role {
        match self {
            Status::Error => Role::Error,
            Status::Pending => Role::Attention,
            Status::Running => Role::Working,
            Status::Done => Role::Success,
            Status::Idle => Role::Muted,
        }
    }

    pub fn glyph_for(self, set: GlyphSet) -> char {
        match set {
            GlyphSet::Plain => match self {
                Status::Idle => '○',
                Status::Running => '◐',
                Status::Pending => '◆',
                Status::Done => '●',
                Status::Error => '✗',
            },
            GlyphSet::Nerd => match self {
                Status::Idle => '\u{eb83}',
                Status::Running => '\u{f110}',
                Status::Pending => '\u{f0f3}',
                Status::Done => '\u{f058}',
                Status::Error => '\u{f057}',
            },
        }
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib status::`
Expected: PASS (all status tests, including the pre-existing ones).

- [ ] **Step 5: Commit**

```bash
git add src/status.rs
git commit -m "feat(status): add Role colors, GlyphSet, and spinner vocabulary"
```

---

### Task 2: Two-line AGENTS header + RenderOpts

**Files:**
- Modify: `src/render.rs` (remove `summary()` ~line 77-95; rewrite `header_lines()` ~line 99-105; add `RenderOpts`; change `render()` signature ~line 107 and its header block)
- Modify: `src/lib.rs` (wasm `render()` call site ~line 236-239)
- Test: `src/render.rs` tests, `src/lib.rs` tests (header-offset expectations)

**Interfaces:**
- Consumes: `GlyphSet` (Task 1), `Role` (Task 1), `TabRow`.
- Produces:
  - `pub struct RenderOpts { pub width: usize, pub height: usize, pub now_tick: u64, pub glyphs: GlyphSet }`.
  - `render(rows: &[TabRow], opts: &RenderOpts) -> String` (signature change from `(rows, width, now_tick)`).
  - `header_lines(rows: &[TabRow]) -> usize` now returns `2` when rows non-empty, else `0`.
  - `summary()` is deleted.

- [ ] **Step 1: Write the failing tests**

In `src/render.rs` tests, add a shared helper at the top of `mod tests` (just after `use` lines):

```rust
fn ro(width: usize, now_tick: u64) -> RenderOpts {
    RenderOpts { width, height: 100, now_tick, glyphs: GlyphSet::Plain }
}
```

Add these tests:

```rust
#[test]
fn header_is_title_then_rule_two_lines() {
    let rows = vec![TabRow {
        number: 1, name: "a".into(), active: false, has_bell: false,
        agg: agg(Status::Running, 0, 0, None),
    }];
    assert_eq!(header_lines(&rows), 2);
    let s = render(&rows, &ro(24, 0));
    let mut lines = s.lines();
    let title = lines.next().unwrap();
    let rule = lines.next().unwrap();
    assert!(title.contains("AGENTS"));
    assert!(title.contains("·1")); // one tab
    assert!(rule.contains('═'));
}

#[test]
fn header_absent_for_empty_rows() {
    let rows: Vec<TabRow> = vec![];
    assert_eq!(header_lines(&rows), 0);
    assert!(render(&rows, &ro(24, 0)).is_empty());
}
```

Update the existing `summary_*` and `header_*` tests: **delete** `summary_counts_tabs_by_dominant_status_active_only`, `summary_empty_when_all_idle`, and `header_line_emitted_when_active` (the bare-summary header is gone). Also update every existing `render(&rows, W, T)` call in this module to `render(&rows, &ro(W, T))`, and every `header_lines`-dependent assertion: the agent-tab tests now emit a 2-line header, so `agent_tab_has_three_lines_with_count_tag_and_msg` and `agent_tab_with_empty_msg_has_two_lines` get re-asserted in Task 3 (where density changes); for now, just make them compile with `ro(...)`.

In `src/lib.rs` tests, update the header-offset expectations (header is now 2 lines, not 1):

```rust
#[test]
fn header_shifts_click_mapping_down_by_two() {
    // One active agent tab (→ 2-line header) at position 0, plain tab at position 1.
    let mut state = make_state_with_tabs(&[(0, "agent", false), (1, "plain", false)]);
    state.tab_panes.insert(0, vec![pane(10)]);
    apply_payload(&mut state, 10, Status::Running, 1);
    // rows 0,1 = header (no tab)
    assert_eq!(state.tab_position_at_line(0), None);
    assert_eq!(state.tab_position_at_line(1), None);
    // running tab now spans rows 2.. (exact span finalized in Task 3)
    assert_eq!(state.tab_position_at_line(2), Some(0));
}
```

Delete the old `header_shifts_click_mapping_down_by_one` test (superseded). Update `agent_tab_with_msg_occupies_three_lines`, `agent_tab_with_empty_msg_occupies_two_lines`, and `multiple_agent_tabs_line_spans_accumulate_correctly` so their header offset is 2 — these are fully re-specified in Task 3, so for now shift each expected line index up by 1 (header grew 1→2). `no_header_when_idle_click_mapping_unchanged` stays valid (all-idle → header 0).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test`
Expected: FAIL — `cannot find type RenderOpts`, plus arity errors on `render(...)` until the helper/signature land.

- [ ] **Step 3: Implement header + RenderOpts**

In `src/render.rs`, add near the top (after the existing `const` block) and import the new items:

```rust
use crate::status::{GlyphSet, Role, Status};

pub struct RenderOpts {
    pub width: usize,
    pub height: usize,
    pub now_tick: u64,
    pub glyphs: GlyphSet,
}
```

Delete `pub fn summary(...)` entirely. Replace `header_lines`:

```rust
/// The rail's identity header is two lines (title + rule) whenever any rows
/// exist. Single source of truth for the header's vertical span (consumed by
/// click mapping in lib.rs).
pub fn header_lines(rows: &[TabRow]) -> usize {
    if rows.is_empty() {
        0
    } else {
        2
    }
}
```

Change `render`'s signature and its header block. Replace the start of `render` (the `let sum = summary(rows); if !sum.is_empty() { … }` block) with:

```rust
pub fn render(rows: &[TabRow], opts: &RenderOpts) -> String {
    let mut out = String::new();
    if rows.is_empty() {
        return out;
    }
    let width = opts.width;
    let now_tick = opts.now_tick;
    let accent = Role::Accent.ansi();

    // Header line 1: " AGENTS" + right-aligned "·N" tab count.
    let title = " AGENTS";
    let count = format!("·{}", rows.len());
    let gap = width
        .saturating_sub(title.chars().count() + count.chars().count())
        .max(1);
    out.push_str(&format!(
        "{}{}{}{}{}\n",
        accent, title, " ".repeat(gap), count, RESET
    ));
    // Header line 2: rule across the full width.
    out.push_str(&format!("{}{}{}\n", accent, "═".repeat(width), RESET));

    for row in rows {
        // … existing per-row body stays for now (Task 3 rewrites it) …
```

Keep the existing per-row loop body intact for this task **except** swap any internal use of `now_tick`/`width` to the locals just defined (they already are). The row body still calls `row.agg.status.glyph()`/`.ansi()` — those still exist (removed in Task 3).

In `src/lib.rs`, update the wasm `render` call site:

```rust
    fn render(&mut self, rows: usize, cols: usize) {
        let tabrows = self.build_rows();
        let opts = render::RenderOpts {
            width: cols.max(1),
            height: rows,
            now_tick: self.tick,
            glyphs: render::GlyphSet::Nerd,
        };
        print!("{}", render::render(&tabrows, &opts));
    }
```

Re-export `GlyphSet` from `render` for the call site, or reference `crate::status::GlyphSet`. Add to `render.rs`: `pub use crate::status::GlyphSet;` is already covered by the `use` above — instead reference it in lib.rs as `crate::status::GlyphSet::Nerd`. (Pick one; the plan uses `render::GlyphSet`, so add `pub use crate::status::GlyphSet;` near the top of `render.rs`.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test`
Expected: PASS. (Width-bound tests still hold; header is two role-colored lines.)

- [ ] **Step 5: Commit**

```bash
git add src/render.rs src/lib.rs
git commit -m "feat(render): two-line AGENTS header + RenderOpts; drop status summary"
```

---

### Task 3: Row gutter, right slot, and adaptive density

**Files:**
- Modify: `src/render.rs` (rewrite the per-row loop body in `render`; rewrite `row_lines()` ~line 68; remove `detail_tag()`; remove now-unused `Status::glyph()`/`Status::ansi()` from `src/status.rs`)
- Modify: `src/status.rs` (delete `glyph()` and `ansi()` methods — render no longer calls them)
- Test: `src/render.rs` tests, `src/lib.rs` tests (final span expectations)

**Interfaces:**
- Consumes: `RenderOpts`, `Role`, `GlyphSet`, `working_spin`, `Status::role`, `Status::glyph_for` (Tasks 1-2).
- Produces:
  - New per-row output: `<bar|space><glyph> <num> <name><bell> … <rightslot>` on line 1; per-state detail lines 2-3.
  - `row_lines(agg) -> usize`: idle/done → 1; running/error → 2 (1 if no detail); pending → 3 if msg, else 2 (1 if no detail).
  - `right_slot(agg, now_tick) -> String` (module-private helper).

- [ ] **Step 1: Write the failing tests**

Replace `agent_tab_has_three_lines_with_count_tag_and_msg`, `agent_tab_with_empty_msg_has_two_lines`, and `row_lines_all_three_cases` with the new density tests, and add gutter/slot tests:

```rust
#[test]
fn row_lines_by_state() {
    assert_eq!(row_lines(&agg(Status::Idle, 0, 0, None)), 1);

    let detail = |status, msg: &str| Some(Detail {
        repo: "r".into(), branch: "b".into(), msg: msg.into(),
        since_tick: 0, status,
    });
    assert_eq!(row_lines(&agg(Status::Done, 1, 1, detail(Status::Done, ""))), 1);
    assert_eq!(row_lines(&agg(Status::Running, 1, 1, detail(Status::Running, "x"))), 2);
    assert_eq!(row_lines(&agg(Status::Error, 1, 1, detail(Status::Error, "x"))), 2);
    assert_eq!(row_lines(&agg(Status::Pending, 1, 1, detail(Status::Pending, ""))), 2);
    assert_eq!(row_lines(&agg(Status::Pending, 1, 1, detail(Status::Pending, "go?"))), 3);
}

#[test]
fn active_row_has_accent_bar_idle_does_not() {
    let rows = vec![
        TabRow { number: 1, name: "a".into(), active: true, has_bell: false,
                 agg: agg(Status::Idle, 0, 0, None) },
        TabRow { number: 2, name: "b".into(), active: false, has_bell: false,
                 agg: agg(Status::Idle, 0, 0, None) },
    ];
    let s = render(&rows, &ro(24, 0));
    let body: Vec<&str> = s.lines().skip(2).collect(); // skip 2-line header
    assert!(body[0].contains('▌'));         // active row → bar
    assert!(body[0].contains(Role::Accent.ansi())); // accent-colored bar
    assert!(!body[1].contains('▌'));        // idle non-active → no bar
}

#[test]
fn active_and_waiting_row_bar_is_attention_not_accent() {
    let detail = Detail { repo: "p".into(), branch: "fix".into(), msg: "".into(),
                          since_tick: 0, status: Status::Pending };
    let rows = vec![TabRow {
        number: 3, name: "pinky".into(), active: true, has_bell: false,
        agg: agg(Status::Pending, 0, 0, Some(detail)),
    }];
    let s = render(&rows, &ro(30, 5));
    let line1 = s.lines().nth(2).unwrap();
    assert!(line1.contains('▌'));
    // the bar uses the attention role when the active tab is also waiting
    assert!(line1.contains(Role::Attention.ansi()));
}

#[test]
fn right_slot_per_state() {
    let mk = |status, done, total| {
        let d = Detail { repo: "r".into(), branch: "b".into(), msg: "".into(),
                         since_tick: 0, status };
        TabRow { number: 1, name: "n".into(), active: false, has_bell: false,
                 agg: agg(status, done, total, Some(d)) }
    };
    assert!(render(&[mk(Status::Done, 1, 1)], &ro(30, 0)).contains("done"));
    assert!(render(&[mk(Status::Error, 0, 1)], &ro(30, 0)).contains("failed"));
    assert!(render(&[mk(Status::Running, 0, 1)], &ro(30, 14)).contains("0:14"));
    let waiting = render(&[mk(Status::Pending, 0, 1)], &ro(30, 2));
    assert!(waiting.contains('⏵'));
    assert!(waiting.contains("0:02"));
    let multi = render(&[mk(Status::Pending, 2, 4)], &ro(30, 18));
    assert!(multi.contains("2/4"));
}

#[test]
fn working_glyph_spins_with_tick() {
    let d = Detail { repo: "r".into(), branch: "b".into(), msg: "".into(),
                     since_tick: 0, status: Status::Running };
    let row = |t| TabRow { number: 1, name: "n".into(), active: false, has_bell: false,
                           agg: agg(Status::Running, 0, 1, Some(d.clone())) };
    let f0 = render(&[row(0)], &RenderOpts { width: 30, height: 100, now_tick: 0, glyphs: GlyphSet::Plain });
    let f1 = render(&[row(1)], &RenderOpts { width: 30, height: 100, now_tick: 1, glyphs: GlyphSet::Plain });
    assert!(f0.contains('◐'));
    assert!(f1.contains('◓'));
}

#[test]
fn idle_row_is_single_line_with_no_right_slot_text() {
    let rows = vec![TabRow {
        number: 7, name: "logs".into(), active: false, has_bell: false,
        agg: agg(Status::Idle, 0, 0, None),
    }];
    let s = render(&rows, &ro(24, 0));
    assert_eq!(s.lines().skip(2).count(), 1); // exactly one body line
    assert!(s.contains('○'));
    assert!(s.contains("logs"));
}
```

In `src/lib.rs` tests, finalize the click-mapping spans (header = 2 lines; running-with-msg = **2** lines now, not 3):

```rust
#[test]
fn agent_tab_running_occupies_two_lines() {
    let mut state = make_state_with_tabs(&[(0, "agent", false), (1, "plain", false)]);
    state.tab_panes.insert(0, vec![pane(10)]);
    apply_payload(&mut state, 10, Status::Running, 1); // running → 2 lines
    // rows 0,1 = header
    assert_eq!(state.tab_position_at_line(1), None);
    // rows 2,3 = running agent tab (position 0)
    assert_eq!(state.tab_position_at_line(2), Some(0));
    assert_eq!(state.tab_position_at_line(3), Some(0));
    // row 4 = plain tab (position 1)
    assert_eq!(state.tab_position_at_line(4), Some(1));
    assert!(state.tab_position_at_line(5).is_none());
}

#[test]
fn agent_tab_pending_with_msg_occupies_three_lines() {
    let mut state = make_state_with_tabs(&[(0, "agent", false), (1, "plain", false)]);
    state.tab_panes.insert(0, vec![pane(10)]);
    apply_payload_with_msg(&mut state, 10, Status::Pending, 1, "approve?"); // pending+msg → 3
    assert_eq!(state.tab_position_at_line(1), None);       // header
    assert_eq!(state.tab_position_at_line(2), Some(0));    // line 1
    assert_eq!(state.tab_position_at_line(3), Some(0));    // line 2
    assert_eq!(state.tab_position_at_line(4), Some(0));    // line 3
    assert_eq!(state.tab_position_at_line(5), Some(1));    // plain tab
}
```

Delete the now-superseded `agent_tab_with_msg_occupies_three_lines`, `agent_tab_with_empty_msg_occupies_two_lines`, `multiple_agent_tabs_line_spans_accumulate_correctly`, and `header_shifts_click_mapping_down_by_two` (replaced by the two above). `plain_tabs_each_occupy_one_line` and `no_header_when_idle_click_mapping_unchanged` stay valid.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test`
Expected: FAIL — `row_lines` wrong counts, no `▌` bar, no `done/failed/⏵` slot text, `glyph_for` not used yet.

- [ ] **Step 3: Implement the gutter, slot, and density**

In `src/render.rs`, rewrite `row_lines`:

```rust
/// Single source of truth for how many lines a tab row occupies.
pub fn row_lines(agg: &TabAgg) -> usize {
    match agg.status {
        Status::Idle | Status::Done => 1,
        Status::Running | Status::Error => {
            if agg.detail.is_some() { 2 } else { 1 }
        }
        Status::Pending => match &agg.detail {
            Some(d) if !d.msg.trim().is_empty() => 3,
            Some(_) => 2,
            None => 1,
        },
    }
}
```

Delete `detail_tag()`. Add the right-slot helper:

```rust
/// Right-aligned status slot text (no color). Empty for idle.
fn right_slot(agg: &TabAgg, now_tick: u64) -> String {
    let elapsed = agg
        .detail
        .as_ref()
        .map(|d| format_elapsed(now_tick.saturating_sub(d.since_tick)))
        .unwrap_or_default();
    let count = if agg.total > 1 {
        format!("{}/{} ", agg.done, agg.total)
    } else {
        String::new()
    };
    match agg.status {
        Status::Idle => String::new(),
        Status::Running => format!("{}{}", count, elapsed),
        Status::Pending => format!("{}⏵ {}", count, elapsed),
        Status::Done => "done".to_string(),
        Status::Error => "failed".to_string(),
    }
}
```

Replace the entire per-row loop body (everything inside `for row in rows { … }`) with:

```rust
    for row in rows {
        let st = row.agg.status;
        let role = st.role().ansi();

        // col 0: active bar — accent normally, attention when active+urgent.
        let bar = if row.active {
            let bar_role = match st {
                Status::Pending | Status::Error => Role::Attention,
                _ => Role::Accent,
            };
            format!("{}▌{}", bar_role.ansi(), RESET)
        } else {
            " ".to_string()
        };

        // col 1: status glyph (working spins).
        let glyph_char = if st == Status::Running {
            crate::status::working_spin(now_tick as usize)
        } else {
            st.glyph_for(opts.glyphs)
        };
        let glyph = format!("{}{}{}", role, glyph_char, RESET);

        // right slot (reserved width even when empty).
        let slot = right_slot(&row.agg, now_tick);
        let slot_styled = if slot.is_empty() {
            String::new()
        } else {
            format!("{}{}{}", role, slot, RESET)
        };

        // bell marker just before the slot.
        let bell = if row.has_bell {
            format!("{}⚑{} ", Role::Working.ansi(), RESET)
        } else {
            String::new()
        };

        // left visible prefix is "X<glyph> <num> " — bar/glyph are 1 cell each.
        let num = row.number.to_string();
        let prefix_len = 1 + 1 + 1 + num.chars().count() + 1; // bar+glyph+sp+num+sp
        let bell_len = if row.has_bell { 2 } else { 0 };
        let slot_len = slot.chars().count();
        let name_budget = width
            .saturating_sub(prefix_len + bell_len + slot_len + 1) // +1 min gap
            .max(1);
        let name = truncate(&row.name, name_budget);
        let name_styled = if row.active {
            format!("{}{}{}", BOLD, name, RESET)
        } else {
            name.clone()
        };

        // pad so the slot sits flush right.
        let used = prefix_len + name.chars().count() + bell_len + slot_len;
        let gap = width.saturating_sub(used).max(1);
        out.push_str(&format!(
            "{}{} {} {}{}{}{}\n",
            bar, glyph, num, name_styled, " ".repeat(gap), bell, slot_styled
        ));

        // detail lines, per state.
        if let Some(d) = &row.agg.detail {
            let muted = Role::Muted.ansi();
            match st {
                Status::Running => {
                    let spin = crate::status::msg_spin(now_tick as usize);
                    let loc = if d.branch.is_empty() { d.repo.clone() } else { format!("{}/{}", d.repo, d.branch) };
                    let body = format!("{} {} {}", loc, spin, d.msg);
                    out.push_str(&format!("   {}{}{}\n", muted, truncate(&body, width.saturating_sub(3)), RESET));
                }
                Status::Error => {
                    let loc = if d.branch.is_empty() { d.repo.clone() } else { format!("{}/{}", d.repo, d.branch) };
                    let body = if d.msg.trim().is_empty() { loc } else { format!("{} · {}", loc, d.msg) };
                    out.push_str(&format!("   {}{}{}\n", muted, truncate(&body, width.saturating_sub(3)), RESET));
                }
                Status::Pending => {
                    let loc = if d.branch.is_empty() { d.repo.clone() } else { d.branch.clone() };
                    out.push_str(&format!("   {}{} · {}needs you{}\n", muted, truncate(&loc, width.saturating_sub(14)), Role::Attention.ansi(), RESET));
                    if !d.msg.trim().is_empty() {
                        out.push_str(&format!("   {}\"{}\"{}\n", muted, truncate(&d.msg, width.saturating_sub(5)), RESET));
                    }
                }
                Status::Done | Status::Idle => {}
            }
        }
    }
    out
}
```

In `src/status.rs`, delete the now-unused `pub fn glyph(self) -> char` and `pub fn ansi(self) -> &'static str` methods, and update the `glyph_and_ansi_are_distinct_per_variant` test to use the new API:

```rust
#[test]
fn glyphs_and_roles_distinct_per_variant() {
    use Status::*;
    use GlyphSet::Plain;
    let all = [Idle, Done, Running, Pending, Error];
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert_ne!(a.glyph_for(Plain), b.glyph_for(Plain));
        }
    }
    assert_eq!(Done.glyph_for(Plain), '●');
    assert_eq!(Error.role().ansi(), "\x1b[31m");
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test`
Expected: PASS. Confirm `no_emitted_line_exceeds_width` still passes (gutter + slot fit within width).

- [ ] **Step 5: Commit**

```bash
git add src/render.rs src/status.rs src/lib.rs
git commit -m "feat(render): gutter (active bar + glyph), right slot, adaptive density"
```

---

### Task 4: Truncation order + narrow status word

**Files:**
- Modify: `src/render.rs` (line-1 name budget already truncates; add the branch→msg→name *ordering* for detail lines and the `failed→err` narrowing)
- Test: `src/render.rs` tests

**Interfaces:**
- Consumes: the row body from Task 3.
- Produces: `right_slot` narrows `failed → err` when `width` is tight; detail-line construction drops branch before message before truncating name (name truncation already on line 1).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn error_word_narrows_when_tight() {
    let d = Detail { repo: "infra".into(), branch: "".into(), msg: "".into(),
                     since_tick: 0, status: Status::Error };
    let rows = vec![TabRow { number: 5, name: "infra".into(), active: false,
                             has_bell: false, agg: agg(Status::Error, 0, 1, Some(d)) }];
    // wide: "failed"; narrow: "err"
    assert!(render(&rows, &ro(30, 0)).contains("failed"));
    let narrow = render(&rows, &ro(14, 0));
    assert!(narrow.contains("err"));
    assert!(!narrow.contains("failed"));
}

#[test]
fn working_detail_drops_branch_before_message_when_narrow() {
    let d = Detail { repo: "web".into(), branch: "main".into(),
                     msg: "running tests".into(), since_tick: 0, status: Status::Running };
    let rows = vec![TabRow { number: 1, name: "api".into(), active: false,
                             has_bell: false, agg: agg(Status::Running, 0, 1, Some(d)) }];
    let narrow = render(&rows, &ro(16, 5));
    for line in narrow.lines() {
        assert!(visible_len(line) <= 16);
    }
    // branch path is the first thing to go: "web/main" should not survive at 16 cols
    assert!(!narrow.contains("web/main"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib render::`
Expected: FAIL — `failed` not narrowed; `web/main` may still render.

- [ ] **Step 3: Implement ordered truncation**

In `right_slot`, narrow the error word by passing width. Change the signature to `fn right_slot(agg: &TabAgg, now_tick: u64, width: usize) -> String` and update its call in `render` (`right_slot(&row.agg, now_tick, width)`). Replace the `Status::Error` arm:

```rust
        Status::Error => if width < 16 { "err".to_string() } else { "failed".to_string() },
```

In the `Status::Running` detail arm, build the location with a fit-aware fallback (drop branch, then drop message):

```rust
                Status::Running => {
                    let spin = crate::status::msg_spin(now_tick as usize);
                    let avail = width.saturating_sub(3);
                    let full = {
                        let loc = if d.branch.is_empty() { d.repo.clone() } else { format!("{}/{}", d.repo, d.branch) };
                        format!("{} {} {}", loc, spin, d.msg)
                    };
                    let body = if full.chars().count() <= avail {
                        full
                    } else {
                        // drop branch
                        let no_branch = format!("{} {} {}", d.repo, spin, d.msg);
                        if no_branch.chars().count() <= avail {
                            no_branch
                        } else {
                            // drop message, keep repo + spinner
                            truncate(&format!("{} {}", d.repo, spin), avail)
                        }
                    };
                    out.push_str(&format!("   {}{}{}\n", Role::Muted.ansi(), truncate(&body, avail), RESET));
                }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/render.rs
git commit -m "feat(render): ordered truncation (branch→msg→name) and failed→err"
```

---

### Task 5: Overflow folding

**Files:**
- Modify: `src/render.rs` (`render` becomes height-aware: fold idle tabs, compress calm rows, mark header with `▲`)
- Modify: `src/lib.rs` (pass real terminal height into `RenderOpts.height`)
- Test: `src/render.rs` tests

**Interfaces:**
- Consumes: `RenderOpts.height`, `row_lines`.
- Produces: when `sum(row_lines) + 2 (header) > height`, idle tabs collapse into one strip line `○ ○ … +N idle ▾`; non-idle rows are never folded; header shows `N ▲`.

- [ ] **Step 1: Write the failing tests**

```rust
fn idle_row(n: u32) -> TabRow {
    TabRow { number: n, name: format!("t{}", n), active: false, has_bell: false,
             agg: agg(Status::Idle, 0, 0, None) }
}

#[test]
fn overflow_folds_idle_into_strip_and_marks_header() {
    // 20 idle tabs, height only fits a few → fold.
    let rows: Vec<TabRow> = (1..=20).map(idle_row).collect();
    let s = render(&rows, &RenderOpts { width: 24, height: 6, now_tick: 0, glyphs: GlyphSet::Plain });
    assert!(s.contains("idle"));   // "+N idle ▾" footer
    assert!(s.contains('▾'));
    assert!(s.lines().next().unwrap().contains('▲')); // header overflow marker
    // total emitted lines fit the height budget
    assert!(s.lines().count() <= 6);
}

#[test]
fn overflow_keeps_non_idle_rows_visible() {
    let mut rows: Vec<TabRow> = (1..=18).map(idle_row).collect();
    // an urgent waiting tab at the very end (high position)
    let d = Detail { repo: "p".into(), branch: "x".into(), msg: "approve?".into(),
                     since_tick: 0, status: Status::Pending };
    rows.push(TabRow { number: 19, name: "pinky".into(), active: false,
                       has_bell: false, agg: agg(Status::Pending, 0, 1, Some(d)) });
    let s = render(&rows, &RenderOpts { width: 30, height: 8, now_tick: 2, glyphs: GlyphSet::Plain });
    assert!(s.contains("pinky"));     // urgent row never folded
    assert!(s.contains("needs you")); // its detail survives
}

#[test]
fn no_overflow_when_everything_fits() {
    let rows: Vec<TabRow> = (1..=3).map(idle_row).collect();
    let s = render(&rows, &RenderOpts { width: 24, height: 40, now_tick: 0, glyphs: GlyphSet::Plain });
    assert!(!s.contains("idle ▾"));
    assert!(!s.lines().next().unwrap().contains('▲'));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib render::`
Expected: FAIL — no folding, no `▾`/`▲`.

- [ ] **Step 3: Implement folding**

Add a fold-planning helper above `render`:

```rust
/// Decide which rows render in full and how many idle tabs fold, given the
/// vertical budget (height minus the 2-line header). Non-idle rows are always
/// kept; idle rows fold into a strip when space is tight.
fn plan_overflow(rows: &[TabRow], body_budget: usize) -> (Vec<usize>, usize) {
    let total: usize = rows.iter().map(|r| row_lines(&r.agg)).sum();
    if total <= body_budget {
        return ((0..rows.len()).collect(), 0); // everything, no fold
    }
    // Keep all non-idle rows (in position order); fold idle ones.
    let kept: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.agg.status != Status::Idle)
        .map(|(i, _)| i)
        .collect();
    let folded = rows.iter().filter(|r| r.agg.status == Status::Idle).count();
    (kept, folded)
}
```

In `render`, after computing the header but before the row loop, compute the plan and adjust the header marker. Change the header line-1 count to include `▲` when folding:

```rust
    let body_budget = opts.height.saturating_sub(2); // header is 2 lines
    let (kept, folded) = plan_overflow(rows, body_budget);
    let overflow = folded > 0;
    let count = if overflow {
        format!("{} ▲", rows.len())
    } else {
        format!("·{}", rows.len())
    };
```

(Use this `count` in the header line-1 you already build.) Then iterate only `kept` indices:

```rust
    for &i in &kept {
        let row = &rows[i];
        // … existing per-row body, unchanged …
    }
    if folded > 0 {
        let dots: String = std::iter::repeat('○').take(folded.min(11))
            .map(|c| c.to_string()).collect::<Vec<_>>().join(" ");
        out.push_str(&format!(
            "{}{} ── +{} idle ▾{}\n",
            Role::Accent.ansi(), dots, folded, RESET
        ));
    }
```

(Change `for row in rows` → `for &i in &kept { let row = &rows[i]; … }`.)

In `src/lib.rs`, the wasm `render(&mut self, rows, cols)` already receives `rows` (terminal height); it's wired into `RenderOpts.height` from Task 2. Confirm `height: rows` is passed (no change if already done).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/render.rs src/lib.rs
git commit -m "feat(render): overflow folding — idle strip, non-idle always visible"
```

---

### Task 6: Onboarding / empty-state panel

**Files:**
- Modify: `src/render.rs` (add `onboarding(opts) -> String`)
- Modify: `src/lib.rs` (branch to onboarding when no tabs known)
- Test: `src/render.rs` tests

**Interfaces:**
- Consumes: `RenderOpts`, `Role`, `Status::glyph_for`.
- Produces: `pub fn onboarding(opts: &RenderOpts) -> String` — the AGENTS header, a hello line, a glyph legend, and a click hint.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn onboarding_shows_legend_and_click_hint() {
    let s = onboarding(&ro(28, 0));
    assert!(s.contains("AGENTS"));
    assert!(s.contains('◆')); // legend includes the waiting glyph (plain set)
    assert!(s.to_lowercase().contains("needs you"));
    assert!(s.to_lowercase().contains("click"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib render::onboarding`
Expected: FAIL — `cannot find function onboarding`.

- [ ] **Step 3: Implement the panel**

In `src/render.rs`:

```rust
/// The rail's resting "hello / how it works" face — shown on cold start or
/// before permission is granted. Not a permission interceptor.
pub fn onboarding(opts: &RenderOpts) -> String {
    let mut out = String::new();
    let accent = Role::Accent.ansi();
    let muted = Role::Muted.ansi();
    let g = opts.glyphs;
    out.push_str(&format!("{} AGENTS{}\n", accent, RESET));
    out.push_str(&format!("{}{}{}\n", accent, "═".repeat(opts.width), RESET));
    out.push_str(&format!("{} watching your tabs for{}\n", muted, RESET));
    out.push_str(&format!("{} AI agent activity.{}\n", muted, RESET));
    out.push('\n');
    let legend = [
        (Status::Pending, "needs you"),
        (Status::Running, "working"),
        (Status::Done, "done"),
        (Status::Error, "error"),
        (Status::Idle, "idle"),
    ];
    for (st, label) in legend {
        out.push_str(&format!(
            " {}{}{} {}{}{}\n",
            st.role().ansi(), st.glyph_for(g), RESET, muted, label, RESET
        ));
    }
    out.push('\n');
    out.push_str(&format!("{} click a row to jump{}\n", muted, RESET));
    out
}
```

In `src/lib.rs` wasm `render`, branch to onboarding when there are no tabs yet:

```rust
    fn render(&mut self, rows: usize, cols: usize) {
        let tabrows = self.build_rows();
        let opts = render::RenderOpts {
            width: cols.max(1),
            height: rows,
            now_tick: self.tick,
            glyphs: render::GlyphSet::Nerd,
        };
        if tabrows.is_empty() {
            print!("{}", render::onboarding(&opts));
        } else {
            print!("{}", render::render(&tabrows, &opts));
        }
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/render.rs src/lib.rs
git commit -m "feat(render): onboarding/empty-state panel with legend + click hint"
```

---

### Task 7: Config (glyph set) + permission-gated onboarding

**Files:**
- Modify: `src/lib.rs` (read `glyphs` config in `load()`; add `glyphs: GlyphSet` and `permission_granted: bool` to `State`; set granted from `PermissionRequestResult`; use both in `render`)
- Test: `src/lib.rs` tests

**Interfaces:**
- Consumes: `GlyphSet::from_config` (Task 1), `onboarding` (Task 6).
- Produces: `State.glyphs` (default Nerd, from `config["glyphs"]`), `State.permission_granted` (default false → onboarding until granted).

- [ ] **Step 1: Write the failing test**

In `src/lib.rs` tests:

```rust
#[test]
fn state_defaults_glyphs_to_nerd_and_ungranted() {
    let s = State::default();
    assert_eq!(s.glyphs, crate::status::GlyphSet::Nerd);
    assert!(!s.permission_granted);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib state_defaults_glyphs`
Expected: FAIL — no field `glyphs` / `permission_granted`.

- [ ] **Step 3: Implement config + permission plumbing**

Add fields to `struct State` (host-visible, so no `#[cfg]` gate — give `glyphs` a `Default`):

```rust
    glyphs: crate::status::GlyphSet,
    permission_granted: bool,
```

`GlyphSet` needs `Default`. In `src/status.rs` add:

```rust
impl Default for GlyphSet {
    fn default() -> Self { GlyphSet::Nerd }
}
```

In the wasm `load()`, read config:

```rust
    fn load(&mut self, config: BTreeMap<String, String>) {
        if let Some(g) = config.get("glyphs") {
            self.glyphs = crate::status::GlyphSet::from_config(g);
        }
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ReadCliPipes,
            PermissionType::ChangeApplicationState,
        ]);
        // … existing subscribe(...) and set_selectable(false) unchanged …
    }
```

In the wasm `update`, set the flag:

```rust
            Event::PermissionRequestResult(_) => {
                self.permission_granted = true;
                true
            }
```

In the wasm `render`, use the real glyph set and gate onboarding on permission too:

```rust
        let opts = render::RenderOpts {
            width: cols.max(1),
            height: rows,
            now_tick: self.tick,
            glyphs: self.glyphs,
        };
        if !self.permission_granted || tabrows.is_empty() {
            print!("{}", render::onboarding(&opts));
        } else {
            print!("{}", render::render(&tabrows, &opts));
        }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test`
Expected: PASS (full suite).

- [ ] **Step 5: Verify the wasm build compiles**

Run: `cargo build --target wasm32-wasi` (or the repo's documented build — see `docs/TOOLCHAIN.md`).
Expected: builds clean (no host-call regressions, no unused warnings on the wasm path).

- [ ] **Step 6: Commit**

```bash
git add src/lib.rs src/status.rs
git commit -m "feat(plugin): glyphs config + permission-gated onboarding"
```

---

## Self-Review

**Spec coverage** (against `2026-06-26-zj-agents-gutter-rail-design.md`):

- §3 color roles → Task 1 (`Role::ansi`), used in Tasks 2-6. ✓
- §4 glyph sets + spinner → Task 1 (`GlyphSet`, `glyph_for`, `working_spin`, `msg_spin`); waiting `◆` in Task 1; spinner wired in Task 3. ✓
- §5 header (AGENTS + ·N + rule, `▲` on overflow) → Task 2 (header) + Task 5 (`▲`). ✓
- §6 gutter (bar col0, glyph col1, tint), right slot → Task 3. ✓
- §7 adaptive density / `row_lines` → Task 3; click mapping re-verified Task 3. ✓
- §8 truncation order + `failed→err` → Task 4. ✓
- §9 overflow folding (idle strip, non-idle never fold) → Task 5. ✓
- §10 onboarding panel → Task 6 (render) + Task 7 (permission gate). ✓
- §11 light theme (role-only colors) → enforced by the Global Constraint + `no_emitted_line_exceeds_width`/role assertions; the `error.role().ansi()` check in Task 3 confirms role codes. ✓
- §12 config (`glyphs`) → Task 7. (`width` hint omitted — pane `cols` is authoritative; YAGNI. Noted as an intentional deviation.)
- §13 module impact → matches (status.rs, render.rs, lib.rs only). ✓
- §14 testing items → covered across Tasks 1-7. ✓

**Deviation noted:** §12's optional `width` config key is dropped (YAGNI — the pane width from `cols` is always correct and a manual override has no clear use). If desired later it's a one-line add in `load()`.

**Placeholder scan:** no TBD/TODO; every code step shows complete code.

**Type consistency:** `RenderOpts { width, height, now_tick, glyphs }` consistent across Tasks 2-7; `right_slot` signature change (adds `width`) is introduced in Task 4 with its call site updated in the same task; `row_lines(&TabAgg)` signature unchanged throughout; `glyph_for(GlyphSet)` / `role()` names consistent; `onboarding`/`render` both take `&RenderOpts`.
