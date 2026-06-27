# zj-radar CLI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** A native `zj-radar` binary with `notify` (universal agent notifier → `zj_radar.status.v1` broadcast) and `setup` (idempotent per-agent hook installer), sharing the plugin's payload/status types, with no `jq`/bash dependency.

**Architecture:** Same crate as the plugin; CLI lives behind a `cli` cargo feature (so the wasm build never pulls clap/toml_edit/serde_yaml). The CLI reuses the pure `payload`/`status`/`config` modules; the producer↔consumer wire contract is the single `payload` type (`payload::to_wire` serializes what `payload::parse` reads). Agent quirks + config-file editing live as pure, unit-tested functions; fs/process/`zellij pipe`/`git` are thin wrappers.

**Tech Stack:** Rust; `clap` v4 (derive), `toml_edit` v0.22, `serde_yaml` v0.9, `serde_json` (existing) — all behind feature `cli`. Plugin build target `wasm32-wasip1` unaffected.

## Global Constraints

- The `cli` module + the `zj-radar` bin are `#[cfg(feature = "cli")]` / `required-features = ["cli"]`. `cargo build --target wasm32-wasip1` must NOT pull clap/toml_edit/serde_yaml.
- Binary names: wasm plugin bin stays `zj_radar` (artifact `zj_radar.wasm`); the CLI bin is `zj-radar` (hyphen).
- Pipe broadcast name is exactly `zj_radar.status.v1`. Wire schema (from `docs/config-design.md`/`docs/design.md`): `{v:1, source, pane:{type:"terminal", id:<u32>}, status, repo, branch, msg, on_focus?}`.
- `payload::to_wire` lives in the un-gated pure `payload` module (shared, no `zellij-tile`/clap dep). `status::Status` is reused (add `as_wire`).
- Status wire values: `running|pending|done|error|idle`. `done` carries `on_focus="idle"`.
- `notify` no-ops when `$ZELLIJ` is unset or `$ZELLIJ_PANE_ID` (after stripping a `terminal_` prefix) isn't numeric. Broadcast failures are swallowed (never break the calling hook).
- Both `cargo test` (no features) and `cargo test --features cli` must be pristine (0 warnings).
- DRY, YAGNI, TDD, frequent commits. Commits use `--no-gpg-sign` (signing times out non-interactively).

## File Structure

- Modify: `src/payload.rs` — add `to_wire(&StatusPayload) -> String` (+ tests).
- Modify: `src/status.rs` — add `Status::as_wire(self) -> &'static str` (+ test).
- Modify: `src/lib.rs` — `#[cfg(feature = "cli")] pub mod cli;`.
- Modify: `Cargo.toml` — `cli` feature, optional deps, the `zj-radar` `[[bin]]`.
- Create: `src/bin/cli.rs` — `fn main()` → `zj_radar::cli::run()`.
- Create: `src/cli/mod.rs` — clap `Cli`/`Command` + `run()` dispatch.
- Create: `src/cli/notify.rs` — `derive()` (pure) + the broadcast pipeline (thin).
- Create: `src/cli/setup.rs` — agent table, detection, the three editors (pure) + fs wrappers.

---

### Task 1: scaffold (`cli` feature, bin, clap skeleton) + `payload::to_wire`

**Files:**
- Modify: `Cargo.toml`, `src/lib.rs`, `src/status.rs`, `src/payload.rs`
- Create: `src/bin/cli.rs`, `src/cli/mod.rs`

**Interfaces:**
- Produces: `Status::as_wire(self) -> &'static str`; `payload::to_wire(p: &StatusPayload) -> String`; `cli::run()`; clap `Cli { command: Command }`, `Command::{Notify(NotifyArgs), Setup(SetupArgs)}`.

- [ ] **Step 1: `Status::as_wire` + test (`src/status.rs`)**

