# Rich Tab Signals Implementation Plan

> **⚠ Historical / completed-and-diverged (kept for context).** Bell, header,
> and tab naming shipped; the long-running `⚠` "stuck" cue (and its `stuck_secs`
> knob) was dropped in commit `d3c6b75`. See `src/render.rs` / `src/naming.rs`.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add four push-only "richer tab signal" features to the zj-radar sidebar — bell/attention marker, roll-up summary header, long-running cue, and real tab naming — so a many-agent Zellij session is triageable at a glance.

**Architecture:** Three features (long-running cue, bell, header) are pure additions to `render.rs` plus small `lib.rs` capture/glue. The fourth (tab naming) adds a pure `naming.rs` module whose `compute_renames` returns a position→name diff, with the host glue making fire-and-forget `rename_tab` calls. No blocking host queries anywhere (the smart-tabs-postmortem.md rule).

**Tech Stack:** Rust (edition 2021), `zellij-tile = "0.44"`, `serde`/`serde_json`, target `wasm32-wasip1`, Zellij 0.44.3.

## Global Constraints

- Pure modules (`status`, `payload`, `state`, `model`, `render`, **`naming`**) must NOT import `zellij-tile`. Only `lib.rs` imports it.
- **No blocking host calls** (`get_pane_running_command`, `get_pane_cwd`, …) on any path. Only fire-and-forget actions (`rename_tab`, `switch_tab_to`) and pushed events.
- Line-2 strings (`repo/branch · tag`) must stay ANSI-free because `truncate()` counts `chars()` — embedding ANSI there breaks width math. Line-1 and header ANSI is fine because it is appended *outside* `truncate()`.
- `render::row_lines` + `render::header_lines` are the single source of truth for vertical layout; both `render` and `lib::tab_position_at_line` must agree.
- Commit after every task with `git commit --no-gpg-sign` (GPG pinentry is non-interactive here).
- Run the full suite with `cargo test` (host target). All tasks must leave it green.
- DRY, YAGNI, TDD, frequent commits.

## File Structure

```
src/
├── lib.rs       # host glue: TabLite(+has_bell), State(+applied_names), PaneLite capture, apply_renames
├── render.rs    # +STUCK_SECS, ⚠ in detail_tag, TabRow.has_bell + ⚑, summary()/header_lines()/header render
├── naming.rs    # NEW (pure): PaneLite, is_default_name, computed_name, compute_renames
├── model.rs     # unchanged (aggregate still takes &[u32])
├── state.rs     # unchanged
├── status.rs    # unchanged
└── payload.rs   # unchanged (sanitize reused for pane titles)
```

---

### Task 1: Long-running cue (⚠ on stuck `running` tabs)

**Files:**
- Modify: `src/render.rs` (`detail_tag`, add `STUCK_SECS`)

**Interfaces:**
- Consumes: existing `detail_tag(agg, now_tick)`, `format_elapsed`.
- Produces: a plain (no-ANSI) ` ⚠` suffix on the line-2 tag when a `Running` tab's elapsed `>= STUCK_SECS`.

- [ ] **Step 1: Add the failing tests** (append to `src/render.rs` `mod tests`)

```rust
#[test]
fn running_under_threshold_has_no_warning() {
    let detail = Detail { repo: "r".into(), branch: "b".into(), msg: "".into(), since_tick: 0, status: Status::Running };
    let rows = vec![TabRow { number: 1, name: "t".into(), active: false, agg: agg(Status::Running, 1, 1, Some(detail)) }];
    assert!(!render(&rows, 30, 599).contains('⚠'));
}

#[test]
fn running_at_threshold_shows_warning() {
    let detail = Detail { repo: "r".into(), branch: "b".into(), msg: "".into(), since_tick: 0, status: Status::Running };
    let rows = vec![TabRow { number: 1, name: "t".into(), active: false, agg: agg(Status::Running, 1, 1, Some(detail)) }];
    assert!(render(&rows, 30, 600).contains('⚠'));
}

#[test]
fn done_with_long_elapsed_has_no_warning() {
    let detail = Detail { repo: "r".into(), branch: "b".into(), msg: "".into(), since_tick: 0, status: Status::Done };
    let rows = vec![TabRow { number: 1, name: "t".into(), active: false, agg: agg(Status::Done, 1, 1, Some(detail)) }];
    assert!(!render(&rows, 30, 10_000).contains('⚠'));
}
```

