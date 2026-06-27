# zj-agents Sidebar Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a native Zellij left-sidebar plugin (Rust→WASM) that lists every tab and shows per-tab AI-agent status (working/waiting/done/error) with color, repo/branch, elapsed time, and last message, fed by per-agent hooks via `zellij pipe`; click a row to switch tabs.

**Architecture:** A thin `zellij-tile` host-glue layer (`lib.rs`) translates Zellij events into calls on five **pure, `zellij-tile`-free** logic modules (`status`, `payload`, `state`, `model`, `render`) that are fully unit-testable on the host. Agent state arrives as a broadcast `zellij pipe` payload produced by tiny per-agent adapter scripts (Claude exists; Codex new). See `docs/design.md`.

**Tech Stack:** Rust (edition 2021), `zellij-tile = "0.44"`, `serde`/`serde_json`, target `wasm32-wasip1`, Zellij 0.44.3, Nix home-manager packaging.

## Global Constraints

- `zellij-tile = "0.44"` pinned (matches Zellij 0.44.3). Read `PaneId`/`TabInfo`/`PaneInfo`/`Mouse` against the 0.44.x API.
- Build target is **`wasm32-wasip1`** (Rust 1.96 renamed/removed `wasm32-wasi`; artifact is WASI-preview1, loaded identically by Zellij).
- **Pure modules (`status`, `payload`, `state`, `model`, `render`) must NOT import `zellij-tile`.** Only `lib.rs` imports it and converts Zellij types to/from plain data. This keeps `cargo test` host-only.
- Pipe broadcast name is exactly `zj_agents.status.v1`.
- Plugin permissions: `ReadApplicationState`, `ReadCliPipes`, `ChangeApplicationState`. **No `RunCommands`** (notifications stay in shell adapters).
- Tab numbering: display number = `TabInfo.position + 1`; `switch_tab_to` is **1-indexed** → call `switch_tab_to(position + 1)`.
- Aggregation severity (highest wins): `error > pending > running > done > idle`.
- `MAX_PAYLOAD_BYTES = 65536`, `MAX_MSG_CHARS = 60`. Sanitize repo/branch/msg (strip control/ANSI, newline→space, truncate).
- DRY, YAGNI, TDD, frequent commits.

## File Structure

```
zj-agents/
├── Cargo.toml                      # crate-type = ["cdylib"]; deps
├── rust-toolchain.toml             # targets = ["wasm32-wasip1"]
├── .gitignore                      # /target
├── src/
│   ├── lib.rs       # ONLY file importing zellij-tile; host glue (load/update/render/pipe)
│   ├── status.rs    # Status enum: parse, severity, glyph, ansi color, label  (pure)
│   ├── payload.rs   # StatusPayload + serde parse + sanitize + size cap        (pure)
│   ├── state.rs     # AgentState + StateStore: apply/on_pane_focused/prune     (pure)
│   ├── model.rs     # TabAgg + aggregate(pane_ids, store)                      (pure)
│   └── render.rs    # TabRow + format_elapsed + render(rows, width, tick)      (pure)
├── dev/
│   └── dev.kdl                     # hot-reload dev layout
└── docs/{design.md, plan.md}
```

Producer-side (in the `dotfiles` repo, `home-manager/modules/zellij/`):
```
claude-zellij-notify.sh   # MODIFY: also broadcast zj_agents.status.v1
codex-zellij-notify.sh    # CREATE: Codex notify adapter (done-only)
default.nix               # MODIFY: vendor zj-agents.wasm + install codex adapter; drop @smartTabs@ fetchurl
zellij.kdl                # MODIFY: sidebar in default_tab_template; drop compact-bar (smart-tabs already removed)
```

---

### Task 0: Project scaffold

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `.gitignore`, `src/lib.rs`

**Interfaces:**
- Produces: a buildable crate; later tasks add modules under `src/`.

- [ ] **Step 1: Init repo and toolchain**

```bash
cd ~/dev/zj-agents
git init
rustup target add wasm32-wasip1
```

- [ ] **Step 2: Write `rust-toolchain.toml`**

```toml
[toolchain]
targets = ["wasm32-wasip1"]
```

- [ ] **Step 3: Write `.gitignore`**

```
/target
```

- [ ] **Step 4: Write `Cargo.toml`**

```toml
[package]
name = "zj-agents"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
zellij-tile = "0.44"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

- [ ] **Step 5: Write a minimal `src/lib.rs` (compiles to wasm, renders a header)**

```rust
use zellij_tile::prelude::*;
use std::collections::BTreeMap;

#[derive(Default)]
struct State;

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, _config: BTreeMap<String, String>) {}
    fn update(&mut self, _event: Event) -> bool {
        false
    }
    fn render(&mut self, _rows: usize, _cols: usize) {
        print!("agents");
    }
}
```

- [ ] **Step 6: Build to wasm**

Run: `cargo build --target wasm32-wasip1`
Expected: compiles; produces `target/wasm32-wasip1/debug/zj_agents.wasm`

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "chore: scaffold zj-agents wasm plugin"
```

---

### Task 1: `status` module

**Files:**
- Create: `src/status.rs`
- Modify: `src/lib.rs` (add `mod status;`)

**Interfaces:**
- Produces: `pub enum Status { Idle, Done, Running, Pending, Error }` with
  `from_wire(&str) -> Status`, `severity(self) -> u8`, `glyph(self) -> char`,
  `ansi(self) -> &'static str`, `is_active(self) -> bool`.

- [ ] **Step 1: Write the failing test (`src/status.rs`)**

