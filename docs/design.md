# zj-radar — a native Zellij sidebar for AI-agent status

**Status:** design / approved for spec-review (revised after external review; reconciled
after the smart-tabs removal — see `smart-tabs-postmortem.md`)
**Date:** 2026-06-26
**Author:** Mark Toda (with Claude)

> **Update (post-postmortem):** `zellij-smart-tabs` has been **removed entirely** from the
> Zellij setup after it melted down under a many-agent workload (poll-every-pane-on-every-output
> issuing blocking host calls on the server's single main thread — full writeup in
> `smart-tabs-postmortem.md`). This invalidates the earlier plan to *keep* smart-tabs for base
> tab naming. **zj-radar now owns all tab display, including naming** (see §6.1). The hard
> architectural constraint that follows: zj-radar must never issue blocking host queries
> (`get_pane_running_command`, `get_pane_cwd`, …) — all signals come from pushed events
> (`pipe`, `TabUpdate`, `PaneUpdate`, `CwdChanged`) or the hook payload.

## 1. Goal

Bring Cmux-style agent awareness into Mark's existing Zellij setup without changing the
parts he likes (keybindings, swap layouts, Nix-managed config). Specifically: an always-on
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

- `✗` red = error · `◑` orange = waiting-for-you · `◐` yellow = working · `●` green = done ·
  `○` dim = plain terminal (no agent).
- **Status vocabulary:** the pipe sends raw values `running`/`pending`/`done`/`error`/`idle`;
  the renderer maps `running`→working, `pending`→waiting-for-you, `idle`/absent→plain.
- Per-tab rows are **two lines**: line 1 = state dot + **display tab number** + name (+
  `done/total` count when a tab holds multiple agents); line 2 = `repo/branch · elapsed` and a
  truncated last message.
- **Display tab number = `TabInfo.position + 1`** (see §6 — position is 0-indexed).
- Plain (non-agent) tabs render name only — agent decoration is purely additive. The name is
  `TabInfo.name` (from the layout or zj-radar' own push-based naming, §6.1) — **not** from
  smart-tabs, which no longer exists.
- Click a row → switch to that tab.

## 4. Architecture

Thin Zellij-host glue around a deep, pure runtime module + pure stores/models/rendering.
The per-agent adapter layer still lives *outside* the plugin (shell scripts / agent
config). The plugin runtime has no `zellij-tile` dependency: `lib.rs` translates raw
Zellij events into repo-owned inputs and applies ordered host effects returned by the
runtime.

```
┌ Agent adapters (per-agent, outside the wasm) ─────────────┐
│ Claude → plugin hook / native CLI  (running/pending/done) │
│ Codex  → native CLI via hooks.json (running/pending/done) │
└───────────────────────────┬───────────────────────────────┘
   zellij pipe --name zj_radar.status.v1 -- {v,source,pane,status,repo,branch,msg,on_focus}
   (BROADCAST by name — not --plugin: see §6)
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
│    input: TabLite/PaneUpdate/PermissionProbe/status/config/timer/mouse │
│    output: Outcome { render, effects: Vec<Effect> }                    │
│                                                                        │
│  session_files.rs: SessionFiles                                        │
│    owns per-session filesystem coordination across sidebar instances:  │
│    snapshot durability, permission marker/lock, root fallback, pruning │
│                                                                        │
│  radar_state.rs/rollup.rs/command.rs/tab_namer.rs: state + tab model   │
│    StatusStore + CommandStore + roll_up(tab) + TabNamer                │
│                                                                        │
│  render.rs: pure rail renderer                                         │
│    render_rail(rows, opts) -> RenderedRail { ansi, line_targets }      │
│    owns layout, overflow, ANSI, and click-target materialization       │
└────────────────────────────────────────────────────────────────────────┘
        │ Effects: switch_tab_to(position + 1), show_pane_with_id, rename_tab,
        │ request_permission, set_timeout, set_selectable, persist session state
        ▼  (desktop notifications stay in the shell adapters, NOT the plugin)
```

**Design principle:** keep host-coupled code thin; push lifecycle decisions into
`PluginRuntime`, filesystem coordination into `SessionFiles`, and layout/click decisions into
`RenderedRail` so the core is unit-testable with `cargo test`. The adapter should not contain
behavior beyond translating host data, owning the session-files module, and performing returned
effects. The real external seam remains the **pipe payload schema** (versioned).

### 4.1 Lifecycle state machine

| Source event                                  | Status    |
|-----------------------------------------------|-----------|
| Claude `UserPromptSubmit` / `PreToolUse` / `PostToolUse` | `running` |
| Claude `Notification` (permission/idle/elicitation)      | `pending` |
| Claude `Stop`                                 | `done` (with `on_focus:"idle"`) |
| Claude `SessionStart` (`source:"clear"` only) | `idle` (resets a stale row on `/clear`) |
| Codex `UserPromptSubmit` / tool hooks / subagents | `running` |
| Codex `PermissionRequest`                     | `pending` |
| Codex `Stop`                                  | `done` (with `on_focus:"idle"`) |
| Codex legacy `agent-turn-complete`            | `done`    |
| Adapter parse/hook failure (optional)         | `error`   |
| Agent pane returns to its shell prompt (observed exit) | `idle` (clears a stale pushed status; see §6.2) |

> **Update (focus no longer drives state):** an earlier design cleared a pushed
> completion when you *focused* the pane (`on_focus`). Focus is per-client and is
> not delivered to background plugin instances, so that cleared the row only on the
> tab you were viewing and left every other tab stale. A finished status now clears
> only via shared signals — a new broadcast, the observed return-to-shell exit-clear
> (`command::is_shell_prompt` → `StatusStore::clear_on_prompt_return`), or a prune —
> which every tab's instance receives, so all tabs converge. The `on_focus` wire
> field is still accepted for back-compat but is inert.

### 4.2 Per-pane → per-tab aggregation

Mark's `compact.kdl` has multi-agent-in-one-tab shapes (`quad-grid` = 4 Claude panes in one
tab), so tab state cannot come from names. The store keys by `PaneId`; `PaneUpdate`'s
`PaneManifest` gives the pane→tab map. Tab aggregation:

