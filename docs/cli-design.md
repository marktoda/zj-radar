# zj-radar CLI (Phase 2) — design

**Status:** design / approved for spec-review
**Date:** 2026-06-27

## Goal

A native `zj-radar` binary that makes the agent notifiers **install-once, no
hand-edited configs, cleanly removable** — replacing the per-agent shell shims
and dropping the `jq`/bash runtime dependency. Two verbs:

- `zj-radar notify <agent>` — the universal notifier agents' hooks call.
- `zj-radar setup` — idempotently wires installed agents' configs to call it.

(`init` — wasm/layout/permission install — is Phase 3, out of scope here.)

## Architecture — one crate, two bins, a `cli` feature

Keep the single crate. The CLI's native-only deps live behind a `cli` feature so
the wasm plugin build never pulls them:

```toml
[features]
cli = ["dep:clap", "dep:toml_edit", "dep:serde_yaml"]

[[bin]]
name = "zj_radar"            # wasm plugin command (unchanged)
path = "src/main.rs"

[[bin]]
name = "zj-radar"            # native CLI
path = "src/bin/cli.rs"
required-features = ["cli"]

[dependencies]
# existing: zellij-tile, serde, serde_json
clap = { version = "4", features = ["derive"], optional = true }
toml_edit = { version = "0.22", optional = true }
serde_yaml = { version = "0.9", optional = true }
```

- `cargo build --target wasm32-wasip1` → only the plugin (lean, dep-free). The
  `cli` module is `#[cfg(feature = "cli")]`, so it's absent from the wasm build.
- `cargo build --features cli` (host) → the `zj-radar` binary.
- Wasm artifact name unchanged (`zj_radar.wasm`); the user-facing command is
  `zj-radar` (hyphen). `cargo test` runs without `cli`; `cargo test --features
  cli` covers the CLI's pure logic.

**Shared wire contract:** add `payload::to_wire(pane_id, status, repo, branch,
msg, on_focus) -> String` next to the existing `payload::parse`, so the CLI
*builds* the `zj_radar.status.v1` JSON from the **same `payload`/`status` types
the plugin parses**. One schema, cannot drift. (`status::Status` already exists;
reuse it.)

### Module layout
- `src/cli/mod.rs` — clap `Cli`/`Command` definitions + `run()` dispatch (`#[cfg(feature="cli")]`).
- `src/cli/notify.rs` — agent payload parsing + status derivation (pure) + broadcast (thin).
- `src/cli/setup.rs` — agent table, detection, the three config editors (pure `String->String`) + fs wrappers.
- `src/bin/cli.rs` — `fn main() { zj_radar::cli::run() }`.
- `payload::to_wire` — in the existing pure `payload` module (shared, no feature gate).

## `notify`

```
zj-radar notify <agent>                              # claude | codex | aider
zj-radar notify --status <s> [--message M] [--repo R] [--branch B] [--source N]
```

Per-agent parsing (the binary owns the quirks; pure function
`derive(agent, stdin, argv) -> Option<Update>` where `Update { status, msg }`):

| agent | input | status |
|---|---|---|
| claude | stdin JSON: `hook_event_name`, `message`/`last_assistant_message`, `cwd` | UserPromptSubmit / PreToolUse / PostToolUse / SubagentStop → `running`; Notification → `pending`; Stop → `done` (on_focus = idle) |
| codex | JSON appended as the trailing argv element: `type`, `last-assistant-message`, `cwd` | `type == "agent-turn-complete"` → `done`; anything else → `None` (no-op, exit 0) |
| aider | none | `done` |

Common pipeline (agent-agnostic, after deriving status+msg):
- pane: `$ZELLIJ_PANE_ID` (strip a `terminal_` prefix; must be numeric, else no-op).
- repo/branch: run `git -C <cwd> rev-parse --show-toplevel` (basename) and `git -C
  <cwd> branch --show-current` natively (`std::process::Command`); empty on
  failure. `cwd` = the agent payload's cwd if present, else `$PWD`.
- Build the payload via `payload::to_wire(...)` and broadcast: `zellij pipe
  --name zj_radar.status.v1 -- <json>`.
- **No-op outside Zellij** (`$ZELLIJ` unset) or on a non-numeric pane id.
- `done` carries `on_focus = "idle"`.

`--status` form bypasses agent parsing (explicit fields; for testing/custom
integrations); `--source` defaults to the agent name (recorded in the payload's
`source`).

