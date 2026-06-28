#!/usr/bin/env bash
# dev/reload.sh — rebuild the debug wasm and hot-reload the running sidebar
# IN PLACE (no Zellij restart), across every tab at once.
#
# Run from anywhere inside the dev Zellij session (it drives the current session
# via `zellij action`). Paths are derived at runtime, so nothing here hardcodes
# a home directory.
#
# Why `-c naming=force`: Zellij identifies a plugin by location + configuration.
# dev/dev.kdl loads radar with `naming "force"`, so the reload must pass the SAME
# config — otherwise Zellij treats it as a different plugin and opens a NEW pane
# instead of reloading. We also read the location back from the running layout so
# it matches byte-for-byte however it was written (relative or absolute).
set -euo pipefail

root="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
cd "$root"

cargo build --target wasm32-wasip1

loc="$(zellij action dump-layout | grep -om1 'file:[^"]*zj_radar\.wasm')"
if [[ -z "$loc" ]]; then
    echo "reload: no running zj_radar plugin found in this session" >&2
    exit 1
fi

zellij action start-or-reload-plugin -c naming=force "$loc"
echo "reloaded: $loc"
