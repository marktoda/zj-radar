#!/usr/bin/env bats
load helper

SCRIPT="$BATS_TEST_DIRNAME/../scripts/notify.sh"
CLI="$BATS_TEST_DIRNAME/../../../target/debug/zj-radar"

# Run both producers over the same hook JSON + status and assert the ENTIRE
# payloads match (key order normalized with jq -S) — msg, task, status, repo,
# branch, pane, source, and v all ride the same broadcast, so parity on a
# single field is not parity. Leaves BASH_PAYLOAD/RUST_PAYLOAD set for extra
# per-field assertions in callers (asserting on one suffices once they're equal).
parity_payloads() { # $1 = hook JSON, $2 = status arg
  # --- bash producer (fallback path: no zj-radar on PATH) ---
  rm -f "$RECORD"
  echo "$1" | "$SCRIPT" "$2"
  BASH_PAYLOAD="$(last_payload)"

  # --- rust producer ---
  rm -f "$RECORD"
  echo "$1" | "$CLI" notify claude --status "$2"
  RUST_PAYLOAD="$(last_payload)"

  [ -n "$BASH_PAYLOAD" ] || { echo "bash produced no payload for input: $1"; return 1; }
  [ -n "$RUST_PAYLOAD" ] || { echo "rust produced no payload for input: $1"; return 1; }
  echo "bash=[$BASH_PAYLOAD]"
  echo "rust=[$RUST_PAYLOAD]"
  [ "$(jq -S . <<<"$BASH_PAYLOAD")" = "$(jq -S . <<<"$RUST_PAYLOAD")" ]
}

parity_case() { # $1 = hook JSON, $2 = status arg
  parity_payloads "$1" "$2"
  # A running-case msg must additionally be non-empty — a blank active row is
  # the bug class these cases exist to catch.
  [ -n "$(jq -r '.msg' <<<"$BASH_PAYLOAD")" ] || { echo "empty msg for input: $1"; return 1; }
}

parity_task_case() { # $1 = hook JSON
  parity_payloads "$1" running
}

# Both producers must DROP the broadcast: run each and assert no payload was
# recorded. The inverse of parity_payloads — used for backstop cases where a
# broadcast (from either producer) is the bug.
parity_noop() { # $1 = hook JSON, $2 = status arg
  rm -f "$RECORD"
  echo "$1" | "$SCRIPT" "$2"
  [ ! -s "$RECORD" ] || { echo "bash broadcast for input: $1 → $(cat "$RECORD")"; return 1; }
  rm -f "$RECORD"
  echo "$1" | "$CLI" notify claude --status "$2"
  [ ! -s "$RECORD" ] || { echo "rust broadcast for input: $1 → $(cat "$RECORD")"; return 1; }
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

@test "parity: UserPromptSubmit task label" {
  parity_task_case '{"hook_event_name":"UserPromptSubmit","cwd":"/home/u/myrepo","prompt":"fix the flaky e2e retries\ndetails follow"}'
  [ "$(jq -r '.task' <<<"$BASH_PAYLOAD")" = "fix the flaky e2e retries" ]
}

@test "parity: slash-command prompt sends no task" {
  parity_task_case '{"hook_event_name":"UserPromptSubmit","cwd":"/home/u/myrepo","prompt":"/clear"}'
  [ "$(jq -r '.task' <<<"$BASH_PAYLOAD")" = "" ]
}

@test "parity: harness-injected tag prompt sends no task" {
  # Background-agent completions fire UserPromptSubmit with a machine turn
  # like <task-notification>…; neither producer may take it as the task label.
  parity_task_case '{"hook_event_name":"UserPromptSubmit","cwd":"/home/u/myrepo","prompt":"<task-notification>\n<task-id>a1</task-id>done\n</task-notification>"}'
  [ "$(jq -r '.task' <<<"$BASH_PAYLOAD")" = "" ]
}

@test "parity: tool event sends no task" {
  parity_task_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Grep","tool_input":{"pattern":"x"},"prompt":"stray"}'
  [ "$(jq -r '.task' <<<"$BASH_PAYLOAD")" = "" ]
}

@test "parity: every ack prompt sends no task (the pinned ack list)" {
  # THE ack list. agents.rs ACK_PROMPTS and notify.sh's case pattern each hold
  # a copy tied to the other only by a comment; this loop is the behavioral pin
  # that makes drift in EITHER producer fail loudly. Add new acks here first.
  local acks=(y yes yep yeah n no ok okay k sure go "go ahead" proceed continue
              "do it" lgtm "sounds good" approved thanks ty "thank you")
  local ack json
  # "Yes." / "OK," / "Sounds good!" exercise the shared lowercase +
  # trailing-punctuation normalization in front of the list.
  for ack in "${acks[@]}" "Yes." "OK," "Sounds good!"; do
    json="$(jq -nc --arg p "$ack" '{hook_event_name:"UserPromptSubmit",cwd:"/home/u/myrepo",prompt:$p}')"
    parity_payloads "$json" running
    [ "$(jq -r '.task' <<<"$BASH_PAYLOAD")" = "" ] || { echo "bash kept a task for ack [$ack]"; return 1; }
    [ "$(jq -r '.task' <<<"$RUST_PAYLOAD")" = "" ] || { echo "rust kept a task for ack [$ack]"; return 1; }
  done
}

@test "classification verbs stay ERE-metachar-free (contains_word contract)" {
  # contains_word interpolates its needle raw into an ERE, while the Rust
  # contains_word matches it literally — a verb containing a regex metachar
  # would silently classify differently in the two producers. Extract every
  # literal needle from the script and require plain [a-z0-9 ] words.
  local verbs
  verbs="$(grep -o 'contains_word "[^"]*" "[^"]*"' "$SCRIPT" | sed 's/.* "//; s/"$//')"
  [ -n "$verbs" ] || { echo "no contains_word call sites found — extraction broke"; return 1; }
  local verb
  while IFS= read -r verb; do
    case "$verb" in
      *[!a-z0-9\ ]*) echo "verb [$verb] contains a char outside [a-z0-9 ] — unsafe in the bash ERE"; return 1;;
    esac
  done <<<"$verbs"
}

