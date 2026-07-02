#!/usr/bin/env bats
load helper

SCRIPT="$BATS_TEST_DIRNAME/../scripts/notify.sh"
CLI="$BATS_TEST_DIRNAME/../../../target/debug/zj-radar"

# Extract just the "msg" field from a recorded zj_radar.status.v1 payload.
payload_msg() { last_payload | jq -r '.msg'; }

parity_case() { # $1 = hook JSON, $2 = status arg
  # --- bash producer (fallback path: no zj-radar on PATH) ---
  rm -f "$RECORD"
  echo "$1" | "$SCRIPT" "$2"
  # bash backgrounds the zellij call; last_payload() polls for it
  local bash_msg; bash_msg="$(payload_msg)"

  # --- rust producer ---
  rm -f "$RECORD"
  echo "$1" | "$CLI" notify claude --status "$2"
  local rust_msg; rust_msg="$(payload_msg)"

  [ -n "$bash_msg" ] || { echo "bash extraction failed (empty) for input: $1"; return 1; }
  [ -n "$rust_msg" ] || { echo "rust extraction failed (empty) for input: $1"; return 1; }
  echo "bash=[$bash_msg] rust=[$rust_msg]"
  [ "$bash_msg" = "$rust_msg" ]
}

setup() { setup_fakes; }
teardown() { teardown_fakes; }

@test "parity: Edit activity" {
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Edit","tool_input":{"file_path":"/home/u/myrepo/src/auth.rs"}}' running
}

@test "parity: Bash git commit activity" {
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Bash","tool_input":{"command":"git commit -m x"}}' running
}

@test "parity: Read activity" {
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Read","tool_input":{"file_path":"/home/u/myrepo/README.md"}}' running
}

@test "parity: Write activity" {
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Write","tool_input":{"file_path":"/home/u/myrepo/src/lib.rs"}}' running
}

@test "parity: Grep activity" {
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Grep","tool_input":{"pattern":"fn main"}}' running
}

@test "parity: Bash git push activity" {
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Bash","tool_input":{"command":"git push origin main"}}' running
}

@test "parity: Bash generic command activity" {
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Bash","tool_input":{"command":"ls -la"}}' running
}

@test "parity: TodoWrite activity" {
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"TodoWrite","tool_input":{"todos":[]}}' running
}

@test "parity: word-bounded classification (no substring misfire)" {
  # "latest" must not read as a test; both producers fall through to the exe.
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Bash","tool_input":{"command":"git checkout latest"}}' running
}

@test "parity: uninstall is not install" {
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Bash","tool_input":{"command":"npm uninstall left-pad"}}' running
}

@test "parity: running with no activity falls back to working" {
  # No tool activity to derive (UserPromptSubmit, empty message) → both
  # producers emit the neutral "working" baseline, never a blank msg.
  parity_case '{"hook_event_name":"UserPromptSubmit","cwd":"/home/u/myrepo"}' running
}

payload_task() { last_payload | jq -r '.task'; }

parity_task_case() { # $1 = hook JSON
  rm -f "$RECORD"
  echo "$1" | "$SCRIPT" running
  local bash_task; bash_task="$(payload_task)"
  rm -f "$RECORD"
  echo "$1" | "$CLI" notify claude --status running
  local rust_task; rust_task="$(payload_task)"
  echo "bash=[$bash_task] rust=[$rust_task]"
  [ "$bash_task" = "$rust_task" ]
}

@test "parity: UserPromptSubmit task label" {
  parity_task_case '{"hook_event_name":"UserPromptSubmit","cwd":"/home/u/myrepo","prompt":"fix the flaky e2e retries\ndetails follow"}'
}

@test "parity: slash-command prompt sends no task" {
  parity_task_case '{"hook_event_name":"UserPromptSubmit","cwd":"/home/u/myrepo","prompt":"/clear"}'
}

@test "parity: ack prompt sends no task" {
  parity_task_case '{"hook_event_name":"UserPromptSubmit","cwd":"/home/u/myrepo","prompt":"Yes."}'
}

@test "parity: harness-injected tag prompt sends no task" {
  # Background-agent completions fire UserPromptSubmit with a machine turn
  # like <task-notification>…; neither producer may take it as the task label.
  parity_task_case '{"hook_event_name":"UserPromptSubmit","cwd":"/home/u/myrepo","prompt":"<task-notification>\n<task-id>a1</task-id>done\n</task-notification>"}'
}

@test "parity: tool event sends no task" {
  parity_task_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Grep","tool_input":{"pattern":"x"},"prompt":"stray"}'
}

@test "parity: Stop ending in a question remaps done to pending" {
  # A turn that ends by asking is blocked on input: both producers must remap
  # done → pending and carry ONLY the trailing question line as the msg.
  local json='{"hook_event_name":"Stop","cwd":"/home/u/myrepo","last_assistant_message":"Refactored the auth module.\n\nShould I also update the tests?"}'

  rm -f "$RECORD"; echo "$json" | "$SCRIPT" done
  local bash_payload; bash_payload="$(last_payload)"

  rm -f "$RECORD"; echo "$json" | "$CLI" notify claude --status done
  local rust_payload; rust_payload="$(last_payload)"

  [ "$(jq -r '.status' <<<"$bash_payload")" = pending ]
  [ "$(jq -r '.status' <<<"$rust_payload")" = pending ]
  [ "$(jq -r '.msg' <<<"$bash_payload")" = "Should I also update the tests?" ]
  [ "$(jq -r '.msg' <<<"$rust_payload")" = "Should I also update the tests?" ]
}

@test "parity: Stop ending in a statement stays done" {
  local json='{"hook_event_name":"Stop","cwd":"/home/u/myrepo","last_assistant_message":"Anything else?\nAll tests pass."}'

  rm -f "$RECORD"; echo "$json" | "$SCRIPT" done
  local bash_payload; bash_payload="$(last_payload)"

  rm -f "$RECORD"; echo "$json" | "$CLI" notify claude --status done
  local rust_payload; rust_payload="$(last_payload)"

  [ "$(jq -r '.status' <<<"$bash_payload")" = done ]
  [ "$(jq -r '.status' <<<"$rust_payload")" = done ]
}

@test "parity: idle clears the message in both producers" {
  # `parity_case` asserts a NON-empty msg, so idle (intentionally blank) needs
  # its own check: both producers must agree on status=idle AND an empty msg,
  # even when a stale message rides in on the SessionStart payload.
  local json='{"hook_event_name":"SessionStart","source":"clear","cwd":"/home/u/myrepo","message":"old work in progress"}'

  rm -f "$RECORD"; echo "$json" | "$SCRIPT" idle
  local bash_payload; bash_payload="$(last_payload)"

  rm -f "$RECORD"; echo "$json" | "$CLI" notify claude --status idle
  local rust_payload; rust_payload="$(last_payload)"

  [ "$(jq -r '.status' <<<"$bash_payload")" = idle ]
  [ "$(jq -r '.status' <<<"$rust_payload")" = idle ]
  [ "$(jq -r '.msg' <<<"$bash_payload")" = "" ]
  [ "$(jq -r '.msg' <<<"$rust_payload")" = "" ]
}
