# zj-radar cleanup + ship-polish findings

Behavior-preserving refactors (R) ranked by structural leverage, plus
test-hardening additions (T) requested for the public release. External behavior
is preserved throughout; the suite (insta snapshots, proptests, `reference_tests`,
wire round-trip guards) is the license to gut internals freely.

Mapped read-only across all three crates + the test suite. Several subagent
suggestions were **considered and rejected** (see the bottom section) because they
*add* surface (traits, generics, configurable policy) against the project's
enums-over-traits / deep-module rules.

---

## R1 · Unify `StatusStore` + `CommandStore` behind one `ObservationStore`   ★ high leverage

The canonical duplication `CONTEXT.md` itself names. Two stores hold the *same*
`HashMap<u32, TrackedObservation>` and implement the *same* lifecycle methods
(`on_pane_focused`, `recede_if_focused`, `prune`, `get`, `observations`,
`insert_snapshot_observation`, plus an "is anything active?" predicate). The only
real differences are intake (`apply` vs `on_command_changed`/`on_timer`/`on_exit`)
and the precedence rule — which already lives correctly in `RadarState`, not in
either store. This is a duplicated concept with a rule bolted on elsewhere.

Before (two parallel types, ~6 methods duplicated each):

    // crates/plugin/src/status_store.rs
    struct StatusStore { obs: HashMap<u32, TrackedObservation> }
    impl StatusStore {
        fn on_pane_focused(&mut self, id: u32, tick: u64) { … }   // identical
        fn recede_if_focused(&mut self, id: u32, tick: u64) { … } // identical
        fn prune(&mut self, live: &HashSet<u32>) { … }            // identical
        fn get(&self, id: u32) -> Option<&TrackedObservation> { … } // identical
        fn observations(&self) -> … { … }                        // identical
        fn insert_snapshot_observation(&mut self, …) { … }        // identical
        fn apply(&mut self, p: StatusPayload, tick: u64) { … }    // status-only
    }
    // crates/core/src/command.rs
    struct CommandStore { resolved, pending, pending_done, exited }
    impl CommandStore { /* the SAME 6 lifecycle methods, + on_command_changed/on_timer/on_exit */ }

After (one shared mechanism by *composition* — no trait):

    // crates/core/src/observation.rs
    struct ObservationStore { obs: HashMap<u32, TrackedObservation> }
    impl ObservationStore {
        fn on_pane_focused / recede_if_focused / prune / get / observations
           / insert_snapshot_observation / any_active   // the shared lifecycle, once
    }
    struct StatusStore  { store: ObservationStore }                 // + apply()
    struct CommandStore { store: ObservationStore, pending, pending_done, exited } // + intake

Composition, not a trait — there is no runtime heterogeneity to dispatch over, so
a trait would be a hypothetical seam. Removes ~6 duplicated methods.

**Blast radius:** `status_store.rs`, `core/command.rs`, `core/observation.rs`,
plus `RadarState`'s `.status()/.command()` accessors (delegation only). Net ~−60
to −90 lines. **Pinned by:** `radar_state/tests.rs`, `rollup/tests.rs`,
`command/tests.rs`, the `command_pane_walks_idle_running_done_idle` walk. Snapshots
must stay byte-identical.

---

## R2 · Collapse the duplicated setup `Outcome`-dispatch tail (zellij ↔ codex)   ★ high leverage

`setup_zellij` (zellij.rs:219–301) and `setup_codex_hooks` / `setup_codex_notify`
(codex.rs:49–123) each hand-roll the *same* `match Outcome { Unchanged | Conflict |
Changed }` control flow — dry-run? → print snippet → confirm → `confirm_and_write`
— differing only in the human strings. The shared write tail (`confirm_and_write`)
already exists; the duplicated part is the branching around it.

