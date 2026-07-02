# zj-radar — domain glossary

Names for the good seams in zj-radar. Architecture vocabulary (module, interface,
depth, seam, leverage, locality) lives in the `codebase-design` skill; this file
names the *domain* concepts those terms attach to.

## Rail

The rendered sidebar: the pinned left column listing every tab with per-tab agent
status. The **rail seam** is the renderer's single deep interface —
`render_rail(rows, opts) -> RenderedRail` (with `onboarding(opts) -> RenderedRail`
as the not-yet-live sibling). Everything a caller needs to draw and to resolve a
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
**target map**. `target_at_line(line)` resolves a physical line to a `RailTarget`
(a tab to switch to, or a pane to show); header / gap / idle-strip lines resolve
to `None`. The runtime caches the last `RenderedRail` and resolves mouse clicks
against it — so the rail the user sees *is* the rail clicks are scored against.

## RailTarget

What a clickable line resolves to: a tab to switch to (`tab_position`) or a
specific pane to show (`pane_id`) — for an expanded multi-pane row's tree
lines, or for a single-pane tab's line-2 detail line(s), which target that
tab's one tracked pane. Header, gap, and idle-strip lines have no
`RailTarget`. The runtime turns a `RailTarget` into a `SwitchTab` / `ShowPane`
effect on click.

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
Fast (1 Hz) while there's tick-windowed work — `has_running_work` (a spinning
glyph), an un-carried completion edge (a status-pipe recede/notify deferred to
the timer because its own focus can't be trusted), a command `Done` awaiting
its `DONE_TTL_TICKS` recede, or an active ping flash. Slow (1/60 Hz — once a
minute) once none of that holds but a ledger entry is still un-saturated
(`ledger_any_unsaturated`): nothing is animating, but a displayed age is still
changing. Fully disarmed (`None`) once every ledger age has hit `1h+` — the
saturation cutoff (`## Ledger`) is exactly what lets the timer stop for good
instead of ticking forever to redraw an age that will never change again.

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

## Lockstep

The load-bearing invariant of the rail: the emitted ANSI and the click-target map
stay in exact 1:1 line correspondence. `line_count() == ansi newline count`, and
every drawn line maps to the intended target (or a deliberate `None`). Lockstep is
why click-to-switch lands on the row the user pointed at. Lockstep is now
structural, not discipline-held: `render_rail` builds a single `Vec<Line>` where
each line carries its own `RailTarget`, and `ansi`/`targets`/line-count all derive
from that one list via `RenderedRail::from_lines`. There is no separate height
predictor — a row's footprint is `block.len()` of the very lines it renders — so
the emitted ANSI and the click-target map cannot drift.

## Status contract

The real external seam between producers and the plugin: the versioned
`zj_radar.status.v1` pipe payload (`{v, source, pane, status, repo, branch,
msg, task}`). Producers (the Claude plugin, the Codex CLI) are adapters that
broadcast it; the plugin defends itself at parse time (sanitize, truncate, drop
oversized/malformed). Ordering is latest-wins — the pipe delivers in order and no
producer stamps a sequence, so there is nothing to reorder. Unknown fields are
tolerated and ignored, so older producers still parse: a legacy `seq` and the
former `on_focus` clear-on-focus hint (dropped when focus stopped driving state)
both round-trip harmlessly. `task` (optional): sticky task label — empty/absent
leaves the stored label unchanged, non-empty replaces it; the plugin clears it
on idle and on return-to-shell.

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
  `crates/core/src/command.rs::command_kind` and infers status from the process
  lifecycle. No wire, no CLI. `cargo test` lives here, **not** in `agents/`.

The two modalities also interact at *exit*: a pushed producer (an agent) fires no
hook when it quits, so its last status (`done`/`pending`/`error`) would otherwise
linger forever. When the observed layer sees that pane return to a shell prompt
(`command::is_shell_prompt` — no foreground command, or a shell/prompt program),
`RadarState` clears the stale pushed status to idle (`StatusStore::clear_on_prompt_return`).
The clear ignores a `Running` status — a live turn re-asserts `Running` via its
hooks, so a transient foreground flicker can't be mistaken for the agent exiting.
Because it rides the shared `CommandChanged` signal (not per-client focus), every
tab's instance clears in lockstep.

Both modalities emit a `source` string that must be a subset of `Kind`
(`Kind::from_source`). Both halves are guarded: the agent half by
`source_round_trips_through_kind` (in `crates/cli/src/agents`), the command half by
`command_source_round_trips_through_kind` (in `crates/core/src/command.rs`) — each pins that its
classifier's `source` token round-trips back to the same `Kind`, never the
`Other` sentinel.

## Tab Roll-Up

The per-pane → per-tab roll-up: severity order `error > pending > running > done >
idle`, with `done/total` counts and a highest-severity detail line. Tab status is
never derived from tab names — a single tab can hold several agent panes.

The **roll-up seam** is `rollup::roll_up(panes, resolve) -> TabDisplay` (in
`crates/plugin/src/rollup.rs`): a deep, pure module that owns its output
vocabulary (`TabDisplay`, `PaneDisplay`,
`PrimaryDetail`, `ProgressCounts`, `Outcome`) — the renderer *consumes* these, so
presentation depends on the roll-up, not the reverse. `resolve(pane_id) ->
Option<&TrackedObservation>` is the only thing crossing in: the "status pipe wins
over command" precedence across observation sources stays in `RadarState`
(`RadarState::resolve`), so `roll_up` never learns there is more than one store.
`Outcome`'s display methods
(`full`/`minimal`/`role` — glyphs and width-driven forms) live in `render`; the
enum here is pure semantics.

## Setup analysis

How `zj-radar setup` learns the current state of the world. The **setup-analysis
seam** is `analyze(&Env) -> Facts`, one per target (`analyze_zellij`,
`analyze_codex` in `crates/cli/src/setup/analyze.rs`): a pure derivation fed a thin
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
