# zj-radar design

How the sidebar works and why it is built this way. This describes the shipped
system; the history is in git and in
[`smart-tabs-postmortem.md`](smart-tabs-postmortem.md). Vocabulary is defined
in [`CONTEXT.md`](../CONTEXT.md); status and class semantics in
[`activity-model.md`](activity-model.md); the exact rendered grid in
[`rail-reference.md`](rail-reference.md).

## 1. Goal

Bring Cmux-style agent awareness into an existing Zellij setup without
changing keybindings, swap layouts, or config: an always-on left sidebar that
lists every tab and, for tabs running AI agents, shows per-pane state
(working, waiting for you, done, error), the task, and the last message, and
jumps to a tab on click.

The hard constraint: **no polling, and no blocking host queries on any
per-event or per-tick path.** Signals come from pushed events (`pipe`,
`TabUpdate`, `PaneUpdate`, `CwdChanged`, `CommandChanged`) or the hook
payload. The single exception is the once-per-pane `Effect::ResolveCwd`
naming bootstrap: one `get_pane_cwd` per freshly opened pane, never repeated.
The predecessor polled every pane on every output event and melted a
many-agent session; the postmortem has the numbers.

## 2. Why an explicit pipe, not OSC sniffing

Cmux owns the terminal surface (it is the emulator), so it can combine OSC
notification signals with per-agent hooks. Even there, the working → waiting →
done status comes from hooks; OSC only says "something dinged".

A Zellij plugin does not own the PTY. It gets a structured event API, not the
byte stream. Zellij forwards some notification OSCs (OSC-99 desktop
notifications since 0.44.1), but those are transient attention events with no
`running`/`pending`/`done` state, no repo or branch, and no pane attribution,
and the plugin API exposes no event for them. The only output-side signal a
plugin sees is the per-tab bell boolean.

So the reliable seam is an explicit, adapter-owned `zellij pipe` payload
delivered to the plugin's `pipe()` entrypoint. That mirrors Cmux's real status
path while fitting Zellij's architecture.

## 3. Architecture

Thin Zellij-host glue around a deep, pure runtime plus pure stores and
rendering. The plugin's domain modules have no `zellij-tile` dependency
(`zellij-tile` is `cfg(target_arch = "wasm32")`-scoped): `lib.rs` translates
raw events into repo-owned inputs and applies the ordered effects the runtime
returns.

The repo is a three-crate workspace: `crates/core` (wire payload, command
classification, `Kind`, the bounded pipe argv; shared by producer and plugin),
`crates/cli` (the host-side binary: `notify`, `setup`, `run`), and
`crates/plugin` (the wasm sidebar). Filenames below live under
`crates/plugin/src/`.

```
┌ Agent adapters (outside the wasm) ──────────────────────────────────────┐
│ Claude Code → plugin hook / native CLI          (running/pending/done)  │
│ Codex       → native CLI via hooks.json         (running/pending/done)  │
│ Opencode    → JS bridge → native CLI            (running/pending/done/error) │
│ pi          → extension → native CLI            (running/pending/done/error) │
│ any script  → zj-radar notify generic                                   │
└───────────────────────────┬─────────────────────────────────────────────┘
   zellij pipe --name zj_radar.status.v1 -- {v,source,pane,status,repo,branch,msg,task,ack,tasks}
   broadcast by name, through the bounded self-limiting argv (§5).
   The plugin is a caller too: the ✓ gesture echoes an ack:true payload here.
                            │
                            ▼
┌ zj-radar plugin (Rust → wasm32-wasip1) ────────────────────────────────┐
│  lib.rs: Zellij adapter                                                │
│    raw Event/PaneInfo/TabInfo ⇄ repo-owned inputs/effects; owns        │
│    SessionFiles and applies returned effects                           │
│                                                                        │
│  runtime.rs: PluginRuntime                                             │
│    lifecycle, permissions, timers, snapshot decisions, naming,         │
│    command activity, config pipes, mouse intent                        │
│    input: tab/pane updates, permission result, status/config/cmd       │
│           pipes, timer, mouse                                          │
│    output: Outcome { render, effects: Vec<Effect> }                    │
│                                                                        │
│  session_files.rs: SessionFiles                                        │
│    per-session filesystem coordination across instances: snapshots,    │
│    permission marker/lock, root fallback, pruning, presence files,     │
│    notification claims                                                 │
│                                                                        │
│  radar_state.rs / rollup.rs / tab_namer.rs: state + tab model          │
│    StatusStore + CommandStore (over core's ObservationStore),          │
│    roll_up(tab), TabNamer                                              │
│                                                                        │
│  ledger.rs / sessions.rs / presence.rs: completion ring;               │
│    cross-session peer state and the presence record (§13)              │
│                                                                        │
│  control.rs / config.rs / theme.rs: cmd + config pipes, colors         │
│  permission.rs / clock.rs / notify_rules.rs: grant state machine,      │
│    timer-chain bookkeeping, notification edge rules                    │
│                                                                        │
│  render.rs: pure rail renderer                                         │
│    render_rail(rows, ledger, opts) -> RenderedRail                     │
│    (target_at_line / hotspot_at; onboarding + needs_permission faces)  │
│    owns layout, overflow, ANSI, click-target + hotspot lockstep        │
└────────────────────────────────────────────────────────────────────────┘
        │ Effects: SwitchTab, ShowPane, RenameTab, RequestPermission,
        │ SetTimeout(Fast|Slow), SetSelectable, PersistSnapshot,
        │ PersistPermissionMarker, HeartbeatPermissionLock, ResolveCwd,
        │ Notify, PersistPresence, ReadPresences, DismissPresence,
        │ SwitchSession, BroadcastStatus, CloseSelf
        ▼
```