Before (one near-identical match per target):

    match outcome {
        Outcome::Unchanged if uninstall => println!(…),
        Outcome::Unchanged              => println!(…),
        Outcome::Conflict               => { eprintln!(…) }
        Outcome::Changed(new)           => { if dry_run {…} else { confirm_and_write(…) } }
    }

After (one shared driver, parameterised by the target's labels/messages):

    apply_edit_outcome(outcome, &EditCtx { label, path, uninstall, dry_run, yes, messages });

A plain helper (a small `struct EditCtx` of strings + the four arms), **not** a
`SetupTarget` trait — there is no open set of targets to abstract, and a trait here
would be a one-/two-impl hypothetical seam. (The CLI mapper's trait proposal is
deliberately *not* taken.)

**Blast radius:** `setup/zellij.rs`, `setup/codex.rs`, `setup/mod.rs`. Net ~−60 to
−100 lines, the dry-run/confirm/write branching tested once. **Pinned by:**
`cli_setup.rs`.

---

## R3 · Extract a shared marker-region strip helper (`layout.rs` ↔ `edit.rs`)   ★ med leverage

Two independent implementations of "find begin-marker, find end-marker, collapse or
delete the fenced region": `layout::uninstall` (layout.rs:327–398, WRAP/BLOCK
fences) and `edit::strip_managed_zellij_alias` (edit.rs:220–237, ALIAS fences).
Same algorithm, two sources of truth for how an edit is safely reversed.

Before: two bespoke `while`-loops scanning for their own marker pair.
After:

    fn strip_marked_region(lines: &mut Vec<String>, begin: &str, end: &str, collapse: bool) -> bool

called by both. **Blast radius:** `setup/edit.rs`, `cli/src/layout.rs` (likely a
shared free fn in one, used by the other). Net ~−40 lines. **Pinned by:**
`cli_setup.rs` uninstall cases + layout uninstall tests.

---

## R4 · Replace `Kind`'s hand-written serde with `wire_serde!(lenient, Kind)`   ★ med (small, very low risk)

`kind.rs:87–97` hand-writes `Serialize`/`Deserialize` that is byte-for-byte what
`wire_serde!(lenient, …)` generates — the exact macro `Status` and
`ObservationOrigin` already use. Pure duplication of the wire-encoding pattern.

Before (12 lines of boilerplate):

    impl serde::Serialize for Kind { fn serialize…(self.as_source()) }
    impl<'de> serde::Deserialize<'de> for Kind { fn deserialize…(Kind::from_source(&s)) }

After:

    wire_serde!(lenient, Kind);

**Blast radius:** `core/src/kind.rs` only. **Pinned by:**
`source_round_trips_for_every_kind`, `command_source_round_trips_through_kind`,
`to_wire_round_trips_through_parse`.

> Note: the core mapper's larger idea — *delete `kind.rs`, fold into `status.rs`* —
> is **rejected**. `Status` (lifecycle) and `Kind` (which tool) are orthogonal
> vocabularies; merging them is code-motion that couples unrelated concepts, and
> the deletion test fails (the enum still has to exist). Do R4 instead.

---

## R5 · Stop re-exporting `render/layout.rs` internals crate-wide   ★ low-med leverage

`render/layout.rs`'s planning types (`RowMeta`, `plan_layout`, `plan_overflow`,
`card_spacing`, `CardSpacing`, `card_block_lines`) are surfaced via
`pub(crate) use layout::*` in `render.rs:11`, but their only caller is `render.rs`
itself. The layout-planning seam is meant to live *behind* `render_rail`
(`CONTEXT.md` → Rail), yet it is visible to the whole crate. Tighten the
re-export to just what `render.rs` needs internally (or drop the `pub(crate) use`
and address as `layout::…`), so the planning intermediates can't leak into new
callers. **Blast radius:** `render.rs` import lines only; no behavior change.
**Pinned by:** the full render suite + reference tests (compile-level).

---

# Test hardening for the public release