> Note: these `TabRow` literals do NOT yet have a `has_bell` field — that's added in Task 2. Task 1 runs against the current `TabRow` shape.

- [ ] **Step 2: Run the tests, verify they fail**

Run: `cargo test --lib render::tests::running_at_threshold_shows_warning`
Expected: FAIL (no `⚠` emitted yet).

- [ ] **Step 3: Implement**

Add near the top of `src/render.rs` (below the existing `BOLD` const):

```rust
/// A `running` agent whose elapsed time reaches this (seconds ≈ ticks) is
/// flagged as long-running / possibly stuck.
const STUCK_SECS: u64 = 600;
```

Replace the `Status::Running` arm of `detail_tag`:

```rust
        Status::Running => {
            let e = format_elapsed(elapsed);
            if elapsed >= STUCK_SECS {
                format!("{} ⚠", e)
            } else {
                e
            }
        }
```

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test`
Expected: all pass (38 existing + 3 new = 41).

- [ ] **Step 5: Commit**

```bash
git add src/render.rs
git commit --no-gpg-sign -m "feat: flag long-running agents with ⚠ after 10m"
```

---

### Task 2: Bell / attention marker (⚑)

**Files:**
- Modify: `src/render.rs` (`TabRow` + line-1 render)
- Modify: `src/lib.rs` (`TabLite` + `TabUpdate` capture + `build_rows` + test helper)

**Interfaces:**
- Produces: `render::TabRow` gains `pub has_bell: bool`; a yellow ` ⚑` is appended to line 1 when set.
- Consumes: `TabInfo.has_bell_notification` (bool) from the `TabUpdate` event.

- [ ] **Step 1: Add the field + failing tests in `src/render.rs`**

Add the field to `TabRow`:

```rust
pub struct TabRow {
    pub number: u32,
    pub name: String,
    pub active: bool,
    pub has_bell: bool,
    pub agg: TabAgg,
}
```

Add a yellow const near `BOLD`:

```rust
const YELLOW: &str = "\x1b[33m";
```

Add tests to `mod tests`:

```rust
#[test]
fn bell_renders_marker() {
    let rows = vec![TabRow { number: 1, name: "t".into(), active: false, has_bell: true, agg: agg(Status::Idle, 0, 0, None) }];
    assert!(render(&rows, 24, 0).contains('⚑'));
}

#[test]
fn no_bell_no_marker() {
    let rows = vec![TabRow { number: 1, name: "t".into(), active: false, has_bell: false, agg: agg(Status::Idle, 0, 0, None) }];
    assert!(!render(&rows, 24, 0).contains('⚑'));
}
```

- [ ] **Step 2: Run, verify compile failure then test failure**

Run: `cargo test`
Expected: FAILS TO COMPILE — every existing `TabRow { … }` literal in `render.rs` tests is now missing `has_bell`. This compile error IS the failing state; Step 3 fixes all constructions.

- [ ] **Step 3: Implement render + fix all `TabRow` constructions**

In `render()`, replace the line-1 block (the `count`/`name_budget`/`push_str` for line 1) with:

```rust
        let dot = format!("{}{}{}", row.agg.status.ansi(), row.agg.status.glyph(), RESET);
        let count = if row.agg.total > 1 {
            format!(" {}/{}", row.agg.done, row.agg.total)
        } else {
            String::new()
        };
        // reserve 2 cols for " ⚑" when a bell is set
        let bell_budget = if row.has_bell { 2 } else { 0 };
        let name_budget = width.saturating_sub(4 + count.chars().count() + bell_budget);
        let name = truncate(&row.name, name_budget);
        let name_styled = if row.active {
            format!("{}{}{}", BOLD, name, RESET)
        } else {
            name
        };
        let bell = if row.has_bell {
            format!(" {}⚑{}", YELLOW, RESET)
        } else {
            String::new()
        };
        // line 1: "<dot> <n> <name><count><bell>"
        out.push_str(&format!("{} {} {}{}{}\n", dot, row.number, name_styled, count, bell));
