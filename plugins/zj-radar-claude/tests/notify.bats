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

@test "done sets on_focus=idle (clear-on-focus)" {
  echo '{"hook_event_name":"Stop","cwd":"/home/u/myrepo"}' | "$SCRIPT" done
  run last_payload
  [[ "$output" == *"on_focus"* ]]
  [[ "$output" == *"idle"* ]]
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

@test "SessionStart clear broadcasts idle with blank msg and no on_focus" {
  # `/clear` fires SessionStart{source:clear}; the plugin wires it to `idle`.
  # The broadcast resets the pane: status idle, no message, and (unlike done)
  # no on_focus — there is nothing left to clear on the next visit.
  echo '{"hook_event_name":"SessionStart","source":"clear","cwd":"/home/u/myrepo"}' | "$SCRIPT" idle
  run last_payload
  [ "$(jq -r '.status' <<<"$output")" = idle ]
  [ "$(jq -r '.msg' <<<"$output")" = "" ]
  [ "$(jq 'has("on_focus")' <<<"$output")" = false ]
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
  # 124 if the hook hangs; we require a clean, prompt exit instead.
  local big; big="$(printf 'x%.0s' {1..200000})"
  run timeout 10 bash -c \
    "printf '{\"hook_event_name\":\"PostToolUse\",\"cwd\":\"%s\",\"tool_name\":\"Edit\",\"tool_input\":{\"file_path\":\"a.rs\"}}' '$big' | '$SCRIPT' running"
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