```rust
//! Pure agent-status vocabulary. No zellij-tile dependency.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Idle,
    Done,
    Running,
    Pending,
    Error,
}

impl Status {
    /// Parse a wire value; anything unknown/absent is Idle.
    pub fn from_wire(s: &str) -> Status {
        match s {
            "running" => Status::Running,
            "pending" => Status::Pending,
            "done" => Status::Done,
            "error" => Status::Error,
            _ => Status::Idle,
        }
    }

    /// Higher = more urgent. Used for per-tab aggregation.
    pub fn severity(self) -> u8 {
        match self {
            Status::Error => 4,
            Status::Pending => 3,
            Status::Running => 2,
            Status::Done => 1,
            Status::Idle => 0,
        }
    }

    pub fn glyph(self) -> char {
        match self {
            Status::Error => '✗',
            Status::Pending => '◑',
            Status::Running => '◐',
            Status::Done => '●',
            Status::Idle => '○',
        }
    }

    /// ANSI SGR foreground color for the glyph.
    pub fn ansi(self) -> &'static str {
        match self {
            Status::Error => "\x1b[31m",   // red
            Status::Pending => "\x1b[33m", // yellow/orange
            Status::Running => "\x1b[93m", // bright yellow
            Status::Done => "\x1b[32m",    // green
            Status::Idle => "\x1b[90m",    // dim grey
        }
    }

    pub fn is_active(self) -> bool {
        self != Status::Idle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_and_unknown() {
        assert_eq!(Status::from_wire("running"), Status::Running);
        assert_eq!(Status::from_wire("done"), Status::Done);
        assert_eq!(Status::from_wire("nonsense"), Status::Idle);
        assert_eq!(Status::from_wire(""), Status::Idle);
    }

    #[test]
    fn severity_orders_error_highest_idle_lowest() {
        assert!(Status::Error.severity() > Status::Pending.severity());
        assert!(Status::Pending.severity() > Status::Running.severity());
        assert!(Status::Running.severity() > Status::Done.severity());
        assert!(Status::Done.severity() > Status::Idle.severity());
    }

    #[test]
    fn is_active_excludes_idle_only() {
        assert!(!Status::Idle.is_active());
        assert!(Status::Done.is_active());
        assert!(Status::Running.is_active());
    }
}
```

- [ ] **Step 2: Register the module in `src/lib.rs`**

Add near the top of `src/lib.rs` (after the `use` lines):

```rust
mod status;
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test status`
Expected: 3 tests pass.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat: status vocabulary (parse/severity/glyph/color)"
```

---

### Task 2: `payload` module (parse + sanitize)

**Files:**
- Create: `src/payload.rs`
- Modify: `src/lib.rs` (add `mod payload;`)

**Interfaces:**
- Consumes: `status::Status`.
- Produces:
  - `pub const MAX_PAYLOAD_BYTES: usize = 65536;`
  - `pub const MAX_MSG_CHARS: usize = 60;`
  - `pub struct StatusPayload { pub pane_id: u32, pub status: Status, pub repo: String, pub branch: String, pub msg: String, pub on_focus: Option<Status>, pub seq: Option<u64>, pub source: String }`
  - `pub fn parse(raw: &str) -> Option<StatusPayload>`
  - `pub fn sanitize(s: &str, max_chars: usize) -> String`

- [ ] **Step 1: Write the failing test (`src/payload.rs`)**

```rust
//! Parse + sanitize the zj_agents.status.v1 pipe payload. No zellij-tile dependency.

use crate::status::Status;
use serde::Deserialize;

pub const MAX_PAYLOAD_BYTES: usize = 65536;
pub const MAX_MSG_CHARS: usize = 60;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusPayload {
    pub pane_id: u32,
    pub status: Status,
    pub repo: String,
    pub branch: String,
    pub msg: String,
    pub on_focus: Option<Status>,
    pub seq: Option<u64>,
    pub source: String,
}

#[derive(Deserialize)]
struct RawPane {
    #[serde(rename = "type")]
    kind: String,
    id: u32,
}

#[derive(Deserialize)]
struct Raw {
    pane: RawPane,
    status: String,
    #[serde(default)]
    repo: String,
    #[serde(default)]
    branch: String,
    #[serde(default)]
    msg: String,
    #[serde(default)]
    on_focus: Option<String>,
    #[serde(default)]
    seq: Option<u64>,
    #[serde(default)]
    source: String,
}

/// Strip control/ANSI chars, fold newlines to spaces, truncate to `max_chars`.
pub fn sanitize(s: &str, max_chars: usize) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
        .filter(|c| !c.is_control() && *c != '\u{1b}')
        .collect();
    cleaned.chars().take(max_chars).collect()
}

