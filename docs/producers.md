# Producers — sending agent status to the sidebar

The sidebar is just a display. A **producer** is whatever broadcasts agent
status to it. zj-radar ships producers for Claude Code, Codex, and Opencode,
and the wire format is a documented pipe payload so you can write your own.

Install the [sidebar](install.md) first, then add a producer below.

## Claude Code

Installing this plugin auto-registers the status hooks — **no `settings.json`
editing**, clean uninstall. One shell command drives Claude Code's own plugin
CLI (marketplace add + install):

```sh
zj-radar setup claude
```

Or do the same from inside Claude Code (these are `/plugin` slash commands,
not shell) — both routes land on the identical marketplace install:

```text
/plugin marketplace add marktoda/zj-radar
/plugin install zj-radar-claude@zj-radar
```

The first command registers this repo as a plugin marketplace named `zj-radar`;
the second installs the `zj-radar-claude` plugin *from* it — that's what the
`zj-radar-claude@zj-radar` (`plugin@marketplace`) syntax means.

Requires `jq` and `git` on `PATH` (used to parse the hook payload and derive
repo/branch) — though `jq` is needed only by the bash fallback: when the native
`zj-radar` binary is on `PATH` the hook prefers it and exits before the `jq`
gate. See [`plugins/zj-radar-claude/README.md`](../plugins/zj-radar-claude/README.md)
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

- **`zj-radar notify <claude|codex|opencode>`** — broadcasts agent status. The Claude
  plugin's hook script automatically prefers it when it's on `PATH` (jq-free);
  otherwise the plugin falls back to its bundled `bash`+`jq` script.
