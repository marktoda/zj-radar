# Recede a completion that finished while focused

**Date:** 2026-06-29
**Status:** implemented

## Problem

The rail's job is to surface work you *might have missed*. But a pane that finished
while you were focused on it currently **persists** as `Done` until you leave and
re-enter it (documented at `State::apply_focus_transition`, the old "stays lit
until visited" intent). That flags something you already watched finish — noise.

Desired rule (from the design's completion table): **"If they were looking at it
when it finished, don't flag it."** A completion that lands on the focused pane
should recede immediately.

Scope chosen: **all successful completions** (agents *and* commands/tasks) recede
when focused. Errors are exempt (they persist even when watched). `Pending` ("needs
you") is not a completion and is never auto-dismissed. Background completions are
unchanged — they persist until visited.

## Mechanism

The codebase already has the right primitive: `on_focus: Option<Status>` is a
*queued transition* — "what this pane becomes when next visited." A completion sets
`on_focus = Some(Idle)`. **Recede-while-focused = apply that queued transition
immediately, at completion time, when the finishing pane is the focused one** —
instead of waiting for a future visit.

The decision lives in `RadarState`, the only place that knows both the completion
and `last_focused`; the stores stay focus-agnostic (handed a pane id, they forward).

### Components

- `TrackedObservation::recede_on_focus(tick)` — applies the queued `on_focus`
  **only when `status == Done`**. Sibling of `apply_on_focus`; the status guard is
  the sole difference between "you saw it finish" (recede) and "you came back to
  it" (visit-clear, which clears any state including errors).
- `StatusStore::recede_if_focused(pane_id, tick)` / `CommandStore::recede_if_focused(pane_id, tick)`
  — thin passthroughs to the observation method.
- `RadarState::settle_focused(tick)` — recedes `last_focused`'s completion. Wired
  into the two events where focus is trustworthy:
  - `panes_changed` — after `apply_focus_transition` (this update's fresh focus;
    the command-exit path)
  - `timer` — on the cadence tick (the watched-agent path, and return-to-shell
    confirms)

  It is **deliberately not** wired into `status_pipe`. A pipe payload is a raw
  completion edge that can arrive in the gap between the user leaving the pane and
  the focus `PaneUpdate` being processed, so `last_focused` may still name the pane
  the user just left. Receding there would silently drop a completion the user
  should see. The timer (armed by the runtime on the pipe event) carries the
  recede instead, firing once focus has settled — so a genuinely-watched agent turn
  still recedes within a tick, while one navigated-away-from stays lit. (Found in
  self-review.)

### Behavior matrix (focused pane)

| Finishes while focused | Result |
|---|---|
| `Done` (agent or task/command) | recede → Idle instantly, no badge |
| `Error` | persists ✗ (clears only on a later visit) |
| `Pending` ("needs you") | persists (not a completion) |

Background panes: unchanged — persist until visited (`apply_focus_transition` on entry).

## Why this is safe (no flicker)

An earlier version cleared the focused pane on *every* `PaneUpdate`, so a
finish-while-focused raced a focus-move → direction-dependent `Done↔Idle` flicker.
The fix then was to gate clearing on a focus *transition* (giving the "stays lit
while focused" behavior this change reverses).

Recede is **monotonic**: `Done → Idle` happens once and `on_focus` is then `None`,
so however many times `settle_focused` runs (e.g. every timer tick) it cannot
oscillate. That is the property that makes it safe regardless of update ordering —
unlike the predecessor, which re-ran an ungated clear on every update and so raced
focus-moves. The transition-gate on `apply_focus_transition` stays — it still owns
the background-visit clear, and is what prevents a focused *error* from being wiped
on the next update (`settle` skips errors; the ungated visit-clear would not).

## Tests

- `observation`: `recede_on_focus` clears Done, leaves Error and Pending.
- `status_store` / `command`: `recede_if_focused` clears Done, not Error.
- `radar_state` (full flow): agent-done-focused recedes (via `status_pipe`);
  command-exit-0-focused recedes (via `panes_changed`); return-to-shell-focused
  recedes (via `timer`); error-focused persists; background-done persists then
  clears on visit; recede is direction-independent across the next focus move.
- `lib`: the `apply_focus_transition` primitive still clears only on entry
  (renamed `focus_transition_clears_only_on_entry`, narrative corrected). The old
  `done_pane_left_behind_is_direction_independent` (which encoded the reversed
  "stays Done" behavior) is replaced by the new-behavior direction-independence
  guard in `radar_state`.

## Out of scope / follow-ups

- The original rules' "long task → brief ✓ for ~3-5s then fade" refinement (this
  change recedes tasks immediately, same as agents). A fade would add a
  deadline-tick to the recede rather than clearing instantly.
- Suppressing badges for *manual* shell commands (`Kind::Command`) regardless of
  focus — a separate rule.

## Update — consolidation into `reconcile_focus`

The two `RadarState` focus methods this spec introduced as separate —
`apply_focus_transition` (visit-clear) and `settle_focused` (recede) — were
subsequently merged into one **`reconcile_focus(focused, tick)`**. It derives the
visit-vs-recede choice from whether focus changed: a focus *entry* visit-clears the
entered pane (Done or Error); focus *held* recedes a fresh Done only. This is
behavior-identical to the two-method form but removes the `panes_changed`
transition-then-settle overlap (review finding [1]) and centralizes the decision in
one place — anticipating the fade follow-up above, which makes `on_focus`
re-queueable and would otherwise activate that overlap. The store/observation
forwarders (`on_pane_focused`/`recede_if_focused`, `apply_on_focus`/`recede_on_focus`)
are unchanged.
