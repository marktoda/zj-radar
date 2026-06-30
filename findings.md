# Cleanup + ship-hardening review — findings

Behavior-preserving cleanup (the externals are pinned by the suite) **plus** test
hardening, since the owner explicitly asked for stronger coverage ahead of a
public release. Mapped read-only across all four code slices and all five test
layers (six parallel agents + first-hand verification of the load-bearing files).

**Headline:** the codebase is already well-architected and has been through prior
cleanup passes — `ObservationStore` unified the storage layer, the rail lockstep
is structural, the wasm shell is genuinely thin, closed enums are guarded by
round-trip tests. There is **no god-module and no poll regression.** The real
value is concentrated in (a) a handful of low-risk dedups/encapsulation fixes and
(b) the **test suite**, where the deterministic host layer is excellent but the
**E2E layer is thin and timing-fragile** and a few ship-paths are untested.

**Explicit non-moves** (all six agents agree; recorded so a later pass doesn't
"fix" them): do **not** abstract the closed `Agent`/`Kind`/`Outcome`/`Command`
enums into traits — they are deliberately compiler-guided and round-trip-guarded.
Do **not** rewrite `paint_card_line` (snapshot risk outweighs benefit). Do **not**
apply the budget-threading "fix" to `plan_layout` — verification shows it would
*invert* the documented "drop gaps before compressing content" rule (see T1·A8).

---

## Tier 1 — high leverage / low risk

### 1 · Collapse the twice-written status-precedence into one `RadarState::resolve()`   ★ high

The "status pipe wins over command" invariant is encoded **twice**, two different
ways: `tab_display` via `.or_else()` and `notify_views` via insert-then-overwrite.
If the rule ever flips you must find both and remember one is `or_else`, the other
is insert order. Two encodings of one invariant, guarded by two near-identical
tests.

```rust
// before — radar_state.rs:651-655
rollup::roll_up(panes, |id| self.status.get(id).or_else(|| self.command.get(id)))
// before — radar_state.rs:498-509
for (id, o) in self.command.observations() { m.insert(id, o); } // command first
for (id, o) in self.status.observations()  { m.insert(id, o); } // status overwrites

// after — one definition
fn resolve(&self, id: u32) -> Option<&TrackedObservation> {
    self.status.get(id).or_else(|| self.command.get(id))   // THE precedence, once
}
// tab_display:  rollup::roll_up(panes, |id| self.resolve(id))
// notify_views: union of both id sets, each mapped through self.resolve(id)
```

Blast radius: `radar_state.rs` only. Pinned by `same_pane_status_observation_wins_over_command`
and `notify_views_status_wins_over_command_for_same_pane` (both still pass,
unchanged — now pin one function instead of two parallel encodings).

### 2 · `#[cfg(test)]`-gate the leaky `RadarState` store accessors   ★ high

`RadarState`'s job is to *compose* the two stores so `roll_up` "never learns there
is more than one store" — but its surface exposes six raw accessors (`status`,
`command`, `status_mut`, `status_store`, `command_mut`, `command_store`,
radar_state.rs:511-539) handing callers the individual stores by name. Production
(`runtime.rs`) uses none of the mutating ones; they exist for test seeding. The
seam is held open only by test convenience, so a future re-seam of storage can't
happen behind `RadarState` without rewriting test call sites.

```rust
// after: keep resolved-fact readers; gate the raw store handles to tests
#[cfg(test)] pub(crate) fn status_mut(&mut self) -> &mut StatusStore { … }
#[cfg(test)] pub(crate) fn command_mut(&mut self) -> &mut CommandStore { … }
#[cfg(test)] pub(crate) fn status_store(&self) -> &StatusStore { … }
#[cfg(test)] pub(crate) fn command_store(&self) -> &CommandStore { … }
```

Blast radius: `radar_state.rs:511-539` + the test modules that call them (verify
every `*_mut`/`*_store` call site is already under `#[cfg(test)]` — grep says yes;
if any has a production caller, drop that accessor from the change). Behavior
unchanged. Combines with #1: `resolve` becomes the only place that knows there are
two stores.

### 3 · Extract the ~180-line snapshot block into `radar_state/snapshot.rs`   ★ high

Snapshot serialization is ~25% of `radar_state.rs` (the 4 snapshot structs +
`load_snapshot`/`snapshot_json`/`parse_snapshot`/`parse_v2`/`parse_legacy`,
lines 179-309 & 666-713) — a self-contained concern (v2 record + v1 migration +
live-pane merge) that only needs `(observations iterator, live_panes, tick)` in
and `String` / `Vec<(pane,obs)>` out. Extracting it slims the composition root
materially and gives the migration logic its own test home.

