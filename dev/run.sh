#!/usr/bin/env bash
# Single dev entrypoint: build the debug wasm and restart the disposable dev session.
set -euo pipefail

root="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
cd "$root"

target="wasm32-wasip1"
artifact_rel="target/$target/debug/zj_radar.wasm"
artifact_abs="$root/$artifact_rel"
layout_src="$root/dev/dev.kdl"
layout_gen="$root/target/dev/dev.kdl"
session="${ZJ_RADAR_DEV_SESSION:-zj-radar-dev}"
mode="${1:-start}"

usage() {
    cat <<'EOF'
usage: ./dev/run.sh [--build-only|--dry-run]

Builds the debug wasm and starts a fresh disposable Zellij dev session.
Run with no arguments for the normal dev loop.
EOF
}

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

    if command -v nix >/dev/null 2>&1 && [[ "${ZJ_RADAR_DEV_NO_NIX:-}" != "1" ]]; then
        echo "dev: $target std is missing from the current Rust toolchain; building via nix develop" >&2
        nix develop -c cargo build --target "$target"
        return
    fi

    echo "dev: $target std is missing from the current Rust toolchain" >&2
    echo "dev: run in 'nix develop', or install it with 'rustup target add $target'" >&2
    cargo build --target "$target"
}

generate_layout() {
    mkdir -p "$(dirname "$layout_gen")"
    sed "s|file:$artifact_rel|file:$artifact_abs|g" "$layout_src" > "$layout_gen"
    echo "layout: $layout_gen"
}

ensure_outside_zellij() {
    if [[ -n "${ZELLIJ:-}" ]]; then
        cat >&2 <<EOF
dev: refusing to restart '$session' from inside Zellij
dev: run ./dev/run.sh from a normal terminal
EOF
        exit 2
    fi
}

restart_session() {
    if zellij list-sessions --short --no-formatting 2>/dev/null | grep -Fxq "$session"; then
        echo "dev: restarting existing Zellij session '$session'" >&2
        zellij delete-session "$session" --force
    fi

    echo "dev: starting Zellij session '$session'" >&2
    exec zellij --session "$session" --new-session-with-layout "$layout_gen"
}

case "$mode" in
    start)
        ;;
    --build-only)
        mode="build-only"
        ;;
    --dry-run)
        mode="dry-run"
        ;;
    -h|--help)
        usage
        exit 0
        ;;
    *)
        usage >&2
        exit 2
        ;;
esac

case "$mode" in
    build-only)
        build_wasm
        echo "built: $artifact_rel"
        generate_layout
        ;;
    dry-run)
        generate_layout
        echo "dev: dry run; not starting Zellij"
        ;;
    start)
        ensure_outside_zellij
        build_wasm
        echo "built: $artifact_rel"
        generate_layout
        restart_session
        ;;
esac
