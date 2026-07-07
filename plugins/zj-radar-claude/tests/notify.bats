#!/usr/bin/env bats
load helper

SCRIPT="$BATS_TEST_DIRNAME/../scripts/notify.sh"

setup()    { setup_fakes; }
teardown() { teardown_fakes; }

@test "PostToolUse Edit derives 'editing <basename>'" {
  echo '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Edit","tool_input":{"file_path":"/home/u/myrepo/src/auth.rs"}}' \
    | "$SCRIPT" running
  run last_payload
  [[ "$output" == *"editing auth.rs"* ]]
}

@test "Bash git push derives 'pushing'" {
  echo '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Bash","tool_input":{"command":"git push origin main"}}' \
    | "$SCRIPT" running
  run last_payload
  [[ "$output" == *"pushing"* ]]
}

@test "generic pending message is skipped (defense-in-depth)" {
  # The script filters out known generic idle phrases for pending status.
  # "Claude needs attention" is one of the matched phrases; it exits 0 early.
  rm -f "$RECORD"
  echo '{"hook_event_name":"Notification","cwd":"/home/u/myrepo","message":"Claude needs attention"}' \
    | "$SCRIPT" pending || true
  # No zellij call should have been made.
  [ ! -s "$RECORD" ]
}

@test "not in Zellij: clean exit, no broadcast" {
  unset ZELLIJ ZELLIJ_PANE_ID
  rm -f "$RECORD"
  run bash -c "echo '{\"hook_event_name\":\"Stop\",\"cwd\":\"/tmp\"}' | '$SCRIPT' done"
  [ "$status" -eq 0 ]
  [ ! -s "$RECORD" ]
}

@test "jq unavailable: clean exit, no broadcast (never errors into Claude)" {
  rm -f "$RECORD"
  # In Zellij (setup_fakes exports ZELLIJ/ZELLIJ_PANE_ID) but with neither
  # zj-radar nor jq resolvable: the bash fallback must no-op (exit 0), not abort
  # under `set -e`. Run through an absolute bash with an empty PATH so `command
  # -v jq` fails; the guard fires before any external tool is needed.
  local bash_abs; bash_abs="$(command -v bash)"
  PATH="" run "$bash_abs" "$SCRIPT" running <<<'{"hook_event_name":"Stop","cwd":"/tmp"}'
  [ "$status" -eq 0 ]
  [ ! -s "$RECORD" ]
}

@test "UserPromptSubmit broadcasts the first prompt line as task" {
  echo '{"hook_event_name":"UserPromptSubmit","cwd":"/tmp","prompt":"  fix flaky e2e  \nrest"}' | "$SCRIPT" running
  [ "$(last_payload | jq -r '.task')" = "fix flaky e2e" ]
  [ "$(last_payload | jq -r '.msg')" = "working" ]
}

@test "Stop broadcasts status=done" {
  echo '{"hook_event_name":"Stop","cwd":"/home/u/myrepo"}' | "$SCRIPT" done
  run last_payload
  [ "$(jq -r '.status' <<<"$output")" = done ]
}

@test "hooks.json wires SessionStart{clear} to notify.sh idle" {
  # The fix for the /clear stale-status bug: SessionStart fires on clear; the
  # matcher scopes the reset to `clear` only (never startup/resume/compact),
  # mirroring how Notification scopes to permission_prompt.
  local hooks="$BATS_TEST_DIRNAME/../hooks/hooks.json"
  [ "$(jq -r '.hooks.SessionStart[0].matcher' "$hooks")" = clear ]
  local cmd; cmd="$(jq -r '.hooks.SessionStart[0].hooks[0].command' "$hooks")"
  [[ "$cmd" == *"notify.sh idle"* ]]
}

@test "hooks.json wires SessionEnd to notify.sh idle" {
  # A closed session must recede its row rather than freeze the last status
  # on the rail (the stale-Running ghost). Unmatchered: every end counts.
  local hooks="$BATS_TEST_DIRNAME/../hooks/hooks.json"
  local cmd; cmd="$(jq -r '.hooks.SessionEnd[0].hooks[0].command' "$hooks")"
  [[ "$cmd" == *"notify.sh idle"* ]]
}

