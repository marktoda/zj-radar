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
