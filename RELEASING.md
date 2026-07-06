# Releasing zj-radar

One tag produces three artifacts (wasm plugin, CLI binaries, crates.io crates),
but only the first two are automated — `release.yml` builds and publishes the
GitHub release on tag push; **crates.io publishing is manual and must happen
before the tag**. The order below matters; each step gates the next.

## Ordering, and why

1. **Sync versions.** `[workspace.package] version`, the exact core pin
   (`zj-radar-core = { …, version = "=X.Y.Z" }` in the root `Cargo.toml`), and
   `plugins/zj-radar-claude/.claude-plugin/plugin.json` must all agree.
   `release.yml` hard-fails a tag that doesn't match the manifest, but nothing
   checks the other two — do it by hand:

   ```sh
   grep -m1 '^version' Cargo.toml
   grep 'zj-radar-core' Cargo.toml
   grep '"version"' plugins/zj-radar-claude/.claude-plugin/plugin.json
   ```

2. **Green suite on the exact release commit:** `just ci`. Also confirm the
   MSRV claim still builds — `just ci` does *not* cover it; CI's `msrv` job
   must be green on this commit, or verify locally with
   `cargo +<rust-version> check --workspace --all-features --locked` (the
   version is `rust-version` in the root `Cargo.toml`; bump it there if the
   dependency floor rose).

3. **Push main.** Docs reference release URLs; they 404 until the tag exists,
   so push + publish + tag should happen in one sitting.

4. **Publish core, then the CLI** — the CLI's exact pin can't resolve until
   core is up:

   ```sh
   cargo publish --dry-run -p zj-radar-core
   cargo publish -p zj-radar-core
   cargo publish --dry-run -p zj-radar   # verifies against the JUST-published core
   cargo publish -p zj-radar
   ```

   Core's API is allowed to break between 0.1.x releases (it's internal); the
   exact pin is what protects previously published CLIs. Never loosen it to a
   caret/minor range.

5. **Tag and push the tag:**

   ```sh
   git tag -s vX.Y.Z -m "vX.Y.Z" && git push origin main vX.Y.Z
   ```

   `-s -m` is required — tags in this repo are GPG-signed (`tag.gpgsign=true`),
   so a bare `git tag vX.Y.Z` opens an editor (or fails in a script) asking for
   a tag message.

   `release.yml` builds the wasm (nix) + portable CLI tarballs, checksums
   everything, and creates the GitHub release. `e2e.yml` also runs on the tag
   but does **not** gate the release — check its result before announcing.

6. **Verify the release assets** from a clean machine (or at least a clean
   shell):

   ```sh
   # Sandbox the install so it doesn't overwrite your daily binary:
   export ZJ_RADAR_BIN_DIR="$(mktemp -d)"
   curl -fsSL https://github.com/marktoda/zj-radar/releases/latest/download/install.sh | sh
   "$ZJ_RADAR_BIN_DIR/zj-radar" --version
   "$ZJ_RADAR_BIN_DIR/zj-radar" setup --check
   ```

   (The installer prints a "not on your PATH" note for the sandbox dir —
   expected; invoke by full path.)

## One-time cleanup after v0.1.2 ships

Yank `zj-radar 0.1.0` (its `^0.1.0` core requirement resolves to the newer,
incompatible core and no longer compiles) and `zj-radar-core 0.1.0` (nothing
else may depend on it; it predates the checksum-verified installer):

`cargo yank` needs a crates.io token with the **yank** scope — a publish-only
token gets `403 Forbidden`. Scope one at <https://crates.io/settings/tokens>
(or yank from each crate's "Versions" page in the web UI):

```sh
cargo yank zj-radar@0.1.0
cargo yank zj-radar-core@0.1.0
```
