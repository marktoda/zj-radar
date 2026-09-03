# zj-radar: domain glossary

Names for the seams in zj-radar. Each entry says what the term means, where it
lives, and which seam it crosses. The mechanisms behind them are in
[`docs/design.md`](docs/design.md); this file stays a glossary.

## Rail

The rendered sidebar: a pinned left column listing every tab with per-tab
agent status. The rail seam is `render_rail(rows, ledger, opts) ->
RenderedRail` in `crates/plugin/src/render.rs`, with `onboarding(opts)` and
`needs_permission(opts, grant_hint)` as sibling faces for the no-agents-yet and
not-yet-granted states. Layout planning (overflow folding, card spacing, tree
expansion) is implementation behind the seam. Color is additive: stripping SGR
from the rail yields the same character grid, so layout and color test apart.
The exact grid is [`docs/rail-reference.md`](docs/rail-reference.md).

## RenderedRail

The rail seam's output: `ansi` paired with a same-height target map and a
same-height hotspot map. `target_at_line(line)` resolves a physical line to a
`RailTarget`; `hotspot_at(line, col)` resolves a glyph cell to a
`HotspotAction` (`Acknowledge` for `✓`, `DismissPresence` for `✕`). The runtime
caches the last `RenderedRail` and scores mouse clicks against it, so the rail
the user sees is the rail clicks resolve on.

## RailTarget

What a clickable line resolves to: a tab (`tab_position`), a pane (`pane_id`),
or a peer session (`session`, carrying its attention tab). Pane targets cover a
multi-pane tab's tree lines and a single-pane tab's pane and detail lines.
Header, gap, and idle-strip lines have none. The runtime turns a `RailTarget`
into a `SwitchTab` / `ShowPane` / `SwitchSession` effect.

## Lockstep

The rail's load-bearing invariant: `ansi`, `targets`, and `hotspots` stay in
exact 1:1 line correspondence. It is structural, not discipline-held:
`render_rail` builds one `Vec<Line>`, each line carrying its own target and
optional hotspot, and `RenderedRail::from_lines` derives all three vectors from
it. There is no separate height predictor to drift.

## RadarState

The plugin's session-state module (`crates/plugin/src/radar_state.rs`): the
current view of tabs, live panes, observations, and snapshot serialization. It
composes `StatusStore` (pushed status) and `CommandStore` (observed commands),
both thin wrappers over one `ObservationStore` in `crates/core`, plus
`TabNamer`. The "status wins over command" precedence between the stores lives
in exactly one place, `RadarState::resolve`, which both `tab_display` and
`notify_views` read through.

Focus never drives rail state. A finished pane's status clears only through
shared inputs (a new broadcast, the return-to-shell exit-clear, a prune), so
every tab's instance converges. `note_focus` records the focused pane only so
the notifier can stay quiet about it. Pruning has a one-manifest grace
(`absent_once`); see design.md → *Per-pane to per-tab aggregation*.

## Status contract

