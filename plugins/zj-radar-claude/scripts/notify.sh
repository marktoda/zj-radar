#!/usr/bin/env bash
# zj-radar Claude Code plugin notifier.
#
# Registered by the bundled hooks/hooks.json; called as `notify.sh <status>`
# where <status> is running | pending | done (the hook event determines which).
# Reads the Claude hook JSON on stdin for cwd + last message, then broadcasts a
# zj_radar.status.v1 message to the zj-radar Zellij sidebar.
#
# Design contract (matches the sidebar plugin's pipe schema):
#   - BROADCAST by name (never --plugin): reaches every sidebar instance and
#     never force-launches a plugin if the sidebar isn't loaded.
#   - Backgrounded: a slow/absent pipe must never block Claude's hook.
#   - No-op outside Zellij, or on a non-terminal pane id.
#
# Dependency: jq (used to parse the hook payload + build JSON). The productized
# `zj-radar notify` binary will remove this dependency.
set -euo pipefail

status="${1:-running}"

# Prefer the native CLI when present (drops the jq/bash dependency). It applies
# the same Zellij gate, pending backstop, and payload schema. Falls back to the
# bash implementation below when the binary isn't installed.
if command -v zj-radar >/dev/null 2>&1; then
    exec zj-radar notify claude --status "$status"
fi

[[ -n "${ZELLIJ:-}" && -n "${ZELLIJ_PANE_ID:-}" ]] || exit 0
pane_num="${ZELLIJ_PANE_ID#terminal_}"
[[ "$pane_num" =~ ^[0-9]+$ ]] || exit 0

input="$(cat 2>/dev/null || true)"
cwd="$(jq -r '.cwd // empty' <<<"$input" 2>/dev/null || true)"
[[ -n "$cwd" ]] || cwd="$PWD"
msg="$(jq -r '.message // .last_assistant_message // empty' <<<"$input" 2>/dev/null || true)"
[[ "$msg" == "Claude needs attention" ]] && msg=""

# Defense-in-depth: if a Claude version fires Notification without a matcher
# and produces a generic idle phrase (or no message), skip broadcasting pending
# — it isn't a real "needs you" event.
if [[ "$status" == "pending" ]]; then
    case "$msg" in
        ""|"Claude needs attention"|"Claude Code needs your attention")
            exit 0
            ;;
    esac
fi

repo="$(basename "$(git -C "$cwd" rev-parse --show-toplevel 2>/dev/null || printf '%s' "$cwd")")"
branch="$(git -C "$cwd" branch --show-current 2>/dev/null || true)"

# done clears itself when you focus the tab
on_focus=""
[[ "$status" == "done" ]] && on_focus="idle"

payload="$(jq -nc \
    --argjson id "$pane_num" \
    --arg status "$status" \
    --arg repo "$repo" \
    --arg branch "$branch" \
    --arg msg "$msg" \
    --arg on_focus "$on_focus" \
    '{v: 1, source: "claude", pane: {type: "terminal", id: $id},
      status: $status, repo: $repo, branch: $branch, msg: $msg}
     + (if $on_focus == "" then {} else {on_focus: $on_focus} end)')"

if [[ "${ZJ_RADAR_DEBUG:-}" == "1" ]]; then
    printf 'zj-radar payload: %s\n' "$payload" >&2
    exit 0
fi

( zellij pipe --name zj_radar.status.v1 -- "$payload" >/dev/null 2>&1 || true ) &