The broadcast is best-effort and quick; failures are swallowed (never break the
calling hook). `--source`/`source` is purely diagnostic.

## `setup`

```
zj-radar setup [agents...]            # default: all detected
zj-radar setup --uninstall [agents...]
  flags: --dry-run   --yes
```

**Declarative agent table** — one entry per agent: `name`, `binary` (PATH
detection), config path (+ env override, e.g. `CODEX_HOME`), `Format`, and the
`zj-radar notify <agent>` invocation it installs. v1 entries: `claude`, `codex`,
`aider`. Adding Gemini/etc. later = one row.

**Detection (both gates, install only):** config dir exists AND `binary` resolves
on `PATH`. On `--uninstall`, bypass gates (clean stale config even if the agent
is gone). Report per agent: `installed at <path>` / `skipped (binary not found)`
/ `skipped (no config dir)`, plus a `N installed, M skipped` summary.

**Three format editors — each a pure `fn(existing: &str, …) -> String`** (the
file I/O is a separate thin wrapper), all **strip-own-then-re-add** (idempotent;
re-running is a no-op), **marker** = the `zj-radar notify <agent>` command
substring, and the I/O wrapper **backs up before writing** + writes atomically:

- **Claude** `~/.claude/settings.json` (JSON via `serde_json`): merge our 6 hook
  entries (`UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `Notification`,
  `SubagentStop`, `Stop`), each `{type:"command", command:"zj-radar notify
  claude"}`. Remove only entries whose command contains the marker; preserve all
  the user's other hooks/settings. Refuse to write if the file exists but isn't
  valid JSON.
- **Codex** `~/.codex/config.toml` (via `toml_edit`, format-preserving): set
  top-level `notify = ["zj-radar", "notify", "codex"]`. (Codex appends its JSON
  as an argv element, which `notify codex` reads.)
- **Aider** `~/.aider.conf.yml` (via `serde_yaml`): set `notifications: true` and
  `notifications-command: "zj-radar notify aider"`.

`--dry-run` prints a unified diff per file and writes nothing. `--yes` skips the
per-file confirm prompt. `--uninstall` removes only marker-tagged entries,
collapses emptied containers, and deletes a file only if it becomes empty/`{}`.

**Double-fire guard (Claude):** Claude can be wired via *either* `setup`
(settings.json) or the `zj-radar-claude` plugin. When installing the Claude
entry, if the plugin appears present (a `zj-radar-claude` dir under
`~/.claude/plugins`), print a warning to use one path, not both. `--uninstall`
cleans the settings.json path (the plugin is removed via `claude plugin
uninstall`).

## Consolidation (incremental, not forced)

- The `zj-radar-claude` plugin's `scripts/notify.sh` becomes a thin
  `exec zj-radar notify claude` (drops its `jq` dependency) — once the CLI is on
  PATH. Documented; not auto-applied.
- The dotfiles `claude-zellij-notify.sh` / `codex-zellij-notify.sh` are
  superseded by `setup`; kept until the user migrates.

## Testing

Pure, fs/process-free unit tests (the whole point of the `String->String` +
`derive()` seam):
- `notify`: `derive(agent, stdin, argv)` → correct `(status, msg)` for each agent
  + each Claude `hook_event_name`; Codex non-`agent-turn-complete` → None; missing
  pane → no-op decision. `payload::to_wire` round-trips through `payload::parse`
  to the same fields (contract test).
- `setup`: each editor — fresh file, idempotent re-run (no change), preserves
  unrelated user content, uninstall removes exactly our entries, malformed-input
  refusal. No real filesystem.

The thin layers (read/write file, run `git`, run `zellij pipe`, PATH lookup) are
not unit-tested; they're exercised manually.

## Dependencies

`clap` (derive), `toml_edit`, `serde_yaml` — all behind the `cli` feature, so the
wasm plugin build is unaffected. `serde_json` is already a dependency.

Note: `serde_yaml` is archived/maintenance-only. Acceptable for v1 (stable,
de-facto standard); if a maintained dep is required before release, swap to
`serde_yml` (active fork) — the Aider editor's `String->String` seam isolates the
choice to one module. (Deferring Aider entirely is the other way to drop it.)

## Out of scope

- `zj-radar init` (wasm install to stable path + layout snippet + permission
  pre-seed) — Phase 3.
- Gemini / opencode / other agents — table-ready, added as needed.
- Binary distribution channels (cargo-install / Nix / Homebrew) — follow-on; for
  now `cargo build --features cli` + put it on PATH (Mark: via home-manager).
