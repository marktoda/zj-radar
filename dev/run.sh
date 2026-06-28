#!/usr/bin/env bash
# Single dev entrypoint: build the debug wasm and open a fresh disposable dev session.
set -euo pipefail

root="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
cd "$root"

target="wasm32-wasip1"
artifact_rel="target/$target/debug/zj_radar.wasm"
artifact_abs="$root/$artifact_rel"
layout_src="$root/dev/dev.kdl"
layout_gen="$root/target/dev/dev.kdl"
session="${ZJ_RADAR_DEV_SESSION:-zj-radar-dev}"
next_session="${session}-next"
mode="${1:-start}"

usage() {
    cat <<'EOF'
usage: ./dev/run.sh [--build-only|--dry-run|--fresh-session]

Builds the debug wasm and opens the Zellij dev session.

From a normal terminal, this starts/restarts the disposable dev session. From
inside Zellij, it switches the current client to a fresh disposable dev session.

--fresh-session is kept as an explicit spelling of the inside-Zellij behavior.
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

session_exists() {
    zellij list-sessions --short --no-formatting 2>/dev/null | grep -Fxq "$1"
}

delete_session_if_exists() {
    local target_session="$1"

    if session_exists "$target_session"; then
        echo "dev: deleting existing Zellij session '$target_session'" >&2
        zellij delete-session "$target_session" --force
    fi
}

restart_from_terminal() {
    delete_session_if_exists "$session"

    echo "dev: starting Zellij session '$session'" >&2
    exec zellij --session "$session" --new-session-with-layout "$layout_gen"
}

current_zellij_session_name() {
    if [[ -z "${ZELLIJ_SESSION_NAME:-}" ]]; then
        cat >&2 <<'EOF'
dev: ZELLIJ is set but ZELLIJ_SESSION_NAME is missing
dev: cannot safely choose a disposable target session from inside Zellij
EOF
        exit 2
    fi

    printf '%s\n' "$ZELLIJ_SESSION_NAME"
}

switch_target_session() {
    local current_session="$1"

    case "$current_session" in
        "$session")
            printf '%s\n' "$next_session"
            ;;
        *)
            printf '%s\n' "$session"
            ;;
    esac
}

switch_from_zellij() {
    local current_session
    local target_session

    current_session="$(current_zellij_session_name)"
    target_session="$(switch_target_session "$current_session")"
    if [[ "$target_session" == "$current_session" ]]; then
        echo "dev: refusing to replace the current Zellij session '$target_session'" >&2
        exit 2
    fi

    delete_session_if_exists "$target_session"

    echo "dev: switching current Zellij client to fresh session '$target_session'" >&2
    zellij action switch-session --layout "$layout_gen" --cwd "$root" "$target_session"
}

open_or_switch_session() {
    if [[ -n "${ZELLIJ:-}" ]]; then
        switch_from_zellij
    else
        restart_from_terminal
    fi
}

open_fresh_session() {
    if [[ -n "${ZELLIJ:-}" ]]; then
        switch_from_zellij
    else
        restart_from_terminal
    fi
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
    --fresh-session)
        mode="fresh-session"
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
        if [[ -n "${ZELLIJ:-}" ]]; then
            current_session="$(current_zellij_session_name)"
            target_session="$(switch_target_session "$current_session")"
            echo "dev: dry run; would switch current Zellij client to fresh session '$target_session'"
        else
            echo "dev: dry run; would restart Zellij session '$session'"
        fi
        ;;
    start)
        build_wasm
        echo "built: $artifact_rel"
        generate_layout
        open_or_switch_session
        ;;
    fresh-session)
        build_wasm
        echo "built: $artifact_rel"
        generate_layout
        open_fresh_session
        ;;
esac
