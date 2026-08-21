# AGENTS.md

Entry point for AI agents (and humans skimming) working in zj-radar. Keep this
thin — it points at the real docs rather than duplicating them.

zj-radar is a native [Zellij](https://zellij.dev) sidebar (Rust → `wasm32-wasip1`)
plus a host-side `zj-radar` CLI and producer adapters for Claude Code (a bundled
Claude plugin) and Codex (hooks installed by `zj-radar setup codex`) — both
first-class producers.

## Read first

- [`CONTEXT.md`](CONTEXT.md) — domain glossary and the load-bearing seams (rail,
  `RadarState`, roll-up, tab naming, the status contract). **Read before changing
  the core.**
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — project shape, full build/test/lint
  details, PR expectations.
- [`docs/design.md`](docs/design.md) — the canonical living design.
- [`docs/activity-model.md`](docs/activity-model.md) — the status/kind semantics:
  attention classes (Job/Service/Companion), interactive-command suppression,
  cadence rules.

## Commands

```sh
cargo build                                    # host library + CLI checks
cargo build --release --target wasm32-wasip1 -p zj-radar-plugin   # the wasm plugin Zellij loads
just test        # L1–L4 deterministic host suite (unit, insta, proptest, vt100)
just test-bash   # bash hook tests (needs bats + shellcheck + jq)
just test-e2e    # L5 live: builds wasm, drives a real Zellij in a PTY (needs zellij)
just ci          # what every PR must pass: test + clippy + wasm build + test-bash
just review      # accept intentional insta snapshot changes (cargo insta review)
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

The `wasm32-wasip1` target is requested by `rust-toolchain.toml` and
auto-installs on first build (see [`docs/TOOLCHAIN.md`](docs/TOOLCHAIN.md)). The
domain logic (`RadarState`, render, runtime, rollup, ledger, sessions) lives in
`crates/plugin`, with the shared wire/classification layer in `crates/core` —
both are host-testable (`zellij-tile` is `cfg(target_arch = "wasm32")`-scoped),
so no wasm build is needed for typical work.

## Non-negotiable rules

- **Do not run `rustfmt` / `cargo fmt`.** The code is intentionally hand-formatted
  (e.g. aligned one-line multi-field structs). A `cargo fmt` diff will be rejected.
  Match the surrounding code.
- **Push-driven, never poll-driven.** No polling, and no per-event or per-tick
  blocking host queries (`get_pane_running_command`, etc.); status arrives via
  `zellij pipe` broadcasts. The single exception is the once-per-pane
  `Effect::ResolveCwd` naming bootstrap (one blocking `get_pane_cwd` per
  freshly-opened pane — pane-creation rate, never re-polled). Polling melted
  the predecessor plugin — see
  [`docs/smart-tabs-postmortem.md`](docs/smart-tabs-postmortem.md).
- **Rail lockstep.** Emitted ANSI and the click-target map stay in exact 1:1 line
  correspondence (`CONTEXT.md` → *Lockstep*). Keep it structural, not
  discipline-held.
- Some files are welded to tests — edit them through the test, not casually.
  The canonical inventory is `flake.nix`'s source filter: every non-Rust file
  listed there is `include_str!`'d by a guard test (rail-reference spec,
  hooks.json timeouts, notify.sh's pipe name/deadlines, configuration.md's
  pipe names and verbs, the example layout, plugin.json's version), and
  cross-crate constants get text-pin tests too (grant probe and Zellij version
  floor in `crates/plugin/src/lib.rs`, `Notify::agent` docs in
  `crates/cli/src/agents.rs`). Rule:
  a new `include_str!` outside crate sources needs a flake.nix filter entry,
  or the hermetic CI job can't see the file.

## Adding a producer or agent

The producer interface is the versioned `zj_radar.status.v1` pipe payload. (The
plugin's other external contracts: the `zj_radar.cmd.v1` keybind-verb pipe and
`zj_radar.config.v1` live-override pipe — both in `docs/configuration.md` — and
the cross-process presence-file format, `crates/plugin/src/presence.rs`.)
New instrumented agent → `enum Agent` variant in `crates/cli/src/agents/` +
`Agent::derive`; the `source_round_trips_through_kind` guard test tells you what
else to wire. Observed (uninstrumented) commands like `cargo test` are classified
in `crates/core/src/command.rs`, not in `agents/`.