```

Then add `has_bell: false,` to every existing `TabRow { … }` literal in `render.rs` tests (the compiler from Step 2 lists each). The new bell tests already set it.

- [ ] **Step 4: Capture the field in `src/lib.rs`**

Add to `TabLite`:

```rust
struct TabLite {
    position: usize,
    name: String,
    active: bool,
    has_bell: bool,
}
```

In the `Event::TabUpdate` arm, map the field:

```rust
            Event::TabUpdate(tabs) => {
                self.tabs = tabs
                    .into_iter()
                    .map(|t| TabLite {
                        position: t.position,
                        name: t.name,
                        active: t.active,
                        has_bell: t.has_bell_notification,
                    })
                    .collect();
                true
            }
```

In `build_rows`, set it on the produced `TabRow`:

```rust
            rows.push(TabRow {
                number: t.position as u32 + 1,
                name: t.name.clone(),
                active: t.active,
                has_bell: t.has_bell,
                agg: model::aggregate(panes, &self.store),
            });
```

In the lib.rs test helper `make_state_with_tabs`, add `has_bell: false,` to the `TabLite` literal.

- [ ] **Step 5: Run tests, verify pass**

Run: `cargo test`
Expected: all pass (41 + 2 = 43).

> Field-name check: it is `TabInfo.has_bell_notification` in zellij-tile 0.44 — confirm in `~/.cargo` source if the build errors on an unknown field.

- [ ] **Step 6: Commit**

```bash
git add src/render.rs src/lib.rs
git commit --no-gpg-sign -m "feat: surface Zellij tab bell as a ⚑ attention marker"
```

---

### Task 3: Roll-up summary header

**Files:**
- Modify: `src/render.rs` (`summary`, `header_lines`, header render, fix newline-count tests)
- Modify: `src/lib.rs` (`tab_position_at_line` header offset + click tests)

**Interfaces:**
- Produces: `render::summary(rows: &[TabRow]) -> Vec<(Status, usize)>` (tabs by dominant active status, severity-descending, non-zero only); `render::header_lines(rows: &[TabRow]) -> usize` (0 or 1).
- Consumes: existing `TabRow`/`TabAgg`.

- [ ] **Step 1: Add failing tests to `src/render.rs`**

```rust
#[test]
fn summary_counts_tabs_by_dominant_status_active_only() {
    let rows = vec![
        TabRow { number: 1, name: "a".into(), active: false, has_bell: false, agg: agg(Status::Running, 0, 0, None) },
        TabRow { number: 2, name: "b".into(), active: false, has_bell: false, agg: agg(Status::Running, 0, 0, None) },
        TabRow { number: 3, name: "c".into(), active: false, has_bell: false, agg: agg(Status::Pending, 0, 0, None) },
        TabRow { number: 4, name: "d".into(), active: false, has_bell: false, agg: agg(Status::Idle, 0, 0, None) },
    ];
    // severity order: Error, Pending, Running, Done; Idle excluded
    assert_eq!(summary(&rows), vec![(Status::Pending, 1), (Status::Running, 2)]);
}

#[test]
fn summary_empty_when_all_idle() {
    let rows = vec![TabRow { number: 1, name: "a".into(), active: false, has_bell: false, agg: agg(Status::Idle, 0, 0, None) }];
    assert!(summary(&rows).is_empty());
    assert_eq!(header_lines(&rows), 0);
}