- **Severity order (highest wins):** `error > pending > running > done > idle/absent`.
  (`error` is highest so failures never hide behind "working".)
- **Count:** `total` = panes in this tab that have *ever* reported a non-idle agent state and
  still exist; `done` = those whose current status is `done`. Render as `done/total` when
  `total > 1`.
- **Second-line detail (which pane's repo/branch/msg to show):** the highest-severity pane;
  tie-break by most-recent `last_change_tick`.
- **Pruning:** on each `PaneUpdate`, drop state for `PaneId`s no longer present, so closed
  agents leave no ghost status.

## 5. The pipe contract (producer ↔ plugin seam)

Broadcast by name `zj_radar.status.v1` (namespaced + versioned). Each sidebar instance
filters on the name and keeps its own copy of the state map (same pattern as the built-in
tab-bar; cheap for a handful of tabs).

**Newcomer rehydration (session snapshot).** Because the plugin lives in the tab template,
Zellij runs *one instance per tab*, and a broadcast only reaches instances alive when it is
sent — it is never replayed. So a tab opened after agents were already running would spawn a
blank instance and render every tab idle. Fix: each instance mirrors its `StateStore` into a
snapshot on every store mutation, and seeds itself from it in `load()`. `SessionFiles` chooses
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
  "source": "claude",                 // claude | codex | test — adapters differ; helps debugging
  "pane": { "type": "terminal", "id": 12 },   // typed to match Zellij's PaneId enum
  "status": "running",                // running | pending | done | error | idle
  "repo": "pinky",
  "branch": "fix/x",
  "msg": "running tests…",            // truncated last assistant message
  "on_focus": "idle" }                // optional: status to apply when this exact pane is next focused
