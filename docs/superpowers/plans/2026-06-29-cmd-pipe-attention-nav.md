# Command Pipe + Attention-Tab Cycling Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `zj_radar.cmd.v1` imperative command pipe with `attention-next` / `attention-prev` verbs that cycle keyboard focus through the tabs needing attention, driven by a `MessagePlugin` keybind.

**Architecture:** A new pure function (`cycle_attention`) computes a deterministic absolute target tab; a thin `RadarState` adapter feeds it live tab/rollup data; `PluginRuntime::command` wraps the result in the existing `Effect::SwitchTab`; the existing `pipe()` glue grows one `cmd.v1` arm mirroring `config.v1`. No new event subscription, no `set_selectable(true)` — the plugin stays passive. Determinism makes the per-tab-instance broadcast idempotent, so no lock is needed.

**Tech Stack:** Rust → `wasm32-wasip1`, `proptest`, `insta` (existing harness). Tests run on the host (`cargo test`); the wasm-gated `pipe()` arm is validated by `cargo check --target wasm32-wasip1`.

## Global Constraints

- No new `subscribe()` entry; no `set_selectable(true)`. The plugin stays passive (`docs/design.md` passive-renderer constraint).
- Parsing never fails: unknown/malformed command payloads are a silent no-op (mirror `config.v1`).
- The module is named `cmd` (NOT `command` — `mod command` already exists for `CommandStore`).
- No rustfmt in this repo (per project convention); match surrounding style by hand.
- Commit messages are conventional and end with the trailer:
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`
- Attention set = `Status::Pending | Status::Error | Status::Done`. `Running`/`Idle` are skipped. Urgency rank (`pending > error > done`) is documentation-only here; index order drives traversal.

---

### Task 1: Pure attention-cycling core

**Files:**
- Modify: `src/status.rs` (add `Status::needs_attention`, near `is_active` ~line 94-98)
- Modify: `src/radar_state.rs` (add `Direction` enum + free `cycle_attention` fn; tests in existing `mod tests`)
- Test: `src/radar_state.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Produces:
  - `Status::needs_attention(self) -> bool`
  - `pub(crate) enum Direction { Next, Prev }` (in `radar_state`)
  - `fn cycle_attention(tabs: &[(usize, Status)], active: Option<usize>, dir: Direction) -> Option<usize>` (module-private free fn in `radar_state`)

- [ ] **Step 1: Write the failing test for `needs_attention`**

In `src/status.rs`, inside its `#[cfg(test)] mod tests` (add the module if absent):

```rust
#[test]
fn needs_attention_covers_pending_error_done_only() {
    assert!(Status::Pending.needs_attention());
    assert!(Status::Error.needs_attention());
    assert!(Status::Done.needs_attention());
    assert!(!Status::Running.needs_attention());
    assert!(!Status::Idle.needs_attention());
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p zj-radar status::tests::needs_attention_covers_pending_error_done_only`
Expected: FAIL — `no method named needs_attention`.

- [ ] **Step 3: Implement `needs_attention`**

In `src/status.rs`, in `impl Status` (alongside `is_active`):

```rust
    /// Tabs in these states want the user's eyes (the radar's "attention set").
    /// `Running`/`Idle` are excluded — they need no action.
    pub fn needs_attention(self) -> bool {
        matches!(self, Status::Pending | Status::Error | Status::Done)
    }
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test -p zj-radar status::tests::needs_attention_covers_pending_error_done_only`
Expected: PASS.

- [ ] **Step 5: Write the failing unit tests for `cycle_attention`**

In `src/radar_state.rs`, inside the existing `#[cfg(test)] mod tests`:

```rust
#[test]
fn cycle_attention_empty_set_is_none() {
    let tabs = [(0usize, Status::Idle), (1, Status::Running)];
    assert_eq!(cycle_attention(&tabs, Some(0), Direction::Next), None);
    assert_eq!(cycle_attention(&tabs, Some(0), Direction::Prev), None);
}

#[test]
fn cycle_attention_sole_member_equal_to_active_is_none() {
    let tabs = [(0usize, Status::Pending), (1, Status::Running)];
    assert_eq!(cycle_attention(&tabs, Some(0), Direction::Next), None);
    assert_eq!(cycle_attention(&tabs, Some(0), Direction::Prev), None);
}

#[test]
fn cycle_attention_next_and_prev_wrap_around() {
    // attention at positions 2 and 5
    let tabs = [(2usize, Status::Pending), (5, Status::Error)];
    // active = 2 → next is 5, prev wraps to 5
    assert_eq!(cycle_attention(&tabs, Some(2), Direction::Next), Some(5));
    assert_eq!(cycle_attention(&tabs, Some(2), Direction::Prev), Some(5));
    // active = 5 → next wraps to 2, prev is 2
    assert_eq!(cycle_attention(&tabs, Some(5), Direction::Next), Some(2));
    assert_eq!(cycle_attention(&tabs, Some(5), Direction::Prev), Some(2));
}

#[test]
fn cycle_attention_active_outside_set_enters_set() {
    let tabs = [(2usize, Status::Pending), (5, Status::Done)];
    // active = 3 (not an attention tab) → next 5, prev 2
    assert_eq!(cycle_attention(&tabs, Some(3), Direction::Next), Some(5));
    assert_eq!(cycle_attention(&tabs, Some(3), Direction::Prev), Some(2));
    // active = None → next = smallest, prev = largest
    assert_eq!(cycle_attention(&tabs, None, Direction::Next), Some(2));
    assert_eq!(cycle_attention(&tabs, None, Direction::Prev), Some(5));
}
```

- [ ] **Step 6: Run them to verify they fail**

Run: `cargo test -p zj-radar radar_state::tests::cycle_attention`
Expected: FAIL — `cannot find function cycle_attention` / `cannot find type Direction`.

- [ ] **Step 7: Implement `Direction` and `cycle_attention`**

In `src/radar_state.rs` at module scope (near the other types, e.g. just below the `use` block; `use crate::status::Status;` already exists at line 10):

```rust
/// Direction for attention-tab cycling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    Next,
    Prev,
}

/// Pick the next/previous tab position that needs attention, relative to the
/// active tab, wrapping at the ends. Pure over `(position, status)` pairs so it
/// is trivially testable and deterministic — every per-tab plugin instance that
/// receives the same broadcast computes the identical target (idempotent switch).
///
/// Returns `None` when no tab needs attention, or when the only attention tab is
/// already active (a no-op).
fn cycle_attention(
    tabs: &[(usize, Status)],
    active: Option<usize>,
    dir: Direction,
) -> Option<usize> {
    let mut members: Vec<usize> = tabs
        .iter()
        .filter(|(_, s)| s.needs_attention())
        .map(|(p, _)| *p)
        .collect();
    members.sort_unstable();
    members.dedup();
    if members.is_empty() {
        return None;
    }
    let target = match (dir, active) {
        (Direction::Next, Some(a)) => members
            .iter()
            .copied()
            .find(|&p| p > a)
            .or_else(|| members.first().copied()),
        (Direction::Next, None) => members.first().copied(),
        (Direction::Prev, Some(a)) => members
            .iter()
            .rev()
            .copied()
            .find(|&p| p < a)
            .or_else(|| members.last().copied()),
        (Direction::Prev, None) => members.last().copied(),
    };
    match target {
        Some(t) if Some(t) != active => Some(t),
        _ => None,
    }
}
```

- [ ] **Step 8: Run the unit tests to verify they pass**

Run: `cargo test -p zj-radar radar_state::tests::cycle_attention`
Expected: PASS (4 tests).

- [ ] **Step 9: Write the failing proptest (cycling invariant)**

In `src/radar_state.rs`, inside the existing `proptest! { ... }` block in `mod tests` (the file already imports proptest; follow the existing macro style):

```rust
#[test]
fn attention_next_visits_every_member_and_returns_to_start(
    members in proptest::collection::btree_set(0usize..64, 1..8),
    start_pick in 0usize..8,
) {
    let members: Vec<usize> = members.into_iter().collect();
    let m = members.len();
    let start = members[start_pick % m];
    let tabs: Vec<(usize, Status)> =
        members.iter().map(|&p| (p, Status::Pending)).collect();

    let mut active = Some(start);
    let mut visited = Vec::new();
    for _ in 0..m {
        match cycle_attention(&tabs, active, Direction::Next) {
            None => {
                // Only legal when the set has a single member equal to active.
                prop_assert_eq!(m, 1);
                visited.push(start);
            }
            Some(n) => {
                prop_assert_ne!(Some(n), active);
                visited.push(n);
                active = Some(n);
            }
        }
    }
    // Returned to the origin after a full lap, having touched every member once.
    prop_assert_eq!(active, Some(start));
    let mut seen = visited.clone();
    seen.sort_unstable();
    seen.dedup();
    prop_assert_eq!(seen, members);
}
```