#[test]
fn header_line_emitted_when_active() {
    let rows = vec![TabRow { number: 1, name: "a".into(), active: false, has_bell: false, agg: agg(Status::Running, 0, 0, None) }];
    assert_eq!(header_lines(&rows), 1);
    let s = render(&rows, 24, 0);
    // first line is the header (contains the running glyph + count), then the tab row
    assert!(s.lines().next().unwrap().contains(Status::Running.glyph()));
}
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test --lib render::tests::summary_counts_tabs_by_dominant_status_active_only`
Expected: FAIL to compile (`summary`/`header_lines` undefined).

- [ ] **Step 3: Implement in `src/render.rs`**

```rust
/// Count tabs by their dominant active status, severity-descending, non-zero only.
pub fn summary(rows: &[TabRow]) -> Vec<(Status, usize)> {
    use Status::*;
    let order = [Error, Pending, Running, Done];
    let mut counts = [0usize; 4];
    for r in rows {
        match r.agg.status {
            Error => counts[0] += 1,
            Pending => counts[1] += 1,
            Running => counts[2] += 1,
            Done => counts[3] += 1,
            Idle => {}
        }
    }
    order
        .iter()
        .enumerate()
        .filter_map(|(i, s)| (counts[i] > 0).then_some((*s, counts[i])))
        .collect()
}

/// 1 if a summary header will be rendered, else 0. Single source of truth for
/// the header's vertical span (consumed by click mapping in lib.rs).
pub fn header_lines(rows: &[TabRow]) -> usize {
    if summary(rows).is_empty() {
        0
    } else {
        1
    }
}
```

At the top of `render()`, before the `for row in rows` loop, prepend the header:

```rust
    let mut out = String::new();
    let sum = summary(rows);
    if !sum.is_empty() {
        let parts: Vec<String> = sum
            .iter()
            .map(|(s, n)| format!("{}{}{}{}", s.ansi(), s.glyph(), n, RESET))
            .collect();
        out.push_str(&parts.join(" "));
        out.push('\n');
    }
```

> The header is short (≤4 buckets like `✗1 ◑1 ◐2 ●3`, ~11 visible cols) and is intentionally not run through `truncate()`; its ANSI lives outside `truncate` so width math is unaffected.

- [ ] **Step 4: Fix existing newline-count tests in `src/render.rs`**

The header now prepends a line whenever a row is active. Update these existing tests:
- `agent_tab_has_three_lines_with_count_tag_and_msg`: change `assert_eq!(s.matches('\n').count(), 3)` → `4`.
- `agent_tab_with_empty_msg_has_two_lines`: change `2` → `3`.
- `no_emitted_line_exceeds_width`: change `assert_eq!(s.matches('\n').count(), 3)` → `4` (the per-line width assertion still holds — the header fits).

`plain_tab_renders_name_only_no_second_line` and `narrow_width_truncates_with_ellipsis` use `Idle` rows → no header → unchanged.

- [ ] **Step 5: Offset click mapping in `src/lib.rs`**

In `tab_position_at_line`, account for the header before walking row spans. Add the header height to the starting cursor:

```rust
    fn tab_position_at_line(&self, line: isize) -> Option<usize> {
        if line < 0 {
            return None;
        }
        let target = line as usize;
        let rows = self.build_rows();
        let mut cursor = render::header_lines(&rows); // header occupies the first line(s)
        if target < cursor {
            return None; // click landed on the header → no tab
        }
        let mut sorted = self.tabs.clone();
        sorted.sort_by_key(|t| t.position);
        for t in &sorted {
            let empty = Vec::new();
            let panes = self.tab_panes.get(&t.position).unwrap_or(&empty);
            let agg = model::aggregate(panes, &self.store);
            let span = render::row_lines(&agg);
            if target >= cursor && target < cursor + span {
                return Some(t.position);
            }
            cursor += span;
        }
        None
    }
