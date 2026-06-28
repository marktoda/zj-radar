# zj-radar CLI (notify + setup) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Superseded note:** this historical plan describes the original Codex
> `config.toml` `notify` integration. Current Codex setup is hook-first via
> `~/.codex/hooks.json`; the notify path is now a `--legacy-notify` fallback.

**Goal:** Ship a native `zj-radar` CLI with `notify` (jq-free Claude/Codex status broadcast) and `setup` (conflict-aware Codex config wiring), packaged via Nix, with the Claude plugin gracefully preferring it.

**Architecture:** One crate, a `cli` feature gating `clap`+`toml_edit`, a second `[[bin]]` (`zj-radar`) with `required-features=["cli"]`. The CLI builds the same `zj_radar.status.v1` JSON the plugin parses via a shared `payload::to_wire`. Pure `derive()`/`edit_codex()` cores (unit-tested), thin IO wrappers (manual). The wasm plugin build must stay free of the CLI deps.

**Tech Stack:** Rust (host bin + existing wasm lib), `clap` 4 (derive), `toml_edit` 0.22, `serde_json` (already present), `crane`/Nix for packaging.

## Global Constraints

- Agents supported: **Claude + Codex only** (no Aider/Gemini/etc.; no `serde_yaml`).
- The `cli` feature gates `clap` + `toml_edit`; both are `optional = true` deps.
- The wasm build (`cargo build --target wasm32-wasip1`, no `--features cli`) must **not** pull `clap`/`toml_edit`. Verify with `cargo tree`.
- Two bins: `zj_radar` (wasm plugin, `src/main.rs`, unchanged) and `zj-radar` (CLI, `src/bin/cli.rs`, `required-features=["cli"]`).
- The existing `[profile.release]` in `Cargo.toml` must be preserved.
- `cargo test` (no feature) and `cargo test --features cli` must both pass with **0 warnings**.
- `notify` is a **no-op** when `$ZELLIJ` is unset or `$ZELLIJ_PANE_ID` (minus a `terminal_` prefix) is non-numeric; it **swallows all errors** (never breaks a hook); `done` carries `on_focus = "idle"`.
- Claude `pending` **backstop**: drop `pending` when the message is empty or a generic phrase (`"Claude needs attention"`, `"Claude Code needs your attention"`).
- Codex: `type == "agent-turn-complete"` → `done`; any other type → no-op. (Verify the exact type string against the installed Codex before relying on it; if it differs, use the observed value and note it in the report.)
- `setup` covers **Codex only**; Claude is plugin-only (no `settings.json` editor, no double-fire guard).
- `setup codex` is **conflict-aware**: write `notify = ["zj-radar","notify","codex"]` only if the slot is empty or already ours; if a foreign program owns it, **refuse** unless `--force`; never silently clobber; back up + write atomically; refuse to write if the file isn't valid TOML.
- Work happens in the worktree `.claude/worktrees/cli-notify-setup` (branch `cli-notify-setup`). Commit per task with `git -c commit.gpgsign=false commit` (gpg-agent times out here). Do not merge to `main` until the user asks.

---

## File Structure

- `Cargo.toml` (modify) — `cli` feature, optional deps, second `[[bin]]`.
- `Cargo.lock` (modify) — regenerated.
- `src/status.rs` (modify) — add `Status::as_wire`.
- `src/payload.rs` (modify) — add `to_wire` + round-trip test.
- `src/lib.rs` (modify) — `#[cfg(feature = "cli")] pub mod cli;`.
- `src/cli/mod.rs` (create) — clap defs + `run()` dispatch.
- `src/cli/notify.rs` (create) — pure `derive_claude`/`derive_codex` + thin `run`.
- `src/cli/setup.rs` (create) — pure `edit_codex` + detection/fs + thin `run`.
- `src/bin/cli.rs` (create) — `fn main() { zj_radar::cli::run() }`.
- `flake.nix` (modify) — `packages.zj-radar-cli` + a `cli-test` check.
- `plugins/zj-radar-claude/scripts/notify.sh` (modify) — graceful shim.
- `README.md` (modify) — CLI usage section.

---

### Task 1: Shared wire contract (`as_wire` + `to_wire`)

**Files:**
- Modify: `src/status.rs`
- Modify: `src/payload.rs`

**Interfaces:**
- Produces: `Status::as_wire(self) -> &'static str`; `payload::to_wire(pane_id: u32, status: Status, repo: &str, branch: &str, msg: &str, on_focus: Option<Status>, source: &str) -> String`. Consumed by `notify` (Task 3).

