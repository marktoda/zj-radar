# zj-radar — a native Zellij sidebar for AI-agent status

**Status:** living design — shipped; kept current as the implementation evolves
(originally approved 2026-06-26; revised after external review, the smart-tabs
removal — see `smart-tabs-postmortem.md` — and the focus-removal refactor)
**Author:** Mark Toda (with Claude)

> **Update (post-postmortem):** `zellij-smart-tabs` has been **removed entirely** from the
> Zellij setup after it melted down under a many-agent workload (poll-every-pane-on-every-output
> issuing blocking host calls on the server's single main thread — full writeup in
> `smart-tabs-postmortem.md`). This invalidates the earlier plan to *keep* smart-tabs for base
> tab naming. **zj-radar now owns all tab display, including naming** (see §6.1). The hard
> architectural constraint that follows: no polling, and no per-event or per-tick blocking
> host queries (`get_pane_running_command`, …) — signals come from pushed events
> (`pipe`, `TabUpdate`, `PaneUpdate`, `CwdChanged`) or the hook payload. The single
> exception is the once-per-pane `Effect::ResolveCwd` naming bootstrap: one blocking
> `get_pane_cwd` per freshly-opened pane, at pane-creation rate — never re-polled
> (`crates/plugin/src/runtime.rs`; `CONTEXT.md` → *Tab naming*).

## 1. Goal

Bring Cmux-style agent awareness into an existing Zellij setup without changing your
existing keybindings, swap layouts, or config. Specifically: an always-on
**left sidebar** that lists every tab and, for tabs running AI coding agents, shows
per-tab state (working / waiting-for-you / done / error) with color, plus repo/branch,
elapsed time, and the last message — and lets you click a row to jump to that tab.

Non-goals for v1 (parked): a separate floating cross-session dashboard overlay; Aider
support; replacing the bottom status-bar.

## 2. Background & key decision: why an explicit pipe channel (not OSC sniffing)

Cmux owns the terminal surface (it **is** the emulator, via libghostty), so it can combine
terminal-level OSC notification signals with explicit agent hooks. Reading its source
(`manaflow-ai/cmux`): the OSC path (libghostty decoding `OSC 9`/`99`/`777` and tagging the
emitting surface, `GhosttyTerminalView.swift:2911`) gives a free, agent-agnostic "something
dinged"; the working→waiting→done **status** still comes from **per-agent hooks** (16 agent
adapters in `CMUXCLI+AgentHookDefinitions.swift`, plus a `claude` wrapper binary that injects
hooks). So Cmux uses hooks for status too.

A Zellij plugin does **not** own the PTY; it receives a structured event API, not the raw
byte stream. Zellij *does* forward some notification OSCs (e.g. **OSC-99 desktop
notifications since 0.44.1**, PR #4931), but those are **transient "attention" events**, not
a durable, structured, per-agent lifecycle signal — they carry no `running`/`pending`/`done`
state, no repo/branch, no pane attribution suitable for our state model, and the plugin API
exposes no event for them anyway (the only output-side signal a plugin sees is the bell, as
a contentless per-tab `TabInfo.has_bell_notification` boolean). Scrollback APIs
(`PaneRenderReport`) don't contain OSC control sequences (they're consumed by the parser,
never become cells).

**Conclusion (version-robust):** even where terminal notification OSCs are forwarded, they
are attention signals rather than a status model. For `running`/`pending`/`done`/`error`,
repo/branch, pane attribution, elapsed time, and last message, the reliable seam is an
explicit adapter-owned `zellij pipe` payload delivered to the plugin's `pipe()` entrypoint.
This mirrors Cmux's real status path while fitting Zellij's plugin architecture.

## 3. What it looks like

```
╔═ agents ═══════════╗┌─ your panes ──────────────┐
║● 1 dotfiles         ║│                            │
║  main · done 2m     ║│   focused tab content      │
║  "refactored the…"  ║│                            │
║◐ 2 pinky      2/4   ║│                            │
║  fix/x · 0:14       ║│                            │
║  "running tests…"   ║│                            │
║◑ 3 api              ║│                            │
║  feat/y · needs you ║│                            │
║○ 4 notes            ║│                            │
╚════════════════════╝└────────────────────────────┘
 NORMAL  <p>ane <t>ab …   ← existing status-bar, untouched
```

- `✗` red = error · `◆` orange = waiting-for-you (pending) · `⠋` spinner = working ·
  `●` green = done · `○` dim = plain terminal (no agent). (The shipped glyph set —
  the sketch above shows the layout, not final glyphs; `docs/rail-reference.md` is
  the executable spec for the rendered rail.)
- **Since shipped, this sketch is stale beyond glyphs too:** the header is a
  single `RADAR` + rule line carrying a live `·N` tab-count / `n!` needs-you
  badge and a heartbeat sweep while any tab is `Running` — not the boxed
  single-word `agents` title drawn above — and a session with completion
  history but zero live tabs still renders that header plus a trailing
  `─ earlier ─` ledger region. `docs/rail-reference.md` is ground truth for
  the exact grid.
- **Status vocabulary:** the pipe sends raw values `running`/`pending`/`done`/`error`/`idle`;
  the renderer maps `running`→working, `pending`→waiting-for-you, `idle`/absent→plain.
  The *semantics* of what each status/kind pairing should look like — attention
  classes (Job/Service/Companion), interactive-command suppression, cadence
  rules — live in [`activity-model.md`](activity-model.md).
- Per-tab rows are a **header line plus one line per tracked pane**: line 1 =
  state glyph + **display tab number** + name; each pane line = tree connector +
  status glyph + kind mark + identity/activity (see `rail-reference.md` — the
  executable spec — for the exact grid; elapsed appears only as the per-pane
  pending wait tag and long-job run tag, rule 2 there).
