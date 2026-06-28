#!/usr/bin/env bash
# dev/reload.sh — rebuild the debug wasm used by the dev sidebar.
#
# Run from anywhere inside the dev Zellij session (it drives the current session
# via `zellij action`). Paths are derived at runtime, so nothing here hardcodes
# a home directory.
#
# Zellij 0.44's `start-or-reload-plugin` does not reload plugin panes that were
# created by a layout; it opens another pane instead. This script refuses that
# unsafe action when it detects the pinned layout sidebar.
set -euo pipefail

root="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
cd "$root"

target="wasm32-wasip1"

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

    if command -v nix >/dev/null 2>&1 && [[ "${ZJ_RADAR_RELOAD_NO_NIX:-}" != "1" ]]; then
        echo "reload: $target std is missing from the current Rust toolchain; building via nix develop" >&2
        nix develop -c cargo build --target "$target"
        return
    fi

    echo "reload: $target std is missing from the current Rust toolchain" >&2
    echo "reload: run in 'nix develop', or install it with 'rustup target add $target'" >&2
    cargo build --target "$target"
}

layout_sidebar_is_running() {
    command -v python3 >/dev/null 2>&1 || return 2

    zellij action list-panes --json --all --geometry --state --tab | python3 -c '
import json
import sys

try:
    panes = json.load(sys.stdin)
except Exception:
    sys.exit(2)

for pane in panes:
    url = pane.get("plugin_url") or ""
    if (
        pane.get("is_plugin")
        and "zj_radar.wasm" in url
        and not pane.get("is_floating")
        and pane.get("pane_x") == 0
    ):
        sys.exit(0)

sys.exit(1)
'
}

explain_layout_reload_limit() {
    cat >&2 <<'EOF'
reload: rebuilt the wasm, but did not hot-reload the layout sidebar
reload: Zellij 0.44 opens a second pane when reloading a layout-created plugin
reload: restart the dev layout/session to pick up the rebuilt debug wasm
EOF
}

build_wasm

sidebar_status=0
layout_sidebar_is_running || sidebar_status=$?
case "$sidebar_status" in
    0)
        explain_layout_reload_limit
        exit 2
        ;;
    2)
        echo "reload: rebuilt the wasm, but could not verify whether reload would duplicate the sidebar" >&2
        echo "reload: refusing start-or-reload-plugin; restart the dev layout/session instead" >&2
        exit 2
        ;;
esac

loc="$(zellij action dump-layout | grep -om1 'file:[^"]*zj_radar\.wasm')"
if [[ -z "$loc" ]]; then
    echo "reload: no running zj_radar plugin found in this session" >&2
    exit 1
fi

zellij action start-or-reload-plugin -c naming=force "$loc"
echo "reloaded: $loc"