- [ ] **Step 1: Write the failing round-trip test in `src/payload.rs`** (inside `mod tests`)

```rust
    #[test]
    fn to_wire_round_trips_through_parse() {
        use crate::status::Status;
        let json = to_wire(12, Status::Running, "pinky", "fix/x", "running tests", Some(Status::Idle), "claude");
        let got = parse(&json).expect("to_wire output must parse");
        assert_eq!(got.pane_id, 12);
        assert_eq!(got.status, Status::Running);
        assert_eq!(got.repo, "pinky");
        assert_eq!(got.branch, "fix/x");
        assert_eq!(got.msg, "running tests");
        assert_eq!(got.on_focus, Some(Status::Idle));
        assert_eq!(got.source, "claude");
    }

    #[test]
    fn to_wire_omits_on_focus_when_none() {
        use crate::status::Status;
        let json = to_wire(3, Status::Done, "r", "b", "m", None, "codex");
        assert!(!json.contains("on_focus"));
        assert_eq!(parse(&json).unwrap().on_focus, None);
    }

    #[test]
    fn as_wire_round_trips_for_all_statuses() {
        use crate::status::Status;
        for s in [Status::Idle, Status::Running, Status::Pending, Status::Done, Status::Error] {
            assert_eq!(Status::from_wire(s.as_wire()), s);
        }
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib payload::tests::to_wire_round_trips_through_parse`
Expected: FAIL — `to_wire`/`as_wire` not found.

- [ ] **Step 3: Add `as_wire` to `src/status.rs`** (inside `impl Status`, after `from_wire`)

```rust
    /// Serialize to the wire vocabulary (inverse of `from_wire`).
    pub fn as_wire(self) -> &'static str {
        match self {
            Status::Running => "running",
            Status::Pending => "pending",
            Status::Done => "done",
            Status::Error => "error",
            Status::Idle => "idle",
        }
    }
```

- [ ] **Step 4: Add `to_wire` to `src/payload.rs`** (top-level, after `parse`)

```rust
/// Build a `zj_radar.status.v1` JSON payload (inverse of `parse`). `on_focus` is
/// omitted entirely when `None`. Shared by the CLI producer and tested against
/// `parse` so the two can never drift.
pub fn to_wire(
    pane_id: u32,
    status: Status,
    repo: &str,
    branch: &str,
    msg: &str,
    on_focus: Option<Status>,
    source: &str,
) -> String {
    let mut obj = serde_json::json!({
        "v": 1,
        "source": source,
        "pane": { "type": "terminal", "id": pane_id },
        "status": status.as_wire(),
        "repo": repo,
        "branch": branch,
        "msg": msg,
    });
    if let Some(f) = on_focus {
        obj["on_focus"] = serde_json::Value::String(f.as_wire().to_string());
    }
    obj.to_string()
}
```

- [ ] **Step 5: Run to verify all three tests pass**

Run: `cargo test --lib payload:: && cargo test --lib status::`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/status.rs src/payload.rs
git -c commit.gpgsign=false commit -m "feat(payload): shared to_wire/as_wire wire contract with round-trip test"
```

---

### Task 2: CLI scaffold (feature, bin, clap skeleton)

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `src/lib.rs`
- Create: `src/cli/mod.rs`
- Create: `src/bin/cli.rs`

**Interfaces:**
- Produces: `zj_radar::cli::run()` (entry point); a `zj-radar` binary with `notify`/`setup` subcommands. Tasks 3 & 4 fill in `cli::notify` / `cli::setup`.

- [ ] **Step 1: Add the feature, optional deps, and second bin to `Cargo.toml`**

After the `[dependencies]` block add:

```toml
clap = { version = "4", features = ["derive"], optional = true }
toml_edit = { version = "0.22", optional = true }

[features]
cli = ["dep:clap", "dep:toml_edit"]

[[bin]]
name = "zj-radar"
path = "src/bin/cli.rs"
required-features = ["cli"]
```

(Leave the existing `[[bin]] name = "zj_radar"`, `[lib]`, and `[profile.release]` untouched.)

- [ ] **Step 2: Create `src/cli/mod.rs`**

```rust
//! Native CLI (`zj-radar`): `notify` + `setup`. Host-only; gated behind the
//! `cli` feature so the wasm plugin build never pulls clap/toml_edit.

