---
name: cleanup-pass
description: Use when rethinking and restructuring the zj-radar codebase for better architecture — deep-module redesign, collapsing duplication, removing indirection and leaky seams, aggressive simplification — without changing external behavior. This is ambitious behavior-preserving refactoring, not a nitpick/lint pass. NOT for bug-hunting (use /code-review) or feature work.
---

# Cleanup Pass

Rethink and restructure the zj-radar codebase for better architecture. The goal is
**depth**: more behavior behind smaller, cleaner interfaces, placed at the right
seams. Be ambitious — big collapses, big simplifications, re-seaming, deleting
whole layers of indirection are all in scope and encouraged. This is *not* a
nitpick pass; cosmetic tidying is the least valuable thing you can do here.

**The one hard constraint: external behavior is preserved** (unless the user
explicitly asks otherwise). This is behavior-preserving refactoring, not a
feature change and not a from-scratch rewrite. The repo's test suite pins external
behavior (insta snapshots, proptests, `reference_tests.rs`, the wire round-trip
guards) — that is your *license to be aggressive inside it*. Gut and rebuild
internals freely; the suite tells you the instant you changed what the code does.

**Philosophy: aggressive subtraction, skeptical addition.** Be fearless about
removing, merging, inlining, and re-seaming. Be suspicious of *adding* — do not
introduce a trait, registry, macro, dependency, generic parameter, or new crate
unless you can name the concrete coupling or duplication it removes. The big wins
here are almost always deletions and collapses, not new structure.

Use the deep-module vocabulary from the `codebase-design` skill — module,
interface, depth, seam, leverage, locality — and its tests (the **deletion test**:
if deleting a module makes complexity vanish it was a pass-through; one adapter is
a hypothetical seam, two is a real one). `CONTEXT.md` already names zj-radar's good
seams in this vocabulary; deepen them.

## How this runs

1. **Isolate.** Create/enter a worktree at `.claude/worktrees/cleanup-<area>`
   (use the `EnterWorktree` tool, or `Agent` with `isolation: "worktree"`). All
   work happens there; `main` stays clean. Big refactors are safe because they're
   isolated and behavior-pinned.
2. **Map for depth (read-only, no edits).** Scope: the path argument if given,
   else the whole workspace. Don't hunt nits — look for *shallow modules* (large
   interface, thin body), *leaky seams* (callers reaching past an interface into
   internals), duplicated concepts, needless indirection, and structures that
   fight the seams documented in `CONTEXT.md`. Judge internals freely; the suite
   pins the externals.
3. **Findings.** Present them in the terminal **and** write `findings.md` in the
   worktree root (durable, pasteable into a PR). **Rank by structural leverage** —
   biggest collapses / simplifications / re-seamings first; cosmetic nits last or
   omitted. Give each finding its own full, unbounded section (a heading, not a
   table row) — see *Findings format* below.
4. **One go-gate.** Recommend a sequence (big moves included as first-class, not
   deferred), then take **one** confirm. Escape hatches: "show N" (full plan +
   blast radius for a finding), "skip N", reorder, or "html" (write an HTML report
   instead). Don't grill pass-by-pass.
5. **Execute.** One conceptually-focused commit per pass on the branch — a pass
   may be *large* (a real re-seam), but it's one structural idea per commit; don't
   bundle unrelated ideas. Run `just ci` + clippy at the end; `just review` to
   accept any intentional snapshot changes.
6. **Summarize** (see end) and point at the branch/worktree so the user can PR.

## Findings format

One section per finding, unbounded — give each the room it needs. No side tables
or compact rows. Each section carries:

- A heading: `## N · <imperative move>` plus a leverage tag (★ high / med).
- One or two sentences naming the *structural* problem in deep-module terms
  (shallow module, leaky seam, duplicated concept, needless indirection).
- **A before → after code block** wherever possible, showing what actually
  changes — types, signatures, the shape of the interface. Snippets, not whole
  files; elide bodies with `…`. The point is to make the move legible at the
  type/seam level, not to paste the diff.
- Blast radius (files/callers touched, what gets deleted) and the tests that pin
  the behavior it touches.

Example of the shape (illustrative):

