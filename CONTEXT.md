# zj-radar — domain glossary

Names for the good seams in zj-radar. This file defines the domain concepts in
zj-radar's architecture, focusing on the key interfaces and state flows.

## Rail

The rendered sidebar: the pinned left column listing every tab with per-tab agent
status. The **rail seam** is the renderer's single deep interface —
`render_rail(rows, ledger, opts) -> RenderedRail` (with `onboarding(opts) -> RenderedRail`
as the sibling face for the no-agents-yet state, and
`needs_permission(opts, grant_hint) -> RenderedRail` as the third face for the
not-yet-granted state). Everything a caller needs to draw and to resolve a
click crosses this one seam; layout planning (overflow folding, card spacing,
multi-pane tree expansion) is implementation *behind* it, not interface.

The rail's canonical *visual* design — the "gutter rail" (2-column status
gutter, theme-adaptive color roles, glyph sets, overflow folding, onboarding
panel) — is captured by [`docs/rail-reference.md`](docs/rail-reference.md) (the
executable spec-by-example) and [`docs/design.md`](docs/design.md). Color is
**purely additive**: stripping SGR from the rail yields the exact same visible
character grid, so layout and color are orthogonal and testable apart.

## RenderedRail

The rail seam's output: the emitted `ansi` paired with a same-height
**target map** and a same-height **hotspot map**. `target_at_line(line)`
resolves a physical line to a `RailTarget` (a tab to switch to, a pane to
show, or a session to switch to); header / gap / idle-strip lines resolve to
`None`. `hotspot_at(line, col)` resolves column-aware: each hotspot entry is
`Option<(start_col, HotspotAction)>`, and only the glyph's own display cells
trigger it — a `HotspotAction::DismissPresence` (`✕` on a stale session-badge
line) or `HotspotAction::Acknowledge` (`✓` on an unacknowledged status-origin
Pending). The runtime caches the last `RenderedRail` and resolves mouse clicks
against it — so the rail the user sees *is* the rail clicks are scored against.

## RailTarget

