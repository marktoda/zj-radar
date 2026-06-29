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

What a clickable line resolves to: a tab to switch to (`tab_position`) or, for an
expanded multi-pane row, a specific pane to show (`pane_id`). Header, gap, and
idle-strip lines have no `RailTarget`. The runtime turns a `RailTarget` into a
`SwitchTab` / `ShowPane` effect on click.

## RadarState

The plugin's session-state module: the current radar view of tabs, live terminal
panes, pane observations, focus transitions, and snapshot serialization.
`RadarState` is not a replacement for the source-specific stores; it composes
`StatusStore` (status-payload observations) and `CommandStore` (command-derived
observations) with live pane topology, then produces `TabRow`s for the rail. It
also composes `TabNamer` for tab naming — assembling the resolved facts that seam
consumes, the same way it hands `roll_up` a `resolve` closure.

The runtime owns host concerns: permission flow, timers, rendered-rail caching,
and turning repo-owned outcomes into Zellij effects. The rail owns layout and
click-target lockstep. `RadarState` owns the domain facts between those seams.

## Tab naming

The policy that decides what each tab is called, and remembers what it last
applied. The **tab-naming seam** is `TabNamer::rename(tabs, mode) -> Vec<TabRename>`
in `src/tab_namer.rs`: a deep module fed resolved `TabFacts` (per-tab `id`,
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
`zj_radar.status.v1` pipe payload (`{v, source, pane, status, repo, branch, msg,
on_focus, seq}`). Producers (the Claude plugin, the Codex CLI) are adapters that
broadcast it; the plugin defends itself at parse time (sanitize, truncate, drop
oversized/out-of-order).

## Information source

Anything that produces a per-pane observation. Two modalities, both converging on
a `Kind`-keyed `Status`:

- **Pushed** — instrumented agents report rich status by broadcasting the *status
  contract* through the host CLI (`zj-radar notify <agent>`). Each agent is a peer
  adapter behind the **agent intake** seam — `Agent::derive(&Intake) ->
  Option<AgentUpdate>` in `src/cli/agents/` — so `notify::run` is a thin,
  agent-agnostic shell (read input → derive → broadcast). Adding an agent is a
  compiler-guided `enum Agent` variant; its `source()` string is the single
  vocabulary shared across the CLI argument, the wire `source`, and
  `Kind::from_source`, pinned by the `source_round_trips_through_kind` guard test.
- **Observed** — uninstrumented commands (e.g. `cargo test`) that Radar watches
  from outside. The plugin classifies the observed argv via
  `command.rs::command_source` and infers status from the process lifecycle. No
  wire, no CLI. `cargo test` lives here, **not** in `agents/`.

Both modalities emit a `source` string that must be a subset of `Kind`
(`Kind::from_source`). Both halves are guarded: the agent half by
`source_round_trips_through_kind` (in `cli/agents`), the command half by
`command_source_round_trips_through_kind` (in `command.rs`) — each pins that its
classifier's `source` token round-trips back to the same `Kind`, never the
`Other` sentinel.

## Tab Roll-Up

The per-pane → per-tab roll-up: severity order `error > pending > running > done >
idle`, with `done/total` counts and a highest-severity detail line. Tab status is
never derived from tab names — a single tab can hold several agent panes.

The **roll-up seam** is `rollup::roll_up(panes, resolve) -> TabDisplay`: a deep,
pure module that owns its output vocabulary (`TabDisplay`, `PaneDisplay`,
`PrimaryDetail`, `ProgressCounts`, `Outcome`) — the renderer *consumes* these, so
presentation depends on the roll-up, not the reverse. `resolve(pane_id) ->
Option<&TrackedObservation>` is the only thing crossing in: the "status pipe wins
over command" precedence across observation sources stays in `RadarState`, so
`roll_up` never learns there is more than one store. `Outcome`'s display methods
(`full`/`minimal`/`role` — glyphs and width-driven forms) live in `render`; the
enum here is pure semantics.
