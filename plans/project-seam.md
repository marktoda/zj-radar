# Plan — the `project` seam: one home for domain-change → host-effect

## Why

Today every `PluginRuntime` event handler hand-assembles its own `Vec<Effect>` from
an **incomplete** `RadarChange`, re-interrogating `RadarState` for notify/timer
decisions, in a per-handler order. No two handlers do it the same way, and two of
them (`tabs_changed`, `command_changed`) silently **drop** fields of the
`RadarChange` they receive. The subtle hazards — notify-baseline drift (a handler
that skips `notify_effects()` never advances `notify_prev`) and load-bearing but
unenforced effect ordering — have no single home or test surface.

This is a **hardening / locality refactor, not a bugfix.** The current code works
and is covered by ~25 runtime integration tests. The payoff is: one projection
seam, one test surface for the ordering + baseline invariants, and the silent
field-drops become structurally impossible.

Not in scope: the permission-policy collapse (separate review candidate), the
render/layout test-reach, the store asymmetry.

## The seam (settled design)

`RadarChange` becomes the **complete** carrier of what an event produced:

```rust
pub(crate) struct RadarChange {
    pub render: bool,
    pub persist_snapshot: bool,
    pub renames: Vec<TabRename>,
    pub cwd_bootstrap: Vec<u32>,
    pub settle: bool,      // reconcile + notify move together (the "Settle" rule)
    pub arm_timer: bool,   // PROVISIONAL host-scheduling flag — see Step 1.5
}
```

One projection in the runtime is the **sole** caller of `notify_effects()` and
`arm_timer_if_needed()`. It accepts a seed `Vec<Effect>` so the `timer` handler can
supply its permission effects *first* without a post-hoc `splice`:

```rust
fn project(&mut self, mut fx: Vec<Effect>, c: RadarChange) -> Outcome {
    fx.extend(self.effects_from_renames(c.renames));         // 1. renames
    if c.persist_snapshot          { fx.push(Effect::PersistSnapshot); }        // 2.
    if !c.cwd_bootstrap.is_empty() { fx.push(Effect::ResolveCwd { pane_ids: c.cwd_bootstrap }); } // 3.
    if c.arm_timer                 { self.arm_timer_if_needed(&mut fx); }        // 4. SetTimeout
    if c.settle                    { fx.extend(self.notify_effects()); }         // 5. Notify
    Outcome::with_effects(c.render, fx)
}
```

Canonical order is renames → snapshot → cwd → SetTimeout → notify, which is
**identical to today's `panes_changed`** (the richest handler), so that handler is
byte-for-byte unchanged. All effects `project()` emits touch disjoint host state
(and multiple `RenameTab`s target distinct positions), so any fixed order is
host-equivalent — only test assertions that pin positions need touching.

Per-handler stamps (`RadarState` sets `settle`/`arm_timer` on the way out; the
`timer` handler synthesizes its change in the runtime):

| Handler         | settle | arm_timer | routed through `project`? |
|-----------------|:------:|:---------:|:-------------------------:|
| `panes_changed` |  true  |   false   | yes |
| `timer`         |  true  |   true    | yes (seed = permission effects) |
| `status_pipe`   |  false |   true    | yes |
| `command_changed`| false |   true    | yes |
| `cwd_changed`   |  false |   false   | yes |
| `config_pipe`   |  false |   false   | yes |
| `tabs_changed`  |  false |   false   | yes |
| `permission_result`, `mouse_click`, `command` | — | — | no (permission / navigation families, no `RadarChange`) |

`settle` gates reconcile (a domain op) + notify, and `RadarState` reads it back in
Step 2 — so it belongs on the domain struct. `arm_timer` is consumed **only** by
the runtime; it is provisional (Step 1.5).

---

## Step 1 — faithful projection (behavior-preserving)

**Goal:** zero behavior change. Extract `project()`, add the two fields, route the
six `RadarChange` handlers + `timer` through it. `reconcile_focus` stays an
intrinsic call inside the radar methods (untouched this step).

### Code
- `radar_state.rs`: add `settle` + `arm_timer` to `RadarChange`; each entry point
  stamps them per the table. `tabs_changed`/`command_changed` now return their real
  (currently-empty) `renames`/`cwd_bootstrap` instead of relying on the runtime to
  drop them — kills the silent-drop fragility.