use clap::{Parser, Subcommand};

mod notify;
mod setup;

#[derive(Parser)]
#[command(name = "zj-radar", about = "Broadcast agent status to the zj-radar Zellij sidebar, and wire agents up.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Broadcast one agent's status to the sidebar (called from an agent hook).
    Notify {
        /// Agent: claude | codex
        agent: String,
        /// For codex: the JSON the agent passes as a trailing argument.
        input: Option<String>,
        /// Explicit status (claude hooks pass this); bypasses event derivation.
        #[arg(long)]
        status: Option<String>,
        /// Print the payload instead of broadcasting.
        #[arg(long)]
        dry_run: bool,
    },
    /// Idempotently wire installed agents' configs to call `zj-radar notify`.
    Setup {
        /// Agents to set up (default: all detected). v1: codex.
        agents: Vec<String>,
        /// Remove our entries instead of adding them.
        #[arg(long)]
        uninstall: bool,
        /// Show what would change; write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
        /// Overwrite a foreign notify entry (codex).
        #[arg(long)]
        force: bool,
    },
}

/// CLI entry point (called by `src/bin/cli.rs`).
pub fn run() {
    let cli = Cli::parse();
    match cli.command {
        Command::Notify { agent, input, status, dry_run } => {
            notify::run(&agent, input.as_deref(), status.as_deref(), dry_run);
        }
        Command::Setup { agents, uninstall, dry_run, yes, force } => {
            setup::run(&agents, uninstall, dry_run, yes, force);
        }
    }
}
```

- [ ] **Step 3: Create stub `src/cli/notify.rs`** (filled in Task 3)

```rust
//! `zj-radar notify <agent>` — derive status, build payload, broadcast.

pub fn run(_agent: &str, _input: Option<&str>, _status: Option<&str>, _dry_run: bool) {
    // Implemented in Task 3.
}
```

- [ ] **Step 4: Create stub `src/cli/setup.rs`** (filled in Task 4)

```rust
//! `zj-radar setup [codex]` — idempotent, conflict-aware agent config wiring.

pub fn run(_agents: &[String], _uninstall: bool, _dry_run: bool, _yes: bool, _force: bool) {
    // Implemented in Task 4.
}
```

- [ ] **Step 5: Create `src/bin/cli.rs`**

```rust
// Native CLI entry point. The real logic lives in the `zj_radar` library's
// `cli` module (gated behind the `cli` feature).
fn main() {
    zj_radar::cli::run();
}
```

- [ ] **Step 6: Gate the module in `src/lib.rs`** — add near the other `mod` declarations:

```rust
#[cfg(feature = "cli")]
pub mod cli;
```

- [ ] **Step 7: Update the lockfile and verify the CLI builds**

Run: `cargo build --features cli --bin zj-radar && ./target/debug/zj-radar --help`
Expected: builds; `--help` lists `notify` and `setup`.

- [ ] **Step 8: Verify the wasm build stays lean (no clap/toml_edit)**

Run: `cargo tree --target wasm32-wasip1 -e normal 2>/dev/null | grep -E '^(│|├|└| )*(clap|toml_edit)' || echo "CLEAN: no clap/toml_edit in wasm deps"`
Expected: `CLEAN: no clap/toml_edit in wasm deps`.

- [ ] **Step 9: Verify no-feature tests still pass with no warnings**

Run: `cargo test 2>&1 | tail -3`
Expected: all tests pass; no warnings.

- [ ] **Step 10: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/cli/mod.rs src/cli/notify.rs src/cli/setup.rs src/bin/cli.rs
git -c commit.gpgsign=false commit -m "feat(cli): scaffold zj-radar binary behind the cli feature (clap skeleton)"
```

---

### Task 3: `notify` — pure derivation + broadcast

**Files:**
- Modify: `src/cli/notify.rs`

**Interfaces:**
- Consumes: `crate::status::Status`, `crate::payload::to_wire` (Task 1).
- Produces: `derive_claude`, `derive_codex`, `Update` (pure, tested); `run` (thin IO).