What a clickable line resolves to: a tab to switch to (`tab_position`), a
specific pane to show (`pane_id`), or a peer session to switch to
(`session: Option<String>`, carrying the peer's attention tab). Pane targets
cover an expanded multi-pane row's tree lines *and* a single-pane tab's pane
line and line-2 detail line(s), which target that tab's one tracked pane.
Header, gap, and idle-strip lines have no `RailTarget`. The runtime turns a
`RailTarget` into a `SwitchTab` / `ShowPane` / `SwitchSession` effect on
click.

## RadarState

The plugin's session-state module: the current radar view of tabs, live terminal
panes, pane observations, focus transitions, and snapshot serialization (the
last delegated to `radar_state::snapshot`). `RadarState` is not a replacement for
the source-specific stores; it composes `StatusStore` (status-payload
observations) and `CommandStore` (command-derived observations) with live pane
topology, then produces `TabRow`s for the rail. Both stores are thin wrappers
over one shared `ObservationStore` (in `crates/core`) that owns the pane-id map,
`prune`, and snapshot insert; the per-source split is only their intake and their
resting-state predicate. The
"status wins over command" precedence *between* the two stores lives in exactly
one place — `RadarState::resolve` — which both `tab_display` and `notify_views`
read through, so the rule can never drift and `roll_up` never learns there is
more than one store. `RadarState` also composes `TabNamer` for tab naming —
assembling the resolved facts that seam consumes, the same way it hands `roll_up`
a `resolve` closure.

Focus does **not** drive rail state. A finished pane's `done`/`error`/`pending`
clears only via a *shared* input — a new broadcast for that pane, the
return-to-shell exit-clear (`command_changed` → `StatusStore::clear_on_prompt_return`),
or a prune. This is the load-bearing convergence property: the plugin runs one
instance per tab, and Zellij delivers pipe broadcasts and per-pane `CommandChanged`
to *every* instance, so all tabs render the same rail. Focus is per-client and is
*not* delivered to background instances — an earlier design that cleared a
completion on focus ("seen it, recede it") therefore cleared it only on the tab you
were looking at, leaving every other tab stale. That focus-driven recede is gone.
`RadarState::note_focus` still records the focused terminal, but *only* so the
notifier can suppress the pane you're watching — it never mutates a status.

Pruning has a **one-manifest grace** (`absent_once`): a tracked pane's first
absence from the pane manifest is held, not pruned — Zellij's break-pane
family reports session state while a moved pane is extracted and in no tab,
so a single absence can't distinguish a close from a mid-move flash. A pane
still absent on the *next* manifest is confirmed gone and prunes then, filed
in the ledger under the tab identity captured at first absence (by the
confirming manifest the live index no longer carries it). Level-triggered:
the grace set is recomputed from the current manifest every `panes_changed`,
so a reappearing pane simply drops out.

The runtime owns host concerns: permission flow, timers, rendered-rail caching,
and turning repo-owned outcomes into Zellij effects. The rail owns layout and
click-target lockstep. `RadarState` owns the domain facts between those seams.

## Settle

Whether *notifications* are fired *now* or deferred to the timer. (Since the
focus-driven recede was removed, `settle` gates only the notifier — not any rail
state.) Radar fires notifications only on events whose focus is *trustworthy* for
the "don't ding the pane you're watching" suppression: `panes_changed` (this update
*carries* the fresh focus) and the `timer` tick (any focus `PaneUpdate` has been
processed by the time it fires, so `last_focused` is settled). A `status_pipe`
payload is a raw completion edge that can arrive *before* the focus `PaneUpdate`
reflecting the user leaving, so its focus may be stale; it deliberately does **not**
settle, and instead arms the timer, which carries the notify once focus has settled.
The remaining intake events (`cwd_changed`, `command_changed`, `config_pipe`,
`tabs_changed`) are not completion edges, so they never settle either. `panes_changed`
and the `timer` each stamp `settle: true` on their `RadarChange`; `project` fires
`notify_effects` exactly on that flag, so the notify call sites line up across every
handler by construction.

**Cadence** is a related but distinct axis — how often the one-shot timer
re-fires, not whether it notifies. Two speeds (`PluginRuntime::desired_cadence`):
Fast (1 Hz) while there's tick-windowed work — the permission flow still live
(`permission.is_waiting()` / `selectable()`), `timer_should_continue` (a
spinning glyph via `needs_fast_ticks`, an un-carried completion edge — a
status-pipe recede/notify deferred to the timer because its own focus can't be
trusted — a command `Done` awaiting its `DONE_TTL_TICKS` recede, or an active
ping flash), or a pending cross-session cycle selection awaiting its
idle-commit (`sessions.wants_fast_cadence()`). Slow (1/60 Hz — once a minute)
once none of that holds but a minute-granular age is still changing — a
ledger entry (`ledger_any_unsaturated`) or a pending row's `· Nm` wait tag
(`pending_wait_unsaturated`) short of the `1h+` saturation cutoff — **and
unconditionally while `own_session_name` is non-empty**: a known name means a
presence file is published, and the Slow tick's heartbeat is the only writer
keeping its mtime fresh for peers (`## Cross-session presence`). Full disarm
(`None`) therefore survives in exactly two shapes: *pre-name* (no `ModeUpdate`
has delivered a session name yet, and every age has saturated) and *denied*
(a permission-denied rail disarms unconditionally — without
`ReadApplicationState` no clearing event ever arrives, so a snapshot-loaded
stale `Running` would otherwise pin Fast ticks forever behind a static
needs-permission face).

## Render gate

Why an event repaints — or deliberately doesn't. Zellij delivers every pipe
broadcast and topology event to **every tab's plugin instance**, and runs each
instance under a wasm *interpreter* (wasmi) — so one chatty producer
multiplies across N tabs at interpreter prices. Three layers keep repaints
proportional to actual change:

- **Intake no-ops.** An intake that provably changed nothing rows-visible
  reports a default `RadarChange` (no render, no persist, no renames): an
  identical status re-broadcast (`status_pipe`'s prev/now compare — producers
  re-assert on every tool hook), an identical `TabUpdate`, a `CommandChanged`
  that only touched the debounce maps (the row appears when the *timer*
  promotes it), a `CwdChanged` (naming rides the `RenameTab` effect's own
  `TabUpdate` echo).
- **Label-only deferral.** A Running→Running update (new activity label, same
  status) on an *animating* row neither renders nor persists inline: the Fast
  (1 Hz) tick is armed while the row animates (`TrackedObservation::animating`
  — Running and not a service) and repaints unconditionally, so the label
  lands ≤1s later, and the snapshot write rides the tick's flush
  (`SnapshotWrite::Deferred` → the runtime's `snapshot_dirty`; an inline
  `SnapshotWrite::Now` on the same pass clears the flag — it supersedes).
  Only while animating — a rewritten Pending question renders now (Pending
  doesn't pin Fast cadence), and so does a Running *service's* label (the
  steady `▸` row doesn't either — deferral there would mean the ≤60s Slow
  heartbeat).
- **Rows-diff gate.** `project` drops a requested render whose content-derived
  key (rows, ledger lines, badge, theme) equals what the last `render()`
  actually drew (`last_render_key`, stamped in `render`). `force_render`
  bypasses it — timer frames and config overrides change the *drawing*
  without changing the key. The gate only ever downgrades; it never invents a
  render.

The machinery under all three is `RadarState::generation` (bumped by every
mutator of anything `rows()` reads) + the `rows()` memo keyed on
`(generation, tick)` — one rollup per event, shared by the presence derive
(`project` gates it on the generation too), the gate compare, and the render.
A missed `touch()` is a **stale rail** bug, not just a slow one: audit new
mutators against `rows()`'s inputs.

## Ledger

The completion history: a fixed-cap ring (`LEDGER_CAP` = 32, newest at front)
that a Done/Error hands off to the moment it stops being shown as a card fact
— rendered as the rail's trailing `─ earlier ─` region beneath the live tab
list. `crates/plugin/src/ledger.rs`'s `Ledger` is pure data + policy
(`push`/`replace`/`merge`/`any_unsaturated`/`format_age`); `RadarState` wires
every edge that can retire a card into it (`ledger_receded`) and prepares
`LedgerLine`s for the renderer (`ledger_lines`) — the renderer only ever
consumes what it's handed, never reaches into the ring itself.

**Entry rule.** An observation enters at the edge where it stops being a card
fact, never before: TTL recede (a command-origin `Done` past
`DONE_TTL_TICKS`), the prompt-return clear (`StatusStore::clear_on_prompt_return`),
an overwrite (a new status-pipe broadcast displacing a still-lit `Done`/`Error`
— including the `/clear` idle-overwrite), or a prune (`panes_changed`'s
exit/prune paths, captured against the pre-close topology so the entry ledgers
under the tab it was actually shown on). `Pending`/`Running` never enter —
`LedgerEntry::from_observation` returns `None` for anything but a stamped
`Done`/`Error` (one without a `completed_epoch_s` is a pre-v3 snapshot
transient, also skipped). A command completion **shadowed** by a status
observation for the same pane never enters either: `resolve`'s
status-wins-over-command precedence means that command fact was never actually
on the card, so its recede must not ghost a row into history —
`ledger_receded`'s `status_tracked` filter cites `resolve` directly. The
filter reads the shadow at *recede* time, not onset: a command `Done` that was
visibly on the card but gets shadowed by a status observation within its TTL
window never ledgers either — deliberate, since the status source now owns
that pane's story and its own completion will ledger instead of double-ghosting
the pane. A
status-origin recede is never filtered; only `resolve` and this one check know
there are two stores at all.

**Convergence.** Every entry edge is a signal every tab's plugin instance
receives — broadcast, `PaneUpdate`, the shared timer tick — the same
convergence property the rail card itself relies on (see `RadarState`).
Snapshot v3 carries the ledger; on load, `Ledger::merge` unions two rings by
nearest-neighbor match on `(pane, outcome, label)` within `MERGE_WINDOW_S`
(4s), keeping the later stamp, so two instances observing the same completion
a beat apart collapse to one row instead of duplicating it.

**Timestamps.** Entries stamp completion-time epoch seconds (`at_epoch_s`,
from `completed_epoch_s`), not ticks — ticks are per-instance and reset,
epochs aren't. Rendered age is relative (`format_age`: `<1m`, `Nm`, frozen at
`1h+` past `SATURATE_S`), and that saturation is load-bearing, not cosmetic:
once every entry's age has stopped changing, `any_unsaturated` goes false and
the idle timer can fully disarm (the cadence note above) instead of ticking
once a minute forever to redraw an age nothing will ever change again.

**Seams.** `ledger.rs` is the pure ring — no knowledge of tabs, panes, or
rendering. `RadarState` owns the recede-edge wiring and `LedgerLine` prep
(each line's tab position is a *live* lookup against `self.tabs`, `None` once
that tab has closed — click-inert, not forgotten; the ring never forgets an
entry just because its tab went away). The renderer consumes prepared lines
only.

## Tab naming

The policy that decides what each tab is called, and remembers what it last
applied. The **tab-naming seam** is `TabNamer::rename(tabs, mode) -> Vec<TabRename>`
in `crates/plugin/src/tab_namer.rs`: a deep module fed resolved `TabFacts` (per-tab `id`,
`name`, `position`, and per-pane `PaneFacts` carrying `repo`, raw `cwd`, raw
`title`, `focused`). `RadarState::name_facts` does the joins across its stores and
pane topology, so the namer never learns about `StatusStore`, `TerminalPane`, or
the cwd map — only `repo` (the one fact it can't derive) crosses pre-resolved;
worktree resolution, basename, and activity-prefix stripping are implementation
behind the seam.

The candidate space is one ordered list (`name_candidates`): focused pane's repo,
any pane's repo, focused/any worktree-resolved cwd, focused/any title. Stickiness
derives from that single list — `computed_name` takes the top, `name_supported`
asks whether a name sits anywhere in it — so an applied name (tracked in
`TabNamer`'s own `applied` state, keyed by stable `TabId`) stays put while any
pane still justifies it, and `Managed` never clobbers a manual rename (only
`Force`, a default `Tab #N`, or a name the namer itself applied is overwritten).
`TabRename` is the namer's output vocabulary; `RadarState` uses it in
`RadarChange` and the runtime turns it into a `RenameTab` effect. Bootstrap (the one-shot `get_pane_cwd`
reads that *feed* naming) stays in `RadarState` — it ensures cwd facts exist; it
is not naming policy.

**Naming ownership is per tab, never session-wide.** Each sidebar instance
learns the position of its own plugin pane from `PaneUpdate`, correlates that
position once with `TabUpdate`, and retains the resulting stable `TabId`.
`RadarState` passes only that tab's facts to `TabNamer`. Until the identity
resolves it emits no renames; an onboarding instance never owns a tab. This
prevents a stale background sidebar from applying its private `applied` history
to another tab after navigation, and retaining `TabId` rather than position
keeps close/reorder events from redirecting ownership to a neighbour. The cwd
bootstrap stays session-wide: the cwd also stamps `repo` on observed commands,
which every instance must agree on for the notification claim key.

## Lockstep

The load-bearing invariant of the rail: the emitted ANSI, the click-target
map, and the hotspot map stay in exact 1:1 line correspondence — three
parallel vectors (`ansi`, `targets`, `hotspots`). `line_count() == ansi
newline count`, and every drawn line maps to the intended target and hotspot
(or a deliberate `None`). Lockstep is why click-to-switch lands on the row the
user pointed at — and why a `✕`/`✓` glyph hotspot fires only on its own cells.
Lockstep is now structural, not discipline-held: `render_rail` builds a single
`Vec<Line>` where each line carries its own `RailTarget` and optional
`(start_col, HotspotAction)`, and `ansi`/`targets`/`hotspots`/line-count all
derive from that one list via `RenderedRail::from_lines`. There is no separate
height predictor — a row's footprint is `block.len()` of the very lines it
renders — so the emitted ANSI and the click maps cannot drift.

## Status contract

The real external seam between producers and the plugin: the versioned
`zj_radar.status.v1` pipe payload (`{v, source, pane, status, repo, branch,
msg, task, ack}`). Producers (the Claude plugin, the Codex CLI) are adapters that
broadcast it; the plugin defends itself at parse time (sanitize, truncate, drop
oversized/malformed). Ordering is latest-wins — the pipe delivers in order and no
producer stamps a sequence, so there is nothing to reorder. Unknown fields are
tolerated and ignored, so older producers still parse: a legacy `seq` and the
former `on_focus` clear-on-focus hint (dropped when focus stopped driving state)
both round-trip harmlessly. `task` (optional): sticky task label — empty/absent
leaves the stored label unchanged, non-empty replaces it; the plugin clears it
on idle and on return-to-shell. `ack` (optional, default false): "the user has
already seen this" — state converges as usual but the notifier stays silent.

The pipe is no longer strictly producer→plugin one-way: the plugin is itself a
caller. The rail's acknowledge gesture broadcasts a synthetic `status.v1`
payload (Pending → Done, `ack: true`) rather than mutating local state, so the
dismissal converges across every tab's instance through the exact same intake
a real producer's broadcast uses (`Effect::BroadcastStatus`).

**Bounded sends.** `zellij pipe` is a backpressure channel: the client process
is held until *every* loaded plugin instance consumes the message, and an
instance wedged at Zellij's permission prompt holds it forever — unbounded
sends at hook rate once turned one wedged rail into an EMFILE crash of the
whole session. Every caller therefore sends through the self-limiting `sh`
subtree built by `core/pipe.rs::self_limiting_pipe_argv`: a detached
sleep+kill watchdog rides inside the spawned subtree and reaps its own hung
client even if the caller is killed mid-send. Deadlines are status-keyed —
`DEFAULT_PIPE_TIMEOUT_SECS` (5s) for the once-per-turn edges,
`RUNNING_PIPE_TIMEOUT_SECS` (2s) for `running` heartbeats (a dropped heartbeat
is replaced by the next tool event; a dropped edge loses real state). The
plugin's own ack broadcast goes through the same argv.

## Information source

Anything that produces a per-pane observation. Two modalities, both converging on
a `Kind`-keyed `Status`:

- **Pushed** — instrumented agents report rich status by broadcasting the *status
  contract* through the host CLI (`zj-radar notify <agent>`). Each agent is a peer
  adapter behind the **agent intake** seam — `Agent::derive(&Intake) ->
  Option<AgentUpdate>` in `crates/cli/src/agents/` — so `notify::run` is a thin,
  agent-agnostic shell (read input → derive → broadcast). Adding an agent is a
  compiler-guided `enum Agent` variant; its `source()` string is the single
  vocabulary shared across the CLI argument, the wire `source`, and
  `Kind::from_source`, pinned by the `source_round_trips_through_kind` guard test.
- **Observed** — uninstrumented commands (e.g. `cargo test`) that Radar watches
  from outside. The plugin classifies the observed argv via
  `crates/core/src/command.rs::classify` and infers status from the process
  lifecycle. No wire, no CLI. `cargo test` lives here, **not** in `agents/`.
  *Interactive* commands (editors/pagers/TUIs — `DEFAULT_INTERACTIVE` +
  the `interactive_commands` config) are observed but never earn a Running
  row: they record a quiet pending whose identity labels exits and the muted
  pane label (`docs/activity-model.md`).

The two modalities also interact at *exit* — the **producer-death model**. A
pushed producer (an agent) fires no hook when it quits, so its last status
would otherwise linger forever. The model: (agent argv present, `exited` =
false) = alive; `exited` = dead. Three paths converge on it:

- **Prompt return.** When the observed layer sees the pane return to a shell
  prompt (`command::is_shell_prompt` — no foreground command, or a
  shell/prompt program), `RadarState` clears a stale terminal status
  (`done`/`pending`/`error`) to idle
  (`StatusStore::clear_on_prompt_return`). A `Running` status is *not*
  cleared immediately — a live turn's foreground can flicker through the
  shell mid-turn — it instead arms a **stale-Running grace clock**
  (`RUNNING_SUSPECT_GRACE_TICKS`, ~15 Fast ticks). If the clock outlives the
  window, `expire_stale_running` clears the row: the producer died mid-turn
  and will never send the clearing broadcast.
- **Live-again evidence.** The agent's exe reappearing as the pane's
  foreground command cancels the grace clock
  (`cancel_running_suspect`) — the flicker resolved as a flicker. Other
  commands don't vouch: a command run in the shell an agent died in must not
  keep its ghost alive.
- **Definitive death.** The pane manifest's `exited` flag is producer death,
  full stop — an agent-rooted pane (`zellij run -- claude`) never shows a
  shell prompt for the first path to see. `StatusStore::clear_on_exit`
  clears even a `Running` immediately, no grace clock.

All three ride shared signals (`CommandChanged`, the pane manifest, the
shared timer — not per-client focus), so every tab's instance clears in
lockstep.

Both modalities emit a `source` string that must be a subset of `Kind`
(`Kind::from_source`). Both halves are guarded: the agent half by
`source_round_trips_through_kind` (in `crates/cli/src/agents`), the command half by
`classify_source_round_trips_through_kind` (in `crates/core/src/command/tests.rs`) — each pins that its
classifier's `source` token round-trips back to the same `Kind`, never the
`Other` sentinel.

## Tab Roll-Up

The per-pane → per-tab roll-up: severity order `error > pending > running > done >
idle`, with `done/total` counts and a highest-severity detail line. Tab status is
never derived from tab names — a single tab can hold several agent panes.

The **roll-up seam** is `rollup::roll_up(panes, resolve, quiet) -> TabDisplay`
(in `crates/plugin/src/rollup.rs`): a deep, pure module that owns its output
vocabulary (`TabDisplay`, `PaneDisplay`,
`PrimaryDetail`, `ProgressCounts`, `ExitOutcome`) — the renderer *consumes* these, so
presentation depends on the roll-up, not the reverse. `resolve(pane_id) ->
Option<&TrackedObservation>` is the main thing crossing in: the "status pipe wins
over command" precedence across observation sources stays in `RadarState`
(`RadarState::resolve`), so `roll_up` never learns there is more than one store.
`quiet(pane_id)` feeds the interactive muted label (`PaneDisplay::Interactive`
from the command store's quiet pendings); `roll_up` owns its precedence — shown
only where no live observation outranks it, contributing nothing to counts or
severity. On equal severity, a bounded job outranks a service for the primary
detail (`docs/activity-model.md` §3).
`ExitOutcome`'s display methods
(`full`/`minimal`/`role`/`renders_tag` — glyphs and width-driven forms) live in
`render`; the enum here is pure semantics. (Not to be confused with the
runtime's `Outcome`, which names its `{render, effects}` return value.)

## Setup analysis

How `zj-radar setup` learns the current state of the world. The **setup-analysis
seam** is a pure analyze function per target —
`analyze_zellij(&ZellijEnv) -> ZellijFacts` and `analyze_codex(&CodexEnv) ->
CodexFacts` in `crates/cli/src/setup/analyze.rs` — each fed a thin
`Env` of already-read values (file contents, fs stat booleans) by the IO shell.
`Facts` (`ZellijFacts`, `CodexFacts`) is the single home for every derived fact —
"is our alias present?" (managed vs unmanaged kept distinct), has-rail, granted,
producer-wired, the Codex hooks-feature and notify states.

Both consumers project from `Facts`: `*_check_items(&Facts)` renders the
`--check` doctor output; the install orchestrators (`setup_zellij`, `setup_codex`)
read `Facts` for their gating decisions and pull raw config text from `Env` for
the `edit_*` splice. The pure mutators (`edit_zellij`, `edit_codex`,
`edit_codex_hooks` → `Outcome`) are NOT driven by `Facts` — they share only the
low-level primitive detectors (`notify_is_ours`, `has_unmanaged_radar_alias`,
`strip_managed_zellij_alias`, `codex_hook_handler_is_ours`), which live in
`crates/cli/src/setup/detect.rs`, a neutral module that both `analyze` and `edit`
depend on. The legacy-notify vs hooks choice is a flag the consumer projects on,
never a fact.

## Cross-session presence

How one session's rail learns another session's counts, without ever asking
Zellij "what sessions exist." Each session's plugin writes its own tiny
`zj-radar.presence.<zellij_pid>.json` (`{session_name, running, attention,
attention_tab_position, updated_epoch_s}`) into the same plugin-URL-scoped
`/cache` root snapshots already use (`session_files.rs`); peers read that
directory back on Fast-cadence timer fires only (see `Cadence`, above) — and
within Fast, only every `PRESENCE_READ_TICK_INTERVAL`th (5th) tick, except
mid-cycle where the Alt+[/] selection wants the freshest roster — and
feed the parsed rows into `Sessions` (`sessions.rs`) — pure state, no
`zellij-tile`, that derives the cross-session badge on demand from
`peers`/`own`, exactly like `RadarState` never caches a derived value.

**Liveness is the mtime, not a roster — graded fresh → stale → dead.** An
earlier iteration subscribed to
`Event::SessionUpdate` and cross-checked a peer list against presence files.
E2E against real Zellij 0.44.3 showed `SessionUpdate` only delivers peers
after some plugin has called the blocking `get_session_list()` host
function — which nothing in this stack does, and which would violate the
push-driven doctrine (`CONTRIBUTING.md`; `smart-tabs-postmortem.md`) if it
did. `SessionUpdate` was dropped entirely (task-8b): liveness is judged from
a presence file's mtime. That judgment rests on a write-side guarantee: a
live session rewrites its presence file at least every 60s (`project`'s
`PRESENCE_HEARTBEAT_S` level trigger — any pass through `project` re-emits
the write once the last one is that old, bypassing the normal content-edge
gate), so file age reliably means what it says. `read_peer_presences`
returns every peer file it finds, unconditionally, paired with that file's
mtime age; `Sessions` (`sessions.rs`) grades that age per entry:
fresh (≤`STALE_AFTER_SECS`, 90s — 50% margin over the 60s heartbeat),
stale (90–300s — dims on the badge and is unreachable via
`session-next`/`session-prev`, since switching onto a likely-dead session
would have Zellij resurrect it as an empty zombie, but stays on screen),
dead (past `DEAD_AFTER_SECS`, 300s — five missed heartbeats: reaped from
the badge and its file unlinked via `Effect::DismissPresence`). Dimming
stays twitchy because it's reversible cosmetics; the reap is conservative
because it deletes a file — though even a false reap (a machine-sleep wake
makes every file look old for up to one heartbeat) is harmless, since the
live session's next heartbeat republishes and the entry returns fresh.
Right-click dismiss remains the manual reap for a stale-not-yet-dead entry;
a 6h open-time sweep at plugin `load()` remains the backstop for debris the
reap can't touch (malformed/unparseable files, which never produce a name
to dismiss by — an own-name corpse is NOT in that bucket, since the on-disk
dismiss spares the live file by path identity and reaps the corpse). The session's own name arrives the same push-style way its
liveness does: `Event::ModeUpdate`'s `ModeInfo.session_name`, not a
session-list lookup.

**Badge and cycling share one order.** `Sessions::badge()` (what the rail
shows) and `Sessions::cycle()`/`tick()` (what `session-next`/`session-prev`
step through and idle-commit) both derive from the same `ordered()`:
current session first, then fresh attention-bearing peers by name, then the
rest of the fresh peers by name, then every stale peer by name — a stale
peer's attention count isn't actionable, so staleness outranks attention for
ordering. `cycle()` filters stale entries out of its target set entirely.
The pending cycle selection is tracked by session *name*, never a list
position, so peers joining or leaving mid-cycle can't silently retarget it
(the same identity-over-position lesson `RailTarget` already applies to
clicks) — and a selection that goes stale between the tap and the
idle-commit is dropped the same way a vanished one is. The badge renders
zero lines with only one session known — the feature is invisible until
there's genuinely something cross-session to show, and a lone fresh own
entry plus only stale peers still counts as "something" — and degrades the
same way persistence does: no writable shared `/cache` root means no
presence file and no badge, nothing else affected.