- `runtime.rs`: add `project(seed, change)`. Rewrite `panes_changed`, `status_pipe`,
  `cwd_changed`, `command_changed`, `config_pipe`, `tabs_changed` to
  `self.project(vec![], self.radar.X(..))`. Rewrite `timer` to compute permission
  effects + `render`, synthesize `RadarChange { settle:true, arm_timer:true, .. }`,
  and call `self.project(permission_effects, change)`.
- `notify_effects()` / `arm_timer_if_needed()` now have exactly one caller
  (`project`). Leave their bodies unchanged.

### Test deltas
- `status_pipe_…arms_timer_and_persists_snapshot` (runtime.rs ~735): today asserts
  `effects[0]==SetTimeout, effects[1]==PersistSnapshot`. Canonical order swaps these
  (snapshot before SetTimeout). **Relax to `contains` / order-insensitive** — the
  order contract moves to the pin below.
- Add `project_emits_effects_in_canonical_order`: one test that pins renames →
  snapshot → cwd → SetTimeout → notify. Sole home of the order contract.
- Every other runtime test (`contains`/`any`/`matches!`) passes unchanged.
- Add a cheap guard: `cwd_changed` never emits `ResolveCwd` (its change's
  `cwd_bootstrap` is empty), so the `ResolveCwd`→`cwd_changed` re-entry can't
  recurse — encodes the bound that was docstring-only.

### Verify
- `just ci` (test + clippy + test-bash). Green with only the two intended test
  edits = behavior preserved.

### Docs
- Add the **`project` seam** to `CONTEXT.md` (now that it exists): domain change →
  host effect, sole home of notify/arm, canonical order, fed by radar stamps.
  Cross-link the existing `## Settle` section.

---

## Step 1.5 — try to delete `arm_timer` (probe, not a promise)

**Hypothesis:** `arm_timer_if_needed()` is self-guarding
(`!timer_armed && (waiting || selectable || has_active_or_pending_work())`). The
handlers that currently *skip* arming (`panes_changed`, `cwd_changed`,
`config_pipe`, `tabs_changed`) cannot *create* new pending work, so wherever they
run with work present the timer was already armed by whatever created it — making
an unconditional arm a **no-op** there.

**Experiment:** delete the `arm_timer` field; call `arm_timer_if_needed()`
unconditionally inside `project()`. Run `just ci`.
- **Green** → the flag was dead weight. Keep the deletion. `RadarChange` is now
  purely domain (`render, persist_snapshot, renames, cwd_bootstrap, settle`), and
  no host-scheduling policy leaks onto the domain struct. Preferred outcome.
- **Red** → the asymmetry is load-bearing. Revert; keep `arm_timer`; **add a test**
  capturing the exact scenario that armed unexpectedly, and a comment on the field
  explaining why it can't be unconditional. We now know *why* it exists.

Either result is a win; today we don't know which. Do this as its own commit so the
outcome is legible in history.

---

## Step 2 — make the Settle coupling structural (explicit)

**Goal:** stop reconcile and notify from being two statements that *happen* to
agree; make them one decision.

### Code
- `radar_state.rs`: in each entry point, compute `settle` first, then **gate the
  `reconcile_focus(...)` call on it** (`if settle { self.reconcile_focus(focus, tick) }`)
  and stamp the same `settle` on the returned `RadarChange`. `panes_changed` uses
  fresh focus, `timer` uses settled `last_focused` — unchanged; only the gating is
  new. For the handlers that never reconcile, `settle=false` and the gate is a
  no-op, so nothing changes.

### Why it's provably equivalent
`settle`'s per-handler values already match exactly where `reconcile_focus` is
called today (verified: reconcile and notify are perfectly coupled across all 7
handlers). Gating the existing call on a flag that's true precisely there is a
no-op — enforced by the unchanged reconcile/recede tests
(`reconcile_focus_recedes_held_done_persists_error_and_clears_on_entry`, the
command-lifecycle walks, the notify tests).

### Verify
- `just ci`. Green with no test edits = equivalent.

### Docs
- Tighten the `## Settle` section: the rule is now structural (one gated decision),
  not an invariant held by discipline.

---

## Rollback / risk

- Each step is an independent commit; Step 1.5 is explicitly a probe that may
  revert. Step 2 is a no-op-by-construction internal tidy.
- Highest residual risk is Step 1's effect-order change to `status_pipe`; mitigated
  by the host-disjointness argument and the single order pin.
- No change to the wire contract, the rail, roll-up, tab-naming, or setup — the
  blast radius is `runtime.rs` + `radar_state.rs` + their tests.
