# zj-radar CLI (notify + setup) — revised design

**Status:** implemented, with Codex setup superseded by the 2026-06-28
hook-first integration.
**Date:** 2026-06-27
**Author:** Mark Toda (with Claude)
**History:** this was the authoritative CLI design for the original notify-based
Codex path. Codex now uses `~/.codex/hooks.json` by default; legacy
`config.toml` `notify` setup remains available behind `--legacy-notify`.

## Goal

A native `zj-radar` binary that (1) replaces the Claude producer's `jq`/`bash`
dependency with a native, jq-free `notify`, and (2) idempotently wires agents
that have no plugin system. Two verbs: `notify` and `setup`. Agents in v1:
**Claude + Codex** (the two installed on the target machine; Aider/Gemini/etc.
are one-row additions later).

Non-goals (explicit): `zj-radar init` (sidebar wasm/layout/permission install),
cross-platform prebuilt release binaries, Aider/Gemini/opencode support, a Claude
`settings.json` editor, and any wrapper/multiplexer that chains a second Codex
notifier.

## Grounding facts (from the target machine)

- **Claude** and **Codex** are installed and on PATH. Aider/Gemini/opencode/amp
  are absent.
- **Codex's legacy `notify` is a single program**, and the slot is already occupied by
  the "Codex Computer Use" notifier (`~/.codex/computer-use/…/SkyComputerUseClient`,
  arg `turn-ended`). The hook-first setup avoids this slot entirely.
- Codex lifecycle hooks can report turn start, tool use, permission requests,
  subagent activity, and turn stop. The legacy `notify` fallback still reports
  only turn completion.

## Architecture — one crate, `cli` feature, two bins

```toml
[features]
cli = ["dep:clap", "dep:toml_edit"]   # NOTE: no serde_yaml (Aider dropped)

[[bin]]
name = "zj_radar"             # wasm plugin command (unchanged)
path = "src/main.rs"

[[bin]]
name = "zj-radar"             # native CLI (hyphen)
path = "src/bin/cli.rs"
required-features = ["cli"]

[dependencies]
# existing: zellij-tile, serde, serde_json, unicode-width
clap = { version = "4", features = ["derive"], optional = true }
toml_edit = { version = "0.22", optional = true }
```

- `cargo build --target wasm32-wasip1` → only the plugin; `cli` module is
  `#[cfg(feature = "cli")]`, so `clap`/`toml_edit` are absent from the wasm build.
- `cargo build --features cli` (host) → the `zj-radar` binary.
- `cargo test` runs without `cli`; `cargo test --features cli` covers the CLI's
  pure logic.

**Shared wire contract:** add `Status::as_wire(self) -> &'static str` and
`payload::to_wire(pane_id, status, repo, branch, msg, on_focus) -> String` next to
the existing `Status::from_wire` / `payload::parse`. A round-trip unit test asserts
`parse(to_wire(x)) == x` for the relevant fields, so the CLI builds the exact
`zj_radar.status.v1` JSON the plugin parses — one schema, cannot drift.

### Module layout
- `src/cli/mod.rs` — clap `Cli`/`Command` defs + `run()` dispatch (`#[cfg(feature="cli")]`).
- `src/cli/notify.rs` — pure `derive(...) -> Option<Update>` + thin broadcast/git wrappers.
- `src/cli/setup.rs` — Codex detection + the conflict-aware `toml_edit` editor (pure `String -> String`) + fs wrapper.
- `src/bin/cli.rs` — `fn main() { zj_radar::cli::run() }`.
- `payload::to_wire` / `status::as_wire` — in the existing pure modules (shared, no feature gate).

## `notify`

```
zj-radar notify <claude|codex> [--status S] [--dry-run]
```

Pure core: `derive(agent, status_arg, stdin_json, argv_json) -> Option<Update>`
where `Update { status, msg }`. Returns `None` for no-op cases.

| agent | input | status |
|---|---|---|
| claude | `--status` from the hook (matcher-driven `hooks.json`), stdin JSON for `cwd` + `message`/`last_assistant_message` | running / pending / done as passed; **`pending` backstop**: drop `pending` when `message` is empty or a generic phrase ("Claude needs attention", "Claude Code needs your attention") — preserves commits `d1dbe1b`/`e86b43f` |
| codex | trailing-argv JSON: `type`, `last-assistant-message`, `cwd` | `type == "agent-turn-complete"` → `done`; else `None` (no-op). Verified: `agent-turn-complete` is the event Codex's `notify` (legacy) program emits, and the payload carries `last-assistant-message`/`cwd`. (`approval-requested` belongs to Codex's separate newer hooks system, not the `notify` program — out of scope.) |