```

> `build_rows()` already sorts by position internally; using it here keeps the header decision identical to what `render()` does.

- [ ] **Step 6: Add click-offset tests to `src/lib.rs`**

```rust
    #[test]
    fn header_shifts_click_mapping_down_by_one() {
        // One active agent tab (→ header present) at position 0 with msg (3 lines),
        // a plain tab at position 1.
        let mut state = make_state_with_tabs(&[(0, "agent", false), (1, "plain", false)]);
        state.tab_panes.insert(0, vec![10]);
        apply_payload(&mut state, 10, Status::Running, 1); // active → header line at row 0
        // row 0 = header (no tab)
        assert_eq!(state.tab_position_at_line(0), None);
        // rows 1,2,3 = agent tab (3 lines) at position 0
        assert_eq!(state.tab_position_at_line(1), Some(0));
        assert_eq!(state.tab_position_at_line(3), Some(0));
        // row 4 = plain tab at position 1
        assert_eq!(state.tab_position_at_line(4), Some(1));
    }

    #[test]
    fn no_header_when_idle_click_mapping_unchanged() {
        // All-idle tabs → no header → line 0 maps to position 0.
        let state = make_state_with_tabs(&[(0, "a", false), (1, "b", false)]);
        assert_eq!(state.tab_position_at_line(0), Some(0));
        assert_eq!(state.tab_position_at_line(1), Some(1));
    }
```

> `tab_position_at_line` now calls `build_rows()`, which needs `tab_panes` ids — for these tests the existing `Vec<u32>` shape still applies (the PaneLite refactor is Task 4).

- [ ] **Step 7: Run tests, verify pass**

Run: `cargo test`
Expected: all pass (43 + 3 render + 2 lib = 48).

- [ ] **Step 8: Commit**

```bash
git add src/render.rs src/lib.rs
git commit --no-gpg-sign -m "feat: roll-up summary header with header-aware click mapping"
```

---

### Task 4: `PaneLite` data-model refactor (prep for naming)

**Files:**
- Create: `src/naming.rs` (just `PaneLite` + its test for now)
- Modify: `src/lib.rs` (`mod naming;`, `tab_panes` value type, `PaneUpdate` capture, `build_rows`, `tab_position_at_line`, tests)

**Interfaces:**
- Produces: `naming::PaneLite { pub id: u32, pub title: String, pub is_focused: bool }` (derives `Clone, Debug, Default, PartialEq, Eq`).
- Changes: `State.tab_panes: HashMap<usize, Vec<PaneLite>>` (was `Vec<u32>`). `model::aggregate` still takes `&[u32]`; call sites extract ids.

This task is a behavior-preserving refactor — all existing tests stay green (after mechanical updates).

- [ ] **Step 1: Create `src/naming.rs` with `PaneLite` + a test**

```rust
//! Pure tab-naming logic. No zellij-tile dependency.

/// Display-relevant subset of a terminal pane (from PaneInfo).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaneLite {
    pub id: u32,
    pub title: String,
    pub is_focused: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_lite_defaults_are_empty() {
        let p = PaneLite::default();
        assert_eq!(p.id, 0);
        assert!(p.title.is_empty());
        assert!(!p.is_focused);
    }
}
```

- [ ] **Step 2: Register the module and flip the type in `src/lib.rs`**

Add the module declaration alongside the others (with the same `dead_code` cfg attr they use):

```rust
#[cfg_attr(all(not(target_arch = "wasm32"), not(test)), allow(dead_code))]
mod naming;
```

Add an import next to `use state::StateStore;`:

```rust
use naming::PaneLite;
```

Change the `State` field:

```rust
    tab_panes: HashMap<usize, Vec<PaneLite>>, // tab position -> terminal panes
```

- [ ] **Step 3: Capture title + is_focused in the `PaneUpdate` arm**

Replace the manifest loop body in `Event::PaneUpdate`:

```rust
            Event::PaneUpdate(manifest) => {
                let mut tab_panes: HashMap<usize, Vec<PaneLite>> = HashMap::new();
                let mut live: HashSet<u32> = HashSet::new();
                let mut focused_terminal: Option<u32> = None;
                for (tab_pos, panes) in manifest.panes {
                    for p in panes {
                        if p.is_plugin {
                            continue;
                        }
                        tab_panes.entry(tab_pos).or_default().push(PaneLite {
                            id: p.id,
                            title: payload::sanitize(&p.title, 40),
                            is_focused: p.is_focused,
                        });
                        live.insert(p.id);
                        if p.is_focused {
                            focused_terminal = Some(p.id);
                        }
                    }
                }
                self.tab_panes = tab_panes;
                self.store.prune(&live);
                if let Some(id) = focused_terminal {
                    self.store.on_pane_focused(id, self.tick);
                }
                true
            }