- **`zj-radar setup [claude|codex|opencode]`** — idempotently wires the named
  agent (bare `setup` wires every detected agent). Codex gets hook entries in
  `hooks.json` under `$CODEX_HOME` (or `~/.codex` when it's unset) calling
  `zj-radar notify codex`. This preserves any existing Codex `notify` program
  (e.g. a Computer Use notifier), because hooks are additive. Claude is wired
  through Claude Code's own `claude plugin` CLI (marketplace add + install —
  the same install as [above](#claude-code)) rather than by editing files.
  Opencode gets a vendored JS bridge plugin (see [below](#opencode)).
  All take the same flags: `--dry-run` to preview, `--uninstall` to remove
  only zj-radar's wiring, and `--check` to diagnose the current setup. After
  installing or changing Codex hooks, run `/hooks` inside Codex once to review
  and trust the command hook.
- **`zj-radar setup codex --legacy-notify`** — opt-in fallback for older Codex
  setups that only support the single `notify` program. It refuses to replace a
  foreign notifier unless `--force` is also passed.
- **`zj-radar setup zellij --wasm <path>`** — copies the sidebar wasm to
  `~/.config/zellij/plugins/zj_radar.wasm` (or fetches the release matching the
  CLI's version with `--download` instead of `--wasm`), manages the `radar`
  alias in `config.kdl`, and offers layout injection: on a TTY it asks before
  adding the rail to your layout, `--inject` consents non-interactively,
  `--layout <name>` targets a specific layout, and otherwise it prints the
  snippet to paste yourself. `--uninstall` strips both the alias and the
  injected rail.

Codex hooks report turn start, tool use, permission requests, subagents, and
turn stop. zj-radar maps those to `running`, `pending`, and `done`. `setup`
writes one entry per event across seven hook events (`UserPromptSubmit`,
`PreToolUse`, `PermissionRequest`, `PostToolUse`, `SubagentStart`,
`SubagentStop`, `Stop`), each with `timeout` 10 — the default send cap plus
5 s of headroom, so Codex's hook runner never races the bounded send (see
[below](#writing-your-own-producer)) — a `commandWindows` variant, and the
`ZJ_RADAR_CODEX_HOOK=v1` marker that idempotency and `--uninstall` key on.

## Opencode

[Opencode](https://opencode.ai) auto-loads JS plugins from its global plugins
dir, so wiring is one file drop — **no `opencode.json` editing**, clean
uninstall = delete one file:

```sh
zj-radar setup opencode
```

This writes the vendored bridge plugin to `$XDG_CONFIG_HOME/opencode/plugins/`
(or `~/.config/opencode/plugins/` when `XDG_CONFIG_HOME` is unset). The bridge
serializes each opencode hook/bus-event payload and spawns
`zj-radar notify opencode --status <s>` with JSON on stdin; all classification
stays in Rust (it requires the `zj-radar` binary — no JS-side derive twin).

- **Requires the `zj-radar` binary on `PATH`.** Unlike Claude's `jq` fallback,
  opencode wiring has no script fallback: `setup opencode` implies the binary.
- **An unwired opencode pane is dark, not un-enriched.** `opencode` is an
  instrumented agent (`AGENT_NAMES`), so the sidebar suppresses ordinary
  command-tracking for its panes and relies on the bridge for every row. With
  the bridge absent — a fresh install that skipped `setup opencode`, or
  `--pure` / `OPENCODE_PURE`, which disables external plugins — the pane shows
  no Running/Done row at all (same tradeoff class as claude/codex).
  `zj-radar setup --check` and the `setup zellij` epilogue both report the
  missing bridge when `opencode` is on `PATH`. **Upgrading from a release
  before opencode support:** panes that previously fell through to
  command-tracking go dark once the wasm is upgraded; run
  `zj-radar setup opencode` and restart opencode.
- **Restart opencode after installing** — plugins load once at startup, so a
  write mid-session needs a restart (or plugin reload) to take effect.
- The bridge covers `chat.message` (running + task), `tool.execute.before`/
  `after` (running + tool activity), the `permission.asked` bus event
  (pending; the pre-1.14 `permission.ask` hook is kept as a fallback) and
  `permission.replied` (back to running), `session.idle` (done, with the
  trailing-question → pending remap), `session.error` (error — a real failure
  signal Claude's hook model lacks), and `session.created`/`session.deleted`
  (idle). Events from subagent (task-tool) sessions are ignored — only the
  root session drives the row — and the user's own prompt text is never
  mistaken for assistant text. Sends are async-only (never synchronous — the
  bridge runs in opencode's process and must not freeze the TUI), one child
  at a time, with a hard ~10 s kill timer per child. Status *edges*
  (pending/done/error/idle) are strictly FIFO; `running` refreshes coalesce to
  the latest unsent one so a slow or wedged pipe never queues stale refreshes
  ahead of an edge. The `ZJ_RADAR_OPENCODE_PLUGIN=v1` marker in the plugin
  header is what idempotency, `--uninstall`, and `--check` key on; a foreign
  plugin file (no marker) is refused unless `--force` replaces it.
- **One TUI, many sessions, one pane:** latest event wins — same semantics as
  Claude. The minimum verified opencode version is 1.18.x (the plugin is
  vendored per zj-radar release; re-running `setup opencode` heals drift via
  the marker rewrite).

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
  `server` ❯ · `command` $, and the agent tokens `claude` ✳ · `codex` ❉ ·
  `opencode` ✺ · `gemini` ✦ — anything else (including the default `generic`)
  renders the neutral `⦿`.
- Repo/branch come from `git` in the calling directory; the pane id from
  `$ZELLIJ_PANE_ID`. Outside Zellij it's a silent no-op (safe under `set -e`).
  `--dry-run` prints the payload instead of broadcasting.

The same lifecycle rules as agents apply: latest broadcast wins, a finished
status clears when the pane returns to its shell prompt, and a `running` row
whose pane sits at the prompt with no follow-up broadcast is cleared by the
stale-Running watchdog after ~15s — so send `done`/`error` when your script
finishes rather than leaning on the watchdog. A pane whose root process exits
clears *any* status immediately, no grace clock — relevant for `zellij run`
script panes, where the pane outlives the process but the row does not.

## Writing your own producer

Writing one in Rust? Depend on
[`zj-radar-core`](https://crates.io/crates/zj-radar-core)
([docs.rs](https://docs.rs/zj-radar-core)) — the same crate both halves of
zj-radar use: build a typed `StatusPayload` and serialize it with `to_wire`,
round-trip-tested against this schema, so your payload can't drift from what
the sidebar accepts. Everything below applies either way; the crate just
handles the encoding for you.

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
- `source`: tokens are lowercase-exact — matching is case-sensitive, so
  `"claude"` classifies as the Claude agent while `"Claude"` falls back to the
  neutral kind.
- `task` (optional): sticky task label, valid with any status — empty/absent
  leaves the stored label unchanged, non-empty replaces it; the plugin clears
  it on idle and on return-to-shell. (The bundled adapters happen to send it
  only on `UserPromptSubmit`; `notify generic --task` sends it whenever you
  like.)
- `ack` (optional, default `false`): "the user has already seen this status" —
  the plugin converges state as usual but never fires a desktop notification
  for it. Set by the rail's own acknowledge gesture (the right-edge `✓`
  hotspot); producers
  reporting real events should leave it absent (an acknowledged `done` would
  otherwise skip the completion notification the user wanted).
- Unknown fields are ignored, so it's safe to send extras. (A former `on_focus`
  clear-on-focus hint is no longer used — the plugin clears a finished status when
  the pane returns to its shell prompt instead — but sending it does no harm.)

The plugin applies the latest broadcast per pane (the pipe delivers in order, so
there is no sequence number). It also defends itself: it strips ANSI/control
chars and Unicode bidi-control characters, folds newlines to spaces, and
silently ignores unknown fields, so extra keys never break a producer. The plugin
also enforces field limits, so you don't have to pre-truncate: `repo`/`branch` are cut to 40 chars,
`msg`/`task` to 60, `source` to 16 — and a payload over **64 KB** is dropped
whole. `pane.type` must be `"terminal"`; any other pane type is rejected. The
store is bounded too: past **256** distinct pane ids the oldest observation (by
last status change) is evicted, so a producer looping over fresh pane ids can't
grow the state without limit.

Quick smoke test (a "fake agent" — broadcast straight from your shell):

```sh
zellij pipe --name zj_radar.status.v1 -- \
  '{"v":1,"source":"test","pane":{"type":"terminal","id":'"${ZELLIJ_PANE_ID#terminal_}"'},"status":"running","repo":"demo","branch":"main","msg":"hello"}'
```

**Bound your sends.** `zellij pipe` is not fire-and-forget: Zellij holds the
client process until *every* loaded plugin instance consumes the message
(CLI-pipe backpressure). A plugin instance stuck at its permission prompt
blocks the client **forever** — and a producer that fires per tool-call then
leaks one blocked process plus two Zellij-server FDs per event, until the
server hits EMFILE and the whole session crashes. Wrap the call in a timeout
(the bundled producers default to 5 s for status edges, 2 s for `running`
heartbeats — see below; `ZJ_RADAR_PIPE_TIMEOUT` overrides both); killing
the client past the deadline loses nothing — the message is already queued
server-side.

The timeout must survive **your own death**, too. Hook runners kill their
hooks, and a producer killed mid-send never runs its kill-on-deadline — the
blocked `zellij pipe` client re-parents to init and leaks forever (this
exact orphan class EMFILE-crashed a real session). Put the watchdog *inside*
the subtree you spawn, not only in your process: run the pipe under a shell
alongside a detached `sleep <deadline>; kill` pair, the way the bundled
producers do (`self_limiting_pipe_argv` in `zj-radar-core`'s `pipe` module,
mirrored by notify.sh's sleep+kill watchdog).

And leave the hook runner headroom to let that graceful path finish: if your
runner enforces its own per-hook timeout, set it **above** your send deadline
plus backstop slack. The graceful exit lands at ~deadline (the in-subtree
watchdog kills the pipe client), with a deadline + 1 s parent-reaper backstop
behind it — so the bundled Claude hooks keep `timeout >= deadline + 2`; equal
budgets would mean the runner races, and under load wins, against your
bounded no-op. Hot-path events that fire per tool call deserve a *shorter*
deadline than rare edges: an expired `running` is a dropped heartbeat the
next event replaces, so the bundled producers key the default on status —
`running` sends cap at 2 s, the `done`/`pending`/`idle` edges keep the full
5 s. A Rust producer should reuse the same numbers: `zj-radar-core` re-exports
them as `RUNNING_PIPE_TIMEOUT_SECS` / `DEFAULT_PIPE_TIMEOUT_SECS` next to
`self_limiting_pipe_argv`. `ZJ_RADAR_PIPE_TIMEOUT` overrides both defaults, and
its parse is strict: whole seconds only, clamped to 3600 — anything else
(`10s`, a negative, an empty string) fails closed to the default rather than
running unbounded.
