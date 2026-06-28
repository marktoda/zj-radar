# zj-radar Test Harness — Design

**Date:** 2026-06-27
**Status:** Approved (brainstorm complete)
**Goal:** A robust, thorough test harness that lets us ship zj-radar publicly with confidence
that it works across devices and scenarios, and that protects against regressions.

## Context

zj-radar is a native Zellij sidebar plugin (WASM binary) that shows live AI-agent status
per tab. Its architecture is already test-friendly: a thin WASM/Zellij glue shell
(`main.rs`, the imperative parts of `lib.rs`) wraps a pure, host-testable functional core
(`status`, `kind`, `payload`, `state`, `model`, `config`, `theme`, `naming`, `render`,
`command`). There are **182 passing host tests** today, concentrated on the pure modules.

The gaps that block a confident public release:

| Surface | Current coverage | Risk |
|---|---|---|
| Pure modules | Strong (182 tests) | Low |
| `lib.rs` WASM event handlers | Partial (pure helpers only) | Event→state wiring untested |
| CLI I/O (`git`/`zellij pipe` subprocess) | Derivation pure-tested; I/O untested | Producer breaks silently |
| `notify.sh` bash hook (136 lines) | **Zero** | Hook breaks on user machines |
| Cross-terminal / color-depth rendering | Substring asserts only | Per-terminal glitches |
| End-to-end (hook → pipe → render) | None | Integration regressions |
| CI | `nix flake check` only | No coverage gates, single OS |

All four failure classes matter (hook plumbing, visual glitches, silent logic regressions,
event-wiring bugs), so the harness covers all of them.

### Key constraints discovered

