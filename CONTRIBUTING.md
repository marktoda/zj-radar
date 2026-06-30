# Contributing to zj-radar

Thanks for your interest! zj-radar is a native [Zellij](https://zellij.dev)
sidebar (Rust → `wasm32-wasip1`) plus a host-side CLI and a Claude Code producer
plugin. This guide covers how to build, test, and propose changes.

## Project shape

| Path | What it is |
|------|------------|
| `src/` | The Zellij sidebar plugin. A thin Zellij adapter (`lib.rs`/`main.rs`, wasm-only) over a pure, host-testable core (runtime, stores, model, renderer). |
| `src/cli/` | The native `zj-radar` CLI (`notify`, `setup`), behind the `cli` feature. |
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
just ci          # what every PR must pass locally: test + test-bash
```

- The host-testable core (`status`, `payload`, `render`, `rollup`,
  `radar_state`, `config`, `theme`, `session_files`, …) carries no
  `zellij-tile` dependency and runs on the native target — no wasm needed for
  most work.
- **Snapshot tests** use [`insta`](https://insta.rs). After an *intentional*
  render change, review and accept with `just review` (`cargo insta review`).
  CI fails on unreviewed snapshot drift.
- **E2E is serial by design** (`--test-threads=1`): each test spawns its own
  Zellij session and parallel sessions contend at startup. It runs nightly and
  on tags, not on every push.

## Lint & formatting

```sh
cargo clippy --all-targets --all-features -- -D warnings
```

> **This project does not use `rustfmt`.** The code is intentionally
> hand-formatted (e.g. aligned one-line multi-field structs). Please **do not**
> run `cargo fmt` / `cargo fmt --all` — it would reformat the whole codebase and
> the diff will be rejected. Match the formatting of the surrounding code.

`shellcheck` runs over `plugins/zj-radar-claude/scripts/notify.sh` in CI; run it
locally if you touch the script.

## Dev loop

```sh
./dev/run.sh            # build the debug wasm + open a disposable dev session
./dev/run.sh --help     # other modes (--build-only, --dry-run, --fresh-session)
```

Works from a normal terminal or from inside Zellij. See the README's *Develop*
section for the inside-Zellij caveats.

## Pull requests

1. Open an issue first for anything non-trivial, so we can agree on the approach.
2. Keep PRs focused; one logical change per PR.
3. `just ci` must pass, and `cargo clippy ... -D warnings` must be clean.
4. Add or update tests at the appropriate layer. New render behavior → a snapshot
   or `rail-reference.md` scenario; new wire/parse behavior → a unit/proptest.
5. Update docs (`README.md`, `docs/`, `CONTEXT.md`) when behavior or interfaces
   change.
6. Don't commit generated artifacts (`target/`) or editor/tool state.

## Adding a producer or an agent

The plugin's only real external interface is the versioned `zj_radar.status.v1`
pipe payload (see the README's *Writing your own producer*). To add a new
instrumented agent to the CLI, add an `enum Agent` variant in `src/cli/agents/`
and implement `Agent::derive`; the `source_round_trips_through_kind` guard test
will tell you what else to wire. Observed (uninstrumented) commands like
`cargo test` are classified in `src/command.rs`, not in `agents/`.

## License

By contributing, you agree that your contributions are licensed under the
project's [MIT License](LICENSE).