- [ ] **Step 1: Write failing tests in `src/cli/notify.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::Status;

    #[test]
    fn claude_explicit_status_passes_through() {
        let u = derive_claude(Some("running"), None, "anything").unwrap();
        assert_eq!(u.status, Status::Running);
    }

    #[test]
    fn claude_pending_with_real_message_is_kept() {
        let u = derive_claude(Some("pending"), None, "approve this?").unwrap();
        assert_eq!(u.status, Status::Pending);
        assert_eq!(u.msg, "approve this?");
    }

    #[test]
    fn claude_pending_backstop_drops_empty_and_generic() {
        assert!(derive_claude(Some("pending"), None, "").is_none());
        assert!(derive_claude(Some("pending"), None, "Claude needs attention").is_none());
        assert!(derive_claude(Some("pending"), None, "Claude Code needs your attention").is_none());
    }

    #[test]
    fn claude_derives_status_from_event_when_no_explicit_status() {
        assert_eq!(derive_claude(None, Some("UserPromptSubmit"), "").unwrap().status, Status::Running);
        assert_eq!(derive_claude(None, Some("PostToolUse"), "").unwrap().status, Status::Running);
        assert_eq!(derive_claude(None, Some("Stop"), "done").unwrap().status, Status::Done);
        assert!(derive_claude(None, Some("SomethingElse"), "").is_none());
    }

    #[test]
    fn codex_turn_complete_is_done_else_none() {
        assert_eq!(derive_codex("agent-turn-complete", "shipped it").unwrap().status, Status::Done);
        assert_eq!(derive_codex("agent-turn-complete", "shipped it").unwrap().msg, "shipped it");
        assert!(derive_codex("task-started", "").is_none());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --features cli --lib cli::notify`
Expected: FAIL — `derive_claude`/`derive_codex`/`Update` not found.

- [ ] **Step 3: Replace `src/cli/notify.rs` with the implementation**

```rust
//! `zj-radar notify <agent>` — derive status, build payload, broadcast.

use crate::payload::to_wire;
use crate::status::Status;
use std::io::Read;
use std::process::Command;

/// The decision a pure derivation produces. `None` from a `derive_*` means no-op.
pub struct Update {
    pub status: Status,
    pub msg: String,
}

const GENERIC_PENDING: [&str; 2] = ["Claude needs attention", "Claude Code needs your attention"];

/// Map a Claude hook event name to a status (used when `--status` is absent).
fn status_from_event(event: &str) -> Option<Status> {
    match event {
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "SubagentStop" => Some(Status::Running),
        "Notification" => Some(Status::Pending),
        "Stop" => Some(Status::Done),
        _ => None,
    }
}

/// Pure: decide Claude's status+msg. `status_arg` (from the matcher-driven
/// hooks.json) wins; else derive from `hook_event`. Applies the pending backstop.
pub fn derive_claude(status_arg: Option<&str>, hook_event: Option<&str>, msg: &str) -> Option<Update> {
    let status = match status_arg {
        Some(s) => Status::from_wire(s),
        None => status_from_event(hook_event?)?,
    };
    if status == Status::Pending {
        let m = msg.trim();
        if m.is_empty() || GENERIC_PENDING.contains(&m) {
            return None; // backstop: not a real "needs you"
        }
    }
    Some(Update { status, msg: msg.to_string() })
}

/// Pure: Codex only reports turn completion → `done`; anything else is a no-op.
pub fn derive_codex(event_type: &str, last_message: &str) -> Option<Update> {
    if event_type == "agent-turn-complete" {
        Some(Update { status: Status::Done, msg: last_message.to_string() })
    } else {
        None
    }
}

/// Terminal pane id from `$ZELLIJ_PANE_ID` (strip a `terminal_` prefix), or None
/// when not running under Zellij or the id is non-numeric.
fn pane_id_from_env() -> Option<u32> {
    if std::env::var_os("ZELLIJ").is_none() {
        return None;
    }
    let raw = std::env::var("ZELLIJ_PANE_ID").ok()?;
    raw.strip_prefix("terminal_").unwrap_or(&raw).parse::<u32>().ok()
}

fn git_repo_branch(cwd: &str) -> (String, String) {
    let top = Command::new("git").args(["-C", cwd, "rev-parse", "--show-toplevel"]).output().ok();
    let repo = top
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|p| p.rsplit('/').next().unwrap_or(&p).to_string())
        .unwrap_or_default();
    let branch = Command::new("git").args(["-C", cwd, "branch", "--show-current"]).output().ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    (repo, branch)
}

fn read_stdin() -> String {
    let mut s = String::new();
    let _ = std::io::stdin().read_to_string(&mut s);
    s
}

/// Thin IO wrapper: parse the agent input, derive, then broadcast. Never panics;
/// any failure is a silent no-op so the calling hook is never broken.
pub fn run(agent: &str, input: Option<&str>, status_arg: Option<&str>, dry_run: bool) {
    let Some(pane_id) = pane_id_from_env() else { return };

    // Per-agent: extract (cwd, event/type, msg) then derive an Update.
    let (update, cwd) = match agent {
        "claude" => {
            let raw = read_stdin();
            let v: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
            let event = v.get("hook_event_name").and_then(|x| x.as_str());
            let msg = v.get("message").and_then(|x| x.as_str())
                .or_else(|| v.get("last_assistant_message").and_then(|x| x.as_str()))
                .unwrap_or("");
            let cwd = v.get("cwd").and_then(|x| x.as_str()).map(str::to_string);
            (derive_claude(status_arg, event, msg), cwd)
        }
        "codex" => {
            let raw = input.unwrap_or("");
            let v: serde_json::Value = serde_json::from_str(raw).unwrap_or(serde_json::Value::Null);
            let ty = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
            let msg = v.get("last-assistant-message").and_then(|x| x.as_str()).unwrap_or("");
            let cwd = v.get("cwd").and_then(|x| x.as_str()).map(str::to_string);
            (derive_codex(ty, msg), cwd)
        }
        _ => {
            eprintln!("zj-radar: unknown agent '{agent}' (expected: claude | codex)");
            return;
        }
    };

    let Some(update) = update else { return };
    let cwd = cwd
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("PWD").ok())
        .unwrap_or_else(|| ".".to_string());
    let (repo, branch) = git_repo_branch(&cwd);
    let on_focus = if update.status == Status::Done { Some(Status::Idle) } else { None };
    let payload = to_wire(pane_id, update.status, &repo, &branch, &update.msg, on_focus, agent);

    if dry_run {
        eprintln!("{payload}");
        return;
    }
    // Best-effort broadcast; ignore failure entirely.
    let _ = Command::new("zellij")
        .args(["pipe", "--name", "zj_radar.status.v1", "--", &payload])
        .output();
}
```

