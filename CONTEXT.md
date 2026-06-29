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

The rail's canonical *visual* design — the "gutter rail" (Direction C: 2-column
status gutter, theme-adaptive color roles, glyph sets, overflow folding,
onboarding panel) — is
`docs/superpowers/specs/2026-06-26-zj-agents-gutter-rail-design.md`, which
supersedes `docs/design.md §3`. Color is **purely additive**: stripping SGR from
the rail yields the exact same visible character grid, so layout and color are
orthogonal and testable apart.

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
panes, pane observations, focus transitions, snapshot serialization, and rename
ownership. `RadarState` is not a replacement for the source-specific stores; it
composes `StatusStore` (status-payload observations) and `CommandStore`
(command-derived observations) with live pane topology, then produces `TabRow`s
for the rail.

The runtime owns host concerns: permission flow, timers, rendered-rail caching,
and turning repo-owned outcomes into Zellij effects. The rail owns layout and
click-target lockstep. `RadarState` owns the domain facts between those seams.

## Lockstep

The load-bearing invariant of the rail: the emitted ANSI and the click-target map
stay in exact 1:1 line correspondence. `line_count() == ansi newline count`, and
every drawn line maps to the intended target (or a deliberate `None`). Lockstep is
why click-to-switch lands on the row the user pointed at. It is verified as a
property of `RenderedRail` through `render_rail` — never by re-deriving heights
from a separate predictor, and never by re-rendering at a different width than was
drawn.

## Status contract

The real external seam between producers and the plugin: the versioned
`zj_radar.status.v1` pipe payload (`{v, source, pane, status, repo, branch, msg,
on_focus, seq}`). Producers (the Claude plugin, the Codex CLI) are adapters that
broadcast it; the plugin defends itself at parse time (sanitize, truncate, drop
oversized/out-of-order).

## Tab Roll-Up

The per-pane → per-tab roll-up: severity order `error > pending > running > done >
idle`, with `done/total` counts and a highest-severity detail line. Tab status is
never derived from tab names — a single tab can hold several agent panes.