Bare `zj-radar notify claude` (no `--status`) may self-derive status from
`hook_event_name` in stdin as a convenience for hand-wiring; the plugin path uses
`--status` for matcher precision.

Common pipeline (after deriving status+msg):
- pane: `$ZELLIJ_PANE_ID` (strip a `terminal_` prefix; must be numeric, else no-op).
- repo/branch: native `git -C <cwd> rev-parse --show-toplevel` (basename) and
  `git -C <cwd> branch --show-current` via `std::process::Command`; empty on
  failure. `cwd` = the payload's cwd if present, else `$PWD`.
- build via `payload::to_wire(...)` and broadcast `zellij pipe --name
  zj_radar.status.v1 -- <json>`.
- `done` carries `on_focus = "idle"`.
- **No-op outside Zellij** (`$ZELLIJ` unset) or non-numeric pane; broadcast is
  best-effort and quick; **all errors swallowed** (never break the calling hook).
- `--dry-run` prints the payload to stderr and does not broadcast (replaces the
  `ZJ_RADAR_DEBUG` env in the bash script).

## `setup`

```
zj-radar setup [codex]                  # default: all detected (= codex in v1)
zj-radar setup --uninstall [codex]
  flags: --dry-run   --yes   --force
```

**Codex only.** Claude is plugin-only (no `settings.json` editor; no double-fire
guard).

- **Detection:** `~/.codex/config.toml` (or `$CODEX_HOME`) exists AND `codex`
  resolves on PATH. Report `installed`/`skipped (binary not found)`/`skipped (no
  config)`.
- **Conflict-aware editor** (`fn edit_codex(existing: &str, install: bool,
  force: bool) -> Result<Outcome>`, pure `String -> String`):
  - The marker is the `notify = ["zj-radar", "notify", "codex"]` array.
  - **Install:** if `notify` is absent or already ours → set it. If `notify`
    belongs to another program → **refuse** (return a conflict outcome) unless
    `force`, which overwrites. Never silently clobber.
  - **Uninstall:** remove `notify` only if it's ours; leave a foreign `notify`
    untouched.
  - Format-preserving via `toml_edit`. Refuse to write if the file exists but
    doesn't parse as TOML.
- **fs wrapper:** back up before writing, write atomically. `--dry-run` prints a
  diff and writes nothing; `--yes` skips the per-file confirm; `--force`
  overrides the conflict refusal.

## Claude plugin shim (graceful)

`plugins/zj-radar-claude/scripts/notify.sh` becomes:

```sh
if command -v zj-radar >/dev/null 2>&1; then
  exec zj-radar notify claude --status "$1"   # native, jq-free
fi
# else: existing bash+jq logic (unchanged) — keeps the marketplace plugin self-contained
```

The plugin stays self-contained for marketplace users (still needs `jq` only on
the fallback path); CLI users get the jq-free path automatically. `hooks.json` is
unchanged (its matchers still decide the per-event status).

## Distribution

- **Flake `packages.zj-radar-cli`** (crane, host target, `--features cli`,
  installs `$out/bin/zj-radar`) alongside the existing `packages.zj-radar` wasm.
  Home-manager consumes it; having it on PATH makes the shim + `setup` work.
- Documented `cargo install --git https://github.com/mark-toda/zj-radar
  --features cli` for non-Nix users.
- Deferred: cross-platform prebuilt CLI binaries on GitHub Releases (native
  per-OS/arch cross-compile), and `zj-radar init`.

## Testing

Pure, fs/process-free unit tests (the point of the `String -> String` + `derive()`
seams):
- **notify:** `derive()` for each Claude status incl. the `pending` backstop
  (generic phrase → `None`/drop pending); Codex `agent-turn-complete` → `done`,
  other type → `None`; missing/non-numeric pane → no-op decision. `to_wire` ↔
  `parse` round-trip (contract test).
- **setup:** `edit_codex` — fresh file (no `notify`) installs; idempotent re-run
  (no change); preserves unrelated user TOML (other keys/tables); foreign `notify`
  → refuse (and `force` → overwrite); uninstall removes only ours; malformed TOML
  → refuse.

The thin layers (read/write file, run `git`, run `zellij pipe`, PATH lookup) are
exercised manually, not unit-tested.

## Verification gates

- `cargo test` (no feature) and `cargo test --features cli` both green, 0 warnings.
- `cargo build --target wasm32-wasip1` (or `nix build .#zj-radar`) still produces
  the lean wasm with **no `clap`/`toml_edit`** linked (the wasm must not regress;
  CI's `nix flake check` builds it).
- `nix build .#zj-radar-cli` produces a runnable `zj-radar`.