- **Display tab number = `TabInfo.position + 1`** (see §6 — position is 0-indexed).
- Plain (non-agent) tabs render name only — agent decoration is purely additive. The name is
  `TabInfo.name` (from the layout or zj-radar' own push-based naming, §6.1) — **not** from
  smart-tabs, which no longer exists.
- Click a row → switch to that tab.

## 4. Architecture

Thin Zellij-host glue around a deep, pure runtime module + pure stores/models/rendering.
The per-agent adapter layer still lives *outside* the plugin (shell scripts / agent
config). The plugin runtime has no `zellij-tile` dependency (`zellij-tile` is
`cfg(target_arch = "wasm32")`-scoped): `lib.rs` translates raw
Zellij events into repo-owned inputs and applies ordered host effects returned by the
runtime.

The repo is a **3-crate virtual workspace**: `crates/core` (wire payload, command
classification, `Kind`, the bounded pipe argv — shared by producer and plugin),
`crates/cli` (the host-side `zj-radar` binary: `notify`, `setup`, `run`), and
`crates/plugin` (everything below — the wasm sidebar). Boxed filenames in the diagram
live under `crates/plugin/src/`.

```
┌ Agent adapters (per-agent, outside the wasm) ─────────────┐
│ Claude → plugin hook / native CLI  (running/pending/done) │
│ Codex  → native CLI via hooks.json (running/pending/done) │
└───────────────────────────┬───────────────────────────────┘
   zellij pipe --name zj_radar.status.v1 -- {v,source,pane,status,repo,branch,msg,task,ack}
   (BROADCAST by name — not --plugin: see §6; sent via the bounded self-limiting
   argv — see §5. The plugin itself is also a caller: the acknowledge gesture
   broadcasts a synthetic ack:true payload back onto this same pipe, §5)
                            │
                            ▼
┌ zj-radar plugin (Rust → wasm32-wasip1) ────────────────────────────────┐
│  lib.rs: Zellij adapter                                                │
│    raw Event/PaneInfo/TabInfo ⇄ repo-owned inputs/effects; owns        │
│    SessionFiles and applies returned effects                           │
│                                                                        │
│  runtime.rs: PluginRuntime                                             │
│    owns lifecycle state, permissions, timers, snapshot decisions,      │
│    naming, focus transitions, command activity, config pipes, and      │
│    mouse intent                                                        │
│    input: RadarTab/PaneUpdate/PermissionProbe/status/config/timer/     │
│    mouse/cmd-verb                                                      │
│    output: Outcome { render, effects: Vec<Effect> }                    │
│                                                                        │
│  session_files.rs: SessionFiles                                        │
│    owns per-session filesystem coordination across sidebar instances:  │
│    snapshot durability, permission marker/lock, root fallback,         │
│    pruning, presence files, notification claims                        │
│                                                                        │
│  radar_state.rs/rollup.rs/tab_namer.rs: state + tab model              │
│    StatusStore (status_store.rs) + CommandStore + roll_up(tab) +       │
│    TabNamer (observed argv classified by crates/core's command.rs)     │
│                                                                        │
│  ledger.rs / sessions.rs / presence.rs: completion history ring;       │
│    cross-session peer state + the per-session presence record (§13)    │
│                                                                        │
│  control.rs / config.rs / theme.rs: cmd-verb + config pipes, colors    │
│  permission.rs / clock.rs / notify_rules.rs: grant state machine,      │
│    timer-chain bookkeeping, desktop-notification edge rules            │
│                                                                        │
│  render.rs: pure rail renderer                                         │
│    render_rail(rows, ledger, opts) -> RenderedRail                     │
│    (target_at_line / hotspot_at; onboarding + needs_permission faces)  │
│    owns layout, overflow, ANSI, click-target + hotspot materialization │
└────────────────────────────────────────────────────────────────────────┘
        │ Effects (runtime.rs): SwitchTab, ShowPane, RenameTab, RequestPermission,
        │ SetTimeout(Fast|Slow), SetSelectable, PersistSnapshot,
        │ PersistPermissionMarker, HeartbeatPermissionLock, ResolveCwd (the
        │ once-per-pane cwd bootstrap), Notify, PersistPresence, ReadPresences,
        │ SwitchSession, BroadcastStatus (the ack echo), CloseSelf
        ▼  (Notify hands osascript/notify-send to the host via run_command — §12)
```

**Design principle:** keep host-coupled code thin; push lifecycle decisions into
`PluginRuntime`, filesystem coordination into `SessionFiles`, and layout/click decisions into
`RenderedRail` so the core is unit-testable with `cargo test`. The adapter should not contain
behavior beyond translating host data, owning the session-files module, and performing returned
effects. The real external seam remains the **pipe payload schema** (versioned).

### 4.1 Lifecycle state machine

| Source event                                  | Status    |
|-----------------------------------------------|-----------|
| Claude `UserPromptSubmit` / `PreToolUse` / `PostToolUse` | `running` (`PostToolUse` usually duplicates `PreToolUse`'s payload — the plugin no-ops identical re-broadcasts — but it is the Pending→Running recovery edge after a mid-turn permission answer, so it stays registered) |
| Claude `Notification` (`permission_prompt` / `elicitation_dialog` matchers) | `pending` |
| Claude `SubagentStop`                         | `running` (a finished subagent means the main turn is still going) |
| Claude `Stop`                                 | `done` — **except** a Stop whose last assistant message ends in a question maps to `pending` (a turn that ends by asking is blocked on input, not finished; the trailing question becomes the message) |
| Claude `SessionStart` (`source:"clear"` only) | `idle` (resets a stale row on `/clear`) |
| Claude `SessionEnd`                           | `idle` (a closed session recedes instead of freezing its last status) |
| Codex `UserPromptSubmit` / tool hooks / subagents | `running` |
| Codex `PermissionRequest`                     | `pending` |
| Codex `Stop`                                  | `done`    |
| Codex ephemeral-fork hooks (`transcript_path: null`) | ignored (the main turn keeps owning the pane) |
| Codex legacy `agent-turn-complete`            | `done`    |
| Observed command exiting nonzero              | `error` — the only `error` source. Claude's map deliberately has no `error`: its hook vocabulary carries no reliable turn-level failure signal (`PostToolUse`'s per-tool `is_error` is normal, recoverable agent behavior), so mapping hooks to `Error` would paint healthy turns red |
| Agent pane returns to its shell prompt (observed exit) | `idle` — terminal statuses clear at once; a `Running` instead arms the 15-tick stale grace (see §6, *Running exit grace*) |
| Agent pane's root process exits (pane manifest `exited`) | `idle` — definitive producer death: clears even a `Running` immediately, no grace (see §6, *Running exit grace*) |

> **Update (focus no longer drives state):** an earlier design cleared a pushed
> completion when you *focused* the pane (`on_focus`). Focus is per-client and is
> not delivered to background plugin instances, so that cleared the row only on the
> tab you were viewing and left every other tab stale. A finished status now clears
> only via shared signals — a new broadcast, the observed return-to-shell exit-clear
> (`command::is_shell_prompt` → `StatusStore::clear_on_prompt_return`), or a prune —
> which every tab's instance receives, so all tabs converge. The former `on_focus` wire
> field is tolerated and ignored — the parser has no such field, so serde drops it
> unread like any unknown key (same story as the legacy `seq`).

### 4.2 Per-pane → per-tab aggregation

Some layouts (e.g. `compact.kdl`) have multi-agent-in-one-tab shapes (`quad-grid` = 4 Claude panes in one
tab), so tab state cannot come from names. The store keys by `PaneId`; `PaneUpdate`'s
`PaneManifest` gives the pane→tab map. Tab aggregation:

- **Severity order (highest wins):** `error > pending > running > done > idle/absent`.
  (`error` is highest so failures never hide behind "working".)
- **Count:** `total` = panes in this tab that have *ever* reported a non-idle agent state and
  still exist; `done` = those whose current status is `done`. Render as `done/total` when
  `total > 1`.
- **Primary detail (which pane's repo/branch/msg summarizes the tab):** the
  highest-severity pane; on equal severity a bounded *job* outranks a *service*
  (a spinning build beats a merely-up dev server — `activity-model.md` §3);
  remaining ties by most-recent `last_change_tick`.
- **Pruning (with a one-manifest grace):** on each `PaneUpdate`, drop state for `PaneId`s
  no longer present, so closed agents leave no ghost status — but a pane's *first* absence
  is held, not pruned (`absent_once`): Zellij's break-pane family reports session state
  while a moved pane is extracted and in no tab, so a single absence can't distinguish a
  close from a mid-move flash. A pane still absent on the confirming next manifest prunes
  then, ledgered under the tab identity captured at first absence; a reappearing pane
  simply drops out of the grace set.

## 5. The pipe contract (producer ↔ plugin seam)

Broadcast by name `zj_radar.status.v1` (namespaced + versioned). Each sidebar instance
filters on the name and keeps its own copy of the state map (same pattern as the built-in
tab-bar; cheap for a handful of tabs).

**Newcomer rehydration (session snapshot).** Because the plugin lives in the tab template,
Zellij runs *one instance per tab*, and a broadcast only reaches instances alive when it is
sent — it is never replayed. So a tab opened after agents were already running would spawn a
blank instance and render every tab idle. Fix: each instance mirrors its `RadarState` stores into a
snapshot on every state *edge*, and seeds itself from it in `load()`. Writes are
edge-gated, not per-mutation: intakes report `SnapshotWrite::{Now, Deferred}` — a
label-only Running→Running update on an animating row defers its write to ride the next
Fast tick's flush instead of hitting disk per tool hook (an inline `Now` on the same pass
supersedes the deferral). `SessionFiles` chooses
the persistence root: `/cache` first, because Zellij 0.44 mounts it as the plugin-URL-scoped
folder shared across all instances, then `/tmp/zj-radar`, then disabled persistence if neither
root is writable. `/data` is not used because it is scoped by `<plugin_id>-<client_id>` and is
removed on plugin unload. Snapshot names are session-scoped by the Zellij server pid; temp files
also include `plugin_id` so concurrent writers never clobber each other's in-progress write.
Writes are temp-file + atomic rename, so a concurrent newcomer never reads a torn file; since
every live instance writes identical content after a given broadcast, the races are benign and
any stale seed self-heals on the next broadcast. If persistence is disabled, the plugin still
runs; late-spawned sidebars start empty until the next broadcast. The producer (hooks) is
unaffected.

```json
{ "v": 1,
  "source": "claude",                 // Kind vocabulary: claude | codex | opencode | gemini | command |
                                      //   test | build | deploy | server | other (unknown → other);
                                      //   the instrumented-agent set is exactly {claude, codex, opencode}
  "pane": { "type": "terminal", "id": 12 },   // typed to match Zellij's PaneId enum
  "status": "running",                // running | pending | done | error | idle
  "repo": "pinky",
  "branch": "fix/x",
  "msg": "running tests…",           // truncated last assistant message
  "task": "fix the flaky auth test",  // optional, sent only on UserPromptSubmit
  "ack": false }                      // optional (absent = false): "user already saw this" —
                                      //   state converges, notifier stays silent
```

**The plugin is a producer too (the pipe is not strictly one-way).** The rail's
acknowledge gesture (`✓` hotspot) does not mutate local state — it broadcasts a
*synthetic* `status.v1` payload (the pane's Pending re-sent as `done` with
`ack: true`) back onto this same pipe (`Effect::BroadcastStatus`). Every tab's
instance — including the sender — converges through the normal `status_pipe`
intake when the pipe echoes it back, and `ack: true` keeps the notifier silent
in every instance the echo reaches.

**Bounded sends (every caller, plugin included).** `zellij pipe` is a
backpressure channel: the client blocks until every loaded plugin instance
consumes the message, and an instance wedged at Zellij's permission prompt
holds it forever — at hook rate, unbounded sends once cascaded into an EMFILE
crash of the whole session. All senders use the self-limiting `sh` subtree
from `crates/core/src/pipe.rs` (`self_limiting_pipe_argv`): a detached
sleep+kill watchdog inside the spawned subtree reaps its own hung client even
when the caller is killed mid-send. Deadlines are status-keyed —
`DEFAULT_PIPE_TIMEOUT_SECS` (5s) for once-per-turn edges,
`RUNNING_PIPE_TIMEOUT_SECS` (2s) for `running` heartbeats — and the bundled
hooks' `timeout` values must clear the cap plus 2s slack (welded by
`hooks_manifest_tests.rs`).

