# Producers — sending agent status to the sidebar

The sidebar is just a display. A **producer** is whatever broadcasts agent
status to it. zj-radar ships producers for Claude Code and Codex, and the wire
format is a documented pipe payload so you can write your own.

Install the [sidebar](install.md) first, then add a producer below.

## Claude Code

Installing this plugin auto-registers the status hooks — **no `settings.json`
editing**, clean uninstall.

```sh
/plugin marketplace add marktoda/zj-radar
/plugin install zj-radar-claude@zj-radar
```

Requires `jq` and `git` on `PATH` (used to parse the hook payload and derive
repo/branch). See [`plugins/zj-radar-claude/README.md`](../plugins/zj-radar-claude/README.md)
for details. It's a no-op outside Zellij, so it's safe to leave enabled
everywhere.

## Codex and the native CLI

A native binary that drops the `jq`/`bash` dependency and wires non-plugin agents.

> **Before the first tagged release**, the prebuilt tarballs and the
> `#zj-radar-cli` Nix output aren't published yet — use the `cargo install --git`
> form below (or build from source). The release workflow produces all three on
> the first `v*` tag.

```sh
# Release tarballs (published on tagged releases):
#   zj-radar-linux-x86_64.tar.gz
#   zj-radar-macos-aarch64.tar.gz
# Nix:
nix build github:marktoda/zj-radar#zj-radar-cli   # -> result/bin/zj-radar
# Cargo:
cargo install --git https://github.com/marktoda/zj-radar zj-radar
```

- **`zj-radar notify <claude|codex>`** — broadcasts agent status. The Claude
  plugin's hook script automatically prefers it when it's on `PATH` (jq-free);
  otherwise the plugin falls back to its bundled `bash`+`jq` script.
- **`zj-radar setup [codex]`** — idempotently wires Codex's
  `~/.codex/hooks.json` to call `zj-radar notify codex`. This preserves any
  existing Codex `notify` program (e.g. a Computer Use notifier), because hooks
  are additive. Use `--dry-run` to preview, `--uninstall` to remove only
  zj-radar's hooks, and `--check` to diagnose the current setup. After installing
  or changing hooks, run `/hooks` inside Codex once to review and trust the
  command hook. (Claude needs no `setup` — use the plugin above.)
- **`zj-radar setup codex --legacy-notify`** — opt-in fallback for older Codex
  setups that only support the single `notify` program. It refuses to replace a
  foreign notifier unless `--force` is also passed.
- **`zj-radar setup zellij --wasm <path>`** — copies the sidebar wasm to
  `~/.config/zellij/plugins/zj_radar.wasm`, manages the `radar` alias in
  `config.kdl`, and prints the layout snippet. It leaves layouts user-owned.

Codex hooks report turn start, tool use, permission requests, subagents, and
turn stop. zj-radar maps those to `running`, `pending`, and `done`.

## Writing your own producer

The plugin's only real interface is the versioned pipe payload. Broadcast (by
name, never `--plugin`) a `zj_radar.status.v1` message:

```json
{ "v": 1,
  "source": "claude",
  "pane": { "type": "terminal", "id": 12 },
  "status": "running",
  "repo": "pinky",
  "branch": "fix/x",
  "msg": "running tests…",
  "on_focus": "idle",
  "seq": 42 }
```

- `status`: `running` → working · `pending` → needs-you · `done` · `error` ·
  `idle`/unknown → plain.
- `pane.id`: strip any `terminal_` prefix from `$ZELLIJ_PANE_ID`.
- `on_focus` (optional): the status to apply when you next focus that exact pane
  (lets `done` persist on other tabs, then auto-clear).
- `seq` (optional): monotonic per-pane counter; a `seq` ≤ the stored one is
  dropped (hook-race guard).

The plugin defends itself: it ignores oversized payloads, strips ANSI/control
chars, folds newlines to spaces, and truncates. Adapters can stay simple.

Quick smoke test (a "fake agent" — broadcast straight from your shell):

```sh
zellij pipe --name zj_radar.status.v1 -- \
  '{"v":1,"source":"test","pane":{"type":"terminal","id":12},"status":"running","repo":"demo","branch":"main","msg":"hello"}'
```
