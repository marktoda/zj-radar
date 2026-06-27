# zj-radar — sidebar install & distribution

**Status:** design / approved for plan
**Date:** 2026-06-27
**Author:** Mark Toda (with Claude)
**Depends on:** `docs/design.md §8` (build & packaging), `docs/distribution.md §4`
(the Zellij-plugin install surface). Supersedes the `@smartTabs@` vendoring noted
there.

## Goal

Make the **sidebar wasm** installable without a Rust toolchain, from a single,
reproducible artifact that both Mark's home-manager (now) and an eventual public
GitHub Release (later) consume — with no divergence between "what Mark runs" and
"what others download." This is the "Tier 2" install story; it deliberately
excludes the `zj-radar init` install *command*, which belongs to the deferred CLI
tier.

Non-goals (explicit): a new install command/binary, a throwaway `install.sh`/
`nix run` stopgap, any change to the separate dotfiles/home-manager repo, and
actually publishing a release (the repo is intentionally not pushed yet).

## Key decisions

1. **Single artifact.** One `crane`-built Nix `packages.default` is the source of
   truth. Home-manager consumes it locally now; the release workflow publishes
   that same artifact later. No second build recipe to drift.
2. **Scope = artifact + docs.** No new install command. The manual `cp` + layout
   snippet path is already documented in the top-level `README.md`; a released
   artifact removes the toolchain burden. Anything fancier waits for `zj-radar
   init` in the CLI tier (avoids building an installer twice).
3. **Tag-driven releases.** A `v*` tag triggers a build + GitHub Release with the
   wasm attached; version = the tag, asserted against `Cargo.toml`. An always-on
   CI gate (test + clippy + wasm build) is separate and runs on every push/PR.
4. **Nix build mechanism = `crane` + `fenix`.** `crane` is the de-facto standard
   for Rust-in-Nix, derives a vendored dep set from `Cargo.lock` (Nix builds are
   offline/sandboxed), caches a deps-only layer, and composes with the `fenix`
   toolchain the flake already uses for the `wasm32-wasip1` std.

## Components

### A. Flake outputs (`flake.nix`)
Extend the existing fenix/flake-utils flake:
- **New input:** `crane`, `inputs.nixpkgs.follows = "nixpkgs"`.
- **`packages.default` / `packages.zj-radar`** — the `crane`-built wasm, using the
  existing combined toolchain (host + `wasm32-wasip1` rust-std). Set
  `CARGO_BUILD_TARGET = "wasm32-wasip1"` and `doCheck = false` (wasm can't run on
  the host builder; host tests run via `checks`). Install the single artifact to
  `$out/lib/zj_radar.wasm` (a wasm is not an executable → not `bin/`).
- **`checks.default`** — `cargo test` + `cargo clippy -- -D warnings` on the
  **host** target (the pure logic). Makes `nix flake check` the same gate CI runs.
- **`devShells.default`** — unchanged.

### B. CI workflow (`.github/workflows/ci.yml`) — always-on
- Triggers: `push`, `pull_request`.
- Installs Nix (`cachix/install-nix-action`), caches the Nix store between runs,
  and runs **`nix flake check`** — which runs host `cargo test` + `clippy -D
  warnings` *and* `nix build` of the wasm package. A change that breaks the wasm
  cross-build (e.g. pulling a non-wasm dep into the plugin) fails CI.
- Single source of truth for "is it green," identical to local `nix flake check`.

### C. Release workflow (`.github/workflows/release.yml`) — dormant until tagged
- Trigger: `push` of a tag matching `v*`.
- **Version guard:** assert the tag (`vX.Y.Z`) matches `Cargo.toml`'s `version`;
  fail fast on mismatch.
- Build via `nix build .#zj-radar` (same artifact as local — no drift).
- Create a GitHub Release at the tag and attach `zj_radar.wasm`.
- Inert until the repo is pushed and a tag is created; nothing fires prematurely.
- Makes the README's `releases/latest/download/zj_radar.wasm` URL resolve on the
  first tag.

### D. Home-manager consumption (boundary — documented, not edited here)
The home-manager module lives in a separate dotfiles repo; this repo only exposes
the interface and documents the pattern:
- This flake's `packages.default` is the single artifact. The dotfiles flake adds
  this repo as an input and references
  `inputs.zj-radar.packages.${system}.default` for a store-path wasm now — or,
  post-release, `fetchurl`s the tagged release asset (mirroring the retired
  `@room@`/`@smartTabs@` vendoring).
- Add a short **"Installing via Nix / home-manager"** subsection to `README.md`
  with both snippets (flake-input build now; `fetchurl`-from-release later) and a
  note that the `@smartTabs@` substitution is fully retired.
- No dotfiles-repo change happens here; that's a follow-up against this stable
  interface.

## Verification / testing

This tier is build plumbing, so verification is "produces a loadable artifact,"
not unit tests:
- `nix build .#zj-radar` produces `zj_radar.wasm` of the expected size-profiled
  shape (~877 KB); `nix flake check` passes (test + clippy + wasm build).
- The release workflow's tag→version guard logic is validated locally; the
  publish step itself can only be fully exercised once the repo is pushed —
  documented as such, not claimed as verified.
- Sanity-load the `nix build` wasm in the `dev/dev.kdl` session to confirm it's a
  real, loadable plugin (parity with the cargo-built artifact).

## Out of scope (follow-ups)

- `zj-radar init` (wasm install to a stable path + layout snippet + permission
  pre-seed) — deferred CLI tier.
- Pushing/publishing the repo and cutting the first release — Mark's call, once
  polish level is satisfactory.
- Editing the dotfiles/home-manager repo to consume the artifact.
- Additional distribution channels (Homebrew, crates.io) — not needed for a wasm
  plugin.
