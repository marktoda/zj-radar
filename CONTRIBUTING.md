# Contributing to zj-radar

Thanks for your interest! zj-radar is a native [Zellij](https://zellij.dev)
sidebar (Rust → `wasm32-wasip1`) plus a host-side CLI and a Claude Code producer
plugin. This guide covers how to build, test, and propose changes.

## Project shape

zj-radar is a three-member Cargo workspace:

| Path | What it is |
|------|------------|
| `crates/core/` | Pure shared library (`zj_radar_core`): the versioned wire schema and status/command classification (`command`, `kind`, `observation`, `payload`, `status`, `wire`). No `clap`, no `zellij-tile`. |
| `crates/cli/` | The native `zj-radar` CLI (`notify`, `setup`, `run`). `build.rs` embeds the wasm via `include_bytes!`. |
| `crates/plugin/` | The Zellij sidebar wasm plugin (`zj_radar_plugin`, Rust → `wasm32-wasip1`). A thin Zellij adapter (`lib.rs`/`main.rs`, wasm-only) over host-testable modules (runtime, stores, model, renderer). |
| `plugins/zj-radar-claude/` | The Claude Code producer plugin (hooks + bundled `notify.sh`). |
| `docs/` | Living design docs. Start with [`CONTEXT.md`](CONTEXT.md) (domain glossary) and [`docs/design.md`](docs/design.md). |

Two ideas are load-bearing — read [`CONTEXT.md`](CONTEXT.md) before changing the
core:

- **Push-driven, never poll-driven.** The plugin never issues blocking host
  queries (`get_pane_running_command`, etc.). Status arrives via `zellij pipe`
  broadcasts. Polling is what melted the predecessor plugin — see
  [`docs/smart-tabs-postmortem.md`](docs/smart-tabs-postmortem.md). A PR that
  reintroduces per-output host polling will not be accepted.
- **Rail lockstep.** The emitted ANSI and the click-target map stay in exact 1:1
  line correspondence, so a click lands on the row the user pointed at. Keep
  that invariant structural, not discipline-held (see `CONTEXT.md` → *Lockstep*).

## Prerequisites

- A stable Rust toolchain. `rust-toolchain.toml` requests the `wasm32-wasip1`
  target, which `rustup` auto-installs on first build. See
  [`docs/TOOLCHAIN.md`](docs/TOOLCHAIN.md).
- For the full suite: `just`, plus `bats`, `shellcheck`, and `jq` (bash hook
  tests) and `zellij` on `PATH` (live E2E).
- Optional: Nix. `nix develop` drops you into a shell with everything pinned;
  `nix flake check` runs the same clippy + tests + wasm build CI uses.

## Build

```sh
cargo build                                          # host library + CLI checks
cargo build --release --target wasm32-wasip1 -p zj-radar-plugin   # the wasm plugin Zellij loads
```

## Test

The suite is layered. `just` is the entry point:

```sh
just test        # L1–L4: deterministic host suite (unit, insta snapshots, proptest, vt100)
just test-bash   # bash hook tests (needs bats + shellcheck + jq)
just test-e2e    # L5: live — builds the wasm and drives a real Zellij in a PTY (needs zellij)
just ci          # what every PR must pass locally: test + clippy + test-bash
```

- The shared core (`status`, `payload`, `command`, `kind`, `observation`,
  `wire`) lives in `crates/core`; the sidebar's own modules (`render`, `rollup`,
  `radar_state`, `config`, `theme`, `session_files`, …) live in
  `crates/plugin/src`. Neither carries a `zellij-tile` dependency on the native
  target, so both run host-side — no wasm needed for most work.
- **Snapshot tests** use [`insta`](https://insta.rs). After an *intentional*
  render change, review and accept with `just review` (`cargo insta review`).
  CI fails on unreviewed snapshot drift.
- **E2E is serial by design** (`--test-threads=1`): each test spawns its own
  Zellij session and parallel sessions contend at startup. It runs nightly and
  on tags, not on every push.

## Lint & formatting

```sh
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

> **This project does not use `rustfmt`.** The code is intentionally
> hand-formatted (e.g. aligned one-line multi-field structs). Please **do not**
> run `cargo fmt` / `cargo fmt --all` — it would reformat the whole codebase and
> the diff will be rejected. Match the formatting of the surrounding code.

`shellcheck` runs over `plugins/zj-radar-claude/scripts/notify.sh` in CI; run it
locally if you touch the script.

## Dev loop

```sh
just dev          # build wasm + CLI, launch a FRESH sandboxed zj-radar-dev session
just dev-build    # build the dev artifacts without launching
```

The dev session is fully sandboxed (config, wasm, and grant live under
`target/dev/data`) and runs alongside your real sessions without touching
them. Start it from a plain terminal — `zj-radar run` refuses to nest inside
Zellij. See the README's *Development* section for details.

## Pull requests

1. Open an issue first for anything non-trivial, so we can agree on the approach.
2. Keep PRs focused; one logical change per PR.
3. `just ci` must pass — it runs the host suite, `cargo clippy ... -D warnings`,
   and the bash hook tests.
4. Add or update tests at the appropriate layer. New render behavior → a snapshot
   or `rail-reference.md` scenario; new wire/parse behavior → a unit/proptest.
5. Update docs (`README.md`, `docs/`, `CONTEXT.md`) when behavior or interfaces
   change.
6. Don't commit generated artifacts (`target/`) or editor/tool state.

## Adding a producer or an agent

The plugin's only real external interface is the versioned `zj_radar.status.v1`
pipe payload (see the README's *Writing your own producer*). To add a new
instrumented agent to the CLI, add an `enum Agent` variant in
`crates/cli/src/agents/` and implement `Agent::derive`; the
`source_round_trips_through_kind` guard test will tell you what else to wire.
Observed (uninstrumented) commands like `cargo test` are classified in
`crates/core/src/command.rs`, not in `agents/`.

## License

By contributing, you agree that your contributions are licensed under the
project's [MIT License](LICENSE).