@test "parity: Notification with a real message is pending in both" {
  # The pending backstop's positive side: a real "needs you" message rides
  # through both producers unchanged. This was the one derive branch with no
  # behavioral pin between them.
  parity_payloads '{"hook_event_name":"Notification","cwd":"/home/u/myrepo","message":"Claude needs your permission to use Bash"}' pending
  [ "$(jq -r '.status' <<<"$BASH_PAYLOAD")" = pending ]
  [ "$(jq -r '.msg' <<<"$BASH_PAYLOAD")" = "Claude needs your permission to use Bash" ]
}

@test "parity: generic pending backstop drops the broadcast in both" {
  parity_noop '{"hook_event_name":"Notification","cwd":"/home/u/myrepo","message":"Claude needs attention"}' pending
  # Whitespace-padded generic phrase and whitespace-only msg must also drop —
  # both producers compare a TRIMMED copy (msg.trim() / the sed trim).
  parity_noop '{"hook_event_name":"Notification","cwd":"/home/u/myrepo","message":"  Claude needs attention  "}' pending
  parity_noop '{"hook_event_name":"Notification","cwd":"/home/u/myrepo","message":"   "}' pending
}

@test "parity: Stop ending in a question remaps done to pending" {
  # A turn that ends by asking is blocked on input: both producers must remap
  # done → pending and carry ONLY the trailing question line as the msg.
  parity_payloads '{"hook_event_name":"Stop","cwd":"/home/u/myrepo","last_assistant_message":"Refactored the auth module.\n\nShould I also update the tests?"}' done
  [ "$(jq -r '.status' <<<"$BASH_PAYLOAD")" = pending ]
  [ "$(jq -r '.msg' <<<"$BASH_PAYLOAD")" = "Should I also update the tests?" ]
}

@test "parity: Stop ending in a statement stays done" {
  parity_payloads '{"hook_event_name":"Stop","cwd":"/home/u/myrepo","last_assistant_message":"Anything else?\nAll tests pass."}' done
  [ "$(jq -r '.status' <<<"$BASH_PAYLOAD")" = done ]
}

@test "parity: idle clears the message in both producers" {
  # idle is intentionally blank: both producers must agree on status=idle AND
  # an empty msg, even when a stale message rides in on the SessionStart payload.
  parity_payloads '{"hook_event_name":"SessionStart","source":"clear","cwd":"/home/u/myrepo","message":"old work in progress"}' idle
  [ "$(jq -r '.status' <<<"$BASH_PAYLOAD")" = idle ]
  [ "$(jq -r '.msg' <<<"$BASH_PAYLOAD")" = "" ]
}
