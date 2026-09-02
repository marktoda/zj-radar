# Producers: sending agent status to the sidebar

The sidebar only displays. A **producer** is whatever broadcasts agent status
to it. zj-radar ships producers for Claude Code, Codex, and Opencode, a
`notify generic` command for any script, and documents the wire format so you
can write your own.

Install the [sidebar](install.md) first. Every producer below needs the
`zj-radar` CLI on `PATH` except the Claude plugin, which falls back to a
bundled `bash`+`jq` script when the binary is absent.

## Claude Code

The Claude Code plugin registers the status hooks itself: no `settings.json`
editing, clean uninstall.

```sh
zj-radar setup claude
```

This drives Claude Code's own plugin CLI (marketplace add + install). The same
thing from inside Claude Code:

```text
/plugin marketplace add marktoda/zj-radar
/plugin install zj-radar-claude@zj-radar
```

The plugin is a no-op outside Zellij, so it is safe to leave enabled. Hook
details and the event-to-status table are in
[`plugins/zj-radar-claude/README.md`](../plugins/zj-radar-claude/README.md).

## Codex

```sh
zj-radar setup codex
```

This adds command hooks to `hooks.json` under `$CODEX_HOME` (default
`~/.codex`) that call `zj-radar notify codex`. Hooks are additive, so an
existing Codex `notify` program is preserved. After installing or changing the
hooks, run `/hooks` inside Codex once to review and trust them.

| Codex event | Status |
|---|---|
| `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `SubagentStart`, `SubagentStop` | `running` |
| `PermissionRequest` | `pending` |
| `Stop` | `done` |

Each entry carries `timeout: 10` (the send deadline plus headroom, see
[Bound your sends](#bound-your-sends)) and the `ZJ_RADAR_CODEX_HOOK=v1` marker
that idempotency and `--uninstall` key on.

`zj-radar setup codex --legacy-notify` uses Codex's older single-slot `notify`
program instead of hooks. It only reports `done`, and it refuses to replace a
foreign notifier unless you also pass `--force`.

## Opencode

```sh
zj-radar setup opencode
```

This writes a bridge plugin to `$XDG_CONFIG_HOME/opencode/plugins/zj-radar.js`
(default `~/.config/opencode/plugins/`). Opencode auto-loads it; no
`opencode.json` edit. The bridge forwards each hook and bus event to
`zj-radar notify opencode`, which does the classification.

| Opencode event | Status |
|---|---|
| `chat.message`, `tool.execute.before` / `after` | `running` (with task and tool activity) |
| `permission.asked`, `question.asked` | `pending`; their `replied` / `rejected` edges return to `running` |
| `session.idle` | `done`, or `pending` when the last message ends in a question |
| `session.error` | `error` (your own Esc interrupt is not an error) |
| `session.created`, `session.deleted` | `idle` |

Things to know:

- **Restart opencode after installing.** Plugins load once at startup.
- **The binary is required.** Unlike Claude, there is no script fallback.
- **An unwired opencode pane shows nothing.** Because opencode is an
  instrumented agent, the sidebar does not command-track its panes; without
  the bridge (not installed, or opencode started with `--pure`) the pane has
  no row at all. `setup --check` reports the missing bridge when `opencode` is
  on `PATH`. Upgrading from a release before opencode support has the same
  effect: run `setup opencode` and restart opencode.
- **Status lands where the server runs.** The default TUI hosts its server
  in-process. With `opencode serve` plus `opencode attach`, status is
  attributed to the server's pane, or dropped when the server is not under
  Zellij.
- **Slash commands label the task with their expansion.** Opencode expands
  `/command` templates before the bridge sees the prompt.
- Minimum verified opencode version: 1.18.x. The bridge is vendored per
  release; re-running `setup opencode` updates it in place.

Bridge internals (event coalescing, subagent filtering, the marker) are in
[`design.md`](design.md#7-agent-adapters).

## Any script: `zj-radar notify generic`

Deploy scripts, cron jobs, and homegrown loops can put a row on the radar
without touching the wire format:

```sh
zj-radar notify generic --status running --msg "deploying site" --task "nightly deploy" --source deploy
# … do the work …
zj-radar notify generic --status done --msg "deploy finished" --source deploy
```

- `--status` (required): `running` | `pending` | `done` | `error` | `idle`. An
  unknown token prints a hint and sends nothing.
- `--msg`: the activity line. `running` without one shows `working`.
- `--task`: the sticky task label. Empty keeps the stored one.
- `--source`: the kind mark. `test` ⚗ · `build` ⚙ · `deploy` ⇡ · `server` ❯ ·
  `command` `$`, or an agent token (`claude` ✳ · `codex` ❉ · `opencode` ✺ ·
  `gemini` ✦). Anything else, including the default `generic`, renders `⦿`.
- Repo and branch come from `git` in the calling directory; the pane id from
  `$ZELLIJ_PANE_ID`. Outside Zellij it is a silent no-op. `--dry-run` prints
  the payload instead of sending.

The lifecycle is the same as for agents: the latest broadcast wins, a finished
status clears when the pane returns to its shell prompt, and a `running` row
with no follow-up is cleared about 15 seconds after the pane reaches the
prompt. Send `done` or `error` when your script finishes rather than relying on
that. A pane whose root process exits clears immediately, which matters for
`zellij run` script panes.

## Writing your own producer

The plugin's interface is one versioned pipe payload. Broadcast it by name
(never with `--plugin`) as a `zj_radar.status.v1` message:

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

Field rules:

- `status`: `running` | `pending` | `done` | `error` | `idle`. **Unknown or
  empty folds to `idle`**, which clears the row and its task. Validate before
  sending; a typo erases the row you meant to update.
- `pane.id`: `$ZELLIJ_PANE_ID` with any `terminal_` prefix stripped.
  `pane.type` must be `"terminal"`.
- `source`: lowercase, case-sensitive. `"claude"` is the Claude agent;
  `"Claude"` is the neutral kind.
- `task` (optional): sticky label, valid with any status. Empty or absent
  keeps the stored label; the plugin clears it on `idle` and on return to
  shell.
- `ack` (optional, default `false`): "the user has already seen this". State
  converges as usual but no desktop notification fires. The rail's `✓` gesture
  sets it; producers reporting real events should leave it out.
- Unknown fields are ignored, so extras are safe.

The plugin defends itself at parse time: it strips ANSI, control, and bidi
characters, folds newlines to spaces, truncates `repo`/`branch` to 40 chars,
`msg`/`task` to 60, `source` to 16, drops payloads over 64 KB, and evicts the
oldest pane past 256 distinct pane ids. Ordering is latest-wins; the pipe
delivers in order and there is no sequence number.

Writing it in Rust? Depend on
[`zj-radar-core`](https://crates.io/crates/zj-radar-core)
([docs.rs](https://docs.rs/zj-radar-core)), the crate both halves of zj-radar
use. Build a `StatusPayload` and serialize it with `to_wire`; it round-trips
against this schema, so the payload cannot drift.

Smoke test from any shell inside the session:

```sh
zellij pipe --name zj_radar.status.v1 -- \
  '{"v":1,"source":"test","pane":{"type":"terminal","id":'"${ZELLIJ_PANE_ID#terminal_}"'},"status":"running","repo":"demo","branch":"main","msg":"hello"}'
