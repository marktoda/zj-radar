#!/usr/bin/env bash
# Build the debug wasm used by dev/dev.kdl.
set -euo pipefail

root="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
cd "$root"

target="wasm32-wasip1"
artifact="target/$target/debug/zj_radar.wasm"

target_has_std() {
    local libdir

    libdir="$(rustc --print target-libdir --target "$target" 2>/dev/null)" || return 1
    [[ -d "$libdir" ]] && compgen -G "$libdir/libstd-*.rlib" >/dev/null
}

build_wasm() {
    if target_has_std; then
        cargo build --target "$target"
        return
    fi

    if command -v nix >/dev/null 2>&1 && [[ "${ZJ_RADAR_BUILD_NO_NIX:-${ZJ_RADAR_RELOAD_NO_NIX:-}}" != "1" ]]; then
        echo "build: $target std is missing from the current Rust toolchain; building via nix develop" >&2
        nix develop -c cargo build --target "$target"
        return
    fi

    echo "build: $target std is missing from the current Rust toolchain" >&2
    echo "build: run in 'nix develop', or install it with 'rustup target add $target'" >&2
    cargo build --target "$target"
}

build_wasm
echo "built: $artifact"