**Plugin-side handling (defensive — the renderer/store, not the adapter, enforces these):**
- Match `pane` to `PaneId::Terminal(id)`. Adapters derive `id` by stripping any `terminal_`
  prefix from `$ZELLIJ_PANE_ID` (its form has varied across Zellij versions).
- Tolerate malformed/older/partial payloads: unknown fields ignored (including a
  legacy `seq` from older producers), missing fields default, unknown `status` →
  treated as `idle`.
- `task` (optional): sticky task label — empty/absent leaves the stored label
  unchanged, non-empty replaces it; the plugin clears it on idle and on
  return-to-shell.
- Ordering is latest-wins: the pipe delivers in order and no producer stamps a
  sequence, so a payload simply overwrites the pane's prior state.
- Sanitize `repo`/`branch`/`msg`: strip ANSI/control chars, convert newlines to spaces, cap
  `msg` to a fixed length before rendering.
- Ignore payloads over a fixed size cap (e.g. 64 KB).
- The former `on_focus` hint is tolerated and ignored — dropped unread like any unknown
  key (see §4.1 update): `done` no longer auto-clears on focus. A finished status persists
  until a new broadcast, the observed return-to-shell exit-clear, or a prune — all shared
  signals, so all tabs converge.

## 6. Plugin ↔ Zellij wiring

