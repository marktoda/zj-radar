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

# Whole-word containment (mirrors zj_radar_core::command::contains_word): is $2
# present in $1 bounded by non-[a-z0-9] chars or string edges? $1 is assumed
# lowercased; $2 is a literal (a phrase like "git push" works — its space is a
# boundary char). Used so "latest"/"uninstall"/"rebuild" don't trip the
# test/install/build verbs. NOTE: $2 is interpolated raw into an ERE, so it must
# stay regex-metachar-free (no . * + ? ( [ etc.) to match the Rust `match_indices`
# version literally — every current verb is a plain word, keep it that way.
contains_word() {
    local re="(^|[^a-z0-9])$2([^a-z0-9]|$)"
    [[ "$1" =~ $re ]]
}

# For running events (PreToolUse/PostToolUse), derive a live activity string
# from the tool being used — same rules as tool_activity() in notify.rs.
if [[ "$status" == "running" ]]; then
    hook_event="$(jq -r '.hook_event_name // empty' <<<"$input" 2>/dev/null || true)"
    if [[ "$hook_event" == "PreToolUse" || "$hook_event" == "PostToolUse" ]]; then
        tool_name="$(jq -r '.tool_name // empty' <<<"$input" 2>/dev/null || true)"
        tool_activity=""
        case "$tool_name" in
            Edit|Write|MultiEdit)
                fp="$(jq -r '.tool_input.file_path // empty' <<<"$input" 2>/dev/null || true)"
                [[ -n "$fp" ]] && tool_activity="editing ${fp##*/}"
                ;;
            NotebookEdit)
                fp="$(jq -r '.tool_input.notebook_path // empty' <<<"$input" 2>/dev/null || true)"
                [[ -n "$fp" ]] && tool_activity="editing ${fp##*/}"
                ;;
            Read)
                fp="$(jq -r '.tool_input.file_path // empty' <<<"$input" 2>/dev/null || true)"
                [[ -n "$fp" ]] && tool_activity="reading ${fp##*/}"
                ;;
            Grep|Glob)
                tool_activity="searching"
                ;;
            WebFetch|WebSearch)
                tool_activity="searching web"
                ;;
            Task)
                tool_activity="delegating"
                ;;
            TodoWrite)
                tool_activity="planning"
                ;;
            Bash)
                cmd="$(jq -r '.tool_input.command // empty' <<<"$input" 2>/dev/null || true)"
                # POSIX lowercase (works on macOS' stock Bash 3.2; ${cmd,,} is Bash 4+).
                cmd_lower="$(printf '%s' "$cmd" | tr '[:upper:]' '[:lower:]')"
                # Non-empty after stripping ALL whitespace (mirrors Rust .trim()).
                if [[ -n "$(printf '%s' "$cmd" | tr -d '[:space:]')" ]]; then
                    if contains_word "$cmd_lower" "git push"; then
                        tool_activity="pushing"
                    elif contains_word "$cmd_lower" "git commit"; then
                        tool_activity="committing"
                    elif contains_word "$cmd_lower" "git pull" || contains_word "$cmd_lower" "git fetch"; then
                        tool_activity="syncing"
                    elif contains_word "$cmd_lower" "test"; then
                        tool_activity="running tests"
                    elif contains_word "$cmd_lower" "build" || contains_word "$cmd_lower" "compile"; then
                        tool_activity="building"
                    elif contains_word "$cmd_lower" "install"; then
                        tool_activity="installing"
                    else
                        # first token, basename only
                        read -r first_token _ <<<"$cmd"
                        first_base="${first_token##*/}"
                        [[ -n "$first_base" ]] && tool_activity="running $first_base"
                    fi
                fi
                ;;
        esac
        [[ -n "$tool_activity" ]] && msg="$tool_activity"
    fi
    # A running broadcast with no derived activity would render as a blank
    # active row — give it a neutral baseline (parity with derive_claude).
    [[ -z "$msg" ]] && msg="working"
fi

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

# idle means "no activity" — never carry a message (drops any stale message the
# payload rides in on, e.g. a SessionStart session_title), so the rail row
# recedes cleanly on /clear. Mirrors derive_claude's idle branch.
[[ "$status" == "idle" ]] && msg=""

# Resolve the repo name from the COMMON git dir so worktrees report the main
# repo (e.g. "pinky"), not the worktree directory (e.g. "reply-register", which
# is what --show-toplevel returns inside a worktree). Fall back to --show-toplevel
# for git < 2.31 (no --path-format), then to the cwd basename.
common="$(git -C "$cwd" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)"
common="${common%/}"
case "$common" in
    */.git) repo="$(basename "$(dirname "$common")")" ;;   # .../pinky/.git → pinky
    *.git)  repo="$(basename "${common%.git}")" ;;          # bare repo acme.git → acme
    ?*)     repo="$(basename "$common")" ;;                  # unusual: use basename
    *)      repo="$(basename "$(git -C "$cwd" rev-parse --show-toplevel 2>/dev/null || printf '%s' "$cwd")")" ;;
esac
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