```rust
// after
mod snapshot;
snapshot::to_json(self.iter_observations(), self.live_panes.as_ref(), tick)
snapshot::load(raw) -> Option<(Vec<(u32, TrackedObservation)>, u64)>
```

Blast radius: `radar_state.rs` + the ~7 `snapshot_*` tests in `radar_state/tests.rs`
(stay as integration tests through `RadarState`, or move to the new module).
Pure move, behavior preserved.

### 4 · (TEST) E2E: click-to-switch-tab lands on the pointed row + mouse injection   ★ high

The single highest-value test gap. Click-to-switch is *the* central interaction
and the headline lockstep invariant ("click lands on the row you pointed at").
It is exhaustively host-tested (`mouse_click_on_tab_row_emits_switch_tab_effect`,
`click_round_trip_hits_drawn_target`) but **never exercised through a real Zellij
mouse event** — the E2E harness has no mouse injection at all. Add SGR mouse
escape injection (`\x1b[<0;col;row M/m`) to the PTY harness; click a known
sidebar row, assert focus moved to the intended tab via `dump-screen`. Pair with
a click on a multi-pane child row and (ties to #11) an overflow-folded row.

Files: `crates/plugin/tests/e2e/harness.rs`, `tests/e2e/main.rs`.

### 5 · (TEST) E2E harness robustness pass   ★ high

The harness is entirely sleep-and-poll timing-driven — fixed sleeps everywhere
(post-pipe 500ms, per-test 600-800ms, `notify.sh` settle 1500ms, recede tests a
fixed 3s wait for a 1s timer). On a loaded runner any can under-wait and flake;
there's no "retry until rendered" poll on the assertion side. Tests #1-#3 also use
weak `pty_text()` substring matches that also see the piped JSON in scrollback.
Three fixes that multiply the value of every existing E2E test for outside
contributors:

- Replace fixed post-pipe sleeps with a bounded `wait_until(|screen| sidebar_region(..).contains(needle), 5s)` poll.
- Add a Zellij version check in `ZellijSession::start` (0.44.x assumed, only a comment today) that fails/skips with a clear message.
- Upgrade tests #1-#3 from `pty_text()` substring to the `sidebar_region` vt100
  assertion the stronger tests (#4-#7) already use.

Files: `crates/plugin/tests/e2e/harness.rs`, `tests/e2e/main.rs`.

---

## Tier 2 — medium leverage

### 6 · Unify the three `setup_*` orchestrator bodies behind one `drive_edit` driver   ★ med

`setup_zellij`, `setup_codex_hooks`, `setup_codex_notify` each hand-roll the same
five-arm "skip / read / edit_or_report / match Outcome { dry-run print, prompt,
confirm_and_write }" shape (~330 lines triplicated), with subtle divergences
(`codex_hooks` has `Conflict => unreachable!()`; zellij threads layout
inject/uninstall into most arms). The *primitives* are factored
(`edit_or_report`, `confirm_and_write`) but the control flow is copy-pasted. A
single `drive_edit(EditStep)` driver with per-target hooks (prompt text, wasm-copy
pre_write, layout epilogue, `conflict_msg: Option<String>` — `None` makes the
`unreachable!` structural) collapses the three callers to building an `EditStep`.

Biggest dedup in the crate **but** real risk of reordering printed lines that bats
tests assert on → do test-first. Files: `setup/zellij.rs`, `setup/codex.rs`,
`setup/mod.rs`. Subsumes the `Outcome::Conflict` unreachable nit.

### 7 · Low-risk CLI dedups: snippet-print helper + reuse `write_atomic`   ★ med

- The "analyze layout → `tailored_snippet` → print with lead-in" sequence appears
  4× in `setup/zellij.rs` (`print_snippet_for`, layout-not-found, `InjectMode::Snippet`,
  declined-prompt); 3 are byte-identical. Fold into one `print_paste_snippet`.
- `do_inject` (zellij.rs:402-408) and `run_layout_uninstall` (zellij.rs:450-456)
  re-implement `setup/mod.rs:250 write_atomic` (backup `.zj-radar.bak` then
  `atomic_write`) inline — the backup-suffix convention lives in 3 places. Call
  the helper.

Both behavior-identical, pinned by existing inject/uninstall bats tests.

### 8 · Collapse `onboarding`/`needs_permission` onto `Line`/`from_lines`   ★ med

Both define a verbatim-duplicated `line()` closure pushing into a flat `String`,
then call `from_ansi_without_targets`, which re-splits by `\n` to fabricate an
all-`None` targets vec — a second way to build a `RenderedRail` that reintroduces
the ansi/line-count duality `from_lines` exists to eliminate. Share one
`untargeted_line(role, text, w) -> Line` feeding `from_lines`; delete
`from_ansi_without_targets`. Byte-identical output (snapshots unaffected); makes
"one way to build a rail" fully true. File: `render.rs:273-335`.

### 9 · (TEST, behavior decision) Enforce + test payload `v` version   ★ med — needs sign-off

`payload::parse` (payload.rs:134) checks byte length but **never reads `v`** —
`v:999` is accepted and parsed as v1. The status contract is "versioned" but
version is unenforced. Before producers proliferate, decide: reject `v != 1`
(recommended — matches the existing "drop malformed" defensiveness; a v1 plugin
shouldn't misrender a v2 payload it can't understand) and add
`rejects_unknown_payload_version` + vary `v` in the parse proptest. **This is the
one behavior-affecting change in the set** — it changes what `parse` accepts, so it
needs explicit approval.

### 10 · (TEST) E2E: onboarding/first-run grant + denial; new-tab rehydration   ★ med

- First-run is what every new public user hits and is never driven live (E2E
  always `pre_grant_permissions`). Add an ungranted path (assert needs-permission
  face renders, answer prompt, assert rail appears) + a denial path (graceful
  degradation).
- New-tab rehydration from the shared `/cache` snapshot (documented,
  previously-broken) has only host-level snapshot coverage — pipe a status, open a
  new tab, assert the new instance rehydrates the rail.

### 11 · (TEST) CLI ship-path + bash git-resolution coverage   ★ med

- `setup zellij --download` (mock release URL / assert fetch attempt), `setup
  --grant` exec path, interactive layout-inject `Prompt` branch — advertised
  install commands with only pure-helper coverage today.
- `notify.sh` git resolution: worktree (`--git-common-dir`), old-git fallback
  (`--show-toplevel`), corrupt/symlinked `.git` — the one notify.sh area with no
  direct bats test.
- E2E overflow folding on a real frame (many tabs, constrained height; assert idle
  strip + "+N more" + a click on a folded row resolves — ties to #4).

---

## Tier 3 — low leverage / cheap polish

### A8 · `plan_layout`: hoist the redundant `plan_overflow` out of the candidate loop + fix the comment   ★ low

The candidate loop (render/layout.rs:240-250) calls `plan_overflow(rows, body_budget)`
**identically every iteration** — the result never depends on `spacing`, so it's a
redundant recompute. The behavior is **correct** (content compresses to budget,
then luxury rows are added and shed via the acceptance check = "drop gaps first"),
but the inline comment claims it budgets content against post-luxury space, which
it does not. Hoist the call above the loop; fix the comment. **Do not** thread
`body_budget - luxury` into the content budget (an agent suggested this) — that
would invert the documented rule and move snapshots.

### 12 · Doc drift fixes (no code)   ★ low, cheap

- `CONTEXT.md:43-44,158-159` reads pre-collapse: it names `StatusStore`/`CommandStore`
  as independent peers and never mentions `ObservationStore`. Add a paragraph: both
  wrap one `ObservationStore`; the per-source split is only intake + the
  live-predicate; precedence stays in `RadarState`.
- `permission.rs:28` documents the seam as `PermissionPolicy::of` — that method
  doesn't exist; the collapse is `PluginRuntime::permission_policy` (runtime.rs:343).

### 13 · Relocate `config_pipe`'s JSON-scalar flattening to `config.rs`   ★ low

`runtime.rs:258-282` does 20 lines of `serde_json::Value → BTreeMap<String,String>`
coercion inline — the only place the runtime couples to `serde_json::Value`.
Move to `config::overrides_from_json`; the runtime method shrinks to
decode→apply→recompute. Pinned by `config_pipe_accepts_json_scalars`.

### 14 · core: dedup the two `wire_serde!` `Serialize` arms; unify `Kind`'s hand-rolled serde   ★ low, macro-internal

`wire_serde!` (wire.rs:27-60) has two arms whose `Serialize` impls are
byte-identical (only deserialize differs); factor a shared `wire_serialize!`.
`Kind` (kind.rs:87-97) hand-writes serde identical to the lenient arm under
different accessor names (`as_source`/`from_source`) — teach the macro the
accessor pair so `Kind` rides the same guarded generator instead of a hand copy.
Pinned by the per-enum round-trip + guard tests.

---

## Deliberately NOT doing (judgment calls / out of scope)

- **Inline `StatusStore`** (it's a shallow wrapper vs deep `CommandStore`): high
  churn, and keeping intake symmetry with `CommandStore` is defensible. Skip.
- **Trait-ify any closed enum** — explicitly wrong for this codebase.
- **Rewrite `paint_card_line`** — snapshot risk; instead a one-line comment
  pointing at the `Seg`-routing assumption it depends on (fold into another render
  commit if touching the file).
