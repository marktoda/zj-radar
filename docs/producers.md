# Producers — sending agent status to the sidebar

The sidebar is just a display. A **producer** is whatever broadcasts agent
status to it. zj-radar ships producers for Claude Code and Codex, and the wire
format is a documented pipe payload so you can write your own.

Install the [sidebar](install.md) first, then add a producer below.

## Claude Code

Installing this plugin auto-registers the status hooks — **no `settings.json`
editing**, clean uninstall. Run both **inside Claude Code** (these are `/plugin`
slash commands, not shell):

```text
/plugin marketplace add marktoda/zj-radar
/plugin install zj-radar-claude@zj-radar
```

The first command registers this repo as a plugin marketplace named `zj-radar`;
the second installs the `zj-radar-claude` plugin *from* it — that's what the
`zj-radar-claude@zj-radar` (`plugin@marketplace`) syntax means.

Requires `jq` and `git` on `PATH` (used to parse the hook payload and derive
repo/branch). See [`plugins/zj-radar-claude/README.md`](../plugins/zj-radar-claude/README.md)
for details. It's a no-op outside Zellij, so it's safe to leave enabled
everywhere.

## Codex and the native CLI

A native binary that drops the `jq`/`bash` dependency and wires non-plugin agents.

```sh
# Release tarballs (published on tagged releases; named by Rust target triple):
#   zj-radar-x86_64-unknown-linux-musl.tar.gz
#   zj-radar-aarch64-unknown-linux-musl.tar.gz
#   zj-radar-aarch64-apple-darwin.tar.gz
# Nix:
nix build github:marktoda/zj-radar#zj-radar-cli   # -> result/bin/zj-radar
# Cargo (crates.io; add `--git https://github.com/marktoda/zj-radar` for HEAD):
cargo install zj-radar
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

## Any script: `zj-radar notify generic`

Anything that isn't an instrumented agent — deploy scripts, cron jobs,
homegrown loops — can put a row on the radar without touching the wire format:

```sh
zj-radar notify generic --status running --msg "deploying site" --task "nightly deploy" --source deploy
# … do the work …
zj-radar notify generic --status done --msg "deploy finished" --source deploy
```

- `--status` (required): `running` | `pending` | `done` | `error` | `idle`. An
  unknown token prints a hint and sends nothing — it never lenient-parses to
  `idle` and erases your row.
- `--msg`: the activity line. `running` with no msg gets a `working` baseline;
  `idle` always broadcasts blank.
- `--task`: the sticky task label (empty keeps the stored one).
- `--source`: picks the kind mark — `test` ⚗ · `build` ⚙ · `deploy` ⇡ ·
  `server` ❯ · `command` $ — anything else (including the default `generic`)
  renders the neutral `⦿`.
- Repo/branch come from `git` in the calling directory; the pane id from
  `$ZELLIJ_PANE_ID`. Outside Zellij it's a silent no-op (safe under `set -e`).
  `--dry-run` prints the payload instead of broadcasting.

The same lifecycle rules as agents apply: latest broadcast wins, a finished
status clears when the pane returns to its shell prompt, and a `running` row
whose pane sits at the prompt with no follow-up broadcast is cleared by the
stale-Running watchdog after ~15s — so send `done`/`error` when your script
finishes rather than leaning on the watchdog.

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
  "task": "fix the flaky auth test" }
```

- `status`: `running` → working · `pending` → needs-you · `done` · `error` ·
  `idle` → plain. An **unknown or empty `status` folds to `idle`**, which
  *clears* the row and resets its sticky task — a typo'd status silently erases
  the row you meant to update, so validate before broadcasting.
- `pane.id`: strip any `terminal_` prefix from `$ZELLIJ_PANE_ID`.
- `task` (optional, sent only on `UserPromptSubmit`): sticky task label —
  empty/absent leaves the stored label unchanged, non-empty replaces it; the
  plugin clears it on idle and on return-to-shell.
- Unknown fields are ignored, so it's safe to send extras. (A former `on_focus`
  clear-on-focus hint is no longer used — the plugin clears a finished status when
  the pane returns to its shell prompt instead — but sending it does no harm.)

The plugin applies the latest broadcast per pane (the pipe delivers in order, so
there is no sequence number). It also defends itself: it strips ANSI/control
chars and Unicode bidi-control characters, folds newlines to spaces, and
silently ignores unknown fields, so extra keys never break a producer. The
limits, so you don't have to pre-truncate: `repo`/`branch` are cut to 40 chars,
`msg`/`task` to 60, `source` to 16 — and a payload over **64 KB** is dropped
whole. `pane.type` must be `"terminal"`; any other pane type is rejected.

Quick smoke test (a "fake agent" — broadcast straight from your shell):

```sh
zellij pipe --name zj_radar.status.v1 -- \
  '{"v":1,"source":"test","pane":{"type":"terminal","id":12},"status":"running","repo":"demo","branch":"main","msg":"hello"}'
```