```

> `payload::sanitize` is already `pub`. Confirm `PaneInfo.title` and `PaneInfo.id` field names against zellij-tile 0.44.

- [ ] **Step 4: Extract ids at the two call sites**

In `build_rows`, replace the `panes` usage:

```rust
        for t in &sorted {
            let empty = Vec::new();
            let panes = self.tab_panes.get(&t.position).unwrap_or(&empty);
            let ids: Vec<u32> = panes.iter().map(|p| p.id).collect();
            rows.push(TabRow {
                number: t.position as u32 + 1,
                name: t.name.clone(),
                active: t.active,
                has_bell: t.has_bell,
                agg: model::aggregate(&ids, &self.store),
            });
        }
```

In `tab_position_at_line`, the same extraction inside the loop:

```rust
            let empty = Vec::new();
            let panes = self.tab_panes.get(&t.position).unwrap_or(&empty);
            let ids: Vec<u32> = panes.iter().map(|p| p.id).collect();
            let agg = model::aggregate(&ids, &self.store);
```

- [ ] **Step 5: Fix the lib.rs tests' `tab_panes` insertions**

Every `state.tab_panes.insert(pos, vec![ID])` in lib.rs tests becomes a `PaneLite` vec. Use a small helper at the top of `mod tests`:

```rust
    fn pane(id: u32) -> PaneLite {
        PaneLite { id, ..Default::default() }
    }