@test "SessionStart clear broadcasts idle with blank msg" {
  # `/clear` fires SessionStart{source:clear}; the plugin wires it to `idle`.
  # The broadcast resets the pane: status idle and no message.
  echo '{"hook_event_name":"SessionStart","source":"clear","cwd":"/home/u/myrepo"}' | "$SCRIPT" idle
  run last_payload
  [ "$(jq -r '.status' <<<"$output")" = idle ]
  [ "$(jq -r '.msg' <<<"$output")" = "" ]
}

@test "idle drops any stale message" {
  # idle means "no activity": a message riding on the payload must not leak into
  # the idle row, or the rail would still show the pre-clear line.
  echo '{"hook_event_name":"SessionStart","source":"clear","cwd":"/home/u/myrepo","message":"old work in progress"}' \
    | "$SCRIPT" idle
  run last_payload
  [ "$(jq -r '.status' <<<"$output")" = idle ]
  [ "$(jq -r '.msg' <<<"$output")" = "" ]
}

# ── adversarial input: the hook runs on EVERY tool use, so hostile or malformed
#    stdin must never hang or crash the user's shell ──────────────────────────

@test "oversized hook payload completes promptly (no hang/OOM)" {
  # A pathological 200 KB payload must not wedge the shell. `timeout` fires exit
  # 124 if the hook hangs; we require a clean, prompt exit instead. The payload
  # travels via a file on stdin, NOT the command line: Linux caps a single exec
  # argument at 128 KB (MAX_ARG_STRLEN), so interpolating it into `bash -c`
  # dies E2BIG before the hook even runs (only on Linux — macOS has no per-arg
  # cap, which is how the argv version passed locally while failing CI).
  local payload; payload="$(mktemp)"
  {
    printf '{"hook_event_name":"PostToolUse","cwd":"'
    printf 'x%.0s' {1..200000}
    printf '","tool_name":"Edit","tool_input":{"file_path":"a.rs"}}'
  } > "$payload"
  run timeout 10 "$SCRIPT" running < "$payload"
  rm -f "$payload"
  [ "$status" -eq 0 ]
}

@test "stdin past the 8 MiB cap is truncated, hook still exits cleanly" {
  # The fallback read is `head -c 8388608`, parity with the Rust CLI's
  # MAX_STDIN_BYTES: a stream larger than the cap must be bounded (never
  # buffered whole) and the truncated, no-longer-valid JSON must degrade
  # through the jq guards to a clean exit.
  local payload; payload="$(mktemp)"
  head -c 9000000 /dev/zero | tr '\0' 'x' > "$payload"
  run timeout 10 "$SCRIPT" running < "$payload"
  rm -f "$payload"
  [ "$status" -eq 0 ]
}

@test "malformed (non-JSON) stdin exits cleanly" {
  # Garbage on stdin (a truncated or non-JSON hook payload) must degrade through
  # the jq `// empty` guards to a clean exit, never a crash.
  run bash -c "printf 'not json at all{{' | '$SCRIPT' running"
  [ "$status" -eq 0 ]
}

@test "raw control bytes in stdin do not crash the hook" {
  # A raw BEL (0x07) inside a JSON string makes the input invalid JSON; jq's
  # guarded reads fall back to empty and the hook must still exit 0, never error
  # out or emit a corrupt broadcast.
  run bash -c "printf '{\"hook_event_name\":\"Stop\",\"cwd\":\"/home/u/my\007repo\"}' | '$SCRIPT' done"
  [ "$status" -eq 0 ]
}

# ── repo resolution from git (the default fake git only stubs --show-toplevel,
#    so these override it to drive the primary --git-common-dir paths) ──────────

@test "git worktree: repo resolves from the common dir, not the worktree dir" {
  # A worktree's --git-common-dir points at the MAIN repo's .git (.../pinky/.git),
  # so the repo name must be the main project (pinky), never the worktree dir.
  cat >"$FAKEBIN/git" <<'EOF'
#!/usr/bin/env bash
case "$*" in
  *"--git-common-dir"*)      echo /home/u/pinky/.git ;;
  *"branch --show-current"*) echo feature-x ;;
  *) exit 0 ;;
esac
EOF
  chmod +x "$FAKEBIN/git"
  echo '{"hook_event_name":"Stop","cwd":"/home/u/pinky-wt/reply"}' | "$SCRIPT" done
  run last_payload
  [[ "$output" == *'"repo":"pinky"'* ]]
  [[ "$output" == *'"branch":"feature-x"'* ]]
}

