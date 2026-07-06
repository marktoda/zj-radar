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
#   - In-order and non-erroring: the pipe is sent synchronously (hooks fire in
#     order, so the producer must not reorder its own broadcasts) and every
#     failure path degrades to a silent no-op — never an error into Claude.
#   - No-op outside Zellij, or on a non-terminal pane id.
#
# Dependency: jq (used to parse the hook payload + build JSON). The productized
# `zj-radar notify` binary will remove this dependency.
set -euo pipefail

status="${1:-running}"

# Read the hook payload up front so the running-path notify below can be
# backgrounded. `running` rides UserPromptSubmit and Pre/PostToolUse — the
# hottest events Claude has — and a synchronous notify blocks the harness
# (UserPromptSubmit blocks the user's prompt) until it exits. On a quiet
# machine that's milliseconds; on a saturated one (test suites, subagent
# fleets) process spawns crawl and the hook eats the 30s timeout. A running
# ping is fire-and-forget by nature: losing one under load is harmless,
# blocking a prompt is not.
# Cap the read at 8 MiB (parity with the Rust CLI's MAX_STDIN_BYTES): a
# degenerate multi-GB stream must bound memory, not buffer whole. Truncated
# input just fails the jq parses below and no-ops — the safe degradation.
input="$(head -c 8388608 2>/dev/null || true)"

# Prefer the native CLI when present (drops the jq/bash dependency). It applies
# the same Zellij gate, pending backstop, and payload schema. Falls back to the
# bash implementation below when the binary isn't installed.
#
# Dispatch split, reconciling the in-order contract with the hot-path note
# above: `running` is backgrounded (self-healing — the next event overwrites
# it, and the plugin's stale-Running grace clock catches a straggler), but the
# EDGES (done/pending/idle) go synchronously — an edge overtaken by a stale
# `running` is exactly the stuck-spinner bug the tail comment on the bash path
# documents, and edges fire once per turn, off the harness's critical path.
if command -v zj-radar >/dev/null 2>&1; then
    if [[ "$status" == "running" ]]; then
        ( printf '%s' "$input" | zj-radar notify claude --status "$status" >/dev/null 2>&1 & )
    else
        printf '%s' "$input" | zj-radar notify claude --status "$status" >/dev/null 2>&1 || true
    fi
    exit 0
fi

# The bash fallback needs jq to build the payload. The final `jq -nc` below has
# no `|| true`, so under `set -euo pipefail` a missing jq would abort mid-hook
# (exit 127) into Claude's output instead of no-op'ing — violating the "never
# block/error Claude" contract. If jq is absent, do nothing, same as when we're
# outside Zellij. (The earlier `jq ... || true` calls already tolerate this; this
# makes the whole script consistent.)
command -v jq >/dev/null 2>&1 || exit 0

[[ -n "${ZELLIJ:-}" && -n "${ZELLIJ_PANE_ID:-}" ]] || exit 0
pane_num="${ZELLIJ_PANE_ID#terminal_}"
[[ "$pane_num" =~ ^[0-9]+$ ]] || exit 0

# ($input was read before the binary dispatch above — stdin is already drained.)
cwd="$(jq -r '.cwd // empty' <<<"$input" 2>/dev/null || true)"
# A real path can't exceed PATH_MAX (4 KB on Linux); anything longer is hostile
# input. The cap also protects every downstream command that receives $cwd as
# an ARGUMENT (`git -C`, `basename`): Linux limits a single exec argument to
# 128 KB (MAX_ARG_STRLEN), past which execve fails E2BIG and `set -e` kills
# the hook mid-turn — erroring into Claude. macOS has no per-arg cap, so this
# only ever bit on Linux.
cwd="${cwd:0:4096}"
[[ -n "$cwd" ]] || cwd="$PWD"
msg="$(jq -r '.message // .last_assistant_message // empty' <<<"$input" 2>/dev/null || true)"
task=""

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
    # UserPromptSubmit: capture the first non-empty prompt line as the sticky
    # task label (mirrors task_from_prompt in agents.rs). Slash commands,
    # harness-injected tag lines (e.g. <task-notification>), and bare acks
    # send no task — the plugin keeps the previous label.
    if [[ "$hook_event" == "UserPromptSubmit" ]]; then
        prompt="$(jq -r '.prompt // empty' <<<"$input" 2>/dev/null || true)"
        task="$(printf '%s\n' "$prompt" \
            | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' \
            | grep -m1 . || true)"
        case "$task" in "/"*|"<"*) task="" ;; esac
        # Ack filter: lowercase, strip trailing punctuation (parity with the
        # Rust ACK_PROMPTS list — keep the two lists identical).
        t_norm="$(printf '%s' "$task" | tr '[:upper:]' '[:lower:]' | sed -e 's/[.!?,]*$//' -e 's/[[:space:]]*$//')"
        case "$t_norm" in
            y|yes|yep|yeah|n|no|ok|okay|k|sure|go|"go ahead"|proceed|continue|"do it"|lgtm|"sounds good"|approved|thanks|ty|"thank you")
                task="" ;;
        esac
        task="${task:0:512}"
    fi
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
# — it isn't a real "needs you" event. The comparison uses a TRIMMED copy
# (parity with the Rust producer's msg.trim()) so a whitespace-padded generic
# phrase is still dropped; the broadcast itself keeps the raw msg, as Rust does.
if [[ "$status" == "pending" ]]; then
    m_trim="$(printf '%s' "$msg" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
    case "$m_trim" in
        ""|"Claude needs attention"|"Claude Code needs your attention")
            exit 0
            ;;
    esac
