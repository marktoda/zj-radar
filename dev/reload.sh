#!/usr/bin/env bash
# Compatibility wrapper: build the debug wasm, but do not call Zellij reload.
set -euo pipefail

root="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"

"$root/dev/build.sh"

cat >&2 <<'EOF'
reload: built the debug wasm, but did not hot-reload the layout sidebar
reload: Zellij 0.44 opens a second pane when reloading a layout-created plugin
reload: restart the dev layout/session to pick up the rebuilt sidebar
reload: from a normal terminal, use ./dev/start.sh for a fresh dev session
EOF