- **Permissions:** `ReadApplicationState` (tab/pane state), `ReadCliPipes` (broadcast),
  `ChangeApplicationState` (`switch_tab_to`, `rename_tab`), and `RunCommands` — the plugin now
  owns OS desktop notifications and hands each one to the host via `run_command` (see §12; a
  reversal of the original "notifications stay in the adapters, no `RunCommands`" stance). When
  the grant is absent, `run_command` is a silent host no-op, so notifications simply don't fire.
  Keep the pane selectable only until `PermissionRequestResult` arrives so the first-run
  permission prompt is reachable; then call `set_selectable(false)` so the pane never steals focus
  from pane keybinds.
  - **Per-tab prompt coordination:** the sidebar is instantiated once per tab. On an uncached
    first run, `SessionFiles` uses a session-scoped lock to elect one instance to call
    `request_permission()`; peer instances stay passive, poll a marker, then request after Zellij
    has cached the answer for this plugin URL. This avoids one y/n prompt per tab while preserving
    Zellij's explicit permission UI. If session files are unavailable, coordination degrades to
    the old behavior rather than blocking startup.
- **Subscriptions:** `TabUpdate`, `PaneUpdate`, `CwdChanged`, `CommandChanged`, `Timer`,
  `Mouse`, `PermissionRequestResult`, `ModeUpdate` (carries `ModeInfo.session_name` — the
  push-style session-name source §13 depends on).
- **Tab index footgun:** `TabInfo.position` is **0-indexed**; `switch_tab_to(idx)` is
  **1-indexed** (0 treated as 1). Define `display_tab_number = position + 1` and use it for
  *both* rendering and click → `switch_tab_to(position + 1)`.
- **Click targeting:** `render_rail()` emits ANSI plus a same-height target map (and a
  same-height hotspot map — `CONTEXT.md` → *Lockstep*). Header, folded-idle strip, and
  external gap rows map to nothing; a tab's header line maps to that tab (`SwitchTab`);
  pane lines map to their pane (`ShowPane`) — a multi-pane tab's tree lines *and* a
  single-pane tab's pane line and line-2 detail line, which carry that tab's one tracked
  pane's `pane_id`. There are no collapse rows: the only fold constructs are the `+N more`
  overflow line at the bottom of an over-tall multi-pane tree and the `+N idle ▾` strip.
  The runtime stores the latest `RenderedRail` and returns `SwitchTab` / `ShowPane` /
  `SwitchSession` effects on mouse clicks instead of replaying layout math in the host
  glue.
- **Why broadcast, not `--plugin`:** broadcasting by name means adapters never create UI
  panes, never need to know the plugin's URL/config identity, and naturally reach every
  already-running sidebar instance. (A `--plugin` destination can also load the plugin if not
  running and the routing across multiple same-plugin instances is fiddly — avoid it here.)
- **Timer is one-shot** in Zellij: re-arm each tick. The shipped model is a two-speed
  cadence plus full disarm (`Cadence::{Fast, Slow}` → `set_timeout(1.0)` /
  `set_timeout(60.0)`, or no re-arm at all — `PluginRuntime::desired_cadence`): Fast while
  anything tick-windowed is live (animation, an un-carried completion edge, the permission
  flow, a pending session-cycle selection), Slow while only minute-granular ages are still
  changing or a presence heartbeat is owed, disarmed only pre-session-name or when
  permission is denied. A backgrounded `done`/`error`/`pending` row is terminal: once its
  one-shot settle has run it does **not** keep the Fast loop alive, so an idle-but-lit
  rail stops waking every second; the loop re-arms on the next pipe/PaneUpdate. The full
  trigger lists live in `CONTEXT.md` → *Cadence*. A companion **rows-diff render gate**
  (`last_render_key`) drops any requested repaint whose content-derived key (rows, ledger
  lines, badge, theme) equals what the last `render()` actually drew — `CONTEXT.md` →
  *Render gate*.
  - **Running exit grace (the producer-death model).** The model:
    (agent argv present, pane-manifest `exited` = false) = alive;
    `exited` = dead. A pushed `Running` row starts its 15-tick stale
    grace (`RUNNING_SUSPECT_GRACE_TICKS`) only on a `CommandChanged` whose
    effective argv identifies a shell
    prompt. Zellij reports the childless pane-root shell with
    `is_foreground: false`, so that shell-name form is the real prompt-return
    edge and must clear terminal statuses/arm Running expiry. A non-shell
    wrapper argv with `is_foreground: false` remains weak evidence and never
    starts the clock: a live agent's child-process transition must not be
    mistaken for exit. The agent's argv *reappearing* as the pane's foreground
    cancels an armed clock (`StatusStore::cancel_running_suspect`) — the
    flicker resolved as a flicker; a clock that outlives the window is cleared
    by `expire_stale_running` (the producer died mid-turn and will never send
    the clearing broadcast). The pane manifest's `exited` flag bypasses all of
    this: it is *definitive* death — an agent-rooted pane
    (`zellij run -- claude`) never shows a shell prompt at all — so
    `StatusStore::clear_on_exit` clears even a `Running` immediately, no grace.