fi

# A turn that ends by asking the user something is blocked on input, not done:
# remap done → pending with the trailing question (the last non-empty line,
# when it ends in a question mark) as the message. Parity with
# trailing_question in agents.rs.
if [[ "$status" == "done" && -n "$msg" ]]; then
    last_line="$(printf '%s\n' "$msg" | awk 'NF{l=$0} END{print l}' \
        | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
    case "$last_line" in
        *"?"|*"？")
            status="pending"
            msg="$last_line"
            ;;
    esac
fi

# idle means "no activity" — never carry a message (drops any stale message the
# payload rides in on, e.g. a SessionStart session_title), so the rail row
# recedes cleanly on /clear. Mirrors derive_claude's idle branch.
[[ "$status" == "idle" ]] && msg=""

# Bound the message so a pathologically long final assistant message can't push
# the payload past the plugin's 64 KB cap (which would drop the whole update,
# e.g. losing a `done` edge). 512 is generous vs the plugin's 60-char display cap
# yet far under 64 KB whether the shell counts this substring in chars or bytes.
msg="${msg:0:512}"

# Resolve the repo name from the COMMON git dir so worktrees report the main
# repo (e.g. "pinky"), not the worktree directory (e.g. "reply-register", which
# is what --show-toplevel returns inside a worktree). Fall back to --show-toplevel
# for git < 2.31 (no --path-format), then to the cwd basename.
common="$(git -C "$cwd" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)"
common="${common%/}"
# git < 2.31 doesn't know --path-format: rev-parse ECHOES the unknown flag to
# stdout and exits 0, so $common would be the flag text plus a relative `.git`
# on a second line — and the `*.git` arm below would then hand basename a
# `--`-leading argument, aborting the whole hook under `set -e`. Require a
# single-line absolute path; anything else falls to --show-toplevel below.
[[ "$common" == /* && "$common" != *$'\n'* ]] || common=""
case "$common" in
    */.git) repo="$(basename "$(dirname "$common")")" ;;   # .../pinky/.git → pinky
    *.git)  repo="$(basename "${common%.git}")" ;;          # bare repo acme.git → acme
    ?*)     repo="$(basename "$common")" ;;                  # unusual: use basename
    *)      repo="$(basename "$(git -C "$cwd" rev-parse --show-toplevel 2>/dev/null || printf '%s' "$cwd")")" ;;
esac
branch="$(git -C "$cwd" branch --show-current 2>/dev/null || true)"

payload="$(jq -nc \
    --argjson id "$pane_num" \
    --arg status "$status" \
    --arg repo "$repo" \
    --arg branch "$branch" \
    --arg msg "$msg" \
    --arg task "$task" \
    '{v: 1, source: "claude", pane: {type: "terminal", id: $id},
      status: $status, repo: $repo, branch: $branch, msg: $msg, task: $task}')"

if [[ "${ZJ_RADAR_DEBUG:-}" == "1" ]]; then
    printf 'zj-radar payload: %s\n' "$payload" >&2
    exit 0
fi

# Synchronous, matching the Rust CLI: hooks fire in order, so an in-order
# producer is what makes the plugin's latest-wins contract hold. An earlier
# version backgrounded this with `( … ) &`, which let a Stop→done pipe be
# overtaken by the preceding PostToolUse→running — the stale spinner stuck
# until the next event. `zellij pipe` is a fast local write; `|| true` keeps
# a dead/absent server from erroring into Claude.
zellij pipe --name zj_radar.status.v1 -- "$payload" >/dev/null 2>&1 || true