- **Dual target gotcha:** the flake sets `CARGO_BUILD_TARGET = "wasm32-wasip1"`, so a bare
  `cargo test` builds test binaries to WASM (can't run natively). Host tests must run with an
  explicit host target. This must be a documented, first-class command.
- **GUI terminals can't run headless in CI.** We cannot truly matrix on *emulator*
  (iTerm/kitty/wezterm). Instead we verify the *escape-sequence correctness* that makes the
  output terminal-portable (L4 in-process), plus run a real Zellij in a PTY (L5).

## Architecture: a five-layer pyramid

Wide deterministic base (fast, runs everywhere, the regression net); narrow live apex
(high-fidelity, catches integration drift).

```
        ▲  L5  Live E2E (real Zellij in a PTY)            ~5-10, nightly + pre-release
       ▲▲▲ L4  Cross-device matrix (color/glyph/OS)       ~dozens, every PR
      ▲▲▲▲▲ L3  Boundary/integration (CLI, bash, events)  ~tens, every PR
     ▲▲▲▲▲▲▲ L2  Property/invariant (proptest)            ~tens, every PR
    ▲▲▲▲▲▲▲▲▲ L1  Unit + golden snapshots (the 182 + new) ~hundreds, every PR
```

**Tooling (all dev-only; none ship in the wasm artifact):**
`insta` (snapshots + review UI), `proptest` (invariants), `vt100` (parse rendered grid),
`assert_cmd` + `assertables` (CLI process tests), `bats-core` + `bats-assert`/`bats-support`
(bash), `shellcheck` (bash lint), `cargo-insta` (snapshot review CLI).

---

## L1 — Unit & Golden Snapshots

**Keep all 182 existing tests.** Two changes:

1. **Convert hand-rolled goldens to `insta`.** Today `render.rs` asserts via
   `contains("\x1b[48;2;…")` substrings and a manual `tint_map`. Replace the
   golden/tint-map style tests with `insta::assert_snapshot!` over full rendered frames for
   canonical scenarios. Intentional rendering changes are reviewed via `cargo insta review`;
   the snapshot becomes the spec. Keep focused `contains`-style asserts only where they
   express a precise invariant better than a snapshot.

   Two snapshot flavors per canonical scenario:
   - **Raw** (escape codes intact) — catches color/escape regressions.
   - **Plain-text grid** (ANSI stripped or via vt100 → visible glyphs only) — human-readable,
     catches layout/alignment/truncation regressions; this is what a reviewer reads in a PR diff.

2. **Fill targeted unit gaps:**
   - Render overflow extremes (≈8-char width, ≈3-row height).
   - Multi-pane adaptive tree with all states co-present (active + pending + error + running +
     done + idle in one tab).
   - Payload defense-in-depth (malformed JSON, oversized input, nulls, mixed control chars).

## L2 — Property / Invariant Tests (`proptest`)

| Target | Invariant |
|---|---|
| `payload::sanitize` | Output never contains raw ESC/CSI/OSC bytes; never exceeds max length, for any input |
| `render` layout | For any rows × any (width ≥ min, height ≥ min): every line's display width ≤ width; line count ≤ height; **no panics** |
| `render` ↔ click | Round-trip: for any layout, `target_at_line(line_of(target)) == target` |
| `state`/`command` seq dedup | Applying payloads in any order converges to the same state as sorted-by-seq |
| `payload` parse ↔ to_wire | `parse(to_wire(x))` preserves semantic fields |

The panic-freedom property *proves* the "panic-free in production" claim from Cargo.toml
across the input space rather than asserting it in a comment. The click round-trip is the
highest-value new test: `render` and `target_at_line` are inverse functions computed by
separate code paths that must agree.

## L3 — Boundary / Integration Tests

The universal seam: a temp dir of **recording-shim scripts** (fake `zellij`/`git`/`jq`)
prepended to `PATH`. They record argv + stdin to a temp file so tests assert on what the
producer emitted. The same technique covers the Rust CLI and the bash script, and enables a
producer-parity test.

**(a) CLI I/O — `assert_cmd` + fakes.** Build the `zj-radar` binary; run with hook JSON on
stdin; fake `zellij` and `git` on `PATH`. Assert the exact `zj_radar.status.v1` payload
broadcast for given hook input. Test `setup` (`--dry-run`, `--force`, `--uninstall`,
idempotency) against a temp `config.toml`.

**(b) `notify.sh` — `bats-core`.** With fake `zellij`/`git`/`jq` on `PATH`:
- Each hook event (PreToolUse/PostToolUse/Stop/Notification) → correct status + activity.
- **bash↔Rust parity test:** feed identical inputs to `notify.sh` and `zj-radar notify` and
  assert identical payloads (the tool→activity logic is intentionally mirrored in two
  languages and will otherwise drift).
- Defense-in-depth: generic messages ("Claude needs attention") are skipped.
- Graceful degradation: missing `jq`/`git`, not-in-Zellij, non-numeric pane id → exits
  cleanly, never blocks the agent.
- `shellcheck` static gate.

**(c) WASM event wiring — synthetic event replay.** Extract `lib.rs` handler bodies
(`PaneUpdate`, `CommandChanged`, `Timer`, `Mouse`, focus, exit) into host-testable functions
taking plain typed inputs. Drive scripted event sequences and assert resulting state/render.
E.g. pane spawns → command runs → debounce timer → exits 0 → focus walks Idle→Running→Done→Idle.

## L4 — Cross-Device Matrix

**In-process axes** (parameterized Rust tests, no real terminal):
- **Color depth:** render each canonical scenario under truecolor / 256 / 16 / `NO_COLOR`.
  Invariants per mode: truecolor emits `48;2;r;g;b`; ANSI-16 fallback emits only `30–47`/
  `90–107`; `NO_COLOR` emits zero SGR color codes but identical text/layout.
- **Glyph set:** render every scenario with Nerd and plain glyphs; assert column alignment
  holds for both via vt100 cell positions (real test of width math).
- A few unicode-width stressors (CJK, emoji, combining marks) in names/messages — guard
  against alignment blowups (not a full axis).

**CI-runner axis:** the entire deterministic suite (L1–L4) runs on **both `macos-latest` and
`ubuntu-latest`**, catching the bash-3.2 / coreutils / path-separator class of bug by running
on the real OS.

## L5 — Live E2E (real Zellij, the apex)

A small suite (~5–10 scenarios), feature-gated (`e2e` feature or `#[ignore]` + runner script)
so it doesn't slow normal `cargo test`. A Rust harness (using `portable-pty` or `expectrl`):

1. Build the wasm plugin (`cargo build --target wasm32-wasip1`).
2. Spawn `zellij` in a PTY with a test layout pinning zj-radar in the sidebar.
3. Drive scenarios: `zellij pipe --plugin file:…/zj_radar.wasm --name zj_radar.status.v1 -- <json>`
   to inject status; `zellij action new-tab` / `write-chars` to manipulate panes; and fire the
   real `notify.sh` against the session for a true hook→pipe→render path.
4. Capture with `zellij action dump-screen -`; parse with `vt100`; assert the rendered grid.

**Canonical E2E scenarios:** cold start + permission grant; single agent Running→Done;
multi-agent across tabs (needs-you wins); click a tab → focus switches; tab auto-rename
applies; `notify.sh` end-to-end produces the expected sidebar.

**Where it runs:** nightly schedule + manual dispatch + release tags. Not blocking on regular
PRs. Deterministic layers are fast feedback; L5 is the "is it actually wired together"
confidence check before tagging a release.

## CI & Infrastructure

**Test organization:**
- L1/L2: `#[cfg(test)]` modules + a proptest-bearing module. Snapshots in `src/snapshots/`.
- L3 CLI/event tests → `tests/` integration dir. `notify.sh` bats →
  `plugins/zj-radar-claude/tests/*.bats`.
- L4 in-process axes → parameterized within render/theme test modules.
- L5 → `tests/e2e/` gated behind an `e2e` feature.

**Flake devshell additions:** `bats`, `bats-assert`, `bats-support`, `shellcheck`, `zellij`,
`cargo-insta`, and a host Rust target alongside the wasm one so `cargo test --target <host>`
works.

**CI restructure (`ci.yml`):**
1. `lint`: `cargo fmt --check`, `cargo clippy -D warnings`, `shellcheck`.
2. `test` (matrix `[macos-latest, ubuntu-latest]`): `cargo test --target <host>`
   (L1–L4) with `CI=1` (insta fails on drift, never writes) + bats suite. Keep
   `nix flake check` for the wasm build.
3. `e2e`: nightly schedule + manual dispatch + release tags; runs L5; not blocking on PRs.
4. (Optional) `coverage` with `cargo-llvm-cov` — reports, does not hard-gate.

**Golden commands** documented (README + `justfile` or shell aliases) so contributors and CI
agree: one command runs the full deterministic suite (correct target + env), one runs E2E.
`CI=1` is mandatory in CI so drifted snapshots fail instead of silently rewriting baselines.

## Success criteria

- All four failure classes have explicit coverage.
- Deterministic suite (L1–L4) runs green on macOS and Linux from a single documented command.
- `notify.sh` and the Rust CLI are proven to emit identical payloads (parity test).
- Render output verified at the grid level (vt100), under all color depths and both glyph sets.
- Live E2E proves the real hook→pipe→render path on a real Zellij.
- CI fails on snapshot drift, clippy warnings, shellcheck findings, and bash/Rust parity drift.

## Out of scope (YAGNI)

- GUI terminal emulator matrix (impossible headless; superseded by L4 escape-sequence checks).
- Zellij-version matrix as a CI axis (pin one; revisit if breakage observed).
- Hard coverage-percentage gate (invites gaming; report-only).
- Performance/benchmark suite (separate concern; the push-driven design already addresses the
  predecessor's perf problem).
