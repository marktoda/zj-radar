# Releasing zj-radar

One tag produces three artifacts: the wasm plugin, the CLI binaries, and the
crates.io crates. `release.yml` builds and publishes the GitHub release on tag
push; **crates.io publishing is manual and must happen before the tag**. Each
step below gates the next.

## Steps

1. **Sync versions.** Three places must agree: `[workspace.package] version`,
   the exact core pin (`zj-radar-core = { …, version = "=X.Y.Z" }`) in the
   root `Cargo.toml`, and `plugins/zj-radar-claude/.claude-plugin/plugin.json`.
   `release.yml` rejects a tag that does not match the manifest; nothing
   checks the other two.

   ```sh
   grep -m1 '^version' Cargo.toml
   grep 'zj-radar-core' Cargo.toml
   grep '"version"' plugins/zj-radar-claude/.claude-plugin/plugin.json
   ```

2. **Green suite on the release commit:** `just ci`. Also confirm the MSRV
   still builds; `just ci` does not cover it. CI's `msrv` job must be green on
   this commit, or run
   `cargo +<rust-version> check --workspace --all-features --locked` locally.
   Glance at the latest nightlies too: the `hermetic` and `deny` jobs in
   `ci.yml`, and `funnel.yml` against `latest`. A red nightly means the
   release inherits a known problem.

3. **Push main.** Docs reference release URLs that 404 until the tag exists,
   so push, publish, and tag in one sitting.

4. **Publish core, then the CLI.** The CLI's exact pin cannot resolve until
   core is up:

   ```sh
   cargo publish --dry-run -p zj-radar-core
   cargo publish -p zj-radar-core
   cargo publish --dry-run -p zj-radar   # verifies against the just-published core
   cargo publish -p zj-radar
   ```

   Core's API may break between 0.x releases; the exact pin protects
   previously published CLIs. Never loosen it to a caret or minor range.

   Between publish and tag, crates.io serves a version whose release assets do
   not exist yet. A `cargo install zj-radar` from crates.io embeds no wasm, so
   `zj-radar run` and `setup zellij --download` fetch the version-pinned
   release URL and 404 until the assets land. Keep the window minutes long
   and do not announce until step 6 passes.

5. **Tag and push the tag:**

   ```sh
   git tag -s vX.Y.Z -m "vX.Y.Z" && git push origin main vX.Y.Z
   ```

   Tags are GPG-signed (`tag.gpgsign=true`), so `-s -m` is required; a bare
   `git tag` opens an editor or fails in a script.

   `release.yml` builds the wasm (nix) and portable CLI tarballs, checksums
   them, and creates the release. The builds run in parallel with the gates
   (the deterministic and bash suites, plus live E2E on both OSes), but the
   publish job waits on all of them. A red gate publishes nothing: fix, delete
   the tag, re-tag.

   To dry-run the pipeline, cut an RC. Any tag containing `-` (for example
   `v0.5.0-rc.1`) is marked a prerelease and never becomes `latest`.

6. **Verify the release assets.** The `verify-funnel` job in `release.yml`
   runs the README quick start verbatim in a pristine container against the
   tag's assets (installer, `--download`, pre-seeded grant, live rail, tab
   naming). **Do not announce until it is green.** `funnel.yml` re-runs the
   same script nightly against `latest`. Manual fallback from a clean shell:

   ```sh
   # Sandbox the install so it doesn't overwrite your daily binary:
   export ZJ_RADAR_BIN_DIR="$(mktemp -d)"
   curl -fsSL https://github.com/marktoda/zj-radar/releases/latest/download/install.sh | sh
   "$ZJ_RADAR_BIN_DIR/zj-radar" --version
   "$ZJ_RADAR_BIN_DIR/zj-radar" setup --check
   ```

   The installer prints a "not on your PATH" note for the sandbox dir; that is
   expected.

## Yanking

`cargo yank` needs a crates.io token with the **yank** scope; a publish-only
token gets `403 Forbidden`.
