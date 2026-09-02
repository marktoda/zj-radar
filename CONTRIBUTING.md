# Contributing to zj-radar

zj-radar is a [Zellij](https://zellij.dev) sidebar (Rust → `wasm32-wasip1`)
plus a host-side CLI and producer adapters for Claude Code, Codex, and
Opencode. This guide covers building, testing, and proposing changes.

## Project shape

A three-crate Cargo workspace plus two non-Rust producers:

| Path | What it is |
|------|------------|
| `crates/core/` | `zj_radar_core`: the versioned wire payload, `Status`/`Kind`, observed-command classification, and the bounded pipe argv. No `clap`, no `zellij-tile`. Published to crates.io. |
| `crates/cli/` | The `zj-radar` binary (`notify`, `setup`, `run`). `build.rs` embeds the wasm with `include_bytes!`. Published to crates.io. |
| `crates/plugin/` | The wasm sidebar. `lib.rs`/`main.rs` are the only files that touch the Zellij host API and are wasm-only; everything else (runtime, stores, roll-up, renderer, naming, ledger, sessions) is host-testable. |
| `plugins/zj-radar-claude/` | The Claude Code producer plugin: `hooks.json` plus the bundled `notify.sh` fallback. |
| `crates/cli/src/setup/opencode_plugin.js` | The Opencode bridge, vendored into opencode's plugins dir by `setup opencode`. |
| `docs/` | User docs, the design doc, the executable rail spec. |

Two rules are load-bearing. Read [`CONTEXT.md`](CONTEXT.md) before changing
the core.

- **Push-driven, never poll-driven.** Status arrives over `zellij pipe`; the
  plugin makes no blocking host queries on any per-event or per-tick path.
  The one exception is a single `get_pane_cwd` per new pane, for tab naming.
  Polling melted the predecessor ([postmortem](docs/smart-tabs-postmortem.md));
  a PR that reintroduces it will not be accepted.
- **Rail lockstep.** The emitted ANSI and the click-target map stay in 1:1
  line correspondence, structurally (one `Vec<Line>` derives both). See
  `CONTEXT.md` → *Lockstep*.

## Prerequisites

- A stable Rust toolchain. `rust-toolchain.toml` requests the `wasm32-wasip1`
  target and `rustup` installs it on first build ([`docs/TOOLCHAIN.md`](docs/TOOLCHAIN.md)).
- **MSRV is Rust 1.95** (`rust-version` in the root `Cargo.toml`). CI's `msrv`
  job builds with exactly that toolchain.
- For the full suite: `just`, `bats`, `shellcheck`, `jq`, GNU `timeout`
  (`brew install coreutils` on macOS; two bash cases skip without it), and
  `zellij` ≥ 0.44.3 for the live E2E layer.
- Optional: Nix. `nix develop` pins all of the above; `nix flake check` runs
  what the `hermetic` CI job runs.

## Build

```sh
cargo build                                                        # host library + CLI
cargo build --release --target wasm32-wasip1 -p zj-radar-plugin    # the wasm Zellij loads
```

## Test

```sh
just test        # L1–L4: unit, insta snapshots, proptest, vt100 (deterministic, host-only)
just test-bash   # bats + shellcheck for notify.sh, install.sh, funnel.sh
just test-e2e    # L5: builds the wasm and drives a real Zellij in a PTY
just ci          # what every PR must pass: test + clippy + wasm build + test-bash
just review      # accept intentional insta snapshot changes
```

Run one test with `cargo test <name>`, scoped with `-p zj-radar-plugin` and so
on. Most work needs no wasm build: the plugin's modules have no `zellij-tile`
dependency on the native target.

- **Snapshots** use [`insta`](https://insta.rs). After an intentional render
  change, accept with `just review`. CI fails on unreviewed drift.
- **E2E is serial** (`--test-threads=1`); parallel Zellij sessions contend at
  startup. It runs nightly on both OSes, on PRs that touch plugin/core/producer
  paths (ubuntu only), as a release gate, and on `workflow_dispatch`.

### Welded files

Some docs and assets are test inputs. Edit them through their test.

| File | Pinned by |
|---|---|
| `docs/rail-reference.md` (`rail-input`/`rail-expect` blocks) | `crates/plugin/src/reference_tests.rs` |
| `docs/configuration.md` (both pipe names; every `cmd.v1` verb) | `config.rs`, `control.rs` |
| `plugins/zj-radar-claude/hooks/hooks.json` (timeouts ≥ send cap + 2) | `hooks_manifest_tests.rs` |
| `plugins/zj-radar-claude/scripts/notify.sh` (pipe name, deadlines) | `crates/plugin/src/lib.rs`, bats |
| `plugins/zj-radar-claude/.claude-plugin/plugin.json` (version) | `release.yml` |
| `examples/radar-sidebar.kdl` | `crates/cli/src/layout.rs` |
| Grant probe and Zellij version floor | text pins in `crates/plugin/src/lib.rs` |
| `Notify::agent` docs | `crates/cli/src/agents.rs` |
| `flake.nix` source filter | the inventory of every non-Rust file the hermetic build reads; a new `include_str!` outside crate sources needs an entry |

The README quick start is run verbatim by `scripts/funnel.sh` on every release.
Change the commands there and in the script together.

## Lint and formatting

```sh
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

> **Do not run `rustfmt` / `cargo fmt`.** The code is hand-formatted (aligned
> one-line structs, for example). A `cargo fmt` diff will be rejected. Match
> the surrounding code.

`cargo deny check` runs nightly in CI (advisories, licenses, sources; config
in `deny.toml`), not on PRs, because the advisory DB changes daily.

## Dev loop

```sh
just dev          # build wasm + CLI, launch a fresh sandboxed zj-radar-dev-<hhmmss> session
just dev-build    # build the dev artifacts without launching
```

`just dev` drives the real `zj-radar run` flow, grant onboarding included,
under `target/dev/data` (`ZJ_RADAR_DATA_DIR` + `ZJ_RADAR_WASM`). Every run is
a new uniquely named session: Zellij does not safely hot-reload layout-created
plugins, and attaching to a leftover would keep running the old wasm. Exited
dev sessions are swept; live ones are never killed. Run it from a plain
terminal; `zj-radar run` refuses to nest inside Zellij. In the Nix shell,
`nix develop -c just dev`.

## Pull requests

1. Open an issue first for anything non-trivial.
2. One logical change per PR.
3. `just ci` must pass.
4. Add tests at the right layer: render behavior → a snapshot or a
   `rail-reference.md` scenario; wire/parse behavior → a unit or proptest.
5. Update the docs when behavior or interfaces change (see the ownership map
   below).
6. Any new `include_str!`/`include_bytes!` of a non-Rust file needs an entry
   in `flake.nix`'s source filter, or the hermetic job cannot see it.
7. Don't commit `target/` or editor state.

## Docs

Each topic has one home; everything else links to it.

| Topic | Home |
|---|---|
| Pitch, quick start, doc index | `README.md` |
| Install, permissions, layouts, removal | `docs/install.md` |
| Reading the rail, mouse, badge, notifications | `docs/using.md` |
| Options and pipes | `docs/configuration.md` |
| Producers and the wire format | `docs/producers.md` |
| Symptom → fix | `docs/troubleshooting.md` |
| Status × class semantics | `docs/activity-model.md` |
| Architecture and mechanisms | `docs/design.md` |
| Glossary | `CONTEXT.md` |
| Exact rendered grid | `docs/rail-reference.md` |

Style: present tense, describe what ships. State a rule once and link to it.
Rationale lives in `design.md`; user docs give the rule. One idea per
sentence. Code identifiers only in contributor docs.

## Adding a producer or an agent

The producer interface is the `zj_radar.status.v1` payload
([`docs/producers.md`](docs/producers.md#writing-your-own-producer)). A new
instrumented agent is an `enum Agent` variant in `crates/cli/src/agents/`
plus `Agent::derive`; the `source_round_trips_through_kind` guard test lists
what else to wire. Observed commands like `cargo test` are classified in
`crates/core/src/command.rs`, not in `agents/`.

## License

By contributing, you agree that your contributions are licensed under the
project's [MIT License](LICENSE).