```

### Bound your sends

`zellij pipe` is not fire-and-forget. Zellij holds the client process until
every loaded plugin instance has consumed the message, and an instance parked
at its permission prompt never does. A producer that fires per tool call then
leaks one blocked process and two server file descriptors per event until the
Zellij server hits `EMFILE` and the whole session crashes. This has happened.

Three rules keep you safe:

1. **Put a deadline on every send.** The bundled producers use 5 seconds for
   status edges (`done`/`pending`/`error`/`idle`) and 2 seconds for `running`
   heartbeats; a dropped heartbeat is replaced by the next event, a dropped
   edge loses real state. `ZJ_RADAR_PIPE_TIMEOUT` (whole seconds, max 3600)
   overrides both. Killing the client past its deadline loses nothing; the
   message is already queued server-side.
2. **Make the deadline survive your own death.** Hook runners kill their
   hooks. A producer killed mid-send never runs its own kill, and the blocked
   `zellij pipe` client is orphaned forever. Run the pipe under a shell
   alongside a detached `sleep <deadline>; kill` watchdog, inside the subtree
   you spawn. `zj-radar-core` exports this as `self_limiting_pipe_argv`, with
   `DEFAULT_PIPE_TIMEOUT_SECS` and `RUNNING_PIPE_TIMEOUT_SECS`.
3. **Give the hook runner headroom.** If your runner has its own per-hook
   timeout, set it at least 2 seconds above your send deadline so the
   graceful path finishes before the runner kills it. The bundled Claude hooks
   keep `timeout >= deadline + 2`; the Codex entries use deadline + 5.