Add to `impl Status`:
```rust
/// Wire value (inverse of `from_wire`).
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
Add a test asserting `Status::from_wire(s.as_wire()) == s` for all five variants.

- [ ] **Step 2: `payload::to_wire` + test (`src/payload.rs`)**

```rust
/// Serialize a StatusPayload to the zj_radar.status.v1 wire JSON. Inverse of
/// `parse`. `on_focus`/`seq` are emitted only when set.
pub fn to_wire(p: &StatusPayload) -> String {
    use serde_json::{json, Map, Value};
    let mut obj = Map::new();
    obj.insert("v".into(), json!(1));
    obj.insert("source".into(), json!(p.source));
    obj.insert("pane".into(), json!({ "type": "terminal", "id": p.pane_id }));
    obj.insert("status".into(), json!(p.status.as_wire()));
    obj.insert("repo".into(), json!(p.repo));
    obj.insert("branch".into(), json!(p.branch));
    obj.insert("msg".into(), json!(p.msg));
    if let Some(of) = p.on_focus {
        obj.insert("on_focus".into(), json!(of.as_wire()));
    }
    if let Some(seq) = p.seq {
        obj.insert("seq".into(), json!(seq));
    }
    Value::Object(obj).to_string()
}
```
Test (contract round-trip): build a `StatusPayload { pane_id: 12, status: Running, repo:"r", branch:"b", msg:"m", on_focus: Some(Idle), seq: None, source:"claude" }`, `to_wire` it, then `parse` the result, and assert the parsed fields equal the originals (pane_id, status, repo, branch, msg, on_focus). Also assert `parse(to_wire(done_payload))` keeps `on_focus`.

- [ ] **Step 3: `Cargo.toml` — feature, deps, bin**

Add (alongside existing deps):
```toml
[features]
cli = ["dep:clap", "dep:toml_edit", "dep:serde_yaml"]

[dependencies]
clap = { version = "4", features = ["derive"], optional = true }
toml_edit = { version = "0.22", optional = true }
serde_yaml = { version = "0.9", optional = true }

[[bin]]
name = "zj-radar"
path = "src/bin/cli.rs"
required-features = ["cli"]
```
(Keep the existing `[[bin]] name = "zj_radar" path = "src/main.rs"` and `[lib]`.)

- [ ] **Step 4: `src/lib.rs` — register the gated module**

```rust
#[cfg(feature = "cli")]
pub mod cli;
```

- [ ] **Step 5: `src/bin/cli.rs`**

```rust
fn main() {
    std::process::exit(zj_radar::cli::run());
}
```

- [ ] **Step 6: `src/cli/mod.rs` — clap surface + dispatch stubs**

```rust
//! Native CLI (`zj-radar`). Gated behind the `cli` feature.
use clap::{Parser, Subcommand, Args};

pub mod notify;
pub mod setup;

#[derive(Parser)]
#[command(name = "zj-radar", version, about = "Agent status radar for Zellij")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Broadcast agent status to the zj-radar sidebar (called from agent hooks).
    Notify(NotifyArgs),
    /// Wire installed agents' hooks to call `zj-radar notify` (idempotent).
    Setup(SetupArgs),
}

#[derive(Args)]
pub struct NotifyArgs {
    /// Agent name (claude|codex|aider). Omit when using --status.
    pub agent: Option<String>,
    /// Explicit status (running|pending|done|error|idle) — bypasses agent parsing.
    #[arg(long)]
    pub status: Option<String>,
    #[arg(long)]
    pub message: Option<String>,
    #[arg(long)]
    pub repo: Option<String>,
    #[arg(long)]
    pub branch: Option<String>,
    #[arg(long)]
    pub source: Option<String>,
    /// Trailing payload (Codex passes its JSON here).
    #[arg(trailing_var_arg = true)]
    pub rest: Vec<String>,
}

#[derive(Args)]
pub struct SetupArgs {
    /// Agents to wire (default: all detected).
    pub agents: Vec<String>,
    #[arg(long)]
    pub uninstall: bool,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long, short = 'y')]
    pub yes: bool,
}

/// Entry point. Returns a process exit code.
pub fn run() -> i32 {
    let cli = Cli::parse();
    match cli.command {
        Command::Notify(args) => notify::run(args),
        Command::Setup(args) => setup::run(args),
    }
}
```
Add minimal stubs so it compiles: in `src/cli/notify.rs` → `use super::NotifyArgs; pub fn run(_a: NotifyArgs) -> i32 { 0 }`; in `src/cli/setup.rs` → `use super::SetupArgs; pub fn run(_a: SetupArgs) -> i32 { 0 }`.

- [ ] **Step 7: Build + test both configurations**

Run: `cargo test` → existing tests + the 2 new (as_wire, to_wire) pass, 0 warnings.
Run: `cargo build --features cli` → produces `target/debug/zj-radar`; `cargo test --features cli` → passes, 0 warnings.
Run: `nix develop -c cargo build --target wasm32-wasip1` → still builds the plugin, 0 warnings (no cli deps pulled).

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/status.rs src/payload.rs src/bin/cli.rs src/cli/
git commit --no-gpg-sign -m "feat(cli): scaffold zj-radar binary (cli feature) + payload::to_wire"
```