/// Parse a broadcast payload. Returns None on oversize, invalid JSON, or a
/// non-terminal pane. Unknown status maps to Idle (never errors).
pub fn parse(raw: &str) -> Option<StatusPayload> {
    if raw.len() > MAX_PAYLOAD_BYTES {
        return None;
    }
    let r: Raw = serde_json::from_str(raw).ok()?;
    if r.pane.kind != "terminal" {
        return None;
    }
    Some(StatusPayload {
        pane_id: r.pane.id,
        status: Status::from_wire(&r.status),
        repo: sanitize(&r.repo, 40),
        branch: sanitize(&r.branch, 40),
        msg: sanitize(&r.msg, MAX_MSG_CHARS),
        on_focus: r.on_focus.as_deref().map(Status::from_wire),
        seq: r.seq,
        source: sanitize(&r.source, 16),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Option<StatusPayload> {
        parse(s)
    }

    #[test]
    fn parses_full_payload() {
        let got = p(r#"{"v":1,"source":"claude","pane":{"type":"terminal","id":12},"status":"running","repo":"pinky","branch":"fix/x","msg":"running tests","on_focus":"idle","seq":42}"#).unwrap();
        assert_eq!(got.pane_id, 12);
        assert_eq!(got.status, Status::Running);
        assert_eq!(got.repo, "pinky");
        assert_eq!(got.on_focus, Some(Status::Idle));
        assert_eq!(got.seq, Some(42));
    }

    #[test]
    fn missing_optionals_default() {
        let got = p(r#"{"pane":{"type":"terminal","id":3},"status":"done"}"#).unwrap();
        assert_eq!(got.pane_id, 3);
        assert_eq!(got.status, Status::Done);
        assert_eq!(got.repo, "");
        assert_eq!(got.on_focus, None);
        assert_eq!(got.seq, None);
    }

    #[test]
    fn rejects_non_terminal_and_garbage_and_oversize() {
        assert!(p(r#"{"pane":{"type":"plugin","id":1},"status":"done"}"#).is_none());
        assert!(p("not json").is_none());
        let big = format!(r#"{{"pane":{{"type":"terminal","id":1}},"status":"done","msg":"{}"}}"#, "x".repeat(MAX_PAYLOAD_BYTES));
        assert!(p(&big).is_none());
    }

    #[test]
    fn unknown_status_is_idle() {
        let got = p(r#"{"pane":{"type":"terminal","id":1},"status":"whatever"}"#).unwrap();
        assert_eq!(got.status, Status::Idle);
    }

    #[test]
    fn sanitize_strips_control_newlines_ansi_and_truncates() {
        let dirty = "a\nb\t\x1b[31mc\x07";
        assert_eq!(sanitize(dirty, 10), "a b c");
        assert_eq!(sanitize("abcdef", 3), "abc");
    }
}
```

- [ ] **Step 2: Register the module in `src/lib.rs`**

```rust
mod payload;
```

- [ ] **Step 3: Run tests**

Run: `cargo test payload`
Expected: 5 tests pass.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat: pipe payload parse + sanitize with size cap"
```

---

### Task 3: `state` module (StateStore)

**Files:**
- Create: `src/state.rs`
- Modify: `src/lib.rs` (add `mod state;`)

**Interfaces:**
- Consumes: `status::Status`, `payload::StatusPayload`.
- Produces:
  - `pub struct AgentState { pub status: Status, pub repo: String, pub branch: String, pub msg: String, pub last_change_tick: u64, pub seq: Option<u64>, pub on_focus: Option<Status>, pub ever_active: bool }`
  - `pub struct StateStore` with `new()`, `apply(&mut self, p: StatusPayload, tick: u64)`, `on_pane_focused(&mut self, pane_id: u32, tick: u64)`, `prune(&mut self, live: &std::collections::HashSet<u32>)`, `get(&self, pane_id: u32) -> Option<&AgentState>`, `any_active(&self) -> bool`.

- [ ] **Step 1: Write the failing test (`src/state.rs`)**

```rust
//! Per-pane agent state, keyed by terminal pane id. No zellij-tile dependency.

use crate::payload::StatusPayload;
use crate::status::Status;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
pub struct AgentState {
    pub status: Status,
    pub repo: String,
    pub branch: String,
    pub msg: String,
    pub last_change_tick: u64,
    pub seq: Option<u64>,
    pub on_focus: Option<Status>,
    pub ever_active: bool,
}

#[derive(Default)]
pub struct StateStore {
    map: HashMap<u32, AgentState>,
}

impl StateStore {
    pub fn new() -> Self {
        StateStore::default()
    }

    /// Apply an incoming payload. Drops out-of-order updates (seq <= stored seq).
    pub fn apply(&mut self, p: StatusPayload, tick: u64) {
        if let (Some(existing), Some(incoming)) = (self.map.get(&p.pane_id).and_then(|s| s.seq), p.seq) {
            if incoming <= existing {
                return;
            }
        }
        let prev_status = self.map.get(&p.pane_id).map(|s| s.status);
        let status_changed = prev_status != Some(p.status);
        let last_change_tick = if status_changed {
            tick
        } else {
            self.map.get(&p.pane_id).map(|s| s.last_change_tick).unwrap_or(tick)
        };
        let ever_active = p.status.is_active()
            || self.map.get(&p.pane_id).map(|s| s.ever_active).unwrap_or(false);
        self.map.insert(
            p.pane_id,
            AgentState {
                status: p.status,
                repo: p.repo,
                branch: p.branch,
                msg: p.msg,
                last_change_tick,
                seq: p.seq,
                on_focus: p.on_focus,
                ever_active,
            },
        );
    }

    /// One-shot: when the exact pane is focused, apply its pending on_focus status.
    pub fn on_pane_focused(&mut self, pane_id: u32, tick: u64) {
        if let Some(s) = self.map.get_mut(&pane_id) {
            if let Some(next) = s.on_focus.take() {
                if s.status != next {
                    s.last_change_tick = tick;
                }
                s.status = next;
            }
        }
    }

    pub fn prune(&mut self, live: &HashSet<u32>) {
        self.map.retain(|id, _| live.contains(id));
    }

    pub fn get(&self, pane_id: u32) -> Option<&AgentState> {
        self.map.get(&pane_id)
    }

    pub fn any_active(&self) -> bool {
        self.map.values().any(|s| s.status.is_active())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(pane_id: u32, status: Status, seq: Option<u64>) -> StatusPayload {
        StatusPayload {
            pane_id,
            status,
            repo: "r".into(),
            branch: "b".into(),
            msg: "m".into(),
            on_focus: None,
            seq,
            source: "test".into(),
        }
    }

    #[test]
    fn apply_sets_last_change_tick_only_on_status_change() {
        let mut s = StateStore::new();
        s.apply(payload(1, Status::Running, None), 5);
        assert_eq!(s.get(1).unwrap().last_change_tick, 5);
        s.apply(payload(1, Status::Running, None), 9); // same status
        assert_eq!(s.get(1).unwrap().last_change_tick, 5);
        s.apply(payload(1, Status::Done, None), 12); // changed
        assert_eq!(s.get(1).unwrap().last_change_tick, 12);
    }

    #[test]
    fn out_of_order_seq_is_dropped() {
        let mut s = StateStore::new();
        s.apply(payload(1, Status::Running, Some(10)), 1);
        s.apply(payload(1, Status::Done, Some(5)), 2); // stale
        assert_eq!(s.get(1).unwrap().status, Status::Running);
        s.apply(payload(1, Status::Done, Some(11)), 3); // newer
        assert_eq!(s.get(1).unwrap().status, Status::Done);
    }

    #[test]
    fn on_focus_applies_once_then_clears() {
        let mut s = StateStore::new();
        let mut p = payload(1, Status::Done, None);
        p.on_focus = Some(Status::Idle);
        s.apply(p, 1);
        s.on_pane_focused(1, 7);
        assert_eq!(s.get(1).unwrap().status, Status::Idle);
        assert_eq!(s.get(1).unwrap().on_focus, None);
        // focusing again does nothing
        s.on_pane_focused(1, 9);
        assert_eq!(s.get(1).unwrap().status, Status::Idle);
    }

    #[test]
    fn prune_removes_dead_panes() {
        let mut s = StateStore::new();
        s.apply(payload(1, Status::Running, None), 1);
        s.apply(payload(2, Status::Done, None), 1);
        let live: HashSet<u32> = [2].into_iter().collect();
        s.prune(&live);
        assert!(s.get(1).is_none());
        assert!(s.get(2).is_some());
    }

    #[test]
    fn ever_active_sticks_after_returning_to_idle() {
        let mut s = StateStore::new();
        s.apply(payload(1, Status::Running, None), 1);
        s.apply(payload(1, Status::Idle, None), 2);
        assert!(s.get(1).unwrap().ever_active);
        assert!(!s.any_active());
    }
}
```

- [ ] **Step 2: Register the module in `src/lib.rs`**

```rust
mod state;
```

- [ ] **Step 3: Run tests**

Run: `cargo test state`
Expected: 5 tests pass.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat: per-pane StateStore with seq guard, on_focus, prune"
```

---

### Task 4: `model` module (per-tab aggregation)

**Files:**
- Create: `src/model.rs`
- Modify: `src/lib.rs` (add `mod model;`)

**Interfaces:**
- Consumes: `status::Status`, `state::StateStore`.
- Produces:
  - `pub struct Detail { pub repo: String, pub branch: String, pub msg: String, pub since_tick: u64, pub status: Status }`
  - `pub struct TabAgg { pub status: Status, pub done: usize, pub total: usize, pub detail: Option<Detail> }`
  - `pub fn aggregate(pane_ids: &[u32], store: &StateStore) -> TabAgg`

- [ ] **Step 1: Write the failing test (`src/model.rs`)**

```rust
//! Aggregate per-pane state into per-tab state. No zellij-tile dependency.

use crate::state::StateStore;
use crate::status::Status;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detail {
    pub repo: String,
    pub branch: String,
    pub msg: String,
    pub since_tick: u64,
    pub status: Status,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabAgg {
    pub status: Status,
    pub done: usize,
    pub total: usize,
    pub detail: Option<Detail>,
}

/// Highest-severity pane wins (tie → most recent last_change_tick). `total`
/// counts panes that have ever been active and still exist; `done` counts
/// those currently done.
pub fn aggregate(pane_ids: &[u32], store: &StateStore) -> TabAgg {
    let mut best_status = Status::Idle;
    let mut best: Option<Detail> = None;
    let mut done = 0usize;
    let mut total = 0usize;

    for &id in pane_ids {
        let Some(s) = store.get(id) else { continue };
        if s.ever_active {
            total += 1;
            if s.status == Status::Done {
                done += 1;
            }
        }
        let better = s.status.severity() > best_status.severity()
            || (s.status.severity() == best_status.severity()
                && best.as_ref().map_or(true, |d| s.last_change_tick >= d.since_tick));
        if s.status.is_active() && better {
            best_status = s.status;
            best = Some(Detail {
                repo: s.repo.clone(),
                branch: s.branch.clone(),
                msg: s.msg.clone(),
                since_tick: s.last_change_tick,
                status: s.status,
            });
        }
    }

    TabAgg {
        status: best_status,
        done,
        total,
        detail: best,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::StatusPayload;

    fn put(store: &mut StateStore, id: u32, status: Status, tick: u64, repo: &str) {
        store.apply(
            StatusPayload {
                pane_id: id,
                status,
                repo: repo.into(),
                branch: "b".into(),
                msg: "m".into(),
                on_focus: None,
                seq: None,
                source: "test".into(),
            },
            tick,
        );
    }

    #[test]
    fn empty_tab_is_idle() {
        let store = StateStore::new();
        let agg = aggregate(&[1, 2], &store);
        assert_eq!(agg.status, Status::Idle);
        assert_eq!(agg.total, 0);
        assert!(agg.detail.is_none());
    }

    #[test]
    fn highest_severity_wins_for_status_and_detail() {
        let mut store = StateStore::new();
        put(&mut store, 1, Status::Done, 1, "done-repo");
        put(&mut store, 2, Status::Pending, 2, "pending-repo");
        put(&mut store, 3, Status::Running, 3, "running-repo");
        let agg = aggregate(&[1, 2, 3], &store);
        assert_eq!(agg.status, Status::Pending); // error>pending>running>done
        assert_eq!(agg.detail.unwrap().repo, "pending-repo");
    }

    #[test]
    fn counts_done_over_total_ever_active() {
        let mut store = StateStore::new();
        put(&mut store, 1, Status::Done, 1, "a");
        put(&mut store, 2, Status::Done, 1, "b");
        put(&mut store, 3, Status::Running, 1, "c");
        let agg = aggregate(&[1, 2, 3], &store);
        assert_eq!(agg.done, 2);
        assert_eq!(agg.total, 3);
    }

    #[test]
    fn severity_tie_breaks_on_most_recent_change() {
        let mut store = StateStore::new();
        put(&mut store, 1, Status::Running, 5, "older");
        put(&mut store, 2, Status::Running, 9, "newer");
        let agg = aggregate(&[1, 2], &store);
        assert_eq!(agg.detail.unwrap().repo, "newer");
    }
}
```

- [ ] **Step 2: Register the module in `src/lib.rs`**

```rust
mod model;
```

- [ ] **Step 3: Run tests**

Run: `cargo test model`
Expected: 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat: per-tab aggregation (severity, counts, tie-break)"
```

---

### Task 5: `render` module (pure ANSI renderer)

**Files:**
- Create: `src/render.rs`
- Modify: `src/lib.rs` (add `mod render;`)

**Interfaces:**
- Consumes: `status::Status`, `model::TabAgg`.
- Produces:
  - `pub struct TabRow { pub number: u32, pub name: String, pub active: bool, pub agg: model::TabAgg }`
  - `pub fn format_elapsed(secs: u64) -> String`
  - `pub fn render(rows: &[TabRow], width: usize, now_tick: u64) -> String`

- [ ] **Step 1: Write the failing test (`src/render.rs`)**

```rust
//! Pure renderer: per-tab rows → ANSI string. No zellij-tile dependency.

use crate::model::TabAgg;
use crate::status::Status;

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";

pub struct TabRow {
    pub number: u32,
    pub name: String,
    pub active: bool,
    pub agg: TabAgg,
}

/// "0:14" under a minute-ish, "2m", "1h3m".
pub fn format_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("0:{:02}", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else if max == 0 {
        String::new()
    } else {
        let kept: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", kept)
    }
}

/// Second-line tag for a tab's aggregate state.
fn detail_tag(agg: &TabAgg, now_tick: u64) -> String {
    let Some(d) = &agg.detail else { return String::new() };
    let elapsed = now_tick.saturating_sub(d.since_tick);
    match d.status {
        Status::Done => format!("done {}", format_elapsed(elapsed)),
        Status::Running => format_elapsed(elapsed),
        Status::Pending => "needs you".to_string(),
        Status::Error => "error".to_string(),
        Status::Idle => String::new(),
    }
}

pub fn render(rows: &[TabRow], width: usize, now_tick: u64) -> String {
    let mut out = String::new();
    for row in rows {
        let dot = format!("{}{}{}", row.agg.status.ansi(), row.agg.status.glyph(), RESET);
        let count = if row.agg.total > 1 {
            format!(" {}/{}", row.agg.done, row.agg.total)
        } else {
            String::new()
        };
        let name_budget = width.saturating_sub(4 + count.chars().count());
        let name = truncate(&row.name, name_budget);
        let name_styled = if row.active {
            format!("{}{}{}", BOLD, name, RESET)
        } else {
            name
        };
        // line 1: "<dot> <n> <name><count>"
        out.push_str(&format!("{} {} {}{}\n", dot, row.number, name_styled, count));

        // line 2: "  repo/branch · tag"  (only when there is agent detail)
        if let Some(d) = &row.agg.detail {
            let loc = if d.branch.is_empty() {
                d.repo.clone()
            } else {
                format!("{}/{}", d.repo, d.branch)
            };
            let tag = detail_tag(&row.agg, now_tick);
            let second = truncate(&format!("{} · {}", loc, tag), width.saturating_sub(2));
            out.push_str(&format!("  {}\n", second));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Detail;

    fn agg(status: Status, done: usize, total: usize, detail: Option<Detail>) -> TabAgg {
        TabAgg { status, done, total, detail }
    }

    #[test]
    fn format_elapsed_buckets() {
        assert_eq!(format_elapsed(14), "0:14");
        assert_eq!(format_elapsed(120), "2m");
        assert_eq!(format_elapsed(3780), "1h3m");
    }

    #[test]
    fn plain_tab_renders_name_only_no_second_line() {
        let rows = vec![TabRow {
            number: 4,
            name: "notes".into(),
            active: false,
            agg: agg(Status::Idle, 0, 0, None),
        }];
        let s = render(&rows, 24, 0);
        assert!(s.contains("notes"));
        assert_eq!(s.matches('\n').count(), 1); // single line
        assert!(s.contains(Status::Idle.glyph()));
    }

    #[test]
    fn agent_tab_has_two_lines_with_count_and_tag() {
        let detail = Detail {
            repo: "pinky".into(),
            branch: "fix/x".into(),
            msg: "m".into(),
            since_tick: 0,
            status: Status::Running,
        };
        let rows = vec![TabRow {
            number: 2,
            name: "pinky".into(),
            active: true,
            agg: agg(Status::Running, 2, 4, Some(detail)),
        }];
        let s = render(&rows, 24, 14);
        assert!(s.contains("2/4"));
        assert!(s.contains("pinky/fix/x"));
        assert!(s.contains("0:14"));
        assert_eq!(s.matches('\n').count(), 2); // two lines
    }

    #[test]
    fn narrow_width_truncates_with_ellipsis() {
        let rows = vec![TabRow {
            number: 1,
            name: "a-very-long-tab-name-indeed".into(),
            active: false,
            agg: agg(Status::Idle, 0, 0, None),
        }];
        let s = render(&rows, 12, 0);
        assert!(s.contains('…'));
    }
}
```

- [ ] **Step 2: Register the module in `src/lib.rs`**

```rust
mod render;
```

- [ ] **Step 3: Run all tests**

Run: `cargo test`
Expected: all pure-module tests pass (status/payload/state/model/render).

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat: pure ANSI sidebar renderer + elapsed formatting"
```

---

### Task 6: Host glue (`lib.rs`) — wire the pure core into Zellij

**Files:**
- Modify: `src/lib.rs` (replace the placeholder `State` with the real one)

**Interfaces:**
- Consumes: `status`, `payload`, `state::StateStore`, `model::{aggregate}`, `render::{render, TabRow}`.
- Produces: a working `.wasm` that subscribes to events, maintains state, renders the sidebar, and switches tabs on click.

- [ ] **Step 1: Replace `src/lib.rs` body (keep the `mod` lines from Tasks 1–5)**

```rust
use zellij_tile::prelude::*;
use std::collections::{BTreeMap, HashMap, HashSet};

mod status;
mod payload;
mod state;
mod model;
mod render;

use render::TabRow;
use state::StateStore;

const PIPE_NAME: &str = "zj_agents.status.v1";

#[derive(Clone)]
struct TabLite {
    position: usize,
    name: String,
    active: bool,
}

#[derive(Default)]
struct State {
    store: StateStore,
    tabs: Vec<TabLite>,
    tab_panes: HashMap<usize, Vec<u32>>, // tab position -> terminal pane ids
    tick: u64,
    timer_armed: bool,
}

register_plugin!(State);

impl State {
    fn arm_timer_if_needed(&mut self) {
        if !self.timer_armed && self.store.any_active() {
            set_timeout(1.0);
            self.timer_armed = true;
        }
    }
}

impl ZellijPlugin for State {
    fn load(&mut self, _config: BTreeMap<String, String>) {
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ReadCliPipes,
            PermissionType::ChangeApplicationState,
        ]);
        subscribe(&[
            EventType::TabUpdate,
            EventType::PaneUpdate,
            EventType::Timer,
            EventType::Mouse,
            EventType::PermissionRequestResult,
        ]);
        set_selectable(false);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::TabUpdate(tabs) => {
                self.tabs = tabs
                    .into_iter()
                    .map(|t| TabLite {
                        position: t.position,
                        name: t.name,
                        active: t.active,
                    })
                    .collect();
                true
            }
            Event::PaneUpdate(manifest) => {
                let mut tab_panes: HashMap<usize, Vec<u32>> = HashMap::new();
                let mut live: HashSet<u32> = HashSet::new();
                let mut focused_terminal: Option<u32> = None;
                for (tab_pos, panes) in manifest.panes {
                    for p in panes {
                        if p.is_plugin {
                            continue;
                        }
                        tab_panes.entry(tab_pos).or_default().push(p.id);
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
            Event::Timer(_) => {
                self.timer_armed = false;
                self.tick += 1;
                self.arm_timer_if_needed();
                self.store.any_active()
            }
            Event::Mouse(Mouse::LeftClick(line, _col)) => {
                // Two display lines per agent tab, one per plain tab — but the
                // simplest robust mapping is to recompute row line-spans the same
                // way render() does. For v1, map by counting rendered lines.
                if let Some(pos) = self.tab_position_at_line(line) {
                    switch_tab_to(pos as u32 + 1);
                }
                false
            }
            Event::PermissionRequestResult(_) => true,
            _ => false,
        }
    }

    fn pipe(&mut self, message: PipeMessage) -> bool {
        if message.name == PIPE_NAME {
            if let Some(raw) = &message.payload {
                if let Some(p) = payload::parse(raw) {
                    self.store.apply(p, self.tick);
                    self.arm_timer_if_needed();
                    return true;
                }
            }
        }
        false
    }

    fn render(&mut self, _rows: usize, cols: usize) {
        let rows = self.build_rows();
        print!("{}", render::render(&rows, cols.max(1), self.tick));
    }
}

impl State {
    fn build_rows(&self) -> Vec<TabRow> {
        let mut rows = Vec::new();
        let mut sorted = self.tabs.clone();
        sorted.sort_by_key(|t| t.position);
        for t in &sorted {
            let empty = Vec::new();
            let panes = self.tab_panes.get(&t.position).unwrap_or(&empty);
            rows.push(TabRow {
                number: t.position as u32 + 1,
                name: t.name.clone(),
                active: t.active,
                agg: model::aggregate(panes, &self.store),
            });
        }
        rows
    }

    /// Map a clicked line back to a tab position by replaying render()'s line
    /// counting (1 line for plain tabs, 2 for tabs with agent detail).
    fn tab_position_at_line(&self, line: isize) -> Option<usize> {
        if line < 0 {
            return None;
        }
        let target = line as usize;
        let mut cursor = 0usize;
        let mut sorted = self.tabs.clone();
        sorted.sort_by_key(|t| t.position);
        for t in &sorted {
            let empty = Vec::new();
            let panes = self.tab_panes.get(&t.position).unwrap_or(&empty);
            let agg = model::aggregate(panes, &self.store);
            let span = if agg.detail.is_some() { 2 } else { 1 };
            if target >= cursor && target < cursor + span {
                return Some(t.position);
            }
            cursor += span;
        }
        None
    }
}
```

- [ ] **Step 2: Verify host build**

Run: `cargo build --target wasm32-wasip1`
Expected: compiles to `target/wasm32-wasip1/debug/zj_agents.wasm`.

**Note (verify against zellij-tile 0.44 during this step):** confirm `Mouse::LeftClick(isize, usize)` is `(line, column)`; confirm `switch_tab_to(u32)` is 1-based; confirm `PaneManifest { panes: HashMap<usize, Vec<PaneInfo>> }` and `PaneInfo { id, is_plugin, is_focused }`. Adjust field/variant names if the API differs; the logic is unaffected.

- [ ] **Step 3: Run unit tests (still pass; glue didn't touch pure modules)**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat: zellij host glue wiring events to the pure core"
```

---

### Task 7: Dev layout + manual Phase-1/2 acceptance

**Files:**
- Create: `dev/dev.kdl`

**Interfaces:**
- Consumes: the built `zj_agents.wasm`.
- Produces: a runnable Zellij session proving the sidebar pins, numbers tabs, renders state from a fake pipe, and switches tabs on click.

- [ ] **Step 1: Write `dev/dev.kdl` (absolute path to the debug wasm)**

```kdl
layout {
    default_tab_template {
        pane split_direction="vertical" {
            pane size=24 borderless=true {
                plugin location="file:/Users/mark.toda/dev/zj-agents/target/wasm32-wasip1/debug/zj_agents.wasm"
            }
            children
        }
        pane size=2 borderless=true {
            plugin location="zellij:status-bar"
        }
    }
    tab name="one" focus=true { pane }
    tab name="two" { pane split_direction="vertical" { pane; pane } }
    tab name="three" { pane }
}
```

- [ ] **Step 2: Launch a dev session**

Run: `cargo build --target wasm32-wasip1 && zellij --layout dev/dev.kdl`
Expected: a left sidebar lists `1 one`, `2 two`, `3 three`; grant the permission prompt.

- [ ] **Step 3: Acceptance checks (Phase 1) — record results in the commit message**

- Sidebar stays pinned when cycling swap layouts (Alt+] / Alt+[).
- Tab numbers read 1,2,3 (not 0,1,2).
- **Click a tab row → focus switches to that tab.** If clicks do NOT arrive on the
  non-selectable pane, change `load()` to omit `set_selectable(false)` and instead, on
  `Event::Mouse`, call `switch_tab_to(...)` then re-focus the previous pane; rebuild and retest.
- Width 24 is tolerable in your real layouts.

- [ ] **Step 4: Acceptance check (Phase 2) — fake agent via pipe**

From a pane inside the dev session, with `$ZELLIJ_PANE_ID` pointing at a real terminal pane:

```sh
zellij pipe --name zj_agents.status.v1 -- \
  "{\"v\":1,\"source\":\"test\",\"pane\":{\"type\":\"terminal\",\"id\":${ZELLIJ_PANE_ID#terminal_}},\"status\":\"running\",\"repo\":\"demo\",\"branch\":\"main\",\"msg\":\"hello\"}"
```
Expected: that pane's tab shows `◐` + `demo/main · 0:NN` and the elapsed counter increments each second. Send `"status":"done"` and confirm green `●` + `done 0:NN`.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "chore: dev layout + record Phase 1/2 acceptance results"
```

---

### Task 8: Producer adapters (Claude payload + Codex) — in the `dotfiles` repo

**Files:**
- Modify: `~/dotfiles/home-manager/modules/zellij/claude-zellij-notify.sh`
- Create: `~/dotfiles/home-manager/modules/zellij/codex-zellij-notify.sh`
- Modify: `~/dotfiles/home-manager/modules/zellij/default.nix`
- Modify: `~/dotfiles/home-manager/modules/zellij/zellij.kdl`

**Interfaces:**
- Consumes: the plugin's `zj_agents.status.v1` pipe contract.
- Produces: live agent state from Claude (full lifecycle) and Codex (`done`).

- [ ] **Step 1: Add a broadcast helper to `claude-zellij-notify.sh`**

Insert after the existing `publish_pane_status` function:

```bash
# Broadcast the rich payload consumed by the zj-agents sidebar plugin.
publish_agent_status() {
    local status="$1"
    local on_focus="${2:-}"
    [[ -n "${ZELLIJ:-}" && -n "$zellij_pane" ]] || return 0
    local pane_num="${zellij_pane#terminal_}"
    local payload
    payload="$(jq -nc \
        --argjson id "$pane_num" \
        --arg status "$status" \
        --arg repo "$repo" \
        --arg branch "$branch" \
        --arg msg "$message" \
        --arg on_focus "$on_focus" \
        '{v:1, source:"claude", pane:{type:"terminal", id:$id}, status:$status, repo:$repo, branch:$branch, msg:$msg}
         + (if $on_focus == "" then {} else {on_focus:$on_focus} end)')"
    run_with_timeout 0.3 zellij pipe --name zj_agents.status.v1 -- "$payload" >/dev/null 2>&1 || true
}
```

- [ ] **Step 2: Call it from each lifecycle branch**

In the `case "$hook_event:$notification_type"` block, add a `publish_agent_status` call beside each existing `publish_pane_status`:
- `UserPromptSubmit` / `PreToolUse` / `PostToolUse` → `publish_agent_status "running"`
- `Notification:*` → `publish_agent_status "pending"`
- `Stop:*` → `publish_agent_status "done" "idle"`
- `SubagentStop:*` → `publish_agent_status "running"`

- [ ] **Step 3: Manually verify the Claude payload (dry pane)**

Run (outside Zellij, expect a no-op return 0; inside Zellij expect a pipe):
```bash
ZELLIJ=1 ZELLIJ_PANE_ID=terminal_7 bash -c '
  message="hi"; repo="demo"; branch="main"; zellij_pane="terminal_7"
  source ~/dotfiles/home-manager/modules/zellij/claude-zellij-notify.sh 2>/dev/null
' 2>&1 | head
```
Expected: no shell errors (function defined). (Full integration is verified in the live session.)

- [ ] **Step 4: Create `codex-zellij-notify.sh`**

```bash
#!/usr/bin/env bash
# Codex `notify` adapter → zj-agents sidebar. Codex passes one JSON arg.
# v1: emit "done" on agent-turn-complete (the only documented event type).
set -euo pipefail

raw="${1:-}"
[[ -n "${ZELLIJ:-}" && -n "${ZELLIJ_PANE_ID:-}" ]] || exit 0

type="$(jq -r '.type // empty' <<<"$raw" 2>/dev/null || true)"
[[ "$type" == "agent-turn-complete" ]] || exit 0

msg="$(jq -r '."last-assistant-message" // empty' <<<"$raw" 2>/dev/null || true)"
cwd="$(jq -r '.cwd // env.PWD' <<<"$raw" 2>/dev/null || true)"
repo="$(basename "$(git -C "$cwd" rev-parse --show-toplevel 2>/dev/null || printf '%s' "$cwd")")"
branch="$(git -C "$cwd" branch --show-current 2>/dev/null || true)"
pane_num="${ZELLIJ_PANE_ID#terminal_}"

payload="$(jq -nc --argjson id "$pane_num" --arg repo "$repo" --arg branch "$branch" --arg msg "$msg" \
    '{v:1, source:"codex", pane:{type:"terminal", id:$id}, status:"done", repo:$repo, branch:$branch, msg:$msg, on_focus:"idle"}')"
zellij pipe --name zj_agents.status.v1 -- "$payload" >/dev/null 2>&1 || true
```

- [ ] **Step 5: Wire both adapters + the wasm + the layout in Nix**

In `default.nix`, add a `fetchurl`/local reference for the built `zj-agents.wasm` (mirror the
`zellij-room` vendoring pattern) bound to `@zjAgents@`, and remove the now-dead `@smartTabs@`
`fetchurl` + `replaceStrings` entry (smart-tabs has been removed — see
`smart-tabs-postmortem.md`). Install `codex-zellij-notify` into `~/.local/bin`, and add
`~/.codex/config.toml` `notify = ["sh","-lc","codex-zellij-notify \"$1\"","_"]`. In `zellij.kdl`:
(a) replace the top `compact-bar` line of `default_tab_template` with the sidebar `pane
split_direction="vertical"` block from `dev/dev.kdl` (using `@zjAgents@`); (b) confirm the
`smart-tabs` plugin alias, its `load_plugins` entry, and the two `MessagePlugin` rename
keybindings are already gone (they were stripped during the postmortem cleanup) — zj-agents owns
all tab display, with naming per design §6.1. No `{% if status %}` format edit is needed anymore;
there is no smart-tabs `format` left to edit.

- [ ] **Step 6: Rebuild and verify end-to-end**

Run: `cd ~/dotfiles && home-manager switch --flake '.#mark.toda@macbook'`, then restart Zellij.
Expected: the sidebar appears; running Claude in a tab drives `◐`→`●`; running Codex drives `●`
on turn completion; clicking a row switches tabs; desktop notifications still fire from the
shell adapters.

- [ ] **Step 7: Commit (dotfiles repo)**

```bash
cd ~/dotfiles
git add -A
git commit -m "feat: drive zj-agents sidebar from Claude + Codex hooks"
```

---

## Self-Review

**Spec coverage:** §3 visual → Tasks 1 (glyph/color), 5 (rows/two-line/count/elapsed). §4 architecture/aggregation → Tasks 3,4. §4.1 state machine → Task 8 (adapter branches). §5 pipe contract → Task 2 (+ seq guard in Task 3). §6 wiring (permissions, subscriptions, position+1, broadcast, one-shot timer, layout) + §6.1 tab naming (push-only, no blocking host calls) → Tasks 6,7,8. §7 adapters (Claude+Codex done-only) → Task 8. §8 build/Nix → Tasks 0,8. §9 testing → Tasks 1–5 unit tests + Task 7 manual + the fake-adapter script. §10 phasing → Tasks map to Phases 0–3; Phase-1/2 acceptance in Task 7. §11 risks (mouse/selectable, pinning) → Task 7 acceptance with fallback.

**Placeholder scan:** No TBD/TODO. The two genuinely API-version-sensitive spots (Mouse variant shape, exact `switch_tab_to`/`PaneManifest` field names) carry concrete code plus an explicit "verify against 0.44" note in Task 6 Step 2 — not placeholders.

**Type consistency:** `StateStore::{apply, on_pane_focused, prune, get, any_active}` used consistently in Tasks 3/4/6. `TabAgg { status, done, total, detail }` and `Detail { repo, branch, msg, since_tick, status }` consistent across Tasks 4/5. `TabRow { number, name, active, agg }` consistent in Tasks 5/6. `payload::parse -> Option<StatusPayload>` and `StatusPayload` fields consistent in Tasks 2/3/6. Pipe name `zj_agents.status.v1` identical in Tasks 6/7/8.