The external seam between producers and the plugin: the `zj_radar.status.v1`
payload `{v, source, pane, status, repo, branch, msg, task, ack}`, parsed and
sanitized in `crates/core/src/payload.rs`. Ordering is latest-wins. Unknown
fields are ignored. `task` is a sticky label; `ack` means "the user already
saw this" and silences the notifier. The plugin is a caller too: the `✓`
gesture broadcasts a synthetic `done` with `ack: true` (`Effect::BroadcastStatus`)
instead of mutating local state. Every sender goes through the bounded argv in
`crates/core/src/pipe.rs` (`self_limiting_pipe_argv`). Field rules for
producers: [`docs/producers.md`](docs/producers.md#writing-your-own-producer);
the backpressure story: design.md → *The pipe contract*.

## Information source

Anything that produces a per-pane observation. Two modalities:

- **Pushed**: instrumented agents broadcast the status contract through the
  CLI. Each agent is an adapter behind `Agent::derive(&Intake) ->
  Option<AgentUpdate>` in `crates/cli/src/agents/`; `notify::run` is an
  agent-agnostic shell around it. The agent's `source()` string is the one
  vocabulary shared by the CLI argument, the wire, and `Kind::from_source`,
  pinned by `source_round_trips_through_kind`.
- **Observed**: uninstrumented commands (`cargo test`) the plugin watches via
  `CommandChanged`, classified by `crates/core/src/command.rs::classify`.
  Interactive commands (`DEFAULT_INTERACTIVE` plus the `interactive_commands`
  option) are observed but never earn a Running row
  ([`docs/activity-model.md`](docs/activity-model.md)).

The two meet at exit in the **producer-death model**: an agent's pane
returning to a shell prompt clears terminal statuses and arms a stale-Running
grace clock; the agent's argv reappearing cancels it; the pane manifest's
`exited` flag clears everything at once. See design.md → *Running exit grace*.

## Tab roll-up

Per-pane to per-tab aggregation: `rollup::roll_up(panes, resolve, quiet) ->
TabDisplay` in `crates/plugin/src/rollup.rs`. Severity is `error > pending >
running > done > idle`; the highest-severity pane supplies the detail line,
with a bounded job outranking a service on ties. `roll_up` owns its output
vocabulary (`TabDisplay`, `PaneDisplay`, `PrimaryDetail`, `ProgressCounts`,
`ExitOutcome`) and never learns there are two stores; `resolve` and `quiet`
are closures `RadarState` hands it. Tab status is never derived from tab
names.

## Settle

Whether notifications fire now or wait for the timer. Only events whose focus
is trustworthy settle: `panes_changed` (carries fresh focus) and the `timer`
tick. A `status_pipe` payload may arrive before the focus update that reflects
the user leaving, so it arms the timer instead. `RadarChange::settle` is the
flag; `project` fires `notify_effects` on it.

## Cadence

How often the one-shot timer re-fires (`PluginRuntime::desired_cadence`):
Fast (1 Hz) while there is tick-windowed work, Slow (once a minute) while only
minute-granular ages change or a presence heartbeat is owed, and disarmed
otherwise. Service rows and interactive rows never pin Fast. Trigger lists:
design.md → *Timer and cadence*.

## Render gate

Why an event repaints, or does not. Four layers keep repaints proportional to
change: intake no-ops (an identical re-broadcast reports a default
`RadarChange`), label-only deferral (a Running→Running relabel on an animating
row rides the next Fast tick), the rows-diff gate (`project` drops a render
whose content key equals the last one drawn), and the visibility gate (a rail
whose tab Zellij reported hidden paints nothing and writes no snapshot until
revealed, while its state keeps ticking). The first three rest on
`RadarState::generation` and the `rows()` memo; the fourth on
`Event::Visible`. A missed `touch()` on a new mutator is a stale-rail bug.
Detail: design.md → *Render gate*.

## Ledger

The completion history: a fixed-cap ring (`LEDGER_CAP` = 32) rendered as the
rail's trailing `─ earlier ─` region. `crates/plugin/src/ledger.rs` is pure
data and policy; `RadarState` wires every edge that retires a card into it
(`ledger_receded`) and prepares `LedgerLine`s; the renderer only consumes
them. An observation enters when it stops being a card fact, never before, and
a command completion shadowed by a status observation never enters. Entries
stamp epoch seconds, and age saturates at `1h+` so the timer can disarm.
Detail: design.md → *Ledger*.

## Tab naming

The policy that names tabs and remembers what it applied: `TabNamer::rename(tabs,
mode) -> Vec<TabRename>` in `crates/plugin/src/tab_namer.rs`, fed resolved
`TabFacts` by `RadarState::name_facts`. Candidates are one ordered list
(focused pane's repo, any repo, worktree-resolved cwd, title); an applied name
sticks while any pane still justifies it, and `Managed` never clobbers a manual
rename. Ownership is per tab: each instance names only the tab holding its own
plugin pane, resolved to a stable `TabId`. The once-per-pane cwd bootstrap
(`Effect::ResolveCwd`) feeds naming but is not naming policy.

## Cross-session presence

How one session's rail learns another's counts without asking Zellij. Each
plugin writes `zj-radar.presence.<zellij_pid>.json` into the shared `/cache`
root (`session_files.rs`); peers read the directory on every Slow tick and on
decimated Fast ticks, and feed `Sessions` (`sessions.rs`), pure state that
derives the badge on demand.
Liveness is the file's mtime, graded fresh (≤ 90 s) → stale (dimmed, skipped
by cycling) → dead (≥ 300 s, reaped and unlinked). Badge order and cycling
order come from one `ordered()`. The `✕` glyph is the manual dismiss. Detail:
design.md → *Cross-session badge*.

## Setup analysis

How `zj-radar setup` learns the state of the world: pure `analyze_*(&Env) ->
Facts` functions per target in `crates/cli/src/setup/analyze.rs`, fed
already-read values by the IO shell. `--check` and the install orchestrators
both project from `Facts`; the pure mutators (`edit_*`) share only the
low-level detectors in `setup/detect.rs`.