Host-coupled code stays thin. Lifecycle decisions live in `PluginRuntime`,
filesystem coordination in `SessionFiles`, layout and click decisions in
`RenderedRail`, so the core is unit-testable with `cargo test`. The external
seam is the versioned pipe payload.

## 4. Lifecycle

### 4.1 Event → status

| Source event | Status |
|---|---|
| Claude `UserPromptSubmit` / `PreToolUse` / `PostToolUse` | `running`. A `PostToolUse` that backgrounded work and a `<task-notification>` wake carry a background-task batch and are never deduped. Otherwise `PostToolUse` usually duplicates `PreToolUse`: the CLI drops the identical repeat before spending anything (§5, *Last-sent dedup*) and the plugin no-ops any that still arrive. It stays registered because it is the Pending → Running recovery edge after a mid-turn permission answer — that one differs from the last-sent `pending`, so it is never dropped. |
| Claude `Notification` (`permission_prompt` / `elicitation_dialog`) | `pending` |
| Claude `SubagentStop` | `running` (the main turn is still going); its `last_assistant_message` (the subagent's report) is never the message, only a `Stop`'s is |
| A **background** subagent's own `PreToolUse` / `PostToolUse` / `SubagentStop` | nothing: the CLI records the `agentId` the parent's launch returns (one marker file per id beside the dedup records) and drops that agent's hooks, which would otherwise overwrite the parent's `waiting on …` or `pending` row. Foreground subagents report as above. Limit: a permission prompt a background subagent raises keeps the row `pending` until the parent's next hook or that subagent's completion wake. The bash fallback keeps no state and reports them. |
| Claude `Stop` | `done`, except: a Stop whose last assistant message ends in a question maps to `pending` (the question becomes the message); otherwise a Stop whose `background_tasks` still lists bounded work (a running shell that isn't a service, a subagent, a workflow) maps to `running` with a `waiting on …` message. Each finished task wakes the model, so the next Stop carries the refreshed list and the real `done`. Monitors, crons, service-looking shells (`run dev`, `serve`, `vite` (not `vite build`), `uvicorn`, `http.server`, `--watch` flags (not on `gh` or `kubectl rollout status`, whose watches end), `cargo watch`, `port-forward`, `tail -f`, …, matched on the task's command as whole shell words, so `test_serve.py`, `./serve/...` and `dev-deps` don't count; descriptions match only multi-word phrases such as "dev server" or "watch mode") and unknown task types don't hold the row. Limit: a snapshot shell entry without a `command` is judged by its description alone, and core only ever lowers a task's hold, so a task the launch classified as a service but the snapshot can't may show `waiting on …` with a working spinner until the next Stop. Every Stop also carries the running set as a `tasks` snapshot; a `PostToolUse` that backgrounds work carries its start, and a `<task-notification>` `UserPromptSubmit` carries each outcome (see `crates/core/src/task.rs`). |
| Claude `SessionStart` (`source: "clear"`) | `idle` (resets the row on `/clear`) |
| Claude `SessionEnd` | `idle` |
| Codex `UserPromptSubmit` / tool hooks / subagents | `running` |
| Codex `PermissionRequest` | `pending` |
| Codex `Stop` / legacy `agent-turn-complete` | `done`, or `pending` when the last assistant message ends in a question (same rule as Claude's `Stop`) |
| Codex ephemeral-fork hooks (`transcript_path: null`) | ignored |
| Opencode events | [`producers.md`](producers.md#opencode) |
| pi events | [`producers.md`](producers.md#pi) |
| Observed command exiting nonzero | `error` |
| Observed remote session (`ssh`/`mosh`/…) ending | `done`, notified as a disconnect (`error` if the exit code is nonzero, notified as "connection lost") — `activity-model.md` §3 |
| Agent pane returns to its shell prompt | terminal statuses clear at once; a `Running` arms the stale grace clock (§10) |
| Agent pane's root process exits (manifest `exited`) | `idle` immediately, no grace (§10) |

Claude's map has no `error` on purpose: its hook vocabulary carries no
reliable turn-level failure signal (`PostToolUse`'s per-tool `is_error` is
normal, recoverable behavior), so mapping it to `Error` would paint healthy
turns red. The two `error` sources are a nonzero command exit and opencode's
`session.error`.

### 4.2 Per-pane to per-tab aggregation

Multi-agent tabs exist (four Claude panes in one tab), so tab state cannot come
from names. The stores key by `PaneId`; `PaneUpdate`'s manifest gives the
pane → tab map.

- **Severity:** `error > pending > running > done > idle`. Error is highest so
  failures never hide behind "working".
- **Counts:** `total` is the panes in the tab that have ever reported a
  non-idle state and still exist; `done` is those currently `done`.
- **Primary detail:** the highest-severity pane. On ties a bounded job
  outranks a steady row — a service or a remote session (a spinning build
  beats a merely-up dev server or ssh session, `activity-model.md` §3), then
  the most recent change wins.
- **Prune grace.** A pane absent from a manifest is not pruned on its first
  absence (`absent_once`). Zellij's break-pane family reports session state
  while a moved pane is extracted and in no tab, so one absence cannot
  distinguish a close from a mid-move flash. A pane still absent on the next
  manifest prunes then, ledgered under the tab captured at first absence. The
  grace set is recomputed from every manifest, so a reappearing pane simply
  drops out.

### 4.3 Focus never drives state

An earlier design cleared a completion when you focused the pane. Focus is
per-client and is not delivered to background plugin instances, so that
cleared the row only on the tab you were viewing. A finished status now clears
only via shared signals: a new broadcast, the return-to-shell exit-clear, or a
prune. Every tab's instance receives those, so all tabs converge. The former
`on_focus` wire field is dropped unread like any unknown key.

## 5. The pipe contract

Producers broadcast `zj_radar.status.v1` by name. Each sidebar instance filters
on the name and keeps its own copy of the state.

```json
{ "v": 1,
  "source": "claude",                 // Kind vocabulary: claude | codex | opencode | pi | gemini |
                                      //   command | test | build | deploy | server | other
  "pane": { "type": "terminal", "id": 12 },
  "status": "running",                // running | pending | done | error | idle
  "repo": "pinky",
  "branch": "fix/x",
  "msg": "running tests…",
  "task": "fix the flaky auth test",  // optional sticky label
  "ack": false,                       // optional: "user already saw this", notifier stays silent
  "tasks": {                          // optional background-task batch (upserts by id)
    "snapshot": true,                 //   true = the complete running set
    "items": [ { "id": "b1", "state": "running",   // running | completed | failed | killed | ended
                 "label": "Run tests", "holds": true } ] } }
```

Field rules for producers are in
[`producers.md`](producers.md#writing-your-own-producer). Plugin-side handling
is defensive: match `pane` to `PaneId::Terminal`; ignore unknown fields
(including a legacy `seq` and the former `on_focus`); fold an unknown `status`
to `idle`; strip ANSI, control, and bidi characters; fold newlines; truncate
`repo`/`branch` to 40, `msg`/`task` to 60, `source` to 16; drop payloads over
64 KB; evict the oldest pane past 256 ids. Ordering is latest-wins.

**The plugin is a producer too.** The `✓` acknowledge gesture does not mutate
local state. It broadcasts a synthetic payload (the pane's Pending re-sent as
`done` with `ack: true`) onto the same pipe (`Effect::BroadcastStatus`), so
every instance, including the sender, converges through the normal
`status_pipe` intake, and `ack` keeps every notifier silent.

**Bounded sends.** `zellij pipe` is a backpressure channel: the client blocks
until every loaded plugin instance consumes the message, and an instance
wedged at Zellij's permission prompt holds it forever. Unbounded sends at hook
rate once turned one wedged rail into an EMFILE crash of the whole session.
Every caller, the plugin's ack echo included, sends through the self-limiting
`sh` subtree from `crates/core/src/pipe.rs` (`self_limiting_pipe_argv`): a
detached `sleep; kill` watchdog inside the spawned subtree reaps its own hung
client even if the caller is killed mid-send. Deadlines are status-keyed:
`DEFAULT_PIPE_TIMEOUT_SECS` (5 s) for once-per-turn edges,
`RUNNING_PIPE_TIMEOUT_SECS` (2 s) for `running` heartbeats — except a
`running` that carries background tasks, which is an edge and keeps 5 s. The
bundled hooks' `timeout` values must clear the worst-case cap plus 2 s
(welded by `hooks_manifest_tests.rs`). The subtree exits with the client's status, so a
caller can tell a delivered send from one killed at the deadline; the CLI
producer blocks on that exit, with a cap + 1 s backstop thread for the corner
where the watchdog's own fork failed.

**Last-sent dedup.** Claude's PreToolUse and PostToolUse derive identical
payloads, so half of tool-loop traffic would be a duplicate every rail
instance pays to receive. The CLI drops an identical `running` re-sent within
`DEDUP_TTL_SECS` at the `broadcast` choke point, before any git work or spawn;
edges always send, which is what keeps PostToolUse the Pending→Running
recovery edge (the `pending` overwrites the record), and so does any payload
carrying background tasks (a start or outcome can't be recovered later). The TTL is pinned below
the plugin's stale-Running grace, which any payload cancels, and only a
confirmed delivery is recorded — hence the wrapper's exit status above. The
rule, its key, and its bounds: `crates/cli/src/dedup.rs`.

**Newcomer rehydration.** The plugin runs one instance per tab, and a
broadcast reaches only the instances alive when it is sent. A tab opened after
agents are already running would otherwise start blank. Each instance mirrors
its stores into a snapshot on every state edge and seeds itself from it in
`load()`. Writes are edge-gated: a label-only Running → Running update defers
its write to the next Fast tick (`SnapshotWrite::Deferred`) instead of hitting
disk per tool hook. `SessionFiles` picks the root: `/cache` first (Zellij
mounts it as the plugin-URL-scoped folder shared across instances), then
`/tmp/zj-radar`, then persistence off. `/data` is not used: it is scoped per
`<plugin_id>-<client_id>` and removed on unload. Snapshot names are scoped by
the Zellij server pid; writes are temp-file plus atomic rename. Every live
instance holds the same converged stores after a broadcast, so one write per
edge is the whole snapshot: the instance whose tab holds the edge's pane
writes, and edges with no nameable owner are written by everyone
(`RadarState::persists_edges_for`). Ownership rather than visibility, because
Zellij spawns fresh plugin instances — seeded from this file — for every
client that attaches, while the detached client's hidden instances are what
kept it current meanwhile. Overlapping writers produce identical content, so
races are benign. With persistence off, late sidebars start empty until the
next broadcast.

## 6. Plugin ↔ Zellij wiring

**Permissions.** `ReadApplicationState`, `ReadCliPipes`,
`ChangeApplicationState` (switch tab/pane/session, rename tabs, close the grant
float), and `RunCommands` (desktop notifications via `osascript` /
`notify-send`, and the ack re-broadcast). Without `RunCommands`, `run_command`
is a silent no-op.

**Selectability.** One level rule, `PluginRuntime::desired_selectable`: the
rail pane is selectable while its own permission request is in flight
(Zellij's y/n prompt is tied to the requesting pane) and until a `PaneUpdate`
shows a terminal pane sharing its tab; then `set_selectable(false)` so it never
steals focus. `SetSelectable` is emitted on change only, from the permission
transitions and `panes_changed`. The terminal-neighbor half exists because
Zellij closes any tab with no selectable tiled pane: a rail that went passive
at load in a tab holding only plugins closed that tab — and, as the last tab,
the session ([#46](https://github.com/marktoda/zj-radar/issues/46); see
[`troubleshooting.md`](troubleshooting.md#tab-opens-with-only-the-rail-or-the-session-exits-at-once)).
A rail that never sees a manifest (denied permission, a tab never activated)
simply stays selectable. The terminal-neighbor evidence is a **latch**, not a
level: once a terminal has shared the tab the rail stays passive even when the
manifest later shows none. Zellij's `ClosePane` arm reports that manifest to
plugins synchronously and defers the tab-closing render, so a rail that flipped
back to selectable on "no terminals left" would beat the render and keep a dead
tab — and the session — alive. With the latch, `exit` in a tab's last shell
closes the tab exactly as before.

**Per-tab prompt coordination.** On an uncached first run, `SessionFiles` uses
a session-scoped lock to elect one instance to call `request_permission()`;
peers wait on a marker and then request after Zellij has cached the answer.
One prompt instead of one per tab. Without writable session files it degrades
to every instance prompting. `setup zellij` normally pre-seeds the grant into
`permissions.kdl` so no prompt appears at all ([`install.md`](install.md#permissions)).

**Subscriptions.** `TabUpdate`, `PaneUpdate`, `CwdChanged`, `CommandChanged`,
`Timer`, `Mouse`, `PermissionRequestResult`, `ModeUpdate` (carries
`ModeInfo.session_name`, the session-name source §13 depends on), and
`InitialKeybinds`, which is never handled: subscribing to it is Zellij's
opt-out that strips the full keybinding table from every `ModeUpdate`, so the
per-instance protobuf decode of each mode change stays small.

**Tab index footgun.** `TabInfo.position` is 0-indexed; `switch_tab_to` is
1-indexed. `display_tab_number = position + 1` is used for both rendering and
clicks.

**Click targeting.** `render_rail()` emits ANSI plus same-height target and
hotspot maps (`CONTEXT.md` → *Lockstep*). Header, folded-idle strip, and gap
rows map to nothing; a tab's header line maps to `SwitchTab`; pane lines map
to `ShowPane`. The runtime caches the latest `RenderedRail` and resolves
clicks against it rather than replaying layout math in the host glue.

**Broadcast, not `--plugin`.** Broadcasting by name means adapters never create
UI panes, never need the plugin's URL, and reach every running instance. A
`--plugin` destination can also load the plugin if it is not running, and
routing across same-plugin instances is fiddly.

**Layout is the integration seam.** The sidebar is a borderless left column
inside a vertical split, outside `children`, so `swap_tiled_layout` cycling
never disturbs it (0.44.3 has the pop-out fix). The tab templates are the only
native place Zellij pins a pane into every tab, so radar integrates like
zjstatus: the user adds one pane to their templates. `setup zellij` installs
the wasm at a stable path and manages a `radar` alias in `config.kdl`, so the
per-layout snippet is path-free.

```kdl
default_tab_template {                       // layout-defined tabs fill `children`
    pane split_direction="vertical" {
        pane size=32 borderless=true {
            plugin location="radar"
        }
        children
    }
    pane size=2 borderless=true {
        plugin location="zellij:status-bar"
    }
}
new_tab_template {                           // runtime tabs need a CONCRETE pane
    pane split_direction="vertical" {
        pane size=32 borderless=true {
            plugin location="radar"
        }
        pane focus=true
    }
    pane size=2 borderless=true {
        plugin location="zellij:status-bar"
    }
}
```

`new_tab_template` is mandatory. When omitted, Zellij derives one from
`default_tab_template` and drops a `children` nested inside a split
([zellij#3247](https://github.com/zellij-org/zellij/issues/3247)); the new tab
then has no focusable terminal. A top-level `children` (the stock compact
layout) is unaffected; only the nested case is. The same nesting has a second
edge: a `tab` node whose body declares no pane fills the nested `children`
with nothing, so Zellij spawns no terminal for it
([zellij#5618](https://github.com/zellij-org/zellij/issues/5618)) — the tab
opens with the rail alone (*Selectability* above is what keeps such a tab,
and the session, alive). `layout::empty_tab_bodies` names those tabs for the
doctor and `--inject`. Plugin nodes stay on their own lines: Zellij 0.44's KDL
parser rejects a one-line `{ plugin location="radar" }` without a trailing `;`.

## 7. Agent adapters

- **Claude Code**: a Claude plugin (`plugins/zj-radar-claude/`) whose hooks
  call `scripts/notify.sh`, which prefers `zj-radar notify claude` when the
  binary is on `PATH` and otherwise runs its own `bash`+`jq` path. The hooks
  register with the plugin; no `settings.json` editing.
- **Codex**: `zj-radar setup codex` installs marker-owned command hooks in
  `hooks.json`; Codex sends hook JSON on stdin and `zj-radar notify codex`
  maps it. The legacy single-slot `config.toml` `notify` path stays behind
  `--legacy-notify` and can only emit `done`.
- **Opencode**: `zj-radar setup opencode` drops two marker-owned JS bridges
  into opencode's auto-loaded plugins dir: a 1.x server plugin file
  (`zj-radar.js`) and a 2.x TUI plugin directory (`zj-radar/tui.js`). Each
  line ignores the other's shape. Both pick the status class (they know which
  event fired) and spawn `zj-radar notify opencode --status <s>` with the
  payload on stdin, using one shared `event` vocabulary; the Rust adapter owns
  every refinement (tool activity, task capture, the trailing-question remap,
  the blank-permission backstop), so the JS never grows a second classifier
  and an opencode API change lands in JS only. Bridge behavior: events from
  subagent (task-tool) sessions are ignored; the user's own prompt text is
  never mistaken for assistant text; sends are async only (the bridge runs in
  opencode's process and must not freeze the TUI), one child at a time, with
  a hard ~10 s kill timer per child; status edges are strictly FIFO while
  `running` refreshes coalesce to the latest unsent one. The 2.x bridge is a
  *TUI* plugin on purpose: 2.x runs one shared, detached background server per
  machine whose environment is the first launching pane's, so a server plugin
  would pin every session to one stale `$ZELLIJ_PANE_ID`; the TUI process is
  the pane, knows which sessions it shows (router + tabs), and dies with it.
  The `ZJ_RADAR_OPENCODE_PLUGIN=<v>` header marker family is what
  idempotency, `--uninstall`, and `--check` key on; a foreign plugin file is
  refused unless `--force`. Because opencode is an instrumented agent
  (`AGENT_NAMES`), its panes are never command-tracked; without the bridge
  they show nothing.
- **pi**: `zj-radar setup pi` vendors a marker-owned bridge extension to pi's
  auto-loaded extensions directory (`~/.pi/agent/extensions/zj-radar.js`, or
  `$PI_CODING_AGENT_DIR/extensions/`); pi auto-loads it, so a restart or
  `/reload` picks it up. The extension spawns `zj-radar notify pi --status <s>`
  off pi's own hook events and reports only in the interactive TUI
  (`ctx.mode === "tui"`); the `ZJ_RADAR_PI_EXTENSION=<v>` marker is what
  idempotency, `--uninstall`, and `--check` key on. Because pi is an
  instrumented agent (`AGENT_NAMES`), its panes are never command-tracked;
  without the extension they show nothing. The command observer's node/bun
  peel (`command::classify`, matching a hosted script's basename against
  `AGENT_NAMES`) is what recognizes a globally-installed `pi` (`node
  .../bin/pi`) — or an npm-installed Codex (`node .../codex.js`) — as the
  agent rather than an anonymous `node` command.

**Two install surfaces, two answers.** Claude Code has a plugin system that
bundles hooks, so a plugin is the right shape: one install command, clean
uninstall, no user-file surgery. Codex, Opencode, and pi have hook or plugin
surfaces but no marketplace, so `zj-radar setup` edits their native config.
The rules the installer keeps (`crates/cli/src/setup/`): strip-own-then-re-add
keyed on a marker string, so re-runs are idempotent and `--uninstall` removes
only ours; refuse to write a file that exists but does not parse; atomic
writes; diff preview with `--yes` and `--dry-run`.

Adding an agent: an `enum Agent` variant in `crates/cli/src/agents/` plus
`Agent::derive`; its `source()` string is shared by the CLI argument, the wire,
and `Kind::from_source`, pinned by `source_round_trips_through_kind`.

## 8. Timer and cadence

Zellij's timer is one-shot, so the plugin re-arms it each tick at one of two
speeds or not at all (`PluginRuntime::desired_cadence`):

- **Fast (1 Hz)** while there is tick-windowed work: the permission flow still
  live; a spinning glyph (`TrackedObservation::animating`, Running and not a
  service); an un-carried completion edge (a status-pipe notify deferred to
  the timer because its own focus cannot be trusted, see *Settle* in
  CONTEXT.md); a command `Done` awaiting its TTL recede; an active flash; a
  pending cross-session cycle selection awaiting its idle-commit; and the
  scheduled one-shots (promotable pendings awaiting debounce, tentative Dones
  awaiting confirm, stale-Running grace clocks).
- **Slow (once a minute)** once none of that holds but a minute-granular age
  is still changing (an unsaturated ledger entry or pending wait tag), and
  unconditionally while `own_session_name` is known, because the Slow tick's
  heartbeat is what keeps the presence file's mtime fresh for peers (§13).
- **Disarmed** in two shapes: pre-name (no `ModeUpdate` has delivered a
  session name and every age has saturated) and denied (a permission-denied
  rail disarms unconditionally, since no clearing event will ever arrive).

Service, remote, and interactive rows never pin Fast. A dev server, an ssh
session, or an editor left open overnight costs zero ticks. A backgrounded `done`/`error`/`pending` row
is terminal: once its one-shot settle has run it does not keep Fast alive.

## 9. Render gate

Zellij delivers every broadcast, `CommandChanged`, and `CwdChanged` to every
tab's instance, each running under a wasm interpreter, so one chatty producer
multiplies across N tabs at interpreter prices. (`TabUpdate`, `PaneUpdate`,
and `ModeUpdate` are different: Zellij 0.44 routes them to the plugins in
each client's *active* tab only, so a hidden rail sees no manifest until its
tab is revealed, at which point it converges from the fresh one.) Four layers
keep repaints proportional to change:

1. **Intake no-ops.** An intake that changed nothing rows-visible reports a
   default `RadarChange`: an identical status re-broadcast (producers
   re-assert on every tool hook), an identical `TabUpdate`, a `CommandChanged`
   that only touched the debounce maps, a `CwdChanged` (naming rides the
   `RenameTab` effect's own `TabUpdate` echo).
2. **Label-only deferral.** A Running → Running relabel on an animating row
   neither renders nor persists inline: the Fast tick is armed and repaints
   unconditionally, so the label lands within a second and the snapshot write
   rides the tick's flush. Only while animating: a rewritten Pending question
   renders now, and so does a running service's label, since neither pins
   Fast.
3. **Rows-diff gate.** `project` drops a requested render whose content key
   (rows, ledger lines, badge, theme) equals what the last `render()` drew
   (`last_render_key`). `force_render` bypasses it for timer frames and config
   overrides, which change the drawing without changing the key.
4. **Visibility gate.** `Event::Visible(false)` (tab switch, tab close, layout
   apply, client detach) marks the instance hidden: it paints nothing, ticks
   included. Nothing else changes — state keeps ticking at full cadence (the
   stores' grace clocks count ticks, so a slowed hidden instance would drift
   from its siblings until reveal) and snapshot writes follow pane
   ownership (§5), not visibility. `Visible(true)` force-renders the skipped
   frame. Zellij never sends `Visible` at load, so an
   instance starts visible, and any manifest it receives (which only reach
   active-tab plugins) also clears the flag — the bias is toward painting,
   since a wrongly hidden rail would be a stale screen. Under the interpreter
   the paint is over 95 % of a tick's cost, so this is what makes N tabs cost
   about one.

Underneath all four: `RadarState::generation` (bumped by every mutator of
anything `rows()` reads) and the `rows()` memo keyed on `(generation, the
tabs mid-flash)` — the tick reaches a row only through the ping flash.
One roll-up per event, shared by the presence derive, the gate compare, and
the render. A missed `touch()` is a stale-rail bug, not a slow one.
`tools/wasm-fuel` measures every layer in the interpreter's own unit
(`CONTRIBUTING.md` → *Measuring plugin cost*).

## 10. Running exit grace (producer death)

A pushed producer fires no hook when it quits, so its last status would linger
forever. The model: (agent argv present, manifest `exited` = false) means
alive; `exited` means dead. Three paths converge:

- **Prompt return.** When the observed layer sees the pane at a shell prompt
  (`command::is_shell_prompt`: no foreground command, or a shell program),
  `RadarState` clears a terminal status (`done`/`pending`/`error`) to idle.
  A `Running` is not cleared at once, because a live turn's foreground can
  flicker through the shell; it arms a grace clock
  (`RUNNING_SUSPECT_GRACE_TICKS`, about 15 Fast ticks). If the clock expires,
  `expire_stale_running` clears the row. Zellij reports a childless pane-root
  shell with `is_foreground: false`, so that shell-name form is the real
  prompt-return edge; a non-shell wrapper argv with `is_foreground: false` is
  weak evidence and never starts the clock.
- **Live-again evidence.** The agent's exe reappearing as the foreground
  command cancels the clock (`cancel_running_suspect`). Other commands do not
  vouch: a command run in the shell an agent died in must not keep its ghost.
- **Definitive death.** The manifest's `exited` flag is death, full stop. An
  agent-rooted pane (`zellij run -- claude`) never shows a shell prompt, so
  `StatusStore::clear_on_exit` clears even a `Running` immediately.

All three ride shared signals, so every instance clears: the broadcast and
`CommandChanged` reach every rail at once, and the manifest-borne `exited`
flag reaches a hidden rail with the manifest its reveal brings.

## 11. Tab naming

zj-radar owns tab naming, push-sourced. The candidate list, in order: the
focused pane's repo (from the hook payload), any pane's repo, the focused or
any pane's worktree-resolved cwd (from `CwdChanged` and the once-per-pane
bootstrap), the focused or any pane's title (from `PaneInfo.title` in the
manifest we already consume). `computed_name` takes the top; an applied name
sticks while any pane still justifies it. `Managed` overwrites only a default
`Tab #N` or a name the namer itself applied; `Force` overwrites manual names
too.

Renames are `rename_tab_with_id` (stable `TabId`, never position) and fire
only on change. Write authority is per tab: each instance learns its own
plugin pane's position from `PaneUpdate`, correlates it once with `TabUpdate`,
and retains the `TabId`; it emits no renames until that resolves, and an
onboarding instance never owns a tab. This stops a stale background instance
from applying its private history to another tab after navigation, and keeps
close/reorder events from redirecting ownership. The cwd bootstrap stays
session-wide because the cwd also stamps `repo` on observed commands, which
every instance must agree on for the notification claim key.

## 12. Ledger

The completion history: a fixed-cap ring (`LEDGER_CAP` = 32, newest first)
rendered as the rail's `─ earlier ─` region, at most ten entries shown.
`ledger.rs` is pure data and policy; `RadarState` wires the edges; the
renderer consumes prepared `LedgerLine`s.

**Entry rule.** An observation enters when it stops being a card fact, never
before: TTL recede of a command `Done`, the prompt-return clear, an overwrite
by a new broadcast (including the `/clear` idle), or a prune (captured against
the pre-close topology so it ledgers under the tab it was shown on).
`Pending` and `Running` never enter. A command completion shadowed by a status
observation for the same pane never enters either: `resolve`'s
status-over-command precedence means it was never on the card, and the status
source's own completion will ledger instead. The shadow is read at recede
time, not onset.

**Convergence.** Every entry edge is a signal every instance receives. Snapshot
v3 carries the ledger; on load, `Ledger::merge` unions two rings by
nearest-neighbor match on `(pane, outcome, label)` within 4 s, keeping the
later stamp, so two instances that saw the same completion a beat apart
collapse to one row.

**Timestamps.** Entries stamp completion-time epoch seconds, not ticks (ticks
are per-instance). Rendered age is relative and freezes at `1h+`. That
saturation is what lets the idle timer fully disarm instead of ticking once a
minute forever to redraw an age that will never change. A line's tab position
is a live lookup, so a closed tab's line is click-inert rather than forgotten.

## 13. Cross-session badge

One session's rail shows counts for every other zj-radar session on the host,
with click or cycle to switch, without ever calling Zellij's session list.
Pure state in `sessions.rs`, file IO in `session_files.rs`, wiring in
`runtime.rs`, render in `render.rs::render_session_badge`.

**Presence files.** Each plugin writes `zj-radar.presence.<zellij_pid>.json`
(`{session_name, running, attention, attention_tab_position,
updated_epoch_s}`) into the shared `/cache` root, temp-file plus atomic rename.
Writes are content-edge-gated (the timestamp is excluded from the compare) and
withheld while `own_session_name` is empty. `running` and `attention` count
live status-origin panes only; command activity is excluded. The local rail's
rows, header badge, and footer stay tab-level summaries.

**Liveness is the mtime, graded fresh → stale → dead.** A live session
rewrites its file at least every 60 s (`PRESENCE_HEARTBEAT_S`, a level trigger
in `project` that bypasses the content gate and, unlike the snapshot, the
visibility gate: a detached session is alive). Every tab's instance runs that
clock against the one pid-keyed file, so the heartbeat is a
`PersistPresence` with an `unless_fresher_than` window the host honors by
stat: a file already younger than a quarter of the interval is left alone —
one stat instead of N writes, and the quarter keeps the file's worst-case age
(one period plus one window) under the 90 s stale threshold. Content edges
write unconditionally. Peers read the directory on every
Slow (60 s) tick, and on every fifth Fast tick except mid-cycle; the Slow read
is what lets an idle rail grade a peer at all, since ages are captured at read
time. `Sessions::update_presences` grades each file's age: fresh (≤ 90 s),
stale (90–300 s: dimmed, unreachable
by cycling since switching onto a dead session would have Zellij resurrect it
as an empty zombie), dead (≥ 300 s, five missed heartbeats: reaped and the
file unlinked via `Effect::DismissPresence`). Dimming stays twitchy because it
is reversible; the reap waits for overwhelming evidence because it deletes a
file. A false reap after a machine-sleep wake is harmless: the live session's
next heartbeat republishes. The `✕` glyph is the manual dismiss for a
stale-not-yet-dead entry. A 6 h sweep at `load()` deletes unparseable debris
the reap cannot name.

**Why not `SessionUpdate`.** E2E against Zellij 0.44.3 showed `SessionUpdate`
only delivers peers after some plugin has called the blocking
`get_session_list()`, which nothing here does and which would violate §1.
Presence is peer-published and liveness is mtime-based; there is no roster to
keep in sync. The session's own name arrives push-style via
`Event::ModeUpdate`, and may legitimately be `None` before Zellij assigns one.

**Badge.** `Sessions::badge()` is re-derived on every call. It renders zero
lines while only the current session is known; from two entries on, one line
per session in the order shared with cycling: current first, then fresh peers
with attention by name, then the rest of the fresh peers, then stale peers (a
stale attention count is not actionable, so staleness outranks attention).
Each line shows the name plus running and attention counts when nonzero. The
current line carries an accent `•` and no click target. A pending cycle
selection renders bold; a stale entry renders dimmer with a `✕` hotspot.

**Hotspots.** The renderer attaches glyph metadata to the same `Line` records
that derive ANSI and targets, so they cannot desynchronize. `✕` on a stale
badge line; `✓` on a tab header or pane line with an unacknowledged
status-origin Pending. Only the glyph's own cells trigger. Right-click keeps
the same whole-row actions for parity but is blocked upstream
([zellij#5350](https://github.com/zellij-org/zellij/issues/5350)).

**Cycling.** `session-next` / `session-prev` on the `cmd.v1` pipe advance a
highlight through the shared order, wrapping, with stale peers excluded. A tap
only moves the highlight and sets a "tap landed" flag; `Sessions::tick` runs
on every timer fire while a selection is pending, and a fire whose interval
saw a tap clears the flag and skips. Only a fire whose whole interval was
tap-free commits, so the commit lands at least one quiet interval after the
last tap. The target is re-resolved by session name, never list position, so
peers joining or leaving mid-cycle cannot retarget it; a selection that went
stale is dropped. Landing back on the current session cancels. A commit emits
`Effect::SwitchSession { name, tab_position }`.

**Degradation.** No writable shared root means no presence file, no peer
reads, and no badge. Everything else is unaffected, the same way persistence
being unavailable degrades only rehydration.

## 14. Setup analysis (CLI)

`zj-radar setup` learns the world through pure `analyze_*(&Env) -> *Facts`
functions in `crates/cli/src/setup/analyze.rs`, each fed a thin `Env` of
already-read values by the IO shell. `Facts` is the single home for derived
facts: alias present (managed vs unmanaged), rail injected, grant present,
producer wired, Codex hooks/notify state, opencode plugin ownership, pi
extension ownership. Both
consumers project from it: `*_check_items` renders `--check`, and the install
orchestrators gate on it. The pure mutators (`edit_*` → `Outcome`) share only
the low-level detectors in `setup/detect.rs`. The legacy-notify vs hooks
choice is a flag the consumer projects on, never a fact.

## 15. Build and packaging

- Rust, `zellij-tile = "0.44"`, target `wasm32-wasip1`. The artifact is a
  binary crate, not `cdylib`: Zellij loads plugins as WASI command modules and
  calls `_start`, which `register_plugin!`'s generated `main` provides.
- Zellij runs one instance per tab per attached client (a detached client's
  instances linger hidden), each under the wasmi interpreter with its own
  linear memory, so per-instance fixed cost is paid N times. The
  release profile optimizes for size, `crates/plugin/build.rs` shrinks the
  WASI shadow stack (the bulk of initial memory) to 256 KiB, and the flake
  runs binaryen's `wasm-opt -Oz` over the release wasm, which executes
  measurably fewer instructions than rustc's output alone. Per-event
  interpreter cost is measured in fuel by `tools/wasm-fuel`
  ([`CONTRIBUTING.md`](../CONTRIBUTING.md#measuring-plugin-cost)).
- The dev loop never reloads in place (Zellij does not safely hot-reload
  layout-created plugins); every `just dev` is a fresh sandboxed session
  ([`CONTRIBUTING.md`](../CONTRIBUTING.md#dev-loop)).
- The flake builds the wasm hermetically with `crane`; `nix flake check` runs
  in CI. Consuming from Nix: [`install.md`](install.md#nix--home-manager).
- Releases: one tag ships the wasm, static CLI tarballs, and the crates
  ([`RELEASING.md`](../RELEASING.md)).
- `zj-radar update` moves the CLI and the wasm as one, because they share the
  status contract and the setup expectations, and a hand-run upgrade of only
  one half is the drift that broke installs before. It replaces the binary
  first, then re-executes the *new* binary for `setup zellij --download`, so
  the wasm it fetches is its own version; the resolved tag travels with the
  re-exec as `ZJ_RADAR_VERSION` so a release landing mid-run cannot split
  them. Nix and cargo installs are handed back to their tool rather than
  overwritten behind its record; `--check` grades the wasm by sha256 against
  the release sidecar, the only version signal a wasm file carries. Pins move
  forward only: refreshing the wasm to an older pin while the binary stays
  put would recreate the split.

## 16. Non-goals and follow-ups

- **Floating dashboard overlay.** The inline badge (§13) is a few rail lines,
  not a panel; the floating dashboard stays a non-goal.
- **Launchable floating mode** (`LaunchOrFocusPlugin`). A different product:
  an on-demand peek, not an ambient column, and it would turn the passive
  renderer into an interactive panel with `Key` handling and focus grabs.
- **Horizontal bar mode.** Would need a from-scratch compact renderer.
- **Collapse-to-strip toggle.**
- **Aider and other adapters.**
- **Terminal-signal detection** (`is_alternate_screen` / `is_raw_mode` on
  `PaneInfo`), the general interactive-program detector; see
  `activity-model.md` §4.