---

### Task 2: `notify`

**Files:**
- Modify: `src/cli/notify.rs`

**Interfaces:**
- Consumes: `crate::status::Status`, `crate::payload::{StatusPayload, to_wire, sanitize}`, `super::NotifyArgs`.
- Produces: `pub struct Update { pub status: Status, pub msg: String }`; `pub fn derive(agent: &str, stdin: &str, rest: &[String]) -> Option<Update>`; `pub fn run(args: NotifyArgs) -> i32`.

- [ ] **Step 1: Write `derive` + tests (pure)**

```rust
use super::NotifyArgs;
use crate::payload::{self, StatusPayload};
use crate::status::Status;

pub struct Update {
    pub status: Status,
    pub msg: String,
}

/// Map an agent's hook payload to a status update. `stdin` is the hook's stdin
/// (Claude), `rest` the trailing argv (Codex passes its JSON there). Returns
/// None when the event is not one we surface (e.g. Codex non-turn-complete).
pub fn derive(agent: &str, stdin: &str, rest: &[String]) -> Option<Update> {
    match agent {
        "claude" => {
            let v: serde_json::Value = serde_json::from_str(stdin).ok()?;
            let event = v.get("hook_event_name").and_then(|x| x.as_str()).unwrap_or("");
            let status = match event {
                "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "SubagentStop" => Status::Running,
                "Notification" => Status::Pending,
                "Stop" => Status::Done,
                _ => return None,
            };
            let raw_msg = v.get("message").and_then(|x| x.as_str())
                .or_else(|| v.get("last_assistant_message").and_then(|x| x.as_str()))
                .unwrap_or("");
            let msg = if raw_msg == "Claude needs attention" { "" } else { raw_msg };
            Some(Update { status, msg: msg.to_string() })
        }
        "codex" => {
            let raw = rest.first().map(String::as_str).unwrap_or("");
            let v: serde_json::Value = serde_json::from_str(raw).ok()?;
            if v.get("type").and_then(|x| x.as_str()) != Some("agent-turn-complete") {
                return None;
            }
            let msg = v.get("last-assistant-message").and_then(|x| x.as_str()).unwrap_or("");
            Some(Update { status: Status::Done, msg: msg.to_string() })
        }
        "aider" => Some(Update { status: Status::Done, msg: String::new() }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_events_map_to_status() {
        let mk = |e: &str| format!(r#"{{"hook_event_name":"{}","message":"hi"}}"#, e);
        assert_eq!(derive("claude", &mk("UserPromptSubmit"), &[]).unwrap().status, Status::Running);
        assert_eq!(derive("claude", &mk("PreToolUse"), &[]).unwrap().status, Status::Running);
        assert_eq!(derive("claude", &mk("Notification"), &[]).unwrap().status, Status::Pending);
        assert_eq!(derive("claude", &mk("Stop"), &[]).unwrap().status, Status::Done);
        assert!(derive("claude", &mk("PreCompact"), &[]).is_none());
    }

    #[test]
    fn claude_message_prefers_message_then_last_assistant_drops_placeholder() {
        let s = derive("claude", r#"{"hook_event_name":"Stop","message":"all done"}"#, &[]).unwrap();
        assert_eq!(s.msg, "all done");
        let s = derive("claude", r#"{"hook_event_name":"Stop","last_assistant_message":"x"}"#, &[]).unwrap();
        assert_eq!(s.msg, "x");
        let s = derive("claude", r#"{"hook_event_name":"Stop","message":"Claude needs attention"}"#, &[]).unwrap();
        assert_eq!(s.msg, "");
    }

    #[test]
    fn codex_only_on_turn_complete() {
        let done = vec![r#"{"type":"agent-turn-complete","last-assistant-message":"shipped"}"#.to_string()];
        let u = derive("codex", "", &done).unwrap();
        assert_eq!(u.status, Status::Done);
        assert_eq!(u.msg, "shipped");
        let other = vec![r#"{"type":"something-else"}"#.to_string()];
        assert!(derive("codex", "", &other).is_none());
        assert!(derive("codex", "", &[]).is_none()); // no payload
    }

    #[test]
    fn aider_is_done() {
        assert_eq!(derive("aider", "", &[]).unwrap().status, Status::Done);
    }

    #[test]
    fn unknown_agent_is_none() {
        assert!(derive("nope", "", &[]).is_none());
    }
}
```

- [ ] **Step 2: Run derive tests**