```

Then change each insertion, e.g.:
- `state.tab_panes.insert(0, vec![42]);` → `state.tab_panes.insert(0, vec![pane(42)]);`
- `state.tab_panes.insert(0, vec![10]);` → `state.tab_panes.insert(0, vec![pane(10)]);`
- `state.tab_panes.insert(0, vec![1]);` / `insert(2, vec![2]);` → wrap each id with `pane(...)`.

(The compiler lists every site after Step 2's type change.)

- [ ] **Step 6: Run tests, verify pass**

Run: `cargo test`
Expected: all pass (48 + 1 = 49). Pure refactor — no behavior change.

- [ ] **Step 7: Commit**

```bash
git add src/naming.rs src/lib.rs
git commit --no-gpg-sign -m "refactor: carry pane title/focus in tab_panes via PaneLite"
```

---

### Task 5: Tab naming logic + `rename_tab` wiring

**Files:**
- Modify: `src/naming.rs` (`is_default_name`, `computed_name`, `compute_renames` + tests)
- Modify: `src/lib.rs` (`State.applied_names`, `apply_renames`, call it from `PaneUpdate` and `pipe`)

**Interfaces:**
- Consumes: `naming::PaneLite`, `state::StateStore`.
- Produces: `naming::compute_renames(tabs: &[(usize, String)], tab_panes: &HashMap<usize, Vec<PaneLite>>, store: &StateStore, applied: &HashMap<usize, String>) -> Vec<(usize, String)>`.

- [ ] **Step 1: Add failing tests to `src/naming.rs`**

```rust
    use crate::payload::StatusPayload;
    use crate::state::StateStore;
    use crate::status::Status;
    use std::collections::HashMap;

    fn store_with(id: u32, repo: &str) -> StateStore {
        let mut s = StateStore::default();
        s.apply(
            StatusPayload {
                pane_id: id,
                status: Status::Running,
                repo: repo.into(),
                branch: "b".into(),
                msg: "m".into(),
                on_focus: None,
                seq: None,
                source: "test".into(),
            },
            1,
        );
        s
    }

    #[test]
    fn is_default_name_matches_zellij_default() {
        assert!(is_default_name("Tab #1"));
        assert!(is_default_name("Tab #12"));
        assert!(!is_default_name("Tab #"));
        assert!(!is_default_name("pinky"));
        assert!(!is_default_name("Tab #x"));
    }

    #[test]
    fn computed_name_prefers_agent_repo() {
        let store = store_with(7, "pinky");
        let panes = vec![PaneLite { id: 7, title: "nvim".into(), is_focused: true }];
        assert_eq!(computed_name(&panes, &store), Some("pinky".into()));
    }

    #[test]
    fn computed_name_falls_back_to_focused_title() {
        let store = StateStore::default();
        let panes = vec![
            PaneLite { id: 1, title: "bash".into(), is_focused: false },
            PaneLite { id: 2, title: "nvim".into(), is_focused: true },
        ];
        assert_eq!(computed_name(&panes, &store), Some("nvim".into()));
    }

    #[test]
    fn computed_name_none_when_no_signal() {
        let store = StateStore::default();
        let panes = vec![PaneLite { id: 1, title: "".into(), is_focused: false }];
        assert_eq!(computed_name(&panes, &store), None);
    }

    #[test]
    fn compute_renames_renames_default_skips_manual_and_equal() {
        let store = store_with(7, "pinky");
        let mut tab_panes: HashMap<usize, Vec<PaneLite>> = HashMap::new();
        tab_panes.insert(0, vec![PaneLite { id: 7, title: "x".into(), is_focused: true }]); // default name → rename
        tab_panes.insert(1, vec![PaneLite { id: 7, title: "x".into(), is_focused: true }]); // manual name → skip
        tab_panes.insert(2, vec![PaneLite { id: 7, title: "x".into(), is_focused: true }]); // already == desired → skip
        let tabs = vec![
            (0, "Tab #1".to_string()),
            (1, "my-manual-name".to_string()),
            (2, "pinky".to_string()),
        ];
        let applied = HashMap::new();
        let out = compute_renames(&tabs, &tab_panes, &store, &applied);
        assert_eq!(out, vec![(0, "pinky".to_string())]);
    }

    #[test]
    fn compute_renames_updates_its_own_prior_name() {
        // tab currently shows our last auto-applied name, but the desired name changed.
        let store = store_with(7, "newrepo");
        let mut tab_panes: HashMap<usize, Vec<PaneLite>> = HashMap::new();
        tab_panes.insert(0, vec![PaneLite { id: 7, title: "x".into(), is_focused: true }]);
        let tabs = vec![(0, "oldrepo".to_string())];
        let mut applied = HashMap::new();
        applied.insert(0usize, "oldrepo".to_string()); // we set "oldrepo" before
        let out = compute_renames(&tabs, &tab_panes, &store, &applied);
        assert_eq!(out, vec![(0, "newrepo".to_string())]);
    }
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test --lib naming::tests::compute_renames_renames_default_skips_manual_and_equal`
Expected: FAIL to compile (functions undefined).

- [ ] **Step 3: Implement in `src/naming.rs`**

```rust
use crate::state::StateStore;
use std::collections::HashMap;

