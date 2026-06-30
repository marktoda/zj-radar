You are doing a structural cleanup pass over the zj-radar codebase.

Goal: improve clarity, modularity, encapsulation, maintainability, and idiomatic
Rust design **without changing external behavior** unless explicitly requested.
Treat this as cleanup and structural tightening — not a rewrite, framework
migration, or architecture-astronaut exercise. (This works well as a post-sprint
reset, but it is a general-purpose tool: run it any time the codebase has drifted.)

**Bias toward deletion, privacy, and simpler concrete code.** Do not add a trait,
registry, macro, dependency, generic parameter, or new crate unless you can name
the concrete coupling or duplication it removes.

## What zj-radar is

A native [Zellij](https://zellij.dev) sidebar (Rust → `wasm32-wasip1`) plus a
host-side `zj-radar` CLI and a Claude Code producer plugin. It is a three-member
Cargo workspace:

- `crates/core/` (`zj_radar_core`) — pure shared library: the versioned wire
  schema and status/command classification (`command`, `kind`, `observation`,
  `payload`, `status`, `wire`). **No `clap`, no `zellij-tile`.** This is the
  bottom of the dependency stack; it must not learn about the CLI or the plugin.
- `crates/cli/` — the native `zj-radar` CLI (`notify`, `setup`, `run`).
  `build.rs` embeds the wasm via `include_bytes!`.
- `crates/plugin/` — the Zellij sidebar wasm plugin. A thin Zellij adapter
  (`lib.rs`/`main.rs`, wasm-only) over host-testable modules (runtime, stores,
  model, renderer). Most modules run host-side with no wasm build.
- `plugins/zj-radar-claude/` — the Claude Code producer plugin (hooks + bundled
  `notify.sh`).

**Read [`CONTEXT.md`](../../CONTEXT.md) before changing the core.** It names the
load-bearing seams — the rail, `RadarState`, `roll_up`, tab naming, the status
contract — in the deep-module vocabulary from the `codebase-design` skill
(module, interface, depth, seam, leverage, locality). Use that vocabulary; align
proposals with the seams already documented there rather than inventing parallel
ones.

## Non-negotiable invariants (do not break these while cleaning up)

These are project rules, not preferences. Violating them gets the change rejected:

- **Do not run `cargo fmt` / `rustfmt`.** The code is intentionally hand-formatted
  (e.g. aligned one-line multi-field structs). A `cargo fmt` diff reformats the
  whole tree and will be rejected. Match the formatting of the surrounding code.
- **Push-driven, never poll-driven.** The plugin must not issue blocking host
  queries (`get_pane_running_command`, etc.); status arrives via `zellij pipe`
  broadcasts. Polling melted the predecessor plugin
  ([`docs/smart-tabs-postmortem.md`](../smart-tabs-postmortem.md)). Do not
  "simplify" any push path into a poll.
- **Rail lockstep.** Emitted ANSI and the click-target map stay in exact 1:1 line
  correspondence (`CONTEXT.md` → *Lockstep*). It is structural — every `Line`
  carries its own `RailTarget` and `ansi`/`targets`/line-count all derive from one
  list. Keep it structural; do not reintroduce a separate height predictor.
- **`docs/rail-reference.md` is an executable spec** — it is `include_str!`'d by
  `crates/plugin/src/reference_tests.rs`. Edit it through that test, not casually.
- **The only external interface is the versioned `zj_radar.status.v1` pipe
  payload.** Don't change its shape, field names, or `v` as part of cleanup. The
  plugin defends itself at parse time (sanitize, truncate, drop oversized/out-of-
  order) — preserve that defensiveness.

Preserve all existing behavior, public contracts, data formats, CLI behavior,
tests, snapshots, and integration expectations unless a change is clearly
identified and justified. Prefer small, reviewable refactor passes; do not bundle
unrelated changes.

## First, map the area you're touching

1. Identify the crates/modules in scope and write each one's purpose in one
   sentence. Cross-check against `CONTEXT.md` and `CONTRIBUTING.md` — if your
   one-sentence summary disagrees with the docs, that gap is itself a finding.
2. Identify the composition paths: where the runtime wires host concerns
   (permission flow, timers, rendered-rail caching, effect translation), how
   `RadarState` composes its stores (`StatusStore`, `CommandStore`) with pane
   topology and `TabNamer`, and how data crosses the documented seams
   (`render_rail`, `roll_up`, `Agent::derive`, `command_source`).
3. Identify public APIs, extension seams, core domain types, the wasm/host
   boundary, and the producer/agent intake boundary.
4. Identify residue: duplicated logic, oversized files, leaky modules, unclear
   names, dead code, stale abstractions, unnecessary indirection, inconsistent
   error handling, and "an agent left a mess here" code.

## Review and improve along these dimensions

### Module boundaries and encapsulation

- Each module should have a crisp purpose. The good seams are already named in
  `CONTEXT.md`; deepen them, don't fragment them.
- Public APIs should be minimal and sufficient for the module's role. Reduce
  `pub` / `pub(crate)` exposure where callers don't need it.
- Keep implementation behind the seam: layout planning lives behind `render_rail`;
  store precedence ("status pipe wins over command") lives in `RadarState`, never
  in `roll_up`; worktree/basename resolution lives behind the tab-naming seam.
  Don't let consumers reach around a seam into its internals.
- Respect dependency direction: `crates/core` depends on neither `cli` nor
  `plugin`; domain logic should not depend on the Zellij adapter. The wasm-only
  shell (`lib.rs`/`main.rs`) stays thin.
- Remove dead exports, duplicate helpers, and "misc/utils" dumping grounds.

### Composition and navigability

- It should be obvious where to assemble the system, add an implementation, or
  trace a workflow. Wiring (constructors, the runtime's effect translation, the
  `Agent` registry) should live in predictable places.
- Avoid hidden coupling and cross-module knowledge leaks — e.g. the namer must not
  learn about `StatusStore`/`TerminalPane`; only pre-resolved facts cross to it.
- Consolidate scattered composition only when it makes the system easier to
  understand, not just shorter.

### Rust API design

- Follow idiomatic Rust naming, conversion, ownership, and error conventions.
- Prefer `From`, `TryFrom`, `AsRef`, `IntoIterator`, etc. where they make APIs
  more natural.
- Public types should implement the standard traits that are *semantically*
  correct (`Debug`, `Clone`, `Eq`, `Hash`, `Default`, `Serialize`) — not
  reflexively.
- Use newtypes to encode domain distinctions (this codebase already does:
  `TabId`, `Kind`, `RailTarget`, `Outcome`) rather than passing ambiguous
  strings/bools/options around. Extend that habit; don't dilute it.
- Prefer meaningful error types and context over stringly errors.
- Avoid unnecessary cloning, boxing, dynamic dispatch, `Arc<Mutex<_>>`, or
  lifetime complexity unless justified. Keep `unsafe` absent or tightly contained.

### Extension seams: enums vs traits vs generics

- This codebase deliberately favors **closed enums over open traits** for its
  variant spaces — `enum Agent`, `Kind`, severity/`Outcome`. Adding an agent is a
  compiler-guided enum variant guarded by `source_round_trips_through_kind`, *not*
  a new trait impl. Respect that; don't "abstract" a closed set into a trait.
- Use enums when the set is closed, domain-known, exhaustively matchable, or
  state-machine-like.
- Use traits only when behavior is genuinely open-ended (multiple backends,
  downstream-supplied) — and `dyn Trait` only when runtime heterogeneity is real.
- Don't create a trait to abstract a single implementation unless it marks a real
  seam, simplifies tests, or removes meaningful coupling. Don't over-generalize —
  prefer concrete, readable code until repetition or extension pressure proves the
  seam.

### Simplification

- Delete unused code rather than reorganizing it.
- Collapse duplicated logic into the smallest natural abstraction.
- Split oversized modules only when the split creates clearer ownership of
  concepts.
- Inline needless wrappers and pass-through functions that hide rather than
  clarify.
- Prefer obvious code over clever code; rename concepts so vocabulary is
  consistent with `CONTEXT.md`.

## Correctness and validation

- Before changing behavior-adjacent code, identify the tests or checks that prove
  behavior is preserved. The suite is layered (L1–L5):
  - `just test` — deterministic host suite (unit, insta snapshots, proptest,
    vt100). No wasm build needed.
  - `just test-bash` — bash hook tests (needs `bats` + `shellcheck` + `jq`).
  - `just ci` — what every PR must pass: `test` + `test-bash`. **Run this.**
  - `just test-e2e` — L5 live: builds the wasm and drives a real Zellij in a PTY
    (needs `zellij`). Serial and slow; run only if your change could affect it.
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings` — must
    be clean.
- Add or improve tests only where they protect a refactor or clarify expected
  behavior. New render behavior → an `insta` snapshot or a `rail-reference.md`
  scenario; new wire/parse behavior → a unit/proptest.
- After an *intentional* render change, accept snapshots with `just review`
  (`cargo insta review`) — CI fails on unreviewed snapshot drift.
- **Do not run `cargo fmt`.** If any check cannot be run, state exactly why.

## Work style

- Start with a brief findings summary and a proposed sequence of small, focused
  passes. Then implement the safest high-value passes first.
- Keep each pass conceptually focused; one logical change per pass (and per PR —
  see `CONTRIBUTING.md`).
- Avoid broad rewrites, new dependencies, public API breaks, wire-format changes,
  migrations, and speculative abstractions unless clearly necessary.
- If you find a larger architectural issue, document it as a follow-up proposal
  rather than mixing it into cleanup.
- Update docs (`README.md`, `docs/`, `CONTEXT.md`) when an interface or behavior
  you touch changes.
- When finished, summarize:
  1. What changed.
  2. What behavior was preserved.
  3. What modules/APIs/seams became tighter.
  4. What tests/checks passed (`just ci`, clippy, and any others you ran).
  5. What risks or follow-up refactors remain.
