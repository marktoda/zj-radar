# AGENTS.md

Entry point for AI agents (and humans skimming) working in zj-radar. Keep this
thin — it points at the real docs rather than duplicating them.

zj-radar is a native [Zellij](https://zellij.dev) sidebar (Rust → `wasm32-wasip1`)
plus a host-side `zj-radar` CLI and a Claude Code producer plugin.

## Read first

- [`CONTEXT.md`](CONTEXT.md) — domain glossary and the load-bearing seams (rail,
  `RadarState`, roll-up, tab naming, the status contract). **Read before changing
  the core.**
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — project shape, full build/test/lint
  details, PR expectations.
- [`docs/design.md`](docs/design.md) — the canonical living design.

## Commands

```sh
cargo build                                    # host library + CLI checks
cargo build --release --target wasm32-wasip1 -p zj-radar-plugin   # the wasm plugin Zellij loads
just test        # L1–L4 deterministic host suite (unit, insta, proptest, vt100)
just test-bash   # bash hook tests (needs bats + shellcheck + jq)
just test-e2e    # L5 live: builds wasm, drives a real Zellij in a PTY (needs zellij)
just ci          # what every PR must pass: test + clippy + test-bash
just review      # accept intentional insta snapshot changes (cargo insta review)
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

The `wasm32-wasip1` target is requested by `rust-toolchain.toml` and
auto-installs on first build (see [`docs/TOOLCHAIN.md`](docs/TOOLCHAIN.md)). Most
of the core lives in `crates/core` and is host-testable — no wasm build needed
for typical work.

## Non-negotiable rules

- **Do not run `rustfmt` / `cargo fmt`.** The code is intentionally hand-formatted
  (e.g. aligned one-line multi-field structs). A `cargo fmt` diff will be rejected.
  Match the surrounding code.
- **Push-driven, never poll-driven.** The plugin must not issue blocking host
  queries (`get_pane_running_command`, etc.); status arrives via `zellij pipe`
  broadcasts. Polling melted the predecessor plugin — see
  [`docs/smart-tabs-postmortem.md`](docs/smart-tabs-postmortem.md).
- **Rail lockstep.** Emitted ANSI and the click-target map stay in exact 1:1 line
  correspondence (`CONTEXT.md` → *Lockstep*). Keep it structural, not
  discipline-held.
- `docs/rail-reference.md` is an executable spec — it is `include_str!`'d by
  `crates/plugin/src/reference_tests.rs`. Edit it through that test, not casually.

## Adding a producer or agent

The only external interface is the versioned `zj_radar.status.v1` pipe payload.
New instrumented agent → `enum Agent` variant in `crates/cli/src/agents/` +
`Agent::derive`; the `source_round_trips_through_kind` guard test tells you what
else to wire. Observed (uninstrumented) commands like `cargo test` are classified
in `crates/core/src/command.rs`, not in `agents/`.