- **Layout — the integration seam.** The sidebar is a pinned, borderless left column *inside* a
  vertical split, *outside* `children`, so `swap_tiled_layout` cycling never disturbs it (same
  mechanism as the existing bars; 0.44.3 has the pop-out fix). The layout layer is the *only*
  native place Zellij pins a pane into every tab (its own bars live there too) — so radar
  integrates exactly like [zjstatus](https://github.com/dj95/zjstatus): the user adds a pane to
  their templates. `zj-radar setup zellij --wasm <path>` installs the wasm at
  `~/.config/zellij/plugins/zj_radar.wasm` and manages a **plugin alias**
  (`plugins { radar location=… }` in `config.kdl`) so layouts reference the bare name `radar`,
  keeping the per-layout snippet path-free and letting users compose the node into *their* layout
  (L/R, any width) rather than adopting ours.
  ```kdl
  default_tab_template {                       // layout-defined tabs fill `children`
      pane split_direction="vertical" {
          pane size=32 borderless=true { plugin location="radar" }
          children
      }
      pane size=2 borderless=true { plugin location="zellij:status-bar" }
  }
  new_tab_template {                           // runtime tabs (Ctrl+t n) need a CONCRETE pane
      pane split_direction="vertical" {
          pane size=32 borderless=true { plugin location="radar" }
          pane focus=true
      }
      pane size=2 borderless=true { plugin location="zellij:status-bar" }
  }
  ```
  - **`new_tab_template` is mandatory, not optional.** A left column forces `children` to nest
    inside a split. When no `new_tab_template` is given, Zellij *derives* one from
    `default_tab_template` and **drops the nested `children`** (upstream
    [zellij#3247](https://github.com/zellij-org/zellij/issues/3247), open) — the new tab then has
    only borderless plugin panes, no focusable terminal, and keystrokes fall through ("can't open
    a new tab"). The explicit `new_tab_template` with a concrete `pane focus=true` sidesteps the
    derivation. A *top-level* `children` (stock compact layout) materializes fine; only the
    nested-in-a-split case is affected.
  - The top `compact-bar` line is removed (the sidebar replaces it); the bottom `status-bar`
    (mode/keybind hints) is kept. A future `MOD+a` `MessagePlugin` keybind can toggle collapse.

### 6.1 Tab naming (zj-radar owns it — smart-tabs is gone)

smart-tabs used to auto-name every tab `git-root + program` by polling
`get_pane_running_command()` / `get_pane_cwd()` on every dirty tick — the exact pattern that
melted the session (`smart-tabs-postmortem.md`). zj-radar must **not** reproduce that. The
replacement is push-driven and tiered — the one deliberate exception is the
**once-per-pane cwd bootstrap** (`Effect::ResolveCwd`): a freshly-opened pane has not yet
emitted `CwdChanged`, so the runtime requests a single blocking `get_pane_cwd` for it,
gated to at most once per pane id — pane-creation rate, never a re-poll (the result feeds
the normal `cwd_changed` path):

- **v1 (default — no naming work in the plugin):** tab names come from the layout's `tab name=…`
  and any manual `MOD+r` renames; zj-radar reads them via `TabInfo.name` and renders them
  verbatim. For *agent* tabs the rich context smart-tabs used to encode in the name
  (repo/branch/program) is already shown on the sidebar's second/third lines, so the tab name is
  no longer load-bearing. This ships zero regression risk and zero added host calls.
- **v1.x (optional auto-naming, push-sourced only):** if generic names on plain tabs feel like a
  regression, derive names from **events we already receive**, never from queries:
  - *Agent tabs* — the hook payload already carries `repo`; on a status change, optionally
    `rename_tab(position+1, repo)`. `rename_tab` is a fire-and-forget `ChangeApplicationState`
    action (no blocking return), and it fires only on change, not per tick — so it cannot
    recreate the poll storm.
  - *Plain tabs* — subscribe to **`CwdChanged`** (pushed) to learn a pane's cwd → git-root
    basename; read program from **`PaneInfo.title`** in the `PaneUpdate` manifest we already
    consume. Both are push signals; the only `get_pane_*` call anywhere is the once-per-pane
    `ResolveCwd` bootstrap above.

  Guardrails: only `rename_tab` when the derived name actually differs (avoid redundant
  main-thread work), and treat naming as best-effort cosmetics — a missing cwd/title just leaves
  the existing name.

## 7. Agent adapters (v1: Claude + Codex + Opencode)

- **Claude Code** — a Claude plugin (`plugins/zj-radar-claude/`) whose `scripts/notify.sh`
  broadcasts the rich `zj_radar.status.v1` payload (computing repo/branch/msg/pane). Claude
  supports the full state set (`running` via UserPromptSubmit/Pre/PostToolUse, `pending` via
  Notification, `done` via Stop). The bundled hooks auto-register — no `settings.json` editing.
- **Codex CLI** — `zj-radar setup codex` installs marker-owned command hooks in
  `~/.codex/hooks.json`; Codex sends hook JSON on stdin and `zj-radar notify codex`
  maps lifecycle events to `running`/`pending`/`done`. The legacy single-slot
  `config.toml` `notify` path remains available behind `--legacy-notify` for older
  Codex installs and can only emit `done`.
- **Aider** — parked (one-line `--notifications-command`, status-only) for a later phase.

## 8. Build & packaging (Nix)

- Rust, `zellij-tile = "0.44"` (pinned to 0.44.3), target `wasm32-wasip1`.
  **Note:** the artifact is a *binary* crate, not `cdylib` —
  Zellij loads plugins as WASI command modules (it calls `_start`, which
  `register_plugin!`'s generated `fn main` provides); a cdylib reactor has no
  `_start` and won't load. See the comment block in `crates/plugin/src/main.rs`.
- **Dev loop:** `just dev` builds the release wasm + CLI and drives the real
  `zj-radar run` flow (grant onboarding included) in a sandbox —
  `ZJ_RADAR_DATA_DIR`/`ZJ_RADAR_WASM` root the run-owned config and plugin
  under `target/dev/data`. Every iteration is a fresh, uniquely named
  `zj-radar-dev-<hhmmss>` session (exited leftovers swept, live sessions never
  killed), never an in-place reload: Zellij 0.44 does not safely
  hot-reload layout-created plugin panes (`start-or-reload-plugin` opens a
  second pane instead).
- **Nix:** the repo's flake builds the wasm hermetically with `crane`
  (`nix flake check` exercises it in CI). For consuming the plugin from Nix /
  home-manager — packaging the release wasm and pointing the `radar` plugin
  alias at the store path — see [`install.md` → Nix / home-manager](install.md#nix--home-manager).

## 9. Testing

Pure-function `cargo test` (runtime/renderer/store/aggregation are pure and warning-free on
the host target):

1. **Tab index:** `TabInfo.position = 0` renders as tab `1`; click calls `switch_tab_to(1)`.
2. **Pane-close pruning:** state for a removed `PaneId` disappears on the next `PaneUpdate`.
3. **Tab reorder:** click targets the current `position`, not a stale cached row.
4. **Payload safety:** huge messages, embedded newlines, ANSI escapes, invalid-UTF-8-ish input,
   unknown `status`, oversized payloads — all handled without panic.
5. **Unicode width:** dots/ellipsis, branch names with emoji/CJK, narrow widths.
6. **Focus inertness:** a legacy `on_focus` field (or `seq`) riding a payload is
   tolerated on the wire and changes nothing — focus never drives rail state.
7. **Aggregation severity:** `error > pending > running > done > idle`.
8. **Count semantics:** `done/total` over panes that ever reported non-idle and still exist.
9. **Idle rendering:** a tab whose agent went idle does not look like an active agent tab.
10. **Broadcast filtering:** unrelated pipe names are ignored.
11. **Timer rearm:** elapsed increments across repeated one-shot timers.
12. **Runtime effects:** permission ownership/peer waiting, config/status pipes, snapshot writes,
    command debounce, tab renames, and click-to-tab/click-to-pane effects are asserted as ordered
    `Outcome` values.
13. **Renderer target map:** `RenderedRail` line count matches emitted ANSI lines, and headers,
    gaps, tab rows, pane rows, and the `+N more` / `+N idle ▾` fold lines resolve to the
    intended target (or a deliberate `None`).
14. **Snapshot renders:** no agents, mixed states, narrow-width truncation, many tabs,
    multi-agent tab.

Manual integration (Phase 2, a "fake agent" before real hooks):
```sh
zellij pipe --name zj_radar.status.v1 -- \
  '{"v":1,"source":"test","pane":{"type":"terminal","id":12},"status":"running","repo":"demo","branch":"main","msg":"hello"}'
```

## 10. Phasing

| Phase | Deliverable |
|---|---|
| 0 | Scaffold: cargo + zellij-tile + permissions + dev layout; renders a static sidebar |
| 1 | Real tab list from `TabUpdate` (names, **display numbers = position+1**, active highlight, click→`switch_tab_to(position+1)`). Replaces compact-bar. **No agent state yet.** |
| 2 | Consume `zj_radar.status.v1` broadcast (start with the **fake shell adapter** above to isolate plugin bugs from hook bugs); per-pane store + per-tab aggregation + pruning; state-color dots. Then extend Claude adapter payload; add Codex (`done`-only) adapter. |
| 3 | Rich second line: repo/branch, elapsed (one-shot Timer), truncated last message. **Sanitization/truncation lives in the renderer**, not the adapter. |

v1 = through Phase 3. Phase 1 alone is already a usable sidebar.

**Phase 1 acceptance criteria (verify before building further):**
- Sidebar stays pinned across `swap_tiled_layout` cycling.
- **A borderless, non-selectable sidebar still receives `Mouse` click events** (Zellij's mouse
  docs phrase events as "while focused on a plugin pane"; the built-in bars appear to handle
  clicks while non-selectable, but do not assume). If clicks don't arrive, fall back to: make
  it selectable and immediately return focus, or bind tab-switch to a keybind.
- Tab numbering is correct (`position + 1`).
- Width 24 is tolerable in the real swap layouts.
- With `compact-bar` **and** smart-tabs both removed, the sidebar is the only tab UI: every tab
  is still identifiable by `TabInfo.name` (layout/manual), and no naming/status behavior that was
  actually in use is lost (agent context now lives on the sidebar's detail lines, §6.1).

## 11. Risks (all bounded)

1. **Mouse clicks vs `set_selectable(false)`** — explicit Phase 1 acceptance test above; clear
   fallback if clicks don't arrive.
2. **Sidebar staying pinned across `swap_tiled_layout` cycling** — same mechanism as existing
   bars + 0.44.3 pop-out fix. Verify in Phase 1.
3. **Left column eats width** from percentage-split swap layouts — width 32 chosen
   deliberately; collapse toggle (future) mitigates.
4. **`zellij-tile` API churn** — pin to 0.44.x; read `PaneInfo`/`TabInfo` field ordering and the
   `PaneId` enum against the 0.44.3 tag.
5. **Per-tab plugin instances** (N timers + N state copies) — the Fast/Slow/disarm cadence
   (§6; `CONTEXT.md` → *Cadence*) and the rows-diff render gate bound the timers and
   repaints, and the state copies are reconciled through `SessionFiles`
   (see §5 "Newcomer rehydration"). The trap here, learned the hard way: a broadcast is *not*
   replayed to instances spawned later, so a new tab's instance starts blank — hence the snapshot
   seed. Note `/data` is per-instance (`…/<plugin_id>-<client_id>/`) despite the docs calling it
   "shared"; `/cache` (`…/plugin_cache/`) is the genuinely shared one in Zellij 0.44, with
   `/tmp/zj-radar` as a degraded fallback.
6. **Repeating the smart-tabs meltdown** (`smart-tabs-postmortem.md`) — bounded *by design*:
   zj-radar is push-driven (hook `pipe` + `TabUpdate`/`PaneUpdate`/`CwdChanged`) and issues no
   per-event or per-tick blocking `get_pane_*` queries, so high-output panes cost it nothing
   and there is no poll loop to storm the server's main thread. The standing rule — no
   polling, no blocking host calls on any per-event/per-tick path; the single exception is
   the once-per-pane `ResolveCwd` cwd bootstrap (§6.1) — keeps it that way; any future
   naming/program feature must stay event-sourced (§6.1).

## 12. Out of scope (follow-ups)

- Floating cross-session **dashboard** overlay (`MOD+a`). Distinct from the
  inline cross-session **badge** shipped in §13 — the badge is a few
  additional lines in the existing rail, not a separate panel; the floating
  dashboard non-goal stands unchanged.
- **Aider** (and other) adapters; richer **Codex** lifecycle (running/pending) via a wrapper.
- Collapse-to-strip toggle. (Per-pane breakdown within a multi-agent tab **shipped**: a
  multi-pane tab renders one line per tracked pane as a tree under the tab header — see §3
  and `rail-reference.md`.)
- Moving notification logic into the plugin. **Update:** the plugin now owns OS desktop
  notifications (macOS `osascript`, Linux `notify-send`). Rationale: single plugin install provides a standard,
  user-configurable notification surface (via `notify*` KDL keys) that survives across agent
  adapters — reversing the prior assumption that notifications belong in shell adapters alone.
  This trade-off is stable: adapters delegate OS delivery to the plugin while owning their own
  pipe payload schema and lifecycle logic.
- **Keybinds, the passive way** — **shipped.** A Zellij `MessagePlugin` binding
  delivers a verb to the `zj_radar.cmd.v1` pipe — `attention-next` /
  `attention-prev` / `session-next` / `session-prev` (`control.rs`; operator
  docs in `docs/configuration.md`; session cycling in §13) — handled in
  `pipe()` exactly like `config.v1`. This keeps the plugin a passive renderer
  (no `Key` subscription, no focus grab), unlike a `LaunchOrFocusPlugin` panel.
- **Launchable floating mode** (`LaunchOrFocusPlugin` keybind, zero layout change) — *deliberate
  non-goal.* It's a different product: an on-demand *peek* (current tab only), not the always-on
  ambient column radar exists to be, and it overlaps `room`/session-manager. It would also force
  the plugin from a pure passive renderer (`set_selectable(false)`, no `Key` subscription,
  mouse-click only) into an *interactive panel* — `Key` handling, dismiss (Esc/Enter), selection
  state — roughly doubling its surface area and reintroducing the focus-grab failure class. If
  ever revisited, it should be a separate, opt-in render/interaction mode, not the default seam.
  A focused first-run/help overlay could be useful for explaining the status lifecycle and any
  future keybinds; the permission grant still has to flow through Zellij's own prompt. Today the
  best install-time approximation is launching the same stable plugin URL once in a roomy floating
  pane, approving it there, then starting the per-tab sidebar layout.
- **Horizontal/compact bar mode** (top-level pane like zjstatus, no nesting, no #3247) — would
  need a from-scratch compact renderer; `render.rs` is vertical/card-per-tab today.

## 13. Cross-session badge & session cycling

Cross-session awareness — one session's rail showing counts for every other
zj-radar session on the same host, with click/cycle-to-switch — added
without ever calling Zellij's session list. Pure state in `sessions.rs`
(`Sessions`/`Presence`), file IO in `session_files.rs`, wiring in
`runtime.rs`; render in `render.rs::render_session_badge`.

**Presence files.** Each session's plugin writes
`zj-radar.presence.<zellij_pid>.json` — `{session_name, running, attention,
attention_tab_position, updated_epoch_s}` — into the same plugin-URL-scoped
`/cache` root persistence already uses (§5's "Newcomer rehydration"; same
temp-file + atomic-rename write discipline). Writes are content-edge-gated:
`project` diffs the freshly computed `Presence` against the last one
actually published, with `updated_epoch_s` zeroed out of the compare (the
same "write on edges only" rule `PersistSnapshot` follows) — so the clock
ticking alone on an unchanged session never re-writes the file. Withheld
entirely while `own_session_name` is empty (see below), since an unnamed
presence file is useless to a peer.

`running` and `attention` are live status-origin pane counts. `running` counts
live panes whose pushed status is `Running`; `attention` counts live panes whose
pushed status needs the user. Command-origin activity is deliberately excluded,
and panes not present in the current topology do not count. This pane-level
accounting is scoped to cross-session presence only: the local rail row rollups,
header badge, and footer tally remain tab-level summaries.

**Liveness heartbeat + staleness (fresh → stale → dead).** Peers never
call `SessionUpdate`/`get_session_list` to learn who's out there (see "Why
not `SessionUpdate`" below) — liveness is read from the filesystem, not
asked for. Peer sessions re-read the shared directory only on Fast (1 Hz)
timer fires (`Effect::ReadPresences`; never on the Slow heartbeat — and
within Fast only every 5th tick, except mid-cycle, since peers heartbeat at
60s and dim at 90s). The read side rests on a write-side guarantee: a live
session refreshes its presence file's mtime at least every 60s — the
**liveness heartbeat**, `project`'s level trigger (`PRESENCE_HEARTBEAT_S`)
that re-emits `PersistPresence` on any pass through `project` once the
last write is 60s old, bypassing the usual content-edge gate. Without it, a
session sitting fully idle (no count change) would let its mtime age and
read as gone while still alive; with it, file age is a reliable liveness
signal, which is what lets peers act on it. `read_peer_presences` returns
every peer file it finds, unconditionally, each paired with its file's
mtime age; `Sessions::update_presences` grades that age into a per-entry
ladder: **fresh** (≤`STALE_AFTER_SECS`, 90s), **stale** (90–300s — dimmed
on the badge and unreachable via `session-next`/`session-prev`, but still
shown), **dead** (past `DEAD_AFTER_SECS`, 300s — five missed heartbeats:
reaped from the badge, and the runtime emits `Effect::DismissPresence` to
unlink the file, so every instance converges). The 90/300 gap is
deliberate: dimming is cheap, self-correcting cosmetics and should stay
twitchy; a reap also deletes a file, so it waits for overwhelming evidence.
(This supersedes the task-14 "never-vanish roster", which predated the
heartbeat's write guarantee — back then an old mtime could mean merely
"idle", so nothing was ever dropped.) A false reap — e.g. right after a
machine-sleep wake, when every file looks old for up to one heartbeat — is
harmless: dismissal is non-destructive, the live session's next heartbeat
republishes and the entry returns fresh. Right-click dismiss remains the
manual path for a stale-but-not-yet-dead entry the user already knows is
gone. Separately, a `6h` sweep (`PRESENCE_MAX_AGE`) deletes abandoned
presence files at plugin `load()` — the backstop for debris the reap can't
touch: malformed/unparseable files, which never produce a name to dismiss
by. (A dead corpse carrying the current session's own name is no longer in
that bucket: the on-disk dismiss spares the live file by *path* identity —
`remove_presences_matching`'s own-file exclusion — so the auto-reap handles
own-name corpses like any other dead entry.)

**Own session name.** Zellij's `Event::ModeUpdate` carries
`ModeInfo.session_name`; the plugin already subscribes to `ModeUpdate` for
other reasons, and `session_name_changed` threads the field straight into
`own_session_name`. This is push-style and can legitimately arrive `None`
before Zellij has assigned the session a name yet — handled as a true no-op,
not an error.

**Why not `SessionUpdate` (or `get_session_list`).** An earlier iteration of
this feature subscribed to `Event::SessionUpdate` and cross-checked a peer
roster against presence files to decide who's live. E2E testing against real
Zellij 0.44.3 proved the roster idea itself was broken: `SessionUpdate` only
delivers peers after some plugin has called the blocking `get_session_list()`
host function (in practice, only the built-in session-manager plugin does) —
so a sidebar with no session-manager pane running never sees peers via that
event at all. Polling `get_session_list()` from the plugin to force it would
reintroduce exactly the blocking-host-query shape the whole plugin exists to
avoid (`smart-tabs-postmortem.md`). The fix (task-8b) drops `SessionUpdate`
entirely: liveness is derived purely from the presence files' own mtimes,
and the session's own name arrives push-style via `Event::ModeUpdate`
instead of a session-list lookup. Net effect: presence is entirely
peer-published and liveness is entirely mtime-based — whatever a
Fast-cadence directory read hands back IS the peer set for that tick,
graded fresh/stale/dead from that same mtime (`Sessions::update_presences`;
see "Liveness heartbeat + staleness" above). No membership roster to keep
in sync with a second signal, no `get_session_list` call anywhere in the
plugin.

**Badge.** `Sessions::badge()` (pure, re-derived on every call — never
cached, so it can't drift from `peers`/`own`) renders **zero lines** while
only the current session's presence is known (a lone fresh own-entry plus
only stale-but-not-yet-dead peers still clears that threshold — a stale
entry stays visible until the dead reap); from 2+ entries on, one line per
session in a single
fixed order shared with cycling: current session first, then any FRESH peer
with `attention > 0` by name, then the rest of the fresh peers by name, then
every STALE peer by name (a stale peer's attention count isn't actionable,
so staleness outranks attention for ordering). Each line shows the session
name plus the status-origin pane running count and attention count when
nonzero, using the same glyphs the per-tab rows use for those statuses. The
current line is marked (an accent-colored `•` — the label itself stays the
muted line color) and carries no click target — you
can't switch to the session you're already in. A pending cycle selection renders
bold+accent; a stale entry renders one step dimmer than the ordinary muted
line color and a right-edge `✕` hotspot; clicking the glyph dismisses it,
while clicking any other cell switches to that session, landing on its
`attention_tab_position` if it has one.

**Hotspots and clicks.** The renderer attaches per-line glyph metadata to the
same `Line` records that derive ANSI and navigation targets, so Cards painting
and finalization cannot desynchronize it. A stale peer badge gets `✕`; a tab
header and a pane identity line with an unacknowledged status-origin Pending
get `✓`. The glyph owns its final display cell (with one reserved separator),
and only those cells trigger its action; continuation, overflow, padding,
ledger, and header lines never do. Right-click preserves the same whole-row
actions for future parity, but is not a usable trigger until
[zellij#5350](https://github.com/zellij-org/zellij/issues/5350) is fixed.

**Cycling.** `session-next`/`session-prev`, delivered on the `zj_radar.cmd.v1`
pipe (documented for operators in `docs/configuration.md`), advance a
highlighted selection through that same shared order, wrapping, with the
current session included as a normal stop and every STALE peer excluded
entirely (switching onto a likely-dead session would have Zellij
resurrect it as an empty zombie pane). A tap only moves the highlight
(`Sessions::cycle`, which arms a per-selection "a tap landed" flag) — the
actual switch is a later **idle-commit**: `Sessions::tick` runs on every
timer fire while a selection is pending, and a fire whose covered interval
saw a tap (the flag is set) clears it and skips, resetting the deadline;
only a fire whose entire interval was tap-free commits. This guarantees a
commit at least one full quiet interval after the LAST tap, at the cost of
at most one extra (skipped) fire — replacing an earlier tick-counter
comparison that could commit instantly on a tap landing just before an
already-scheduled fire, or stall at the first tap in a fast burst instead of
the last. The commit target is re-resolved by the selected session's *name*,
never a remembered list position, so a peer joining, leaving, or going
stale mid-cycle can't silently retarget the selection (a selection that
lapsed into staleness before its commit is dropped, the same as a vanished
one). Landing the commit back on the current session is the cancel
gesture — no effect, selection just clears. A real commit emits
`Effect::SwitchSession { name, tab_position }`, which switches sessions and,
when the target had an `attention_tab_position`, jumps straight to it;
otherwise it leaves Zellij to restore that session's last focus.

**Degradation.** No writable shared cache root (`SessionFiles` falls back to
`/tmp/zj-radar`, or disables persistence altogether when neither is
writable) means no presence file is ever written and no peer reads happen —
the badge simply never appears. Every other rail behavior (status, ledger,
naming, notifications) is unaffected, exactly as persistence being
unavailable degrades only snapshot rehydration, never live rendering (§5).
