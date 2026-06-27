# Sidebar Install & Distribution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Produce one reproducible, size-optimized `zj_radar.wasm` from a `crane`-built Nix flake package that home-manager consumes now and a tag-driven GitHub Release publishes later, gated by an always-on CI check.

**Architecture:** Extend the existing fenix-based `flake.nix` with `crane` to add `packages.default` (the wasm) and `checks` (host test + clippy). Two GitHub Actions workflows: `ci.yml` runs `nix flake check` on every push/PR; `release.yml` builds + publishes the wasm on a `v*` tag (dormant until the repo is pushed). A README subsection documents Nix/home-manager consumption.

**Tech Stack:** Nix flakes, `crane` (Rust-in-Nix), `fenix` (toolchain incl. `wasm32-wasip1` std), GitHub Actions, `cachix/install-nix-action`, `softprops/action-gh-release`.

## Global Constraints

- Build target for the plugin artifact: `wasm32-wasip1` (verbatim; not `wasm32-wasi`).
- The plugin artifact is a **binary** crate output named `zj_radar.wasm` (not a cdylib).
- The size-optimized `[profile.release]` already in `Cargo.toml` must be preserved; expect the wasm to be ~877 KB (range 700 KB–1 MB acceptable).
- The wasm build must stay dependency-lean: the `cli`/native deps do **not** exist yet and must not be introduced by this work.
- Host tests run with plain `cargo test` (135 tests today, all passing); they run on the host target, never wasm.
- This repo must NOT be pushed/published as part of this work, and no dotfiles/home-manager repo is edited here.
- The wasm installs to `$out/lib/zj_radar.wasm` (a wasm is not an ELF executable → not `$out/bin`).
- Work happens on branch `feat/install-distribution` (already created); commit per task, do not merge to `main` until the user asks.

---

## File Structure

- `flake.nix` (modify) — add `crane` input + `packages` + `checks` outputs; keep `devShells.default` working.
- `flake.lock` (modify) — regenerated to pin the new `crane` input.
- `.github/workflows/ci.yml` (create) — always-on `nix flake check` gate.
- `.github/workflows/release.yml` (create) — tag-triggered build + GitHub Release.
- `README.md` (modify) — add an "Installing via Nix / home-manager" subsection under Install.

---

### Task 1: Flake — `crane` wasm package + host checks

**Files:**
- Modify: `flake.nix`
- Modify: `flake.lock` (regenerated)

**Interfaces:**
- Produces: flake outputs `packages.default` / `packages.zj-radar` (a derivation whose `$out/lib/zj_radar.wasm` is the built plugin), and `checks.{zj-radar,clippy,test}`. The release workflow (Task 3) consumes `packages.zj-radar`; CI (Task 2) consumes `checks` via `nix flake check`.

- [ ] **Step 1: Replace `flake.nix` with the crane-based flake**

Write `flake.nix` to exactly this content:

```nix
{
  description = "zj-radar — Zellij sidebar plugin for AI-agent status";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, fenix, crane, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        fx = fenix.packages.${system};
        # Host toolchain + the wasm32-wasip1 std the Zellij plugin compiles against.
        toolchain = fx.combine [
          fx.stable.toolchain
          fx.targets.wasm32-wasip1.stable.rust-std
        ];
        craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;

        src = craneLib.cleanCargoSource ./.;
        commonArgs = {
          inherit src;
          strictDeps = true;
        };

        # ── wasm plugin artifact (cross-compiled to wasm32-wasip1) ──
        wasmArgs = commonArgs // {
          CARGO_BUILD_TARGET = "wasm32-wasip1";
          doCheck = false; # wasm can't execute on the host builder; see `checks` for tests
        };
        cargoArtifactsWasm = craneLib.buildDepsOnly wasmArgs;
        zj-radar = craneLib.buildPackage (wasmArgs // {
          cargoArtifacts = cargoArtifactsWasm;
          doInstallCargoArtifacts = false;
          # The wasm bin isn't an ELF executable; install it by hand to $out/lib.
          installPhaseCommand = ''
            mkdir -p $out/lib
            cp target/wasm32-wasip1/release/zj_radar.wasm $out/lib/zj_radar.wasm
          '';
        });

        # ── host-target deps shared by the test/clippy checks ──
        cargoArtifactsHost = craneLib.buildDepsOnly commonArgs;
      in {
        packages.default = zj-radar;
        packages.zj-radar = zj-radar;

        checks = {
          inherit zj-radar;
          clippy = craneLib.cargoClippy (commonArgs // {
            cargoArtifacts = cargoArtifactsHost;
            cargoClippyExtraArgs = "--all-targets -- -D warnings";
          });
          test = craneLib.cargoTest (commonArgs // {
            cargoArtifacts = cargoArtifactsHost;
          });
        };

        devShells.default = pkgs.mkShell {
          packages = [ toolchain pkgs.zellij ];
          shellHook = ''
            echo "zj-radar dev shell: $(rustc --version)"
            echo "build:  cargo build --release --target wasm32-wasip1"
            echo "test:   cargo test"
          '';
        };
      });
}
```

