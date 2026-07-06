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

# Hold this pane open (and its last pushed status on the rail) until the
# recording ends. Deliberately NOT `exec`: Zellij reports a pane whose root
# process has no child as "back at the shell prompt" (is_foreground=false),
# and the plugin rightly exit-clears the pane's pushed status on that signal —
# so tail must run as our CHILD, keeping the process tree agent-shaped.
# (`sleep infinity` is GNU-only — stock macOS sleep rejects it.)
hold() { tail -f /dev/null; }

# Resolve our own pane id (the sidebar keys status by pane → tab).
[[ -n "${ZELLIJ:-}" && -n "${ZELLIJ_PANE_ID:-}" ]] || {
    echo "agent.sh: not inside Zellij (no ZELLIJ_PANE_ID) — nothing to broadcast." >&2
    hold
}
pane="${ZELLIJ_PANE_ID#terminal_}"

# emit <status> <msg> [task] — one zj_radar.status.v1 broadcast for this pane.
# Ordering is latest-wins on the plugin side; the pipe delivers in order, so
# each emit simply overwrites the pane's previous state. A non-empty [task]
# becomes the sticky identity line; while pending, the rail spends an extra
# `↳` line on <msg> (the actionable question).
emit() {
    local status="$1" msg="$2" task="${3:-}"
    local payload
    payload=$(printf '{"v":1,"source":"%s","pane":{"type":"terminal","id":%s},"status":"%s","repo":"%s","branch":"%s","msg":"%s"' \
        "$source_kind" "$pane" "$status" "$repo" "$branch" "$msg")
    if [[ -n "$task" ]]; then
        payload+=$(printf ',"task":"%s"' "$task")
    fi
    payload+='}'
    zellij pipe --name zj_radar.status.v1 -- "$payload"
}

# A little context in the focused content pane so it doesn't read as "broken".
printf '\033[2m%s · %s\033[0m\n\n' "$repo" "$branch"

case "$scenario" in
needs-you) # the star of the show: works with a sticky task, blocks on approval
           # while the viewer is on another tab (the rail carries the ↳
           # question there), and resumes when the recorded `y` lands — the
           # keystroke actually answers the read, so the beat can't misfire.
           # Its stdout reads like a Claude session for the beat-3 jump.
    task="add auth middleware"
    printf '> add auth middleware\n\n'
    sleep 1;  emit running "reading auth middleware" "$task"
    printf '\033[2m●\033[0m Read  src/auth.rs\n'
    printf '\033[2m●\033[0m Edit  src/auth.rs\n'
    sleep 5;  emit pending "run database migration?" "$task"
    printf '\n\033[33m❯ Run migration?\033[0m (y/n) '
    # `read` is a bash builtin: while it blocks, this script has no child
    # process, and a childless pane root reads to Zellij (and so the plugin)
    # as "producer exited, back at the prompt" — which would exit-clear the
    # pending card mid-beat. Park a sleep child for the duration of the wait.
    sleep 30 & guard=$!
    read -r -n 1 -t 20 || true   # tape types `y` (into REPLY); timeout so an
    kill "$guard" 2>/dev/null || true   # unattended run still completes
    printf '\n\n'
    emit running "applying migration" "$task"
    printf '\033[2m●\033[0m Bash  sqlx migrate run\n'
    sleep 2;  emit "done" "migration applied" "$task"
    printf '\033[2m●\033[0m done\n'
    ;;
tests) # an observed task: a test run that progresses, then passes late enough
       # to land during the settle beat
    sleep 1;  emit running "running 48 tests"
    sleep 12; emit "done" "48 passed in 11s"
    ;;
deploy-error) # an observed task that fails during the settle beat
    sleep 1;  emit running "terraform plan"
    sleep 5;  emit running "terraform apply"
    sleep 8;  emit error "apply failed: exit 1"
    ;;
form) # one pane of the multi-pane `web` tab (the opening focus): builds the
      # form on-camera, finishes as the story settles
    printf '> build LoginForm.tsx\n\n'
    sleep 1;  emit running "implementing login form"
    printf '\033[2m●\033[0m Edit  LoginForm.tsx\n'
    sleep 11; emit "done" "login form done"
    printf '\033[2m●\033[0m done\n'
    ;;
suite) # second pane of the multi-pane tab: keeps a spinner alive longest, then
       # completes just before the closing screenshot
    printf '> add login tests\n\n'
    sleep 1;  emit running "writing login tests"
    printf '\033[2m●\033[0m Edit  login.test.ts\n'
    sleep 13; emit "done" "+2 tests passing"
    ;;
*) # generic: just stay working
    sleep 1;  emit running "working…"
    ;;
esac

hold
