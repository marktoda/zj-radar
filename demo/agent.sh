#!/usr/bin/env bash
# A scripted "demo agent": broadcasts a timed arc of zj_radar.status.v1 messages
# for THIS pane, so the sidebar animates a believable story with no real agent,
# no API keys, and byte-identical output every run. Same pipe the Claude/Codex
# producers use — see the README's "Writing your own producer".
#
#   agent.sh <source> <repo> <branch> <scenario>
#
# <source> is a Kind wire token (claude|codex|gemini|test|build|deploy|command).
# <scenario> selects one of the arcs below. Run only inside Zellij.
set -euo pipefail

source_kind="${1:-claude}"
repo="${2:-demo}"
branch="${3:-main}"
scenario="${4:-working}"

# Resolve our own pane id (the sidebar keys status by pane → tab).
[[ -n "${ZELLIJ:-}" && -n "${ZELLIJ_PANE_ID:-}" ]] || {
    echo "agent.sh: not inside Zellij (no ZELLIJ_PANE_ID) — nothing to broadcast." >&2
    exec sleep infinity
}
pane="${ZELLIJ_PANE_ID#terminal_}"
seq=0

# emit <status> <msg> [on_focus]
# on_focus is only set for terminal states (done) so they clear when you focus
# the tab; omit it for running/pending so focusing doesn't reset live work.
emit() {
    seq=$((seq + 1))
    local status="$1" msg="$2" on_focus="${3:-}"
    local payload
    payload=$(printf '{"v":1,"source":"%s","pane":{"type":"terminal","id":%s},"status":"%s","repo":"%s","branch":"%s","msg":"%s","seq":%s' \
        "$source_kind" "$pane" "$status" "$repo" "$branch" "$msg" "$seq")
    [[ -n "$on_focus" ]] && payload+="$(printf ',"on_focus":"%s"' "$on_focus")"
    payload+='}'
    zellij pipe --name zj_radar.status.v1 -- "$payload"
}

# A little context in the focused content pane so it doesn't read as "broken".
printf '\033[2m%s · %s\033[0m\n\n' "$repo" "$branch"

case "$scenario" in
needs-you) # the focused agent: works, blocks on approval, then resumes. Its
           # stdout reads like a Claude session so the content pane mirrors the
           # rail (an agent, not a build).
    printf '> add auth middleware\n\n'
    sleep 1;  emit running "reading auth middleware"
    printf '\033[2m●\033[0m Read  src/auth.rs\n'
    printf '\033[2m●\033[0m Edit  src/auth.rs\n'
    sleep 4;  emit pending "run database migration?"
    printf '\n\033[33m❯ Run migration?\033[0m (y/n)\n'
    sleep 4;  emit running "applying migration"
    printf '\n\033[2m●\033[0m Bash  sqlx migrate\n'
    ;;
done) # an agent that finishes; the card persists on its tab
    sleep 1;  emit running "implementing login form"
    sleep 7;  emit "done" "added login + 2 tests" idle
    ;;
tests) # an observed task: a test run that progresses, then passes
    sleep 1;  emit running "running 48 tests"
    sleep 7;  emit "done" "48 passed in 11s" idle
    ;;
deploy-error) # an observed task that fails
    sleep 1;  emit running "terraform plan"
    sleep 3;  emit running "terraform apply"
    sleep 4;  emit error "apply failed: exit 1"
    ;;
form) # one pane of a multi-pane tab: builds the form, finishes early
    printf '> build LoginForm.tsx\n\n'
    sleep 1;  emit running "implementing login form"
    printf '\033[2m●\033[0m Edit  LoginForm.tsx\n'
    sleep 7;  emit "done" "login form done" idle
    printf '\033[2m●\033[0m done\n'
    ;;
suite) # second pane of the multi-pane tab: writes tests, keeps running (so a
       # spinner is still animating during the closing dwell)
    printf '> add login tests\n\n'
    sleep 1;  emit running "writing login tests"
    printf '\033[2m●\033[0m Edit  login.test.ts\n'
    sleep 12; emit "done" "+2 tests passing" idle
    ;;
*) # generic: just stay working
    sleep 1;  emit running "working…"
    ;;
esac

# Hold the final pushed status (and keep the pane's command alive) until the
# recording ends.
exec sleep infinity