- [ ] **Step 2: Regenerate the lockfile for the new input**

Run: `nix flake lock`
Expected: `flake.lock` gains a `crane` node; no error.

- [ ] **Step 3: Build the wasm package and verify the artifact**

Run: `nix build .#zj-radar -L && ls -l result/lib/zj_radar.wasm`
Expected: build succeeds; `result/lib/zj_radar.wasm` exists, size roughly 700 KB–1 MB.

If the build fails at the install step (crane not honoring `installPhaseCommand`), the fallback is to replace `doInstallCargoArtifacts = false; installPhaseCommand = ...` with a `postInstall` hook:
```nix
          postInstall = ''
            mkdir -p $out/lib
            cp target/wasm32-wasip1/release/zj_radar.wasm $out/lib/zj_radar.wasm
          '';
```
Re-run the build and confirm the artifact appears.

- [ ] **Step 4: Run the full flake check (test + clippy + wasm build)**

Run: `nix flake check -L`
Expected: passes. The `test` check runs the 135 host tests; `clippy` passes with `-D warnings`; `zj-radar` builds the wasm.

- [ ] **Step 5: Confirm the dev shell still works**

Run: `nix develop -c cargo test 2>&1 | grep "test result"`
Expected: `test result: ok. 135 passed`.

- [ ] **Step 6: Commit**

```bash
git add flake.nix flake.lock
git commit -m "build(nix): crane-built packages.default (wasm) + flake checks (test/clippy)"
```

---

### Task 2: CI workflow (always-on gate)

**Files:**
- Create: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: the `checks` flake output from Task 1 (via `nix flake check`).
- Produces: a CI workflow that fails on any test/clippy/wasm-build regression.

- [ ] **Step 1: Create `.github/workflows/ci.yml`**

Write exactly:

```yaml
name: CI

on:
  push:
  pull_request:

jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: cachix/install-nix-action@v30
        with:
          extra_nix_config: |
            experimental-features = nix-command flakes
      # Caches the /nix/store between runs so flake check isn't a cold build every time.
      - uses: DeterminateSystems/magic-nix-cache-action@main
      - name: nix flake check
        run: nix flake check -L
```

- [ ] **Step 2: Lint the workflow file**

Run: `nix run nixpkgs#actionlint -- .github/workflows/ci.yml`
Expected: no output (exit 0) — the workflow is valid.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: run nix flake check (test + clippy + wasm build) on push/PR"
```

---

### Task 3: Release workflow (tag-triggered, dormant until pushed)

**Files:**
- Create: `.github/workflows/release.yml`

**Interfaces:**
- Consumes: `packages.zj-radar` from Task 1 (via `nix build .#zj-radar`).
- Produces: a workflow that, on a `v*` tag, verifies the tag matches `Cargo.toml`, builds the wasm, and attaches it to a GitHub Release.

- [ ] **Step 1: Verify the version-guard shell logic locally first**

Run:
```bash
manifest="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
echo "parsed manifest version: $manifest"
```
Expected: `parsed manifest version: 0.1.0`. (Confirms the grep/sed extracts the version the workflow will compare against.)

- [ ] **Step 2: Create `.github/workflows/release.yml`**

Write exactly:

```yaml
name: Release

on:
  push:
    tags:
      - 'v*'

permissions:
  contents: write # required to create the GitHub Release

jobs:
  release:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Verify tag matches Cargo.toml version
        run: |
          tag="${GITHUB_REF_NAME#v}"
          manifest="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
          if [ "$tag" != "$manifest" ]; then
            echo "::error::tag $GITHUB_REF_NAME (=$tag) does not match Cargo.toml version $manifest" >&2
            exit 1
          fi
          echo "tag $GITHUB_REF_NAME matches Cargo.toml version $manifest"

      - uses: cachix/install-nix-action@v30
        with:
          extra_nix_config: |
            experimental-features = nix-command flakes

      - name: Build wasm
        run: nix build .#zj-radar -L

      - name: Stage artifact
        run: cp result/lib/zj_radar.wasm zj_radar.wasm

      - name: Create GitHub Release
        uses: softprops/action-gh-release@v2
        with:
          files: zj_radar.wasm
          fail_on_unmatched_files: true
          generate_release_notes: true
```

- [ ] **Step 3: Lint the workflow file**

Run: `nix run nixpkgs#actionlint -- .github/workflows/release.yml`
Expected: no output (exit 0).

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci: tag-triggered release builds + publishes zj_radar.wasm (version-guarded)"
```

---

### Task 4: README — Nix / home-manager install subsection

**Files:**
- Modify: `README.md`

**Interfaces:**
- Consumes: the flake `packages.default` (Task 1) and the future release artifact (Task 3) — documents both consumption paths.

- [ ] **Step 1: Add the subsection after the existing "1. The sidebar plugin" block**

In `README.md`, locate the end of the `### 1. The sidebar plugin` subsection (just before `### 2. The Claude Code producer`). Insert this new subsection immediately before `### 2. The Claude Code producer`:

```markdown
#### Installing via Nix / home-manager

This flake exposes the wasm as `packages.default`, so a flake-based config can
consume the exact same artifact this repo builds. Add the repo as an input:

```nix
# flake.nix
inputs.zj-radar.url = "github:mark-toda/zj-radar";
```

Then reference the built wasm at a stable store path in your Zellij layout
derivation (build-from-source, works today):

```nix
plugin location="file:${inputs.zj-radar.packages.${system}.default}/lib/zj_radar.wasm"
```

Once tagged releases exist, you can instead pin a prebuilt artifact without a
Rust toolchain (mirrors the older `room`/`smart-tabs` vendoring this replaces):

```nix
zjRadarWasm = pkgs.fetchurl {
  url = "https://github.com/mark-toda/zj-radar/releases/download/v0.1.0/zj_radar.wasm";
  hash = "sha256-..."; # nix-prefetch-url the asset to fill this in
};
```

The old `@smartTabs@` substitution is fully retired — zj-radar owns the rail.
```

- [ ] **Step 2: Verify the rendered structure**

Run: `grep -n "Installing via Nix" README.md && grep -n "### 2. The Claude Code producer" README.md`
Expected: the Nix subsection line number is **less than** the "### 2." line number (i.e. it was inserted in the right place).

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs(readme): document Nix / home-manager install (flake input now, fetchurl later)"
```

---

## Self-Review

**Spec coverage:**
- §Key decision 1 (single crane artifact) → Task 1. ✓
- §Key decision 2 (artifact + docs, no install command) → no install-command task exists; Task 4 documents only. ✓
- §Key decision 3 (tag-driven release + always-on CI) → Task 3 (release) + Task 2 (CI). ✓
- §Key decision 4 (crane + fenix) → Task 1. ✓
- §Component A (flake outputs: package, checks, devShell) → Task 1 Steps 1–5. ✓
- §Component B (CI = nix flake check, cached) → Task 2. ✓
- §Component C (release: version guard, nix build, attach) → Task 3. ✓
- §Component D (home-manager doc, both snippets, @smartTabs@ retired note) → Task 4. ✓
- §Verification (nix build size, nix flake check, dev.kdl sanity-load) → Task 1 Steps 3–5; the manual dev.kdl load is a post-merge manual check (not automatable without a Zellij session — noted, not faked).

**Placeholder scan:** The only deferred content is the `fetchurl` `hash` in the README snippet, which is intentionally a fill-in-at-release-time value (documented as `nix-prefetch-url the asset to fill this in`), not a plan placeholder. No "TBD"/"add error handling"/"similar to Task N" anywhere.

**Type consistency:** Output name `packages.zj-radar` / `packages.default` and install path `$out/lib/zj_radar.wasm` are used identically in Task 1 (definition), Task 3 (`nix build .#zj-radar`, `result/lib/zj_radar.wasm`), and Task 4 (`.default}/lib/zj_radar.wasm`). Consistent. ✓