Run: `cargo test --features cli notify::tests`
Expected: 5 tests pass.

- [ ] **Step 3: Implement `run` (thin wrappers around the pure core)**

Append to `src/cli/notify.rs`. The wrappers — `pane_id_from_env`, `git_repo_branch`, `broadcast` — are thin and exercised manually.
```rust
fn pane_id_from_env() -> Option<u32> {
    if std::env::var_os("ZELLIJ").is_none() {
        return None;
    }
    let raw = std::env::var("ZELLIJ_PANE_ID").ok()?;
    raw.strip_prefix("terminal_").unwrap_or(&raw).parse::<u32>().ok()
}

fn git_repo_branch(cwd: &str) -> (String, String) {
    let run = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C").arg(cwd).args(args).output().ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let repo = run(&["rev-parse", "--show-toplevel"])
        .map(|top| top.rsplit('/').next().unwrap_or(&top).to_string())
        .unwrap_or_default();
    let branch = run(&["branch", "--show-current"]).unwrap_or_default();
    (repo, branch)
}

fn broadcast(json: &str) {
    let _ = std::process::Command::new("zellij")
        .args(["pipe", "--name", "zj_radar.status.v1", "--", json])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

pub fn run(args: NotifyArgs) -> i32 {
    let Some(pane_id) = pane_id_from_env() else { return 0 }; // no-op outside Zellij

    // Resolve status + msg + source: explicit --status wins; else derive from agent.
    let (status, mut msg, source);
    if let Some(s) = args.status.as_deref() {
        status = Status::from_wire(s);
        msg = args.message.clone().unwrap_or_default();
        source = args.source.clone().unwrap_or_else(|| "cli".into());
    } else if let Some(agent) = args.agent.as_deref() {
        let mut stdin = String::new();
        use std::io::Read;
        let _ = std::io::stdin().read_to_string(&mut stdin);
        match derive(agent, &stdin, &args.rest) {
            Some(u) => {
                status = u.status;
                msg = args.message.clone().unwrap_or(u.msg);
                source = args.source.clone().unwrap_or_else(|| agent.to_string());
            }
            None => return 0, // event we don't surface
        }
    } else {
        eprintln!("zj-radar notify: provide an agent or --status");
        return 2;
    }

    // cwd: prefer the agent payload's cwd if present on stdin/argv? Keep simple:
    // use $PWD (hooks run in the agent's cwd).
    let cwd = std::env::var("PWD").unwrap_or_else(|_| ".".into());
    let (mut repo, mut branch) = git_repo_branch(&cwd);
    if let Some(r) = args.repo { repo = r; }
    if let Some(b) = args.branch { branch = b; }

    msg = payload::sanitize(&msg, crate::payload::MAX_MSG_CHARS);
    let on_focus = (status == Status::Done).then_some(Status::Idle);
    let p = StatusPayload { pane_id, status, repo, branch, msg, on_focus, seq: None, source };
    broadcast(&payload::to_wire(&p));
    0
}
```
(If `StatusPayload` fields aren't all `pub`, make them `pub` — they are per the existing module. `MAX_MSG_CHARS` is already `pub`.)

- [ ] **Step 4: Build + test**

Run: `cargo test --features cli` → all pass, 0 warnings.
Run: `cargo build --features cli` → builds.
Manual smoke (optional): `ZELLIJ=1 ZELLIJ_PANE_ID=terminal_7 echo '{"hook_event_name":"Stop","message":"hi"}' | ./target/debug/zj-radar notify claude` — but it will try to run `zellij pipe`; outside a session it just no-ops on the pipe. To inspect the payload without piping, temporarily not needed — rely on unit tests.

- [ ] **Step 5: Commit**

```bash
git add src/cli/notify.rs
git commit --no-gpg-sign -m "feat(cli): notify — agent payload parsing + broadcast"
```

---

### Task 3: `setup` framework + detection + Claude editor

**Files:**
- Modify: `src/cli/setup.rs`

**Interfaces:**
- Consumes: `super::SetupArgs`.
- Produces: `pub struct Agent { name, binary, config_rel, env_override, format, marker }`; `pub fn agents() -> Vec<Agent>`; `pub fn edit_claude(existing: &str, install: bool) -> Result<String, String>`; `pub fn run(args: SetupArgs) -> i32`.

- [ ] **Step 1: Agent table + the Claude JSON editor + tests**

```rust
use super::SetupArgs;

#[derive(Clone, Copy, PartialEq)]
pub enum Format { ClaudeJson, CodexToml, AiderYaml }

pub struct Agent {
    pub name: &'static str,
    pub binary: &'static str,
    /// Config path relative to $HOME (unless `env_override` is set to a dir).
    pub config_rel: &'static str,
    pub env_override: Option<&'static str>,
    pub format: Format,
}

pub fn agents() -> Vec<Agent> {
    vec![
        Agent { name: "claude", binary: "claude", config_rel: ".claude/settings.json", env_override: None, format: Format::ClaudeJson },
        Agent { name: "codex",  binary: "codex",  config_rel: ".codex/config.toml",   env_override: Some("CODEX_HOME"), format: Format::CodexToml },
        Agent { name: "aider",  binary: "aider",  config_rel: ".aider.conf.yml",       env_override: None, format: Format::AiderYaml },
    ]
}

const CLAUDE_EVENTS: &[&str] = &[
    "UserPromptSubmit", "PreToolUse", "PostToolUse", "Notification", "SubagentStop", "Stop",
];
const CLAUDE_CMD: &str = "zj-radar notify claude";

/// Install or remove zj-radar's hooks in a Claude settings.json string.
/// Idempotent: strips our entries (command == CLAUDE_CMD) then re-adds on install.
/// Preserves all other settings + the user's other hook entries. Errors if the
/// input is non-empty and not valid JSON.
pub fn edit_claude(existing: &str, install: bool) -> Result<String, String> {
    use serde_json::{json, Value};
    let mut root: Value = if existing.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(existing).map_err(|e| format!("settings.json is not valid JSON: {e}"))?
    };
    if !root.is_object() {
        return Err("settings.json root is not an object".into());
    }
    let hooks = root.as_object_mut().unwrap()
        .entry("hooks").or_insert_with(|| json!({}));
    let hooks_obj = hooks.as_object_mut().ok_or("hooks is not an object")?;

    for event in CLAUDE_EVENTS {
        // strip our prior entries from this event's array
        if let Some(arr) = hooks_obj.get_mut(*event).and_then(|v| v.as_array_mut()) {
            arr.retain(|group| {
                group.get("hooks").and_then(|h| h.as_array()).map_or(true, |inner| {
                    !inner.iter().any(|h| h.get("command").and_then(|c| c.as_str()) == Some(CLAUDE_CMD))
                })
            });
        }
        if install {
            let entry = json!({ "hooks": [ { "type": "command", "command": CLAUDE_CMD } ] });
            hooks_obj.entry(*event).or_insert_with(|| json!([]))
                .as_array_mut().ok_or("event is not an array")?.push(entry);
        }
        // drop now-empty event arrays
        if hooks_obj.get(*event).and_then(|v| v.as_array()).map_or(false, |a| a.is_empty()) {
            hooks_obj.remove(*event);
        }
    }
    if hooks_obj.is_empty() {
        root.as_object_mut().unwrap().remove("hooks");
    }
    Ok(serde_json::to_string_pretty(&root).unwrap() + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn cmds(s: &str, event: &str) -> Vec<String> {
        let v: Value = serde_json::from_str(s).unwrap();
        v["hooks"][event].as_array().map(|a| a.iter().filter_map(|g|
            g["hooks"][0]["command"].as_str().map(String::from)).collect()).unwrap_or_default()
    }

    #[test]
    fn install_adds_all_events() {
        let out = edit_claude("", true).unwrap();
        for e in CLAUDE_EVENTS {
            assert_eq!(cmds(&out, e), vec![CLAUDE_CMD.to_string()], "event {e}");
        }
    }

    #[test]
    fn install_is_idempotent() {
        let once = edit_claude("", true).unwrap();
        let twice = edit_claude(&once, true).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn install_preserves_user_hooks_and_settings() {
        let user = r#"{"theme":"dark","hooks":{"Stop":[{"hooks":[{"type":"command","command":"my-own-thing"}]}]}}"#;
        let out = edit_claude(user, true).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["theme"], "dark");
        let stop = cmds(&out, "Stop");
        assert!(stop.contains(&"my-own-thing".to_string()));
        assert!(stop.contains(&CLAUDE_CMD.to_string()));
    }

    #[test]
    fn uninstall_removes_only_ours() {
        let user = r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"my-own-thing"}]}]}}"#;
        let installed = edit_claude(user, true).unwrap();
        let removed = edit_claude(&installed, false).unwrap();
        let stop = cmds(&removed, "Stop");
        assert_eq!(stop, vec!["my-own-thing".to_string()]);
        // our other events fully removed
        let v: Value = serde_json::from_str(&removed).unwrap();
        assert!(v["hooks"].get("PreToolUse").is_none());
    }

    #[test]
    fn malformed_json_errors() {
        assert!(edit_claude("{not json", true).is_err());
    }
}
```

- [ ] **Step 2: Run editor tests**

Run: `cargo test --features cli setup::tests`
Expected: 5 tests pass.

- [ ] **Step 3: Implement detection + `run` (install/uninstall/dry-run/report) — Claude only wired this task**

Append to `src/cli/setup.rs`. Thin wrappers (`on_path`, `home`, `config_path`, `read`/`write_atomic`/`backup`) are exercised manually.
```rust
use std::path::PathBuf;

fn home() -> PathBuf { std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default() }

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH").map_or(false, |paths| {
        std::env::split_paths(&paths).any(|d| d.join(bin).is_file())
    })
}

fn config_path(a: &Agent) -> PathBuf {
    if let Some(env) = a.env_override {
        if let Some(dir) = std::env::var_os(env) {
            // env override points at the config DIR; take the file name from config_rel
            let file = std::path::Path::new(a.config_rel).file_name().unwrap();
            return PathBuf::from(dir).join(file);
        }
    }
    home().join(a.config_rel)
}

fn edit(a: &Agent, existing: &str, install: bool) -> Result<String, String> {
    match a.format {
        Format::ClaudeJson => edit_claude(existing, install),
        Format::CodexToml => Err("codex editor lands in Task 4".into()),
        Format::AiderYaml => Err("aider editor lands in Task 4".into()),
    }
}

pub fn run(args: SetupArgs) -> i32 {
    let all = agents();
    let selected: Vec<&Agent> = if args.agents.is_empty() {
        all.iter().collect()
    } else {
        all.iter().filter(|a| args.agents.iter().any(|n| n == a.name)).collect()
    };
    let (mut installed, mut skipped) = (0, 0);
    for a in selected {
        let path = config_path(a);
        // detection (install only): config dir exists AND binary on PATH
        if !args.uninstall {
            if !path.parent().map_or(false, |p| p.exists()) {
                println!("  {}: skipped (no config dir)", a.name); skipped += 1; continue;
            }
            if !on_path(a.binary) {
                println!("  {}: skipped (binary not found)", a.name); skipped += 1; continue;
            }
        }
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let updated = match edit(a, &existing, !args.uninstall) {
            Ok(s) => s,
            Err(e) => { println!("  {}: skipped ({e})", a.name); skipped += 1; continue; }
        };
        if updated == existing {
            println!("  {}: already up to date", a.name); continue;
        }
        if args.dry_run {
            println!("  {}: would update {}", a.name, path.display());
            continue;
        }
        if !args.yes {
            // simple confirm
            use std::io::Write;
            print!("  {}: update {}? [y/N] ", a.name, path.display());
            let _ = std::io::stdout().flush();
            let mut ans = String::new();
            let _ = std::io::stdin().read_line(&mut ans);
            if !matches!(ans.trim(), "y" | "Y") { println!("  {}: skipped", a.name); skipped += 1; continue; }
        }
        // backup + atomic write
        if !existing.is_empty() {
            let _ = std::fs::write(path.with_extension("zjbak"), &existing);
        }
        if let Some(parent) = path.parent() { let _ = std::fs::create_dir_all(parent); }
        let tmp = path.with_extension("zjtmp");
        if std::fs::write(&tmp, &updated).and_then(|_| std::fs::rename(&tmp, &path)).is_err() {
            println!("  {}: FAILED to write {}", a.name, path.display()); skipped += 1; continue;
        }
        println!("  {}: {} {}", a.name, if args.uninstall {"removed from"} else {"installed at"}, path.display());
        installed += 1;
    }
    println!("Done: {installed} {}, {skipped} skipped", if args.uninstall {"removed"} else {"installed"});
    0
}
```

- [ ] **Step 4: Build + test**

Run: `cargo test --features cli` → all pass, 0 warnings.
Run: `cargo build --features cli`. Optional manual: `./target/debug/zj-radar setup --dry-run claude`.

- [ ] **Step 5: Commit**

```bash
git add src/cli/setup.rs
git commit --no-gpg-sign -m "feat(cli): setup framework + detection + Claude settings.json editor"
```

---

### Task 4: Codex (toml_edit) + Aider (serde_yaml) editors + double-fire guard

**Files:**
- Modify: `src/cli/setup.rs`

**Interfaces:**
- Consumes: `toml_edit`, `serde_yaml`.
- Produces: `pub fn edit_codex(existing: &str, install: bool) -> Result<String, String>`; `pub fn edit_aider(existing: &str, install: bool) -> Result<String, String>`; wires both into `edit()`; adds the Claude/plugin double-fire warning.

- [ ] **Step 1: Codex editor + tests**

```rust
const CODEX_NOTIFY: [&str; 3] = ["zj-radar", "notify", "codex"];

/// Set/remove top-level `notify` in Codex config.toml (format-preserving).
pub fn edit_codex(existing: &str, install: bool) -> Result<String, String> {
    use toml_edit::{Array, DocumentMut, Item, Value};
    let mut doc: DocumentMut = existing.parse().map_err(|e| format!("config.toml is not valid TOML: {e}"))?;
    // Is the current notify ours? (array equal to CODEX_NOTIFY)
    let is_ours = doc.get("notify").and_then(|i| i.as_array()).map_or(false, |a| {
        a.len() == 3 && a.iter().zip(CODEX_NOTIFY).all(|(v, s)| v.as_str() == Some(s))
    });
    if install {
        let mut arr = Array::new();
        for s in CODEX_NOTIFY { arr.push(s); }
        doc["notify"] = Item::Value(Value::Array(arr));
    } else if is_ours {
        doc.remove("notify");
    }
    Ok(doc.to_string())
}

#[cfg(test)]
mod codex_tests {
    use super::*;
    #[test]
    fn install_sets_notify() {
        let out = edit_codex("model = \"o3\"\n", true).unwrap();
        assert!(out.contains("notify = [\"zj-radar\", \"notify\", \"codex\"]"));
        assert!(out.contains("model = \"o3\"")); // preserved
    }
    #[test]
    fn idempotent() {
        let once = edit_codex("", true).unwrap();
        assert_eq!(once, edit_codex(&once, true).unwrap());
    }
    #[test]
    fn uninstall_removes_only_ours_not_user_notify() {
        let installed = edit_codex("", true).unwrap();
        assert!(!edit_codex(&installed, false).unwrap().contains("notify"));
        // a user's own notify is left intact
        let user = "notify = [\"my-notifier\"]\n";
        assert_eq!(edit_codex(user, false).unwrap(), user);
    }
    #[test]
    fn malformed_errors() { assert!(edit_codex("x = [", true).is_err()); }
}
```

- [ ] **Step 2: Aider editor + tests**

```rust
const AIDER_CMD: &str = "zj-radar notify aider";

/// Set/remove `notifications`/`notifications-command` in .aider.conf.yml.
pub fn edit_aider(existing: &str, install: bool) -> Result<String, String> {
    use serde_yaml::{Mapping, Value};
    let mut doc: Value = if existing.trim().is_empty() {
        Value::Mapping(Mapping::new())
    } else {
        serde_yaml::from_str(existing).map_err(|e| format!(".aider.conf.yml is not valid YAML: {e}"))?
    };
    let map = doc.as_mapping_mut().ok_or("aider config root is not a mapping")?;
    let cmd_key = Value::from("notifications-command");
    let notif_key = Value::from("notifications");
    let ours = map.get(&cmd_key).and_then(|v| v.as_str()) == Some(AIDER_CMD);
    if install {
        map.insert(notif_key, Value::from(true));
        map.insert(cmd_key, Value::from(AIDER_CMD));
    } else if ours {
        map.remove(&cmd_key);
        map.remove(&notif_key);
    }
    serde_yaml::to_string(&doc).map_err(|e| e.to_string())
}

#[cfg(test)]
mod aider_tests {
    use super::*;
    #[test]
    fn install_sets_keys() {
        let out = edit_aider("", true).unwrap();
        assert!(out.contains("notifications-command: zj-radar notify aider"));
        assert!(out.contains("notifications: true"));
    }
    #[test]
    fn idempotent() {
        let once = edit_aider("", true).unwrap();
        assert_eq!(once, edit_aider(&once, true).unwrap());
    }
    #[test]
    fn preserves_other_keys_and_uninstall_removes_ours() {
        let out = edit_aider("model: gpt-4o\n", true).unwrap();
        assert!(out.contains("model: gpt-4o"));
        let removed = edit_aider(&out, false).unwrap();
        assert!(!removed.contains("zj-radar notify aider"));
        assert!(removed.contains("model: gpt-4o"));
    }
}
```

- [ ] **Step 3: Wire into `edit()` + add the double-fire guard**

Replace the two `Err(...)` arms in `edit()`:
```rust
        Format::CodexToml => edit_codex(existing, install),
        Format::AiderYaml => edit_aider(existing, install),
```
In `run`, before processing the `claude` agent on install, warn if the plugin is present:
```rust
        if a.name == "claude" && !args.uninstall
            && home().join(".claude/plugins/zj-radar-claude").exists() {
            println!("  claude: NOTE the zj-radar-claude plugin is installed; using both double-fires. Use one.");
        }
```

- [ ] **Step 4: Build + test**

Run: `cargo test --features cli` → all pass (Claude + Codex + Aider editor tests + notify), 0 warnings.
Run: `nix develop -c cargo build --target wasm32-wasip1` → plugin still builds clean (no cli deps).

- [ ] **Step 5: Commit**

```bash
git add src/cli/setup.rs
git commit --no-gpg-sign -m "feat(cli): Codex + Aider setup editors + Claude/plugin double-fire guard"
```

---

### Task 5: consolidation + docs

**Files:**
- Modify: `plugins/zj-radar-claude/scripts/notify.sh`
- Create: `docs/cli.md` (or extend the plugin README)

**Interfaces:** none (docs + a shim).

- [ ] **Step 1: Make the Claude plugin notify.sh defer to the CLI when present**

Replace the body of `plugins/zj-radar-claude/scripts/notify.sh` with a thin shim that prefers the native CLI and falls back to the existing jq-based broadcast if `zj-radar` isn't on PATH:
```bash
#!/usr/bin/env bash
# zj-radar Claude plugin notifier. Prefer the native `zj-radar` CLI (no jq);
# fall back to the bundled jq broadcaster if the CLI isn't installed.
set -euo pipefail
if command -v zj-radar >/dev/null 2>&1; then
    exec zj-radar notify claude
fi
# --- fallback: existing jq-based broadcast (unchanged) ---
# [keep the current script body here]
```
(Keep the prior jq body below the `exec` fallback so the plugin still works without the CLI. The hooks.json keeps passing `<status>` arg — note the CLI ignores extra args and reads `hook_event_name` from stdin, so both paths work; if the implementer prefers, simplify hooks.json later.)

- [ ] **Step 2: Write `docs/cli.md`**

Document: install (`cargo build --release --features cli` → put `zj-radar` on PATH; Mark: via home-manager), `zj-radar notify` (the agent-hook entrypoint), `zj-radar setup [--dry-run] [--uninstall] [agents...]`, the supported agents (claude/codex/aider) + which config each touches, the Claude plugin-vs-setup mutual-exclusion note, and that `notify` no-ops outside Zellij.

- [ ] **Step 3: Verify the shim still parses**

Run: `bash -n plugins/zj-radar-claude/scripts/notify.sh` → OK.

- [ ] **Step 4: Commit**

```bash
git add plugins/zj-radar-claude/scripts/notify.sh docs/cli.md
git commit --no-gpg-sign -m "feat(cli): plugin notify shim prefers zj-radar CLI; CLI docs"
```

---

## Self-Review

**Spec coverage:** crate/feature/bin + `to_wire`/`as_wire` → Task 1. `notify` agent parsing + explicit + pipeline → Task 2. `setup` framework/detection/dry-run/uninstall/report + Claude editor → Task 3. Codex + Aider editors + double-fire guard → Task 4. Consolidation (plugin shim) + docs → Task 5. Shared wire contract (`to_wire`↔`parse`) → Task 1 Step 2 round-trip test. Pure `String->String` editors + `derive` with unit tests → Tasks 2/3/4. Wasm-build-stays-lean assertion → Tasks 1/4 build steps. Out-of-scope (init, Gemini, distribution) → not built.

**Placeholder scan:** No TBD/TODO. The Task 3 `edit()` Codex/Aider arms intentionally `Err("lands in Task 4")` as a compiling interim, replaced in Task 4 Step 3 — explicit, not a placeholder. Task 5 Step 1 says "keep the current script body" referencing the real existing file (implementer has it).

**Type consistency:** `derive(agent,&str,&[String]) -> Option<Update>` and `Update{status,msg}` consistent (Task 2). `edit_claude/edit_codex/edit_aider(&str, bool) -> Result<String,String>` consistent across Tasks 3/4 and `edit()`. `Agent`/`Format`/`agents()` consistent (Tasks 3/4). `payload::to_wire(&StatusPayload)`/`Status::as_wire` consistent (Tasks 1/2). `StatusPayload` field set matches the existing module.