- [ ] **Step 10: Run the proptest to verify it passes**

Run: `cargo test -p zj-radar radar_state::tests::attention_next_visits_every_member_and_returns_to_start`
Expected: PASS.

- [ ] **Step 11: Commit**

```bash
git add src/status.rs src/radar_state.rs
git commit -m "feat(radar): pure attention-tab cycling core

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: `RadarState::next_attention_tab` adapter

**Files:**
- Modify: `src/radar_state.rs` (add method on `impl RadarState`, near `rows()`/`tab_display`)
- Test: `src/radar_state.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Consumes: `cycle_attention`, `Direction` (Task 1); existing `self.tabs: Vec<RadarTab>` (fields `position`, `active`), `self.tab_panes`, `fn tab_display(&self, panes: &[TerminalPane]) -> TabDisplay` (returns `.status: Status`).
- Produces: `pub(crate) fn next_attention_tab(&self, dir: Direction) -> Option<usize>`

- [ ] **Step 1: Write the failing test**

In `src/radar_state.rs` `mod tests` (the file already has helpers to build tabs/panes via `tabs_changed` + `set_tab_panes_for_position`; `status_mut().apply(payload, tick)` sets a pane's status — mirror existing tests):

```rust
#[test]
fn next_attention_tab_skips_running_and_idle() {
    let mut st = RadarState::default();
    // 3 tabs at positions 0,1,2; tab 0 active.
    st.tabs_changed(vec![
        RadarTab { id: TabId::new(1), position: 0, name: "a".into(), active: true,  has_bell: false },
        RadarTab { id: TabId::new(2), position: 1, name: "b".into(), active: false, has_bell: false },
        RadarTab { id: TabId::new(3), position: 2, name: "c".into(), active: false, has_bell: false },
    ]);
    // tab 0: running (not attention); tab 1: pending (attention); tab 2: idle.
    st.set_tab_panes_for_position(0, vec![pane(10)]);
    st.set_tab_panes_for_position(1, vec![pane(11)]);
    st.status_mut().apply(payload_for(10, Status::Running), 1);
    st.status_mut().apply(payload_for(11, Status::Pending), 1);

    assert_eq!(st.next_attention_tab(Direction::Next), Some(1));
    assert_eq!(st.next_attention_tab(Direction::Prev), Some(1));
}

#[test]
fn next_attention_tab_none_when_no_attention() {
    let mut st = RadarState::default();
    st.tabs_changed(vec![
        RadarTab { id: TabId::new(1), position: 0, name: "a".into(), active: true, has_bell: false },
    ]);
    st.set_tab_panes_for_position(0, vec![pane(10)]);
    st.status_mut().apply(payload_for(10, Status::Running), 1);
    assert_eq!(st.next_attention_tab(Direction::Next), None);
}
```

> Note: confirm the exact field set of `RadarTab` and the `pane(...)` / `payload_for(...)` helper names in this file's test module before running; reuse whatever the neighboring tests use. If a `tab(pos, name, active)` helper exists, prefer it over the struct literal.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p zj-radar radar_state::tests::next_attention_tab`
Expected: FAIL — `no method named next_attention_tab`.

- [ ] **Step 3: Implement the adapter**

In `src/radar_state.rs`, on `impl RadarState` (mirror `rows()`'s sort + `tab_display` use):

```rust
    /// Target tab position for an `attention-next`/`attention-prev` command, or
    /// `None` for a no-op. Reads the live active tab and per-tab rollup; the
    /// pure `cycle_attention` owns the ordering/wrap logic.
    pub(crate) fn next_attention_tab(&self, dir: Direction) -> Option<usize> {
        let mut sorted = self.tabs.clone();
        sorted.sort_by_key(|t| t.position);
        let active = self.tabs.iter().find(|t| t.active).map(|t| t.position);
        let empty = Vec::new();
        let pairs: Vec<(usize, Status)> = sorted
            .iter()
            .map(|t| {
                let panes = self.tab_panes.get(&t.position).unwrap_or(&empty);
                (t.position, self.tab_display(panes).status)
            })
            .collect();
        cycle_attention(&pairs, active, dir)
    }
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test -p zj-radar radar_state::tests::next_attention_tab`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/radar_state.rs
git commit -m "feat(radar): next_attention_tab over live tab rollup

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: `cmd` module — verb parsing

**Files:**
- Create: `src/cmd.rs`
- Modify: `src/lib.rs` (add `mod cmd;` near the other `mod` decls, ~line 36)
- Test: `src/cmd.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Produces:
  - `pub(crate) enum Command { AttentionNext, AttentionPrev }`
  - `pub(crate) fn parse(s: &str) -> Option<Command>`

- [ ] **Step 1: Create the module with a failing test**

Create `src/cmd.rs`:

```rust
//! The `zj_radar.cmd.v1` imperative command vocabulary.
//!
//! Verbs arrive as bare strings on the command pipe (typically from a Zellij
//! `MessagePlugin` keybind). Unknown verbs are `None` — the caller treats that
//! as a silent no-op, matching the radar's "parsing never fails" stance.

/// A parsed command verb.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    AttentionNext,
    AttentionPrev,
}

/// Parse a bare verb string. Trims surrounding whitespace; case-sensitive
/// lowercase verbs. Returns `None` for unknown/empty input.
pub(crate) fn parse(s: &str) -> Option<Command> {
    match s.trim() {
        "attention-next" => Some(Command::AttentionNext),
        "attention-prev" => Some(Command::AttentionPrev),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_verbs_trimmed() {
        assert_eq!(parse("attention-next"), Some(Command::AttentionNext));
        assert_eq!(parse("  attention-prev\n"), Some(Command::AttentionPrev));
    }

    #[test]
    fn rejects_unknown_and_empty() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("attention-top"), None);
        assert_eq!(parse("ATTENTION-NEXT"), None);
    }
}
```

- [ ] **Step 2: Register the module**

In `src/lib.rs`, add alongside the other `mod` declarations (e.g. after `mod command;` at line 36):

```rust
mod cmd;
```

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test -p zj-radar cmd::tests`
Expected: PASS (2 tests). (They are written and implemented together because the module can't compile without the impl; the regression value is the verb table.)

- [ ] **Step 4: Commit**

```bash
git add src/cmd.rs src/lib.rs
git commit -m "feat(cmd): zj_radar.cmd.v1 verb parsing

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: `PluginRuntime::command` + `command_pipe`

**Files:**
- Modify: `src/runtime.rs` (add two methods on `impl PluginRuntime`, near `mouse_click`; add `use crate::cmd::Command;` and bring `Direction` into scope from `radar_state`)
- Test: `src/runtime.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Consumes: `cmd::Command`, `cmd::parse` (Task 3); `RadarState::next_attention_tab` + `Direction` (Task 2); existing `Effect::SwitchTab`, `Outcome`, `self.permission_granted`.
- Produces:
  - `pub(crate) fn command(&self, cmd: Command) -> Outcome`
  - `pub(crate) fn command_pipe(&self, payload: &str) -> Outcome`

- [ ] **Step 1: Write the failing tests**

In `src/runtime.rs` `mod tests` (reuse helpers `tab(pos, name, active)`, `pane(id)`, `payload_for(id, Status)`, `config()` seen in existing tests):

```rust
#[test]
fn command_attention_next_emits_switch_tab() {
    let mut runtime = PluginRuntime {
        permission_granted: true,
        config: config(),
        ..Default::default()
    };
    // tab 0 active (running), tab 1 pending → attention.
    runtime.tabs_changed(vec![tab(0, "a", true), tab(1, "b", false)]);
    runtime.radar.set_tab_panes_for_position(0, vec![pane(10)]);
    runtime.radar.set_tab_panes_for_position(1, vec![pane(11)]);
    runtime.radar.status_mut().apply(payload_for(10, Status::Running), 1);
    runtime.radar.status_mut().apply(payload_for(11, Status::Pending), 1);

    let out = runtime.command(Command::AttentionNext);
    assert_eq!(out.effects, vec![Effect::SwitchTab { position: 1 }]);
}

#[test]
fn command_is_inert_without_permission() {
    let mut runtime = PluginRuntime { config: config(), ..Default::default() };
    runtime.tabs_changed(vec![tab(0, "a", true), tab(1, "b", false)]);
    runtime.radar.set_tab_panes_for_position(1, vec![pane(11)]);
    runtime.radar.status_mut().apply(payload_for(11, Status::Pending), 1);

    assert_eq!(runtime.command(Command::AttentionNext), Outcome::default());
}

#[test]
fn command_no_op_when_no_attention() {
    let mut runtime = PluginRuntime {
        permission_granted: true,
        config: config(),
        ..Default::default()
    };
    runtime.tabs_changed(vec![tab(0, "a", true)]);
    assert_eq!(runtime.command(Command::AttentionNext), Outcome::default());
}

#[test]
fn command_pipe_unknown_verb_is_no_op() {
    let runtime = PluginRuntime {
        permission_granted: true,
        config: config(),
        ..Default::default()
    };
    assert_eq!(runtime.command_pipe("attention-top"), Outcome::default());
    assert_eq!(runtime.command_pipe(""), Outcome::default());
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p zj-radar runtime::tests::command`
Expected: FAIL — `no method named command` / `command_pipe`.

- [ ] **Step 3: Implement the methods**

In `src/runtime.rs`, add imports at the top of the file with the other `use` lines:

```rust
use crate::cmd::Command;
use crate::radar_state::Direction;
```

Add to `impl PluginRuntime` (next to `mouse_click`):

```rust
    /// Run an imperative command verb. Read-only navigation today: resolves a
    /// deterministic target tab and emits `SwitchTab`. Inert until permission is
    /// granted, mirroring `mouse_click`.
    pub(crate) fn command(&self, cmd: Command) -> Outcome {
        if !self.permission_granted {
            return Outcome::none();
        }
        let dir = match cmd {
            Command::AttentionNext => Direction::Next,
            Command::AttentionPrev => Direction::Prev,
        };
        match self.radar.next_attention_tab(dir) {
            Some(position) => Outcome::with_effects(false, vec![Effect::SwitchTab { position }]),
            None => Outcome::none(),
        }
    }

    /// Parse a `cmd.v1` payload and dispatch it. Unknown verbs are a no-op.
    pub(crate) fn command_pipe(&self, payload: &str) -> Outcome {
        match crate::cmd::parse(payload) {
            Some(cmd) => self.command(cmd),
            None => Outcome::none(),
        }
    }
```

- [ ] **Step 4: Run them to verify they pass**

Run: `cargo test -p zj-radar runtime::tests::command`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add src/runtime.rs
git commit -m "feat(runtime): command + command_pipe dispatch

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Wire `cmd.v1` into `pipe()`

**Files:**
- Modify: `src/lib.rs` (add `CMD_PIPE` const near `CONFIG_PIPE` ~line 67; add the dispatch arm in `fn pipe` ~line 354-369)

**Interfaces:**
- Consumes: `PluginRuntime::command_pipe` (Task 4); existing `handle_outcome`.
- Produces: routes `zj_radar.cmd.v1` pipe messages. (Behavior verified by Task 4's host tests + the wasm build; `pipe()` itself is `#[cfg(target_arch = "wasm32")]`-gated.)

- [ ] **Step 1: Add the pipe-name constant**

In `src/lib.rs`, beneath the `CONFIG_PIPE` const (keep the same `#[cfg(target_arch = "wasm32")]` attribute):

```rust
#[cfg(target_arch = "wasm32")]
const CMD_PIPE: &str = "zj_radar.cmd.v1";
```

- [ ] **Step 2: Add the dispatch arm in `pipe()`**

In `src/lib.rs` `fn pipe`, extend the `if/else if` chain (after the `CONFIG_PIPE` branch, before the trailing `false`):

```rust
        } else if message.name == CMD_PIPE {
            if let Some(raw) = &message.payload {
                let outcome = self.runtime.command_pipe(raw);
                return self.handle_outcome(outcome);
            }
```

- [ ] **Step 3: Verify the host build + full host suite still pass**

Run: `cargo test`
Expected: PASS (all existing + new tests).

- [ ] **Step 4: Verify the wasm target compiles (covers the gated arm)**

Run: `cargo check --target wasm32-wasip1`
Expected: Finished with no errors. (If `wasm32-wasip1` is missing, the repo's Nix flake provides it — see `docs/TOOLCHAIN.md`.)

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs
git commit -m "feat(plugin): route zj_radar.cmd.v1 pipe to command dispatch

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: Documentation

**Files:**
- Modify: `README.md` (add a `cmd.v1` keybind subsection after the "Binding keys to runtime config" subsection)
- Modify: `docs/design.md` (one-line future-work note)

**Interfaces:** none (docs only).

- [ ] **Step 1: Add the `cmd.v1` README subsection**

In `README.md`, immediately after the "Binding keys to runtime config" subsection (before `## Writing your own producer`), add:

````markdown
### Binding keys to commands

`config.v1` only *sets* state. For *imperative* actions — like jumping to the
next agent that needs you — the plugin also accepts `zj_radar.cmd.v1`, whose
payload is a single bare verb string:

```kdl
keybinds {
    shared_except "locked" {
        // Cycle focus to the next tab needing attention (pending / error / done)
        bind "Alt n" {
            MessagePlugin "radar" { name "zj_radar.cmd.v1"; payload "attention-next"; }
        }
        bind "Alt p" {
            MessagePlugin "radar" { name "zj_radar.cmd.v1"; payload "attention-prev"; }
        }
    }
}
```

`attention-next` / `attention-prev` walk the tabs whose agents are *waiting for
you*, *errored*, or *done* — in tab order, wrapping around — and switch focus to
each. Tabs that are merely *running* or *idle* are skipped. Repeated presses
sweep every attention tab and cycle. Like every command pipe, an unknown verb is
ignored, and the action is inert until the sidebar has been granted permissions.
````

- [ ] **Step 2: Add the design.md note**

In `docs/design.md`, in the future-work / keybind discussion (near the `LaunchOrFocusPlugin` non-goal, ~line 423-431), add a line:

```markdown
- **Keybinds, the passive way** — the supported keyboard path is a Zellij
  `MessagePlugin` binding that delivers a verb to the `zj_radar.cmd.v1` pipe
  (e.g. `attention-next`), handled in `pipe()` exactly like `config.v1`. This
  keeps the plugin a passive renderer (no `Key` subscription, no focus grab),
  unlike a `LaunchOrFocusPlugin` panel.
```

- [ ] **Step 3: Commit**

```bash
git add README.md docs/design.md
git commit -m "docs: document zj_radar.cmd.v1 attention-nav keybinds

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

> Note: the README "Binding keys to runtime config" (`config.v1`) subsection was
> added during design and is already in the working tree — commit it with this
> task if it is still uncommitted (`git add README.md` covers it).

---

## Self-Review

**1. Spec coverage:**
- `cmd.v1` pipe name + bare-verb payload → Task 3 (parse), Task 5 (routing). ✅
- `attention-next` / `attention-prev` cycle-by-index, wrap → Task 1 (`cycle_attention`). ✅
- Attention set = pending/error/done; skip running/idle → Task 1 (`needs_attention`), proven in Task 2. ✅
- Deterministic/idempotent target, no lock → Task 1 (pure fn, documented). ✅
- Reuse `Effect::SwitchTab`, no new subscription/selectable → Task 4. ✅
- Permission gate (inert until granted) → Task 4 (`command_is_inert_without_permission`). ✅
- Unknown/empty payload no-op → Task 3 + Task 4 (`command_pipe_unknown_verb_is_no_op`). ✅
- Tests: proptest cycling invariant, unit edges, runtime emit/inert/no-op → Tasks 1, 2, 4. ✅
- README `cmd.v1` keybind docs + design.md note → Task 6. ✅
- Deferred verbs (`attention-top`, toggles, `ack-all`) → out of scope; `cmd::parse` table extends cleanly. ✅

**2. Placeholder scan:** No TBD/TODO/"handle edge cases"; every code step shows full code. One explicit verification note in Task 2 asks the implementer to confirm test-helper names against neighbors — that is a guardrail, not a placeholder (the struct-literal form given compiles regardless).

**3. Type consistency:** `Direction { Next, Prev }`, `cycle_attention(&[(usize, Status)], Option<usize>, Direction) -> Option<usize>`, `next_attention_tab(&self, Direction) -> Option<usize>`, `Command { AttentionNext, AttentionPrev }`, `parse(&str) -> Option<Command>`, `command(&self, Command) -> Outcome`, `command_pipe(&self, &str) -> Outcome`, `Effect::SwitchTab { position }` — names/signatures consistent across Tasks 1→5. ✅

---

## Execution Handoff

Plan complete. Two execution options:

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks.
2. **Inline Execution** — execute tasks in this session with checkpoints.