/// True if `name` is a Zellij default tab name like "Tab #1".
pub fn is_default_name(name: &str) -> bool {
    name.strip_prefix("Tab #")
        .map_or(false, |rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

/// Desired display name for one tab, or None if no push signal is available.
/// Agent repo (focused pane first, then any) wins; else the focused (then first)
/// pane's title.
pub fn computed_name(panes: &[PaneLite], store: &StateStore) -> Option<String> {
    let repo_of = |p: &PaneLite| {
        store
            .get(p.id)
            .map(|s| s.repo.clone())
            .filter(|r| !r.is_empty())
    };
    let focused = panes.iter().find(|p| p.is_focused);
    if let Some(p) = focused {
        if let Some(r) = repo_of(p) {
            return Some(r);
        }
    }
    for p in panes {
        if let Some(r) = repo_of(p) {
            return Some(r);
        }
    }
    if let Some(p) = focused {
        if !p.title.is_empty() {
            return Some(p.title.clone());
        }
    }
    if let Some(p) = panes.first() {
        if !p.title.is_empty() {
            return Some(p.title.clone());
        }
    }
    None
}

/// Position→new-name diff. Only renames a tab whose current name is a Zellij
/// default OR equals the name we last auto-applied (clobber guard); and only
/// when the desired name differs from the current name (change/loop guard).
pub fn compute_renames(
    tabs: &[(usize, String)],
    tab_panes: &HashMap<usize, Vec<PaneLite>>,
    store: &StateStore,
    applied: &HashMap<usize, String>,
) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (pos, current) in tabs {
        let empty = Vec::new();
        let panes = tab_panes.get(pos).unwrap_or(&empty);
        let Some(desired) = computed_name(panes, store) else {
            continue;
        };
        if &desired == current {
            continue;
        }
        let ours = applied.get(pos).map_or(false, |n| n == current);
        if is_default_name(current) || ours {
            out.push((*pos, desired));
        }
    }
    out
}
```

- [ ] **Step 4: Run, verify naming tests pass**

Run: `cargo test --lib naming`
Expected: all naming tests pass.

- [ ] **Step 5: Wire `apply_renames` into `src/lib.rs`**

Add the field to `State` (with the same dead-code cfg the wasm-only fields use):

```rust
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    applied_names: HashMap<usize, String>,
```

Add a wasm-only method in the `#[cfg(target_arch = "wasm32")] impl State` block (next to `arm_timer_if_needed`):

```rust
    fn apply_renames(&mut self) {
        let tabs: Vec<(usize, String)> = self
            .tabs
            .iter()
            .map(|t| (t.position, t.name.clone()))
            .collect();
        let changes =
            naming::compute_renames(&tabs, &self.tab_panes, &self.store, &self.applied_names);
        for (pos, name) in changes {
            rename_tab(pos as u32 + 1, &name);
            self.applied_names.insert(pos, name);
        }
    }
```

> Verify `rename_tab`'s signature in zellij-tile 0.44 — it is `rename_tab(tab_position: u32, name: impl Into<String>)` (1-indexed, matching `switch_tab_to`). If it takes `&str`, `&name` is correct; if `String`, pass `name.clone()` before the `insert`.

Call it after the state that feeds naming changes — at the end of the `Event::PaneUpdate` arm (after `on_pane_focused`, before `true`) and in `pipe` (after `self.store.apply`):

```rust
                // …end of PaneUpdate arm:
                self.apply_renames();
                true
```

```rust
    fn pipe(&mut self, message: PipeMessage) -> bool {
        if message.name == PIPE_NAME {
            if let Some(raw) = &message.payload {
                if let Some(p) = payload::parse(raw) {
                    self.store.apply(p, self.tick);
                    self.apply_renames();
                    self.arm_timer_if_needed();
                    return true;
                }
            }
        }
        false
    }
```

- [ ] **Step 6: Build for wasm to typecheck the glue**

Run: `cargo build --target wasm32-wasip1`
Expected: compiles (this is the only check that exercises the `rename_tab` call and the wasm-gated `apply_renames`).

- [ ] **Step 7: Run host tests**

Run: `cargo test`
Expected: all pass (49 + 6 naming = 55).

- [ ] **Step 8: Commit**

```bash
git add src/naming.rs src/lib.rs
git commit --no-gpg-sign -m "feat: auto-name tabs from agent repo / pane title via rename_tab"
```

---

## Self-Review

**Spec coverage:**
- A bell → Task 2. B roll-up → Task 3. C long-running → Task 1. D naming → Tasks 4 (data model) + 5 (logic/wiring). Cross-cutting click offset → Task 3 Step 5–6. `tab_panes` carrying title/focus → Task 4. `applied_names` + guards → Task 5. No-blocking-host-calls → only `rename_tab` (fire-and-forget) added; verified in Task 5 notes.

**Placeholder scan:** none — every step has concrete code or an exact mechanical instruction the compiler enforces. The three "verify against zellij-tile 0.44" notes are real API-version checks (`has_bell_notification`, `PaneInfo.title/id`, `rename_tab` signature), not deferred work.

**Type consistency:** `TabRow` gains `has_bell` in Task 2 and every literal is updated there. `tab_panes` becomes `Vec<PaneLite>` in Task 4 and `aggregate` keeps `&[u32]` via id extraction. `compute_renames`/`computed_name`/`is_default_name` signatures match between Task 5 tests and impl. `apply_renames` reads `self.applied_names` defined in the same task.
