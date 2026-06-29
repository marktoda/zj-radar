# Design: `zj_radar.cmd.v1` command pipe + attention-tab cycling

**Date:** 2026-06-29
**Branch:** `cmd-pipe-attention-nav`
**Status:** approved design, pre-implementation

## Context

zj-radar is a passive, push-driven Zellij sidebar. It deliberately does **not**
subscribe to `EventType::Key` and calls `set_selectable(false)` after the
permission grant, so it never steals keyboard focus. Its only external seam is
the versioned `zellij pipe` payload, dispatched in `lib.rs::pipe()` by message
name: `zj_radar.status.v1` (agent status) and `zj_radar.config.v1` (config
overrides). The only interactive path today is `Mouse::LeftClick`, which routes
through `runtime.mouse_click(line) -> Outcome { render, effects }` and emits
`Effect::SwitchTab { position }` / `Effect::ShowPane { pane_id }`.

We want keyboard control of the sidebar **without** turning it into an
interactive panel (no `Key` subscription, no focus grab). The mechanism is
Zellij's `MessagePlugin` keybind action, which delivers a named pipe message
straight to `pipe()`.

This spec covers the first slice: a new **imperative** command pipe and the
**attention-next / attention-prev** navigation verbs. Documentation of the
existing declarative `config.v1` pipe as keybinds (already-shipped behavior)
ships alongside in the README.

## Goals

- Add a third pipe name, `zj_radar.cmd.v1`, carrying a **bare verb string**.
- Implement `attention-next` and `attention-prev`: cycle keyboard focus through
  the tabs that need attention, by tab index.
- Stay passive: no new event subscription, no `set_selectable(true)`, reuse the
  existing `Effect::SwitchTab` host effect.
- Be safe under the per-tab-instance broadcast multiplicity with **no lock**, by
  computing a deterministic absolute target (idempotent across instances).

## Non-goals (designed, deferred)

- `attention-top` (jump to most-urgent), `cycle-density`, `toggle-header`,
  `toggle-collapse`, `ack-all`. The pipe and `Command` enum are shaped to accept
  these later; they are out of scope for this slice.
- `LaunchOrFocusPlugin` / floating "peek" mode — an explicit design non-goal
  (`docs/design.md:423`); it would reintroduce `Key` handling and focus-grab.
- JSON command payloads. Bare verb strings are sufficient for v1; if a future
  verb needs arguments it can carry JSON under the same `.v1` name or bump.

## The command pipe

- **Name:** `zj_radar.cmd.v1`.
- **Payload:** a single trimmed verb string (e.g. `attention-next`). Unknown or
  malformed verbs are a **silent no-op** — consistent with the repo's
  "parsing never fails / unknown keys ignored" stance for `config.v1`.
- **Declarative vs imperative:** `config.v1` *sets* state and cannot toggle;
  `cmd.v1` *runs verbs*. This is the reason a config toggle needs an imperative
  verb (deferred) rather than another `config.v1` key.

## Attention semantics

- **Attention set:** a tab needs attention if its rolled-up display status is
  `pending`, `error`, or `done`. `running` and `idle` are skipped. (The rollup
  per tab is already computed for rendering via `rollup::TabDisplay`.)
- **Urgency ranking** is `pending > error > done`. With cycle-by-index it does
  **not** affect traversal order; it is recorded for a future `attention-top`
  and documents membership intent.
- **`attention-next`:** among attention tabs, select the smallest `position`
  strictly greater than the active tab's `position`; if none, wrap to the
  smallest attention `position`. **`attention-prev`:** symmetric (largest
  `position` strictly less; wrap to the largest).
- **Edge cases:** empty attention set → no-op; the active tab is the only
  attention tab → no-op (target == current). Both produce an empty `Outcome`.
- **Permission gate:** inert until permission is granted, mirroring
  `mouse_click` (`lib.rs:325`, `runtime.rs` permission guard).

### Idempotency under broadcast (why no lock)

`MessagePlugin` broadcasts by plugin URL, and the radar runs one instance per
tab. Every instance receives `attention-next` simultaneously. Because the target
is a **deterministic function of global state** — all instances see the same
`TabUpdate` (active tab `position`) and the same per-tab rollup — every instance
computes the *same* `Effect::SwitchTab { position: N }`. Applying the identical
absolute switch from K instances is indistinguishable from applying it once.
No session-file lock is needed (unlike the permission flow). **Determinism of
the absolute target is the design constraint every navigation verb must meet.**

## Modules / seam

- **New `src/cmd.rs`** — small, deep boundary:
  - `enum Command { AttentionNext, AttentionPrev }`
  - `Command::parse(&str) -> Option<Command>` (trim, match; `None` for unknown).
- **`RadarState::next_attention_tab(active: Option<usize>, dir: Direction) -> Option<usize>`**
  — a **pure** function over the stored tabs (`position` + `active`) and the
  per-tab rollup. Returns the target `position`, or `None` for no-op. This is
  the testable core; it owns the ordering/wrap/membership logic.
- **`PluginRuntime::command(cmd: Command) -> Outcome`** — sibling to
  `mouse_click`. Resolves the active tab position, calls `next_attention_tab`,
  and wraps a `Some(position)` in `Effect::SwitchTab { position }` (empty
  `Outcome` otherwise). Honors the existing permission gate.
- **`lib.rs::pipe()`** — one new `else if message.name == CMD_PIPE` arm that
  parses the payload via `Command::parse`, calls `runtime.command`, and feeds the
  `Outcome` through the existing `handle_outcome` glue. Add
  `const CMD_PIPE: &str = "zj_radar.cmd.v1";`.

No new `subscribe()` entry. No new `Effect` variant (`SwitchTab` already exists).

## Testing (matches the 5-layer harness)

- **proptest** on `next_attention_tab`:
  - Cycling invariant: starting from any active tab, repeated `Next` over an
    M-member attention set visits each member exactly once before repeating
    (a cyclic permutation), and returns to the origin after M steps.
  - `Next` then `Prev` from the same active position is identity when |set| > 1.
- **unit** (`radar_state` / `cmd`): empty set → `None`; single attention tab ==
  active → `None`; single attention tab != active → that tab; wrap-around in
  both directions; `Command::parse` accepts the two verbs (trimmed,
  case-sensitive lower) and rejects unknown/empty.
- **runtime** (`runtime.rs`): `command(AttentionNext)` emits the expected
  `Effect::SwitchTab`; inert (empty `Outcome`) without permission; no-op when the
  attention set is empty.
- **pipe** (`lib.rs`): a `cmd.v1` message with a known verb routes and emits the
  effect; unknown verb / empty payload is a silent no-op; an unrelated pipe name
  is unaffected.

## Documentation

- README "Binding keys to runtime config" subsection (already added) documents
  the `config.v1` keybind recipe via `MessagePlugin`.
- Add a sibling subsection for `cmd.v1` with `attention-next` / `attention-prev`
  `MessagePlugin` keybind examples, and note the bare-verb payload format.
- A one-line note in `docs/design.md` future-work that the deliberate
  passive-renderer keybind path is `MessagePlugin` → `cmd.v1`, not
  `LaunchOrFocusPlugin`.

## Out-of-scope risks noted

- "Active tab position" is read from the stored `RadarTab.active`/`position`, not
  derived from the focused pane — confirm a single tab is flagged `active` after
  `TabUpdate` (Zellij guarantees exactly one).
- If a future non-idempotent verb is added (e.g. `ack-all` mutating the status
  store), it must either be proven consistent across instances or gated by the
  existing session-file lock — out of scope here.