- [ ] **Step 4: Run to verify the unit tests pass**

Run: `cargo test --features cli --lib cli::notify`
Expected: PASS (all 5 tests).

- [ ] **Step 5: Smoke-test the dry-run path**

Run: `ZELLIJ=1 ZELLIJ_PANE_ID=terminal_7 ./target/debug/zj-radar notify claude --status running --dry-run <<< '{"cwd":".","message":"hi"}'` (build first with `cargo build --features cli`)
Expected: prints a JSON payload to stderr containing `"status":"running"` and `"pane":{"type":"terminal","id":7}`.

- [ ] **Step 6: Commit**

```bash
git add src/cli/notify.rs
git -c commit.gpgsign=false commit -m "feat(cli): notify — claude/codex derivation (+ pending backstop) and broadcast"
```

---

### Task 4: `setup` — conflict-aware Codex editor

**Files:**
- Modify: `src/cli/setup.rs`

**Interfaces:**
- Produces: `edit_codex(existing: &str, install: bool, force: bool) -> Result<Outcome, String>` (pure, tested) and `run` (thin IO).

- [ ] **Step 1: Write failing tests in `src/cli/setup.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn assert_top_level_notify_is_ours(toml: &str) {
        let doc = toml.parse::<toml_edit::DocumentMut>().expect("valid toml");
        assert!(notify_is_ours(doc.get("notify")), "notify must be top-level and ours:\n{toml}");
    }

    #[test]
    fn fresh_file_installs_our_notify() {
        let out = edit_codex("", true, false).unwrap();
        match out { Outcome::Changed(s) => assert_top_level_notify_is_ours(&s), o => panic!("{o:?}") }
    }

    #[test]
    fn installs_above_existing_tables_stays_top_level() {
        // A top-level key appended AFTER a table would bind to the table — guard against it.
        let existing = "[marketplaces.x]\nsource = \"local\"\n";
        let out = edit_codex(existing, true, false).unwrap();
        match out {
            Outcome::Changed(s) => {
                assert_top_level_notify_is_ours(&s);
                assert!(s.contains("[marketplaces.x]"), "must preserve the user's table");
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn idempotent_when_already_ours() {
        let existing = "notify = [\"zj-radar\", \"notify\", \"codex\"]\n";
        assert!(matches!(edit_codex(existing, true, false).unwrap(), Outcome::Unchanged));
    }

    #[test]
    fn foreign_notify_refuses_without_force() {
        let existing = "notify = [\"/some/other/notifier\", \"turn-ended\"]\n";
        assert!(matches!(edit_codex(existing, true, false).unwrap(), Outcome::Conflict));
    }

    #[test]
    fn foreign_notify_overwritten_with_force_preserves_rest() {
        let existing = "model = \"gpt-5.5\"\nnotify = [\"/other\", \"turn-ended\"]\n";
        match edit_codex(existing, true, true).unwrap() {
            Outcome::Changed(s) => {
                assert_top_level_notify_is_ours(&s);
                assert!(s.contains("model = \"gpt-5.5\""), "must preserve other keys");
                assert!(!s.contains("/other"), "foreign notifier must be gone");
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn uninstall_removes_only_ours() {
        let ours = "notify = [\"zj-radar\", \"notify\", \"codex\"]\nmodel = \"x\"\n";
        match edit_codex(ours, false, false).unwrap() {
            Outcome::Changed(s) => { assert!(!s.contains("notify")); assert!(s.contains("model = \"x\"")); }
            o => panic!("{o:?}"),
        }
        let foreign = "notify = [\"/other\", \"turn-ended\"]\n";
        assert!(matches!(edit_codex(foreign, false, false).unwrap(), Outcome::Unchanged));
    }

    #[test]
    fn malformed_toml_is_refused() {
        assert!(edit_codex("this = = not toml", true, false).is_err());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --features cli --lib cli::setup`
