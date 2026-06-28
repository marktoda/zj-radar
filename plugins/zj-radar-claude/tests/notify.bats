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