@test "git bare repo: repo strips the .git suffix" {
  # A bare repo's common dir is itself (acme.git); strip .git and take basename.
  cat >"$FAKEBIN/git" <<'EOF'
#!/usr/bin/env bash
case "$*" in
  *"--git-common-dir"*)      echo /srv/git/acme.git ;;
  *"branch --show-current"*) echo main ;;
  *) exit 0 ;;
esac
EOF
  chmod +x "$FAKEBIN/git"
  echo '{"hook_event_name":"Stop","cwd":"/srv/checkout"}' | "$SCRIPT" done
  run last_payload
  [[ "$output" == *'"repo":"acme"'* ]]
}

@test "git fallback: old git ECHOES --path-format and must not crash the hook" {
  # git < 2.31 doesn't know --path-format. Crucially it does NOT exit 1: rev-parse
  # ECHOES the unknown flag to stdout (exit 0), followed by the relative common
  # dir. Un-guarded, that string fell into the `*.git` case arm and `basename`
  # aborted the whole hook under `set -e` — erroring into Claude on every event.
  # The guard must reject it and fall back to --show-toplevel's basename.
  cat >"$FAKEBIN/git" <<'EOF'
#!/usr/bin/env bash
case "$*" in
  *"--path-format="*)        printf -- '--path-format=absolute\n.git\n' ;;
  *"--show-toplevel"*)       echo /home/u/legacy-repo ;;
  *"branch --show-current"*) echo main ;;
  *) exit 0 ;;
esac
EOF
  chmod +x "$FAKEBIN/git"
  echo '{"hook_event_name":"Stop","cwd":"/home/u/legacy-repo/src"}' | "$SCRIPT" done
  run last_payload
  [[ "$output" == *'"repo":"legacy-repo"'* ]]
}

@test "git fallback: rev-parse hard failure still resolves repo from cwd" {
  # No git repo at all (every rev-parse fails): the repo falls back to the cwd
  # basename and the hook still broadcasts rather than erroring.
  cat >"$FAKEBIN/git" <<'EOF'
#!/usr/bin/env bash
exit 1
EOF
  chmod +x "$FAKEBIN/git"
  echo '{"hook_event_name":"Stop","cwd":"/home/u/scratch"}' | "$SCRIPT" done
  run last_payload
  [[ "$output" == *'"repo":"scratch"'* ]]
}

@test "hung zellij pipe is killed at the send deadline (server FD-leak guard)" {
  # A rail instance wedged at Zellij's permission prompt blocks `zellij pipe`
  # forever (backpressure: the client is held until every plugin consumes the
  # message). Hooks fire per tool call, so unbounded blocked clients each pin
  # two server FDs until the server EMFILEs and the whole session crashes.
  # The producer must bound the send; killing the client never retracts the
  # message (it is already queued server-side), so latest-wins still holds.
  # `exec` so the shim process IS the sleeper — the watchdog's kill must reap
  # the actual hung process, not an intermediate shell.
  cat >"$FAKEBIN/zellij" <<EOF
#!/usr/bin/env bash
printf '%s\t\n' "\$*" >> "$RECORD"
exec sleep 60
EOF
  chmod +x "$FAKEBIN/zellij"
  local start=$SECONDS
  run bash -c "echo '{\"hook_event_name\":\"Stop\",\"cwd\":\"/tmp\"}' | ZJ_RADAR_PIPE_TIMEOUT=1 '$SCRIPT' done"
  [ "$status" -eq 0 ]
  (( SECONDS - start < 10 ))
  [ -s "$RECORD" ]  # the broadcast was still attempted before the hang
}

@test "malformed ZJ_RADAR_PIPE_TIMEOUT falls back to the 5s deadline (fail closed)" {
  # The watchdog subshell inherits `set -e`: an override that `sleep` rejects
  # would kill the subshell before its `kill` line runs, silently making the
  # send unbounded again. The producer must sanitize the value instead.
  cat >"$FAKEBIN/zellij" <<EOF
#!/usr/bin/env bash
printf '%s\t\n' "\$*" >> "$RECORD"
exec sleep 60
EOF
  chmod +x "$FAKEBIN/zellij"
  local start=$SECONDS
  run bash -c "echo '{\"hook_event_name\":\"Stop\",\"cwd\":\"/tmp\"}' | ZJ_RADAR_PIPE_TIMEOUT=abc '$SCRIPT' done"
  [ "$status" -eq 0 ]
  (( SECONDS - start < 30 ))  # 5s fallback + slack, nowhere near the 60s hang
}
