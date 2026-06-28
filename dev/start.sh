#!/usr/bin/env bash
# Build and start a fresh zj-radar dev Zellij session.
set -euo pipefail

root="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
cd "$root"

session="${ZJ_RADAR_DEV_SESSION:-zj-radar-dev}"

if [[ -n "${ZELLIJ:-}" ]]; then
    cat >&2 <<EOF
start: already inside a Zellij session
start: run this from a normal terminal after closing or detaching the old dev session
EOF
    exit 2
fi

if zellij list-sessions --short --no-formatting 2>/dev/null | grep -Fxq "$session"; then
    cat >&2 <<EOF
start: Zellij session '$session' already exists
start: attach:  zellij attach $session
start: restart: zellij delete-session $session --force && ./dev/start.sh
EOF
    exit 2
fi

"$root/dev/build.sh"

exec zellij --session "$session" --new-session-with-layout "$root/dev/dev.kdl"
