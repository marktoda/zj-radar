# Task 3 Report: `config_is_managed` guard

## TDD: RED

Added test `detects_symlinked_config_as_managed` before implementation:
- `#[cfg(unix)]` + `std::os::unix::fs::symlink`
- tempdir with a real file and a symlink pointing at it
- asserts symlink → `config_is_managed` true, regular file → false, missing path → false

Before adding `config_is_managed`, the test failed to compile (function not found). Classic RED.

## GREEN

### `config_is_managed` (added near other path helpers, ~line 537)

```rust
pub(crate) fn config_is_managed(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}
```

Uses `symlink_metadata` so it doesn't follow the link — a broken symlink still returns `true`.

### Guard in `setup_zellij` (before `edit_zellij` call)

```rust
if !uninstall && config_is_managed(&config_path) {
    eprintln!(
        "zellij: config.kdl at {} is a symlink (managed by Nix / home-manager).\n\
         zj-radar will not overwrite a managed config — add the plugin alias via\n\
         your Nix config instead. See docs/install.md for the home-manager snippet.",
        config_path.display()
    );
    println_layout_snippet();
    return;
}
```

- Skips `edit_zellij` + `confirm_and_write` (no config clobber)
- Still calls `println_layout_snippet()` so the user gets actionable layout guidance
- Only fires on install (not uninstall); uninstall of a managed config is not our problem

## Files Changed

- `src/cli/setup.rs`: added `config_is_managed`, guard in `setup_zellij`, test `detects_symlinked_config_as_managed`

## Test Summary

`cargo test --all-features`: all unit tests pass (confirmed `cli::setup::tests::detects_symlinked_config_as_managed ... ok`). Dead-code warnings on not-yet-wired layout functions are pre-existing and expected per brief.

## Self-Review

- Implementation is minimal: one pure function (10 lines), one guard (10 lines), one test (12 lines).
- `config_is_managed` is `pub(crate)` for potential reuse by `--check` doctor in a later task.
- Guard position is correct: BEFORE `edit_zellij` (the alias write seam), AFTER wasm validation.
- `println_layout_snippet()` call in the guard gives users actionable next steps even when we refuse.
- Uninstall path is unguarded — removing a plugin alias from a managed config won't happen anyway (no alias written there), and `edit_zellij` on a managed uninstall is a no-op if alias was never written.

## Concerns

- None material. The `return` in the managed guard also skips the wasm copy (it's bundled in `confirm_and_write`). This is intentional: wasm copy without the alias registration would be misleading. If a user wants the wasm at that path, they can copy manually or use `--wasm`.
- The message points at `docs/install.md` which covers home-manager. The path is relative (not a URL), which is consistent with how Nix docs are referenced elsewhere in the project.

## Fix: comment + guard order

### What changed

Two fixes to `src/cli/setup.rs` in `setup_zellij`:

1. **Comment corrected**: The old comment on the managed-config guard said it would "keep going so wasm handling and the layout snippet still run" — directly contradicting the `return` on the next line. Replaced with an accurate comment:
   `// Refuse to clobber a managed (symlinked) config.kdl: print the layout snippet for guidance, then return early. A Nix/home-manager user gets the wasm + alias via their config, not from us.`

2. **Guard moved earlier**: The managed-config guard was placed after the wasm download/validation block. A `--download` user with a managed config would perform a network download before being refused. The guard was moved to run immediately after `config_path` is available (line ~937) — before the `let downloaded: PathBuf` / wasm resolution block — so a managed config short-circuits without any network call. The `!uninstall` gating is preserved.

### Test command and result

```
cargo test --all-features -p zj-radar cli::setup::tests
test result: ok. 28 passed; 0 failed; 0 ignored
```

Full suite (`cargo test --all-features`): all tests pass (28 unit + 3 integration + other suites, 5 e2e ignored as expected).

### Commit

`60b0281 fix(setup): correct managed-config guard comment; short-circuit before wasm download` — sits atop `f1dfac3`.