Expected: FAIL — `edit_codex`/`Outcome`/`notify_is_ours` not found.

- [ ] **Step 3: Replace `src/cli/setup.rs` with the implementation**

```rust
//! `zj-radar setup [codex]` — idempotent, conflict-aware agent config wiring.
//! v1 supports Codex only (Claude is handled by the marketplace plugin).

use std::path::PathBuf;
use toml_edit::{Array, DocumentMut, Item};

/// Our Codex notify invocation — also the idempotency/uninstall marker.
const MARKER: [&str; 3] = ["zj-radar", "notify", "codex"];

#[derive(Debug)]
pub enum Outcome {
    /// New TOML content to write.
    Changed(String),
    /// Already in the desired state; write nothing.
    Unchanged,
    /// A foreign `notify` owns the slot; refused (use --force).
    Conflict,
}

/// True iff `notify` exists and equals our exact marker array.
pub fn notify_is_ours(item: Option<&Item>) -> bool {
    item.and_then(|i| i.as_array())
        .map(|a| a.len() == MARKER.len() && a.iter().zip(MARKER).all(|(v, m)| v.as_str() == Some(m)))
        .unwrap_or(false)
}

fn our_array() -> Array {
    let mut a = Array::new();
    for m in MARKER {
        a.push(m);
    }
    a
}

/// Pure editor. `install=true` adds/keeps our notify; `install=false` uninstalls.
/// Never clobbers a foreign notify unless `force`. Errors on unparseable TOML.
pub fn edit_codex(existing: &str, install: bool, force: bool) -> Result<Outcome, String> {
    let mut doc = existing
        .parse::<DocumentMut>()
        .map_err(|e| format!("config.toml is not valid TOML: {e}"))?;
    let present = doc.get("notify").is_some();
    let ours = notify_is_ours(doc.get("notify"));

    if install {
        if ours {
            return Ok(Outcome::Unchanged);
        }
        if present && !force {
            return Ok(Outcome::Conflict);
        }
        if present {
            // force overwrite of a foreign notify — in place, position preserved.
            doc["notify"] = toml_edit::value(our_array());
            return Ok(Outcome::Changed(doc.to_string()));
        }
        // Absent: prepend at byte 0 so the key stays top-level (a key appended
        // after an existing [table] would bind to that table). Preserves the
        // rest verbatim.
        let line = format!("notify = [\"{}\", \"{}\", \"{}\"]\n", MARKER[0], MARKER[1], MARKER[2]);
        return Ok(Outcome::Changed(format!("{line}{existing}")));
    }

    // Uninstall: remove only if it's ours; leave a foreign/absent notify alone.
    if ours {
        doc.as_table_mut().remove("notify");
        Ok(Outcome::Changed(doc.to_string()))
    } else {
        Ok(Outcome::Unchanged)
    }
}

// ── Thin IO layer (not unit-tested) ──

fn codex_config_path() -> PathBuf {
    if let Some(home) = std::env::var_os("CODEX_HOME") {
        return PathBuf::from(home).join("config.toml");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".codex").join("config.toml")
}

fn codex_installed() -> bool {
    codex_config_path().exists()
        && which("codex")
}

fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|p| p.join(bin).is_file()))
        .unwrap_or(false)
}

/// Entry point for `zj-radar setup`.
pub fn run(agents: &[String], uninstall: bool, dry_run: bool, yes: bool, force: bool) {
    // v1: only codex is supported; default (empty) = all detected = codex.
    let want_codex = agents.is_empty() || agents.iter().any(|a| a == "codex");
    let unknown: Vec<&String> = agents.iter().filter(|a| a.as_str() != "codex").collect();
    for a in unknown {
        eprintln!("zj-radar: setup does not support '{a}' (v1 supports: codex). Skipping.");
    }
    if !want_codex {
        return;
    }
    setup_codex(uninstall, dry_run, yes, force);
}

fn setup_codex(uninstall: bool, dry_run: bool, yes: bool, force: bool) {
    let path = codex_config_path();
    if !uninstall && !codex_installed() {
        if !path.exists() {
            println!("codex: skipped (no config at {})", path.display());
        } else {
            println!("codex: skipped (binary not found on PATH)");
        }
        return;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let outcome = match edit_codex(&existing, !uninstall, force) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("codex: refused — {e}");
            return;
        }
    };
    match outcome {
        Outcome::Unchanged => println!("codex: already up to date ({})", path.display()),
        Outcome::Conflict => {
            eprintln!(
                "codex: {} already has a different `notify` program. Refusing to overwrite it.\n\
                 Re-run with --force to replace it, or wire zj-radar manually.",
                path.display()
            );
        }
        Outcome::Changed(new) => {
            if dry_run {
                println!("--- {} (dry-run) ---\n{new}", path.display());
                return;
            }
            if !yes && !confirm(&format!("Write {}?", path.display())) {
                println!("codex: skipped (declined)");
                return;
            }
            if let Err(e) = write_atomic(&path, &new) {
                eprintln!("codex: write failed — {e}");
                return;
            }
            println!("codex: {} ({})", if uninstall { "removed" } else { "installed" }, path.display());
        }
    }
}

fn confirm(prompt: &str) -> bool {
    use std::io::Write;
    print!("{prompt} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Back up the existing file, then write atomically (temp file + rename).
fn write_atomic(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    if path.exists() {
        let _ = std::fs::copy(path, path.with_extension("toml.zj-radar.bak"));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("toml.zj-radar.tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}
```