```

**Plugin-side handling (defensive — the renderer/store, not the adapter, enforces these):**
- Match `pane` to `PaneId::Terminal(id)`. Adapters derive `id` by stripping any `terminal_`
  prefix from `$ZELLIJ_PANE_ID` (its form has varied across Zellij versions).
- Tolerate malformed/older/partial payloads: unknown fields ignored (including a
  legacy `seq` from older producers), missing fields default, unknown `status` →
  treated as `idle`.
- Ordering is latest-wins: the pipe delivers in order and no producer stamps a
  sequence, so a payload simply overwrites the pane's prior state.
- Sanitize `repo`/`branch`/`msg`: strip ANSI/control chars, convert newlines to spaces, cap
  `msg` to a fixed length before rendering.
- Ignore payloads over a fixed size cap (e.g. 64 KB).
- `on_focus` is accepted for back-compat but **inert** (see §4.1 update): `done` no longer
  auto-clears on focus. A finished status persists until a new broadcast, the observed
  return-to-shell exit-clear, or a prune — all shared signals, so all tabs converge.

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
  `Mouse`, `PermissionRequestResult`.
- **Tab index footgun:** `TabInfo.position` is **0-indexed**; `switch_tab_to(idx)` is
  **1-indexed** (0 treated as 1). Define `display_tab_number = position + 1` and use it for
  *both* rendering and click → `switch_tab_to(position + 1)`.
- **Click targeting:** `render_rail()` emits both ANSI and a same-height target map. Header,
  folded-idle strip, and external gap rows map to nothing; tab header/collapse/single-pane rows
  map to a tab; expanded multi-pane child rows map to that pane. The runtime stores the latest
  `RenderedRail` and returns `SwitchTab` or `ShowPane` effects on mouse clicks instead of
  replaying layout math in the host glue.
- **Why broadcast, not `--plugin`:** broadcasting by name means adapters never create UI
  panes, never need to know the plugin's URL/config identity, and naturally reach every
  already-running sidebar instance. (A `--plugin` destination can also load the plugin if not
  running and the routing across multiple same-plugin instances is fiddly — avoid it here.)
- **Timer is one-shot** in Zellij: re-arm each tick.
  ```rust
  // load():   set_timeout(1.0);
  // update(Event::Timer(_)):  tick_elapsed(); set_timeout(1.0); return true;
  ```
  Optimization: only keep re-arming while there is something to tick *for* — either
  animating work (a `Running` agent/command whose glyph spins) or an un-carried
  completion edge (a `status_pipe` payload defers its recede + notification to the
  timer). A backgrounded `done`/`error`/`pending` row is terminal: once its one-shot
  settle has run it does **not** keep the loop alive, so an idle-but-lit rail stops
  waking every second. The loop re-arms on the next pipe/PaneUpdate. (See
  `PluginRuntime::timer_should_continue`.)
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
replacement is push-only and tiered:

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
    consume. Both are push signals; no `get_pane_*` call is ever made.

  Guardrails: only `rename_tab` when the derived name actually differs (avoid redundant
  main-thread work), and treat naming as best-effort cosmetics — a missing cwd/title just leaves
  the existing name.

## 7. Agent adapters (v1: Claude + Codex)

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

- Rust, `zellij-tile = "0.44"` (pinned to 0.44.3), target `wasm32-wasip1`. Repo:
  `~/dev/zj-radar`. **Note:** the artifact is a *binary* crate, not `cdylib` —
  Zellij loads plugins as WASI command modules (it calls `_start`, which
  `register_plugin!`'s generated `fn main` provides); a cdylib reactor has no
  `_start` and won't load. See the comment block in `crates/plugin/src/main.rs`.
- **Dev loop:** `./dev/run.sh` builds the debug wasm, generates a layout with an
  absolute plugin path, and refreshes the dev surface. From a normal terminal it
  restarts the disposable `zj-radar-dev` session; from inside Zellij it reloads
  the current session's existing zj-radar sidebar panes in place. Zellij 0.44
  does not safely hot-reload layout-created plugin panes; `start-or-reload-plugin`
  opens a second pane instead, so the script uses `launch-plugin --in-place` for
  in-session reloads.
- **Nix:** build the wasm with `crane`/`naersk` (or, simplest first, `fetchurl` from a GitHub
  release — the same way `room` is vendored in `home-manager/modules/zellij/default.nix`), then
  reference via a `@zjRadar@` `replaceStrings` substitution alongside `@room@`. The `@smartTabs@`
  substitution and its `fetchurl` are **removed** (smart-tabs is gone).

## 9. Testing

Pure-function `cargo test` (runtime/renderer/store/aggregation are pure and warning-free on
the host target):

1. **Tab index:** `TabInfo.position = 0` renders as tab `1`; click calls `switch_tab_to(1)`.
2. **Pane-close pruning:** state for a removed `PaneId` disappears on the next `PaneUpdate`.
3. **Tab reorder:** click targets the current `position`, not a stale cached row.
4. **Payload safety:** huge messages, embedded newlines, ANSI escapes, invalid-UTF-8-ish input,
   unknown `status`, oversized payloads — all handled without panic.
5. **Unicode width:** dots/ellipsis, branch names with emoji/CJK, narrow widths.
6. **Focus clear:** `on_focus` clears only the intended pane (not merely its tab becoming active).
7. **Aggregation severity:** `error > pending > running > done > idle`.
8. **Count semantics:** `done/total` over panes that ever reported non-idle and still exist.
9. **Idle rendering:** a tab whose agent went idle does not look like an active agent tab.
10. **Broadcast filtering:** unrelated pipe names are ignored.
11. **Timer rearm:** elapsed increments across repeated one-shot timers.
12. **Runtime effects:** permission ownership/peer waiting, config/status pipes, snapshot writes,
    command debounce, tab renames, and click-to-tab/click-to-pane effects are asserted as ordered
    `Outcome` values.
13. **Renderer target map:** `RenderedRail` line count matches emitted ANSI lines, and headers,
    gaps, tab rows, expanded pane rows, and collapsed rows resolve to the intended target.
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
5. **Per-tab plugin instances** (N timers + N state copies) — the only-tick-while-active
   optimization bounds the timers, and the state copies are reconciled through `SessionFiles`
   (see §5 "Newcomer rehydration"). The trap here, learned the hard way: a broadcast is *not*
   replayed to instances spawned later, so a new tab's instance starts blank — hence the snapshot
   seed. Note `/data` is per-instance (`…/<plugin_id>-<client_id>/`) despite the docs calling it
   "shared"; `/cache` (`…/plugin_cache/`) is the genuinely shared one in Zellij 0.44, with
   `/tmp/zj-radar` as a degraded fallback.
6. **Repeating the smart-tabs meltdown** (`smart-tabs-postmortem.md`) — bounded *by design*:
   zj-radar is push-driven (hook `pipe` + `TabUpdate`/`PaneUpdate`/`CwdChanged`) and issues no
   blocking `get_pane_*` queries, so high-output panes cost it nothing and there is no poll loop
   to storm the server's main thread. The standing rule (no blocking host calls on any path)
   keeps it that way; any future naming/program feature must stay event-sourced (§6.1).

## 12. Out of scope (follow-ups)

- Floating cross-session **dashboard** overlay (`MOD+a`).
- **Aider** (and other) adapters; richer **Codex** lifecycle (running/pending) via a wrapper.
- Collapse-to-strip toggle; per-pane breakdown within a multi-agent tab.
- Moving notification logic into the plugin. **Update:** the plugin now owns OS desktop
  notifications (macOS, this version). Rationale: single plugin install provides a standard,
  user-configurable notification surface (via `notify*` KDL keys) that survives across agent
  adapters — reversing the prior assumption that notifications belong in shell adapters alone.
  This trade-off is stable: adapters delegate OS delivery to the plugin while owning their own
  pipe payload schema and lifecycle logic.
- **Keybinds, the passive way** — the supported keyboard path is a Zellij
  `MessagePlugin` binding that delivers a verb to the `zj_radar.cmd.v1` pipe
  (e.g. `attention-next`), handled in `pipe()` exactly like `config.v1`. This
  keeps the plugin a passive renderer (no `Key` subscription, no focus grab),
  unlike a `LaunchOrFocusPlugin` panel.
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