Additive (not behavior-preserving-refactor) — explicitly requested. Strengthens
the external contract and the lockstep/color invariants, and adds the missing
end-to-end *behavior* coverage.

## T1 · Wire-contract property + boundary tests   ★ high

The `zj_radar.status.v1` payload is the only external interface, yet there is no
round-trip property test and the defensiveness is only spot-checked.

- proptest: arbitrary `StatusPayload` → `to_wire` → `parse` → **identical** (the
  producer/consumer contract).
- proptest: `sanitize(arbitrary string)` never panics, output ≤ `MAX_MSG_CHARS`,
  and contains **no** control bytes or surviving ANSI escapes.
- boundary unit tests: exactly `MAX_PAYLOAD_BYTES`; field truncation at 40/60;
  truncated CSI / unterminated OSC; non-terminal pane types; pane id `0` and
  `u32::MAX`.

**Where:** `crates/core/src/payload.rs` tests.

## T2 · Color-orthogonality + severity invariants as proptests   ★ high

`CONTEXT.md` claims "stripping SGR yields the exact same visible character grid"
and lockstep "`line_count() == ansi newline count`" — pinned today only by single
examples. Make them properties:

- proptest over `arb_rows × widths`: every rendered line `visible_len ≤ width`;
  strip-SGR yields an identical character grid; no dangling `ESC`; stripped
  newline count == raw newline count.
- proptest: `Status::Ord` ranks `idle < done < running < pending < error` and is
  transitive (the roll-up's `.max()` depends on it).

**Where:** `crates/plugin/src/render/tests.rs`, `crates/plugin/src/rollup/tests.rs`.

## T3 · End-to-end behavior scenarios in the live PTY harness   ★ high

The E2E layer (5 tests) proves load/pipe/render but **none of the interactive
behaviors** that define the product. Add live-Zellij scenarios for:

- **Idle recede:** running → done; focus away then back → still shown; the
  `reconcile_focus` contract from `CONTEXT.md`.
- **Overflow folding surfaces urgency:** many idle tabs + one pending agent →
  pending stays visible above the fold.
- **Snapshot rehydration:** a tab opened *after* agents are already running shows
  their real status (the `/cache`-backed seed path).
- **Click-to-switch** if the harness can inject a mouse event; otherwise document
  the limitation rather than silently skipping.

**Where:** `crates/plugin/tests/e2e/main.rs` (+ helpers in `harness.rs`).
Slow/serial, but the highest-confidence coverage for a public launch.

## T4 · Bash hook adversarial tests   ★ low-med

`notify.sh` is the producer most users touch first. Add bats cases: oversized
payload doesn't hang/OOM; embedded null bytes don't corrupt the emitted JSON.
**Where:** `plugins/zj-radar-claude/tests/notify.bats`.

---

# Considered and rejected (kept here so the trail is explicit)

- **Delete `kind.rs`, merge into `status.rs`** — code motion, couples orthogonal
  vocabularies; fails the deletion test. (Superseded by R4.)
- **`TabNamer`: expose the naming-tier order as configurable** — adds public
  surface to a deliberately deep module; the closed policy is the design.
- **`reconcile_focus`: encode the "no reconcile on status_pipe" rule via a sealed
  trait** — adds a trait for what is correctly a documented call-site discipline.
- **`roll_up`: take an abstract iterator instead of `&[TerminalPane]`** — adds
  generics; `TerminalPane` is the legitimate domain pane type, not a leak.
- **Fold the `Outcome` enum into render** — it is a good domain newtype the design
  intends; the display methods already live in `render`.
- **Relocate `lib.rs` test fixtures into `runtime.rs` / add `#[cfg(test)]` setters**
  — cosmetic; co-located tests reaching `pub(crate)` fields within the crate is
  fine and the `render_at` force-grant is documented intentional.
- **Extract `resolve_cwd`'s 8-line loop into a callback method** — marginal
  testability gain for an added closure param; the once-per-pane gate already lives
  in the runtime.