- [ ] **Step 4: Run to verify the unit tests pass**

Run: `cargo test --features cli --lib cli::setup`
Expected: PASS (all 7 tests).

- [ ] **Step 5: Verify the whole feature build is warning-free**

Run: `cargo test --features cli 2>&1 | tail -3 && cargo build --features cli 2>&1 | grep -c warning`
Expected: all tests pass; warning count `0`.

- [ ] **Step 6: Commit**

```bash
git add src/cli/setup.rs
git -c commit.gpgsign=false commit -m "feat(cli): setup — conflict-aware codex notify editor (never clobbers)"
```

---

### Task 5: Integration — plugin shim, Nix package, docs

**Files:**
- Modify: `plugins/zj-radar-claude/scripts/notify.sh`
- Modify: `flake.nix`
- Modify: `README.md`

**Interfaces:**
- Consumes: the `zj-radar` binary (Tasks 2–4); `craneLib`/`commonArgs`/`cargoArtifactsHost` already defined in `flake.nix`.

- [ ] **Step 1: Add the graceful shim to the top of `notify.sh`** — insert immediately after the `status="${1:-running}"` line (before the `[[ -n "${ZELLIJ:-}" ... ]]` gate):

```bash
# Prefer the native CLI when present (drops the jq/bash dependency). It applies
# the same Zellij gate, pending backstop, and payload schema. Falls back to the
# bash implementation below when the binary isn't installed.
if command -v zj-radar >/dev/null 2>&1; then
    exec zj-radar notify claude --status "$status"
fi
```

- [ ] **Step 2: Verify the shim's fallback path is syntactically intact**

Run: `bash -n plugins/zj-radar-claude/scripts/notify.sh && echo "syntax OK"`
Expected: `syntax OK`.

- [ ] **Step 3: Add the CLI package + check to `flake.nix`** — in the `let` block, after the `cargoArtifactsHost` line, add:

```nix
        # ── native CLI (host target, `cli` feature) ──
        cliArgs = commonArgs // { cargoExtraArgs = "--features cli"; };
        cargoArtifactsCli = craneLib.buildDepsOnly cliArgs;
        zj-radar-cli = craneLib.buildPackage (cliArgs // {
          pname = "zj-radar-cli";
          cargoArtifacts = cargoArtifactsCli;
          cargoExtraArgs = "--features cli --bin zj-radar";
          doCheck = false;
        });
```

Then in the returned attrset, add the package and a check:

```nix
        packages.zj-radar-cli = zj-radar-cli;
```

(add this line alongside the existing `packages.default` / `packages.zj-radar`), and inside the `checks = { ... }` block add:

```nix
          cli-test = craneLib.cargoTest (cliArgs // {
            cargoArtifacts = cargoArtifactsCli;
          });
```

- [ ] **Step 4: Build the CLI via Nix and run it**

Run: `nix build .#zj-radar-cli && ./result/bin/zj-radar --help | head -1`
Expected: builds; prints the CLI's about line.

- [ ] **Step 5: Run the full flake check (now includes cli-test + the lean wasm)**

Run: `nix flake check 2>&1 | grep -v warning | tail -3`
Expected: passes (wasm `zj-radar`, host `clippy`/`test`, and `cli-test` all green).

- [ ] **Step 6: Add a CLI section to `README.md`** — insert after the "### 2. The Claude Code producer" subsection (before the "## Configuration" heading):

````markdown
### Optional: the `zj-radar` CLI

A native binary that drops the `jq`/`bash` dependency and wires non-plugin agents.

```sh
# Nix:
nix build github:mark-toda/zj-radar#zj-radar-cli   # -> result/bin/zj-radar
# Cargo:
cargo install --git https://github.com/mark-toda/zj-radar --features cli
```

- **`zj-radar notify <claude|codex>`** — broadcasts agent status. The Claude
  plugin's hook script automatically prefers it when it's on `PATH` (jq-free);
  otherwise the plugin falls back to its bundled `bash`+`jq` script.
- **`zj-radar setup [codex]`** — idempotently wires Codex's `~/.codex/config.toml`
  `notify` to call `zj-radar notify codex`. It **never overwrites** an existing
  `notify` program (e.g. a Computer Use notifier); pass `--force` to replace it,
  `--dry-run` to preview, `--uninstall` to remove. (Claude needs no `setup` — use
  the plugin in §2.)

Codex reports only turn-completion, so it shows as `done` only (no `working`).
````

- [ ] **Step 7: Commit**

```bash
git add plugins/zj-radar-claude/scripts/notify.sh flake.nix README.md
git -c commit.gpgsign=false commit -m "feat(cli): graceful plugin shim, flake packages.zj-radar-cli, README CLI section"
```

---

## Self-Review

**Spec coverage:**
- Crate shape / `cli` feature / two bins → Task 2. ✓
- Shared `to_wire`/`as_wire` + round-trip → Task 1. ✓
- `notify` claude (status + backstop) + codex (turn-complete) + no-op/error rules → Task 3. ✓
- `setup` codex conflict-aware (empty/ours/foreign/force/uninstall/malformed) + atomic write/backup → Task 4. ✓
- Claude plugin-only (no settings.json editor) → reflected by *absence* of any such task; `notify.sh` shim → Task 5. ✓
- Distribution (`packages.zj-radar-cli` + cargo install doc) → Task 5. ✓
- Testing (pure derive/edit + round-trip; no fs/process in unit tests) → Tasks 1,3,4. ✓
- Wasm leanness (no clap/toml_edit) → Task 2 Step 8. ✓

**Placeholder scan:** No "TBD"/"add error handling"/"similar to Task N". The Codex `agent-turn-complete` string carries an explicit verify-against-real-Codex instruction (Global Constraints) rather than a guess left blank. Stub files in Task 2 are immediately replaced in Tasks 3–4 (intentional scaffold, not a placeholder).

**Type consistency:** `Update { status, msg }`, `derive_claude(Option<&str>, Option<&str>, &str)`, `derive_codex(&str, &str)`, `edit_codex(&str, bool, bool) -> Result<Outcome, String>`, `Outcome::{Changed(String),Unchanged,Conflict}`, `notify_is_ours(Option<&Item>)`, and `to_wire(u32, Status, &str, &str, &str, Option<Status>, &str)` are used identically in their definitions (Tasks 1/3/4) and call sites (`cli::run`, tests). ✓
