# Task 2 Report: Move State Glue + Modules into crates/plugin

## Status: Complete ✓

Single commit: `b311a43` (the two working commits were squashed into one with the
required message + `Co-Authored-By` footer; tree byte-identical to the verified state).

All verification gates pass (re-run independently against the final commit):
- `cargo test --all-features` — all results 0 failed (281 plugin tests + root CLI tests)
- `cargo clippy --all-targets --all-features -- -D warnings` — clean
- `cargo build --target wasm32-wasip1 -p zj-radar-plugin` — success

## What Was Done

### Files Moved (git mv)

- `src/config.rs`, `src/notify_rules.rs`, `src/tab_namer.rs`, `src/status_store.rs`,
  `src/radar_state.rs`, `src/rollup.rs`, `src/render.rs`, `src/runtime.rs`,
  `src/theme.rs`, `src/session_files.rs`, `src/reference_tests.rs`
  → `crates/plugin/src/`

- `src/cmd.rs` → `crates/plugin/src/control.rs` (renamed)

- `src/snapshots/` → `crates/plugin/src/render/snapshots/` (relocated when tests were split)

- `proptest-regressions/` → `crates/plugin/proptest-regressions/`

- `tests/e2e/` → `crates/plugin/tests/e2e/`

### Snapshot Renames

All 9 `.snap` files renamed from `zj_radar__render__tests__*` to
`zj_radar_plugin__render__tests__*` to match the new crate name, and their
`source:` metadata updated from `src/render.rs` to
`crates/plugin/src/render/tests.rs`.

### Code Changes

1. **`crates/plugin/src/runtime.rs`**: `crate::cmd::Command` → `crate::control::Command`,
   `crate::cmd::parse` → `crate::control::parse`

2. **`crates/plugin/src/reference_tests.rs`**: `include_str!("../docs/rail-reference.md")`
   → `include_str!("../../../docs/rail-reference.md")`

3. **`crates/plugin/src/lib.rs`** (new): Houses `State`, `ZellijPlugin` impl, all module
   declarations, and the test module. Replaced 13 per-module `#[cfg_attr]` annotations
   with one crate-level `#![cfg_attr(…, allow(dead_code))]`.

4. **`crates/plugin/src/main.rs`**: Updated to reference `zj_radar_plugin::State` instead
   of `zj_radar::State`.

5. **`crates/plugin/Cargo.toml`**: Added `[lib]` section (`name = "zj_radar_plugin"`),
   switched from `zj-radar` root dep to `zj-radar-core`, added proper dev-dependencies,
   added `e2e` feature and `[[test]]` section, removed `[profile.release]`.

6. **`src/lib.rs`** (root, stripped): Now contains only the compile_error guard,
   `pub(crate) use zj_radar_core::{command, kind, payload, status};`, and
   `#[cfg(feature = "cli")] pub mod cli;`. Added `#[cfg_attr(not(test), allow(unused_imports))]`
   for `command`/`kind` (used only in test code).

7. **Root `Cargo.toml`**: Removed `zellij-tile` target dependency, removed `e2e`
   feature/test, removed `vt100`/`portable-pty`/`insta`/`proptest`/`assertables`
   dev-deps (now only in plugin crate).

### Test Module Splits

Four modules had their inline `#[cfg(test)] mod tests { ... }` blocks split into
separate files:
- `crates/plugin/src/rollup.rs` + `crates/plugin/src/rollup/tests.rs`
- `crates/plugin/src/runtime.rs` + `crates/plugin/src/runtime/tests.rs`
- `crates/plugin/src/radar_state.rs` + `crates/plugin/src/radar_state/tests.rs`
- `crates/plugin/src/render.rs` + `crates/plugin/src/render/tests.rs`

Each `tests.rs` starts with `use super::*;` plus the imports from the original block.

**Byte-identity verified:** for all four modules the extracted `tests.rs` is the
original `mod tests { … }` body with one level of indentation removed (pure
de-nest). Line counts match exactly (render 3863, radar_state 1137, runtime 649,
rollup 273) and re-indenting each file reproduces the original body byte-for-byte
(render's only "diff" is `\`-continuation lines inside a multi-line string literal,
correctly left at column 0 so string content is unchanged). No test logic or
assertions were altered.

## Re-export set settled on

`pub(crate) use zj_radar_core::{command, kind, observation, payload, status};` in
`crates/plugin/src/lib.rs` — `wire` omitted because nothing in the plugin crate
references `crate::wire` (clippy `-D warnings` would flag it as a dead import; it
passes clean). The 12-module set + `observation` matches what the moved sources
address as `crate::…`.

## Dependency Graph After

```
zj-radar (CLI lib) → zj-radar-core
zj-radar-plugin (wasm lib + bin) → zj-radar-core
```

Root crate no longer has any plugin modules or `zellij-tile` dependency.
