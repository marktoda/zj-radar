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
needs-you) # an agent that works, blocks on a permission prompt, then resumes
    sleep 1;  emit running "reading auth middleware"
    sleep 4;  emit pending "run database migration?"
    sleep 5;  emit running "applying migration"
    ;;
done) # an agent that finishes; the card persists on its tab
    sleep 1;  emit running "implementing login form"
    sleep 7;  emit "done" "added login + 2 tests" idle
    ;;
tests) # an observed task: a test run that progresses, then passes. This is the
       # focused tab, so its stdout fills the content pane next to the rail.
    printf '$ cargo test\n'
    sleep 1;  emit running "running 48 tests"
    printf '   Compiling core v0.1.0\n'
    sleep 3;  printf '    Finished test profile in 8.2s\n     Running unittests src/lib.rs\n'
    sleep 1;  emit running "37 / 48 passing"
    printf 'test rollup::severity_order ... ok\ntest render::cards_grid ... ok\n'
    sleep 3;  emit "done" "48 passed in 11s" idle
    printf '\ntest result: ok. 48 passed; 0 failed; finished in 11.04s\n'
    ;;
deploy-error) # an observed task that fails
    sleep 1;  emit running "terraform plan"
    sleep 3;  emit running "terraform apply"
    sleep 4;  emit error "apply failed: exit 1"
    ;;
*) # generic: just stay working
    sleep 1;  emit running "working…"
    ;;
esac

# Hold the final pushed status (and keep the pane's command alive) until the
# recording ends.
exec sleep infinity
