# zj-radar — command activity (CommandChanged fallback) — design

**Status:** approved design (brainstormed 2026-06-27)
**Depends on:** `design.md` (base sidebar), `smart-tabs-postmortem.md` (no-blocking-host-calls law)
**Scope:** v1 = plugin-side detection of running / done / failed for non-agent panes. No progress, no shell hook.

## Goal

Surface non-agent command activity in the rail — builds, tests, deployments, any
long-running foreground command — so a pane that isn't running its own agent
producer still shows *working → done → failed*. This is the "general-purpose
fallback if the thing isn't producing its own events" originally requested.

It is **push-only**, consistent with the founding constraint: the plugin never
calls `get_pane_running_command`/`get_pane_cwd` or any blocking host query. All
signal comes from pushed events.

## Source (decided): plugin-side, zero producer

Approach **A** (of A/B/C considered). The plugin subscribes to the
`CommandChanged` event (Zellij 0.44.2+, no permission required) and reads
`PaneInfo.exit_status` from the already-handled `PaneUpdate`. No shell config, no
new producer, works for every pane on install.

**Probe-confirmed runtime behavior** (branch `probe/command-changed`, 2026-06-27):
- `CommandChanged(PaneId, Vec<String> command, bool is_foreground, Vec<ClientId>)`
  fires on foreground change in **both** directions: command start (`fg`, argv)
  and return-to-shell (`bg`, the shell, e.g. `/bin/zsh`).
- Detection is **sampled** (~1s, tied to serialization). Long commands are
  caught reliably; sub-second commands (`ls`) slip the window — desired for a
  status rail.
- The shell at a prompt reports `is_foreground == false`. A real running command
  reports `is_foreground == true`.
- **Prompt noise:** `starship` (and any prompt framework) fires transient
  `fg (starship)` → `bg` blips on prompt redraw, within one sample tick.
- **Exit codes:** `zellij run -- false` → `EXIT status=Some(1)`; `-- true` →
  `Some(0)`. Available for **command panes** only (shell-typed commands carry no
  exit code through this path).

Deferred (not v1): a zsh/bash `preexec`/`precmd` hook (approach B/C) broadcasting
over the existing `zj_radar.status.v1` pipe to add `✗` for *shell-typed*
failures. The wire contract already exists, so it bolts on later without rework.

## Architecture & data model

Command activity is a **parallel channel** the agent pipe overrides.

- **New `command` module** with a `CommandStore`, keyed by `pane_id`, mirroring
  `state::StateStore`. Each entry carries the same fields the renderer already
  consumes (`status`, `repo`, `branch`, `msg`, `last_change_tick`,
  `on_focus`) plus debounce bookkeeping. Command activity therefore resolves
  into the **same `AgentState`/`Detail`/`Status` types the rail already draws**.
- **Precedence resolution** at aggregation. A pane is *agent-owned* iff it has an
  entry in `StateStore` (the pipe has spoken for it). `model::aggregate` resolves
  each pane as **`StateStore.get(id)` if present, else `CommandStore.get(id)`**.
  A Claude pane never flips to raw `fg node`; a plain pane with no producer gets
  command status. Agent-then-command in one pane stays agent-owned (no
  flip-flop) — accepted v1 behavior.
- **Event wiring in `lib.rs`** (thin glue, push-only):
  - subscribe to `EventType::CommandChanged`.
  - `Event::CommandChanged` → `CommandStore` state machine.
  - existing `PaneUpdate` loop additionally feeds `PaneInfo.exit_status` into
    `CommandStore` (deduped on `pane → exit_status`).
  - `prune` `CommandStore` alongside `StateStore` on every `PaneUpdate`.
  - timer-arm condition gains "or a pending/running command exists".

## Status derivation, debounce, lifecycle

`CommandChanged(pane, command, is_foreground)`:

- `is_foreground == false` → the pane is idle / has finished.
- `is_foreground == true` → a **candidate**. Recorded as a *pending* entry
  `{ command, since_tick }`, **not** promoted to `Running` yet. Promotion is
  **timer-driven**: on a `Timer` tick, any pending entry whose age
  `now - since_tick >= DEBOUNCE_TICKS` (=1, ~1s) is promoted to `Running`.
  A return-to-shell (`is_foreground == false`) or a superseding `CommandChanged`
  **clears** the pending entry, so prompt blips (`fg (starship)` → `bg` within
  one sample) never reach promotion — prompt-tool-agnostic, no fragile name
  list. A small shell/prompt basename ignore-set (`zsh`, `bash`, `fish`, `sh`,
  `dash`, `starship`) short-circuits obvious noise before it's even recorded
  (belt-and-suspenders). Promotion latency ≤ ~1 tick, already acceptable.
  The timer-arm condition therefore includes "a pending entry exists" so the
  promotion tick actually fires.

Concretely, `CommandStore` reuses `state::AgentState` for the **resolved** view
(one `HashMap<u32, AgentState>`), plus a parallel `HashMap<u32, Pending>` for
debounce and a `HashMap<u32, Option<i32>>` for exit dedupe. Reusing `AgentState`
is the key seam: `model::aggregate` then resolves a pane as
`agent.get(id).or(command.get(id))` — both `&AgentState`, uniform downstream.

Display fields for a command entry:
- `msg` = cleaned command: `basename(argv[0])` + remaining args (e.g. `cargo
  test`), truncated via the existing sanitizer budget.
- `repo` = `basename(pane_cwd)` (cwd tracked via `CwdChanged`); empty if no cwd
  seen yet — row still renders.
- `branch` = empty (the plugin cannot run git; command rows omit branch).

Finish & failure:
- Foreground returns to shell while `Running` → `Done`, with `on_focus = Idle`
  (persists across tabs, clears when you focus that pane — mirrors agents).
- `PaneInfo.exit_status` (command panes): `Some(0)` → `Done`; `Some(n≠0)` →
  `Error` (the `✗`). Refines the coarse `Done` with real pass/fail when present.
- `CommandStore` has its own `on_pane_focused` (clear-on-focus), called from the
  same `PaneUpdate` focus path as `StateStore`.

## Rendering

No `render.rs` changes. `render` already maps `Status` → glyph/color (`◐`/`●`/`✗`)
and draws `repo·branch · status · elapsed` + message. Command panes flow through
`model::aggregate` → `TabRow` → the existing renderer unchanged, including
multi-pane tab aggregation (severity-wins, `done/total`, roster).

`naming.rs` is unchanged: tabs are still named from pane title / cwd, not the
running command (out of scope for v1; avoids churn).

## Edge cases & non-goals

- Agent-then-command in one pane: stays agent-owned (command ignored).
- cwd unknown: `repo` empty; row still renders.
- Command pane lingering after exit: shows `Done`/`✗` until focused or pruned.
- Quick commands (`ls`): intentionally not shown (sample window + debounce).
- **Non-goals (v1):** progress percentage; exit codes for shell-typed commands
  (only `zellij run` panes get `✗`); shell hook; git branch for command rows;
  command-derived tab naming.

## Testing

All pure, host-testable (`cargo test`, no wasm), matching the existing
pure-logic discipline. New `command` module tests:
- fg non-shell command surviving a tick → `Running`; fg blip flipping to bg
  before the confirm tick → never `Running` (debounce).
- fg → bg → `Done` with `on_focus = Idle`; `on_pane_focused` clears to `Idle`.
- `exit_status` `Some(0)` → `Done`, `Some(3)` → `Error`; repeated manifest
  deduped (one transition).
- `msg` = `basename(argv[0]) + args`; `repo` = `basename(cwd)`; branch empty.
- precedence: a pane with a `StateStore` entry resolves to agent status,
  ignoring `CommandStore`.
- `prune` drops command state for dead panes.

Thin host glue (the `Event::CommandChanged` arm, exit wiring in `PaneUpdate`,
timer-arm condition) stays minimal and is exercised manually — the same
split the project already uses.

## Implementation note

The `probe/command-changed` branch is a throwaway instrument (replaces `render`,
widens `dev/dev.kdl`, free-running timer). It must **not** be merged. Implement
v1 fresh from `main`; the probe stays only as the record of confirmed behavior.