```
## 2 · Merge StatusStore + CommandStore into one ObservationStore   ★ high leverage

Two stores with identical shape; the "status pipe wins over command" precedence is
bolted on by hand in RadarState, so the rule leaks across both types (a duplicated
concept + a leaky seam).

Before:
    struct StatusStore  { obs: HashMap<PaneId, TrackedObservation> }
    struct CommandStore { obs: HashMap<PaneId, TrackedObservation> }

    impl RadarState {
        fn resolve(&self, p: PaneId) -> Option<&TrackedObservation> {
            self.status.get(p).or_else(|| self.command.get(p))  // precedence by hand
        }
    }

After:
    struct ObservationStore { obs: HashMap<PaneId, BySource> }   // precedence is data

    impl RadarState {
        fn resolve(&self, p: PaneId) -> Option<&TrackedObservation> {
            self.store.winner(p)            // one lookup; rule lives in BySource
        }
    }

Blast radius: radar_state.rs (resolve, name_facts); deletes command_store.rs and
its tests; ~120 lines net. Pinned by: radar_state unit tests + roll_up tests
(resolved output must be byte-identical).
```

Write the same shape into `findings.md`. (`html` at the gate renders the same
content as collapsible sections instead.)

## What zj-radar is

A native [Zellij](https://zellij.dev) sidebar (Rust → `wasm32-wasip1`) plus a
host-side `zj-radar` CLI and a Claude Code producer plugin. Three-member Cargo
workspace:

- `crates/core/` (`zj_radar_core`) — pure shared library: the versioned wire
  schema and status/command classification (`command`, `kind`, `observation`,
  `payload`, `status`, `wire`). **No `clap`, no `zellij-tile`.** Bottom of the
  dependency stack; must not learn about the CLI or the plugin.
- `crates/cli/` — the native `zj-radar` CLI (`notify`, `setup`, `run`).
  `build.rs` embeds the wasm via `include_bytes!`.
- `crates/plugin/` — the Zellij sidebar wasm plugin. A thin Zellij adapter
  (`lib.rs`/`main.rs`, wasm-only) over host-testable modules (runtime, stores,
  model, renderer). Most modules run host-side with no wasm build.
- `plugins/zj-radar-claude/` — the Claude Code producer plugin (hooks + bundled
  `notify.sh`).

**Read `CONTEXT.md` before restructuring the core.** It names the load-bearing
seams — the rail (`render_rail`), `RadarState`, `roll_up`, tab naming, the status
contract — in deep-module vocabulary. Align your moves with those seams; deepen
them rather than inventing parallel ones.

## Non-negotiable invariants (even big refactors respect these)

These are the externals the test suite pins, plus project rules. A refactor that
breaks one is wrong, however clean it looks:

- **Do not run `cargo fmt` / `rustfmt`.** The code is intentionally hand-formatted
  (e.g. aligned one-line multi-field structs). A `cargo fmt` diff reformats the
  whole tree and will be rejected. Match the surrounding code.
- **Push-driven, never poll-driven.** The plugin must not issue blocking host
  queries (`get_pane_running_command`, etc.); status arrives via `zellij pipe`
  broadcasts. Polling melted the predecessor plugin
  (`docs/smart-tabs-postmortem.md`). A "simplification" that turns a push path
  into a poll is a regression, not a cleanup.
- **Rail lockstep.** Emitted ANSI and the click-target map stay in exact 1:1 line
  correspondence (`CONTEXT.md` → *Lockstep*) — every `Line` carries its own
  `RailTarget`; `ansi`/`targets`/line-count all derive from one list. A render
  re-seam is high-value *and* the most dangerous place to break this; keep it
  structural, never reintroduce a separate height predictor.
- **`docs/rail-reference.md` is an executable spec** — `include_str!`'d by
  `crates/plugin/src/reference_tests.rs`. Edit it through that test, not casually.
- **The versioned `zj_radar.status.v1` pipe payload is the only external
  interface.** Don't change its shape, field names, or `v` as part of a refactor.
  Preserve the plugin's parse-time defensiveness (sanitize, truncate, drop
  oversized/out-of-order).

## Map for depth — what to look for

Lead with structure, not style:

- **Shallow modules.** Large interface, thin implementation, or callers reaching
  through it. Re-seam so a lot of behavior sits behind a small interface. Apply
  the deletion test to every wrapper, pass-through, and "manager/utils" module.
- **Duplicated concepts.** Two types/stores/functions that are the same shape with
  a rule bolted on (the precedence between `StatusStore`/`CommandStore` is the
  canonical example) — collapse to one, make the rule data.
- **Leaky seams.** Where `pub`/`pub(crate)` exposes internals callers shouldn't
  know (layout planning behind `render_rail`; worktree/basename resolution behind
  the tab-naming seam; store precedence behind `RadarState`, never in `roll_up`).
  Push the seam down; shrink the surface.
- **Needless indirection.** Wrapper enums/structs with one variant or one caller,
  newtypes that wrap nothing, generics with one instantiation, traits with one
  impl. Inline them.
- **Dependency-direction violations.** `crates/core` must depend on neither `cli`
  nor `plugin`; domain logic must not depend on the Zellij adapter; the wasm-only
  shell (`lib.rs`/`main.rs`) stays thin. Anything pointing the wrong way is a
  high-leverage fix.
- **Vocabulary drift.** The same concept under two names across modules — rename
  to match `CONTEXT.md`.

Cosmetic items (match-arm order, import grouping, one-line wording) are not worth a
pass. Drop them or batch them into a larger structural commit, never on their own.

## Design judgment while restructuring

- **Enums over traits for closed sets.** This codebase deliberately uses closed
  enums — `enum Agent`, `Kind`, severity/`Outcome` — guarded by round-trip tests
  (`source_round_trips_through_kind`). Adding an agent is a compiler-guided enum
  variant, not a trait impl. Don't "abstract" a closed set into a trait; if a
  refactor can turn open dispatch into a closed enum, that's a win.
- **Traits/`dyn` only for real seams.** Two real adapters, genuine runtime
  heterogeneity, or downstream-supplied behavior. One implementation is a
  hypothetical seam — collapse it.
- **Newtypes for domain distinctions** (`TabId`, `Kind`, `RailTarget`, `Outcome`),
  not stringly/bool/option soup. Extend that habit.
- **Idiomatic Rust.** Prefer `From`/`TryFrom`/`AsRef`/`IntoIterator` where they
  make the interface natural; implement standard traits (`Debug`, `Clone`, `Eq`,
  `Hash`, `Default`, `Serialize`) only where *semantically* correct; meaningful
  error types over stringly errors; avoid gratuitous clone/box/`dyn`/`Arc<Mutex>`;
  keep `unsafe` absent or tightly contained.

## Validation

Run inside the worktree:

- `just ci` — what every PR must pass (`just test` host suite + `just test-bash`).
  **Run this** after the passes.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — clean.
- `just review` (`cargo insta review`) — accept *intentional* snapshot changes;
  CI fails on unreviewed drift. For a behavior-preserving refactor, snapshots
  should usually be **unchanged** — a snapshot diff is a signal to double-check you
  didn't alter render output.
- `just test-e2e` — live Zellij in a PTY; slow/serial. Run only if a refactor could
  plausibly affect runtime wiring.
- Add/strengthen tests where a refactor needs a behavior pin it doesn't yet have.
- **Do not run `cargo fmt`.** If a check can't run, state exactly why.

## Work style

- Big architectural moves are the point. Bring them to the go-gate as first-class,
  fully-scoped proposals (with blast radius) — don't silently defer them as
  "follow-ups." Only genuinely out-of-scope or beyond-the-suite-risk items become
  follow-up notes.
- One structural idea per commit; a commit may be large, but keep it conceptually
  focused (matches one-logical-change-per-PR in `CONTRIBUTING.md`).
- Update docs (`README.md`, `docs/`, `CONTEXT.md`) when a seam or interface you
  reshaped changes.
- When finished, summarize:
  1. What was restructured (the big moves).
  2. What behavior was preserved, and which tests prove it.
  3. What modules/seams got deeper (interface shrunk, callers stopped reaching in,
     duplication/indirection deleted — with rough line counts).
  4. What checks passed (`just ci`, clippy, snapshots).
  5. What risks or follow-up refactors remain — and the branch/worktree to PR from.
