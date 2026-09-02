# AGENTS.md

Entry point for AI agents (and humans skimming) working in zj-radar. It points
at the real docs rather than duplicating them.

zj-radar is a [Zellij](https://zellij.dev) sidebar (Rust → `wasm32-wasip1`)
plus a host-side `zj-radar` CLI and producer adapters for Claude Code (a
bundled Claude plugin), Codex (hooks installed by `zj-radar setup codex`), and
Opencode (a JS bridge installed by `zj-radar setup opencode`).

## Read first

- [`CONTEXT.md`](CONTEXT.md): domain glossary and the seams. Read before
  changing the core.
- [`CONTRIBUTING.md`](CONTRIBUTING.md): project shape, build/test/lint, welded
  files, PR rules, docs ownership.
- [`docs/design.md`](docs/design.md): architecture and mechanisms.
- [`docs/activity-model.md`](docs/activity-model.md): what each status and
  kind means and how it renders.

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

No wasm build is needed for typical work: the plugin's domain modules and
`crates/core` are host-testable (`zellij-tile` is wasm-only).

## Non-negotiable rules

- **Do not run `rustfmt` / `cargo fmt`.** The code is hand-formatted. Match
  the surrounding code.
- **Push-driven, never poll-driven.** No polling, no blocking host queries on
  any per-event or per-tick path. The one exception is the once-per-pane
  `Effect::ResolveCwd` naming bootstrap. See
  [`docs/smart-tabs-postmortem.md`](docs/smart-tabs-postmortem.md).
- **Rail lockstep.** Emitted ANSI and the click-target map stay in 1:1 line
  correspondence, structurally (`CONTEXT.md` → *Lockstep*).
- **Welded files.** Several docs and assets are test inputs; edit them
  through their test. The table is in
  [`CONTRIBUTING.md`](CONTRIBUTING.md#welded-files).

## Adding a producer or agent

The producer interface is the `zj_radar.status.v1` pipe payload. The plugin's
other external contracts are the `zj_radar.cmd.v1` and `zj_radar.config.v1`
pipes ([`docs/configuration.md`](docs/configuration.md)) and the presence-file
format (`crates/plugin/src/presence.rs`). A new instrumented agent is an
`enum Agent` variant in `crates/cli/src/agents/` plus `Agent::derive`; the
`source_round_trips_through_kind` guard test lists what else to wire.
Observed commands like `cargo test` are classified in
`crates/core/src/command.rs`, not in `agents/`.
