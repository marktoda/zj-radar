#!/usr/bin/env bats
load helper

SCRIPT="$BATS_TEST_DIRNAME/../scripts/notify.sh"
CLI="${CARGO_TARGET_DIR:-$BATS_TEST_DIRNAME/../../../target}/debug/zj-radar"

# Run both producers over the same hook JSON + status and assert the ENTIRE
# payloads match, `tasks` aside (CLI-only; key order normalized with jq -S) — msg, task, status, repo,
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
  # Background-task lines (`tasks`) are CLI-only by design: the fallback must
  # never send them, and everything else must match exactly.
  [ "$(jq 'has("tasks")' <<<"$BASH_PAYLOAD")" = false ] || { echo "bash sent tasks"; return 1; }
  [ "$(jq -S 'del(.tasks)' <<<"$BASH_PAYLOAD")" = "$(jq -S 'del(.tasks)' <<<"$RUST_PAYLOAD")" ]
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

setup() {
  setup_fakes
  # FAIL, don't skip: a skipped parity suite is zero weld coverage between the
  # two producers that still reads as a green run.
  [ -x "$CLI" ] || { echo "build the CLI first: cargo build -p zj-radar" >&2; return 1; }
}
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

@test "parity: apply_patch activity" {
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"apply_patch","tool_input":{"patch":"*** Begin Patch"}}' running
  [ "$(jq -r '.msg' <<<"$BASH_PAYLOAD")" = "editing files" ]
}

@test "parity: mcp tool derives 'using <last __ segment>'" {
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"mcp__slack__send_message","tool_input":{}}' running
  [ "$(jq -r '.msg' <<<"$BASH_PAYLOAD")" = "using send_message" ]
}

@test "parity: mcp tool with empty trailing segment falls back to working" {
  # rsplit("__").next() filters an empty segment to None in Rust; both
  # producers must land on the neutral baseline, never "using ".
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"mcp__broken__","tool_input":{}}' running
  [ "$(jq -r '.msg' <<<"$BASH_PAYLOAD")" = "working" ]
}

@test "parity: git pull derives 'syncing'" {
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Bash","tool_input":{"command":"git pull --rebase"}}' running
  [ "$(jq -r '.msg' <<<"$BASH_PAYLOAD")" = "syncing" ]
}

@test "parity: git fetch derives 'syncing'" {
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Bash","tool_input":{"command":"git fetch origin"}}' running
  [ "$(jq -r '.msg' <<<"$BASH_PAYLOAD")" = "syncing" ]
}

@test "parity: build verb derives 'building'" {
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Bash","tool_input":{"command":"cargo build --release"}}' running
  [ "$(jq -r '.msg' <<<"$BASH_PAYLOAD")" = "building" ]
}

@test "parity: compile verb derives 'building'" {
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Bash","tool_input":{"command":"gcc -c compile main.c"}}' running
  [ "$(jq -r '.msg' <<<"$BASH_PAYLOAD")" = "building" ]
}

# The `test` and `install` verbs were pinned only negatively (git-checkout-latest
# isn't a test, npm-uninstall isn't install). Without a positive case the two
# producers could diverge on the verb output with the whole suite green.
@test "parity: test verb activity matches between producers" {
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Bash","tool_input":{"command":"cargo test --workspace"}}' running
}

@test "parity: install verb activity matches between producers" {
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Bash","tool_input":{"command":"pip install requests"}}' running
}

# The bash producer groups Edit|Write|MultiEdit and reads NotebookEdit's
# `notebook_path` (not `file_path`); the Rust side tests all three. Pin the
# two untested file-tool branches so the key path difference stays in sync.
@test "parity: MultiEdit activity" {
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"MultiEdit","tool_input":{"file_path":"/home/u/myrepo/src/auth.rs"}}' running
}

@test "parity: NotebookEdit activity" {
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"NotebookEdit","tool_input":{"notebook_path":"/home/u/myrepo/nb.ipynb"}}' running
}

@test "parity: leading-newline Bash command still derives its first token" {
  # The first token comes from the WHOLE string (Rust split_whitespace().next());
  # a line-based read would see an empty first line and derive nothing.
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Bash","tool_input":{"command":"\n  ls -la"}}' running
  [ "$(jq -r '.msg' <<<"$BASH_PAYLOAD")" = "running ls" ]
}

@test "parity: trailing-slash edit path derives no activity (empty basename)" {
  # basename("src/") is empty — Rust filters it to None; the bash producer must
  # guard the STRIPPED value too, or it broadcasts a dangling "editing ".
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Edit","tool_input":{"file_path":"/home/u/myrepo/src/"}}' running
  [ "$(jq -r '.msg' <<<"$BASH_PAYLOAD")" = "working" ]
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
  # both producers compare a TRIMMED copy (msg.trim() / the trim() helper).
  parity_noop '{"hook_event_name":"Notification","cwd":"/home/u/myrepo","message":"  Claude needs attention  "}' pending
  parity_noop '{"hook_event_name":"Notification","cwd":"/home/u/myrepo","message":"   "}' pending
}

@test "parity: generic phrase outside pending rides through untouched" {
  # The generic-phrase filter is PENDING-ONLY in both producers. A done
  # broadcast whose message happens to be a generic phrase must keep it —
  # an unconditional filter here is the drift this case pins against.
  parity_payloads '{"hook_event_name":"Stop","cwd":"/home/u/myrepo","last_assistant_message":"Claude needs attention"}' done
  [ "$(jq -r '.status' <<<"$BASH_PAYLOAD")" = done ]
  [ "$(jq -r '.msg' <<<"$BASH_PAYLOAD")" = "Claude needs attention" ]
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

@test "parity: Stop with running background work stays running" {
  # Tests backgrounded by the turn are still running: both producers hold the
  # row running with a "waiting on …" msg instead of painting it done.
  parity_payloads '{"hook_event_name":"Stop","cwd":"/home/u/myrepo","last_assistant_message":"started","background_tasks":[{"id":"b1","type":"shell","status":"running","description":"  Run the test suite ","command":"cargo nextest run"}]}' done
  [ "$(jq -r '.status' <<<"$BASH_PAYLOAD")" = running ]
  [ "$(jq -r '.msg' <<<"$BASH_PAYLOAD")" = "waiting on Run the test suite" ]
  parity_payloads '{"hook_event_name":"Stop","cwd":"/home/u/myrepo","last_assistant_message":"started","background_tasks":[{"id":"b1","type":"shell","status":"running","command":"pytest"},{"id":"a1","type":"subagent","status":"running","description":"Explore"}]}' done
  [ "$(jq -r '.msg' <<<"$BASH_PAYLOAD")" = "waiting on 2 tasks" ]
  parity_payloads '{"hook_event_name":"Stop","cwd":"/home/u/myrepo","background_tasks":[{"id":"w1","type":"workflow","status":"running","description":42}]}' done
  [ "$(jq -r '.msg' <<<"$BASH_PAYLOAD")" = "waiting on 1 task" ]
}

@test "parity: services, finished and unknown tasks leave the Stop done" {
  local tasks='[{"id":"b1","type":"shell","status":"running","command":"cd web && PNPM run dev"},{"id":"b2","type":"shell","status":"running","command":"tail -F x.log"},{"id":"b3","type":"shell","status":"running","command":"mkdocs serve"},{"id":"m1","type":"monitor","status":"running"},{"id":"x1","type":"new_kind","status":"running"},{"id":"b4","type":"shell","status":"completed","command":"cargo test"},"junk"]'
  parity_payloads "{\"hook_event_name\":\"Stop\",\"cwd\":\"/home/u/myrepo\",\"last_assistant_message\":\"started\",\"background_tasks\":$tasks}" done
  [ "$(jq -r '.status' <<<"$BASH_PAYLOAD")" = done ]
  local bad
  for bad in '[]' 'null' '"oops"'; do
    parity_payloads "{\"hook_event_name\":\"Stop\",\"cwd\":\"/home/u/myrepo\",\"last_assistant_message\":\"ok\",\"background_tasks\":$bad}" done
    [ "$(jq -r '.status' <<<"$BASH_PAYLOAD")" = done ]
  done
}

@test "parity: a trailing question outranks background work" {
  parity_payloads '{"hook_event_name":"Stop","cwd":"/home/u/myrepo","last_assistant_message":"Tests running.\nBump the version too?","background_tasks":[{"id":"b1","type":"shell","status":"running","command":"cargo test"}]}' done
  [ "$(jq -r '.status' <<<"$BASH_PAYLOAD")" = pending ]
}

@test "cli: every Stop carries the turn-end task snapshot (bash matches everything else)" {
  # The CLI's `tasks` object: labels (description, else command basename),
  # holds, omitted empty label / false holds, dropped non-running / id-less /
  # junk entries. The fallback sends no `tasks` but must agree on the rest
  # (status + the waiting msg).
  local tasks='[{"id":"b1","type":"shell","status":"running","description":"  Run the suite ","command":"cargo nextest run"},{"id":"b2","type":"shell","status":"running","command":"/usr/bin/make -j4"},{"id":"d1","type":"shell","status":"running","command":"cd web && pnpm run dev"},{"id":"a1","type":"subagent","status":"running","description":"Explore"},{"id":"m1","type":"monitor","status":"running"},{"id":"x","type":"shell","status":"completed","command":"true"},{"type":"shell","status":"running"},{"id":7,"type":"shell","status":"running"},"junk"]'
  parity_payloads "{\"hook_event_name\":\"Stop\",\"cwd\":\"/home/u/myrepo\",\"last_assistant_message\":\"started\",\"background_tasks\":$tasks}" done
  [ "$(jq -r '.status' <<<"$RUST_PAYLOAD")" = running ]
  [ "$(jq -r '.msg' <<<"$BASH_PAYLOAD")" = "waiting on 3 tasks" ]
  [ "$(jq -c '[.tasks.items[].id]' <<<"$RUST_PAYLOAD")" = '["b1","b2","d1","a1","m1"]' ]
  # No field at all → an empty snapshot, status unchanged.
  parity_payloads '{"hook_event_name":"Stop","cwd":"/home/u/myrepo","last_assistant_message":"ok"}' done
  [ "$(jq -c '.tasks' <<<"$RUST_PAYLOAD")" = '{"snapshot":true,"items":[]}' ]
  # A question still wins, and still carries the snapshot.
  parity_payloads '{"hook_event_name":"Stop","cwd":"/home/u/myrepo","last_assistant_message":"Ship it?","background_tasks":[{"id":"b1","type":"shell","status":"running","command":"pytest"}]}' done
  [ "$(jq -r '.status' <<<"$BASH_PAYLOAD")" = pending ]
  [ "$(jq -r '.tasks.items[0].id' <<<"$RUST_PAYLOAD")" = b1 ]
}

@test "cli: a background launch reports its start (bash matches everything else)" {
  parity_payloads '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Bash","tool_input":{"command":"sleep 25; echo hi","description":"Background sleep","run_in_background":true},"tool_response":{"stdout":"","backgroundTaskId":"bks7"}}' running
  [ "$(jq -c '.tasks' <<<"$RUST_PAYLOAD")" = '{"snapshot":false,"items":[{"id":"bks7","state":"running","label":"Background sleep","holds":true}]}' ]
  parity_payloads '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Agent","tool_input":{"prompt":"x","description":"Explore rail","run_in_background":true},"tool_response":{"isAsync":true,"status":"async_launched","agentId":"a7c6"}}' running
  [ "$(jq -r '.tasks.items[0].id' <<<"$RUST_PAYLOAD")" = a7c6 ]
  # A dev server launch is a service; an ordinary tool result carries nothing.
  parity_payloads '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Bash","tool_input":{"command":"npm run dev"},"tool_response":{"backgroundTaskId":"d1"}}' running
  [ "$(jq -c '.tasks.items[0] | has("holds")' <<<"$RUST_PAYLOAD")" = false ]
  parity_payloads '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Bash","tool_input":{"command":"ls"},"tool_response":{"stdout":"a"}}' running
  [ "$(jq -c 'has("tasks")' <<<"$RUST_PAYLOAD")" = false ]
  parity_payloads '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Bash","tool_input":{"command":"ls"},"tool_response":"text"}' running
  [ "$(jq -c 'has("tasks")' <<<"$RUST_PAYLOAD")" = false ]
  # A non-object tool_input still reports the start (unlabeled, and not
  # holding: nothing says the work is bounded).
  parity_payloads '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Bash","tool_input":"x","tool_response":{"backgroundTaskId":"b9"}}' running
  [ "$(jq -c '.tasks.items' <<<"$RUST_PAYLOAD")" = '[{"id":"b9","state":"running"}]' ]
}

@test "cli: a task-notification wake reports every outcome block (bash matches everything else)" {
  local prompt='<task-notification>\n<task-id>b9</task-id>\n<status>failed</status>\n<summary>Background command \"x\" failed with exit code 3</summary>\n</task-notification>\n<task-notification>\n<task-id> b3 </task-id>\n<status>completed</status>\n</task-notification>\n<task-notification>\n<task-id>m1</task-id>\n<status>event</status>\n</task-notification>'
  parity_payloads "{\"hook_event_name\":\"UserPromptSubmit\",\"cwd\":\"/home/u/myrepo\",\"prompt\":\"$prompt\"}" running
  [ "$(jq -c '.tasks' <<<"$RUST_PAYLOAD")" = '{"snapshot":false,"items":[{"id":"b9","state":"failed"},{"id":"b3","state":"completed"}]}' ]
  [ "$(jq -r '.task' <<<"$BASH_PAYLOAD")" = "" ]
  # A human prompt mentioning the tag is not a wake.
  parity_payloads '{"hook_event_name":"UserPromptSubmit","cwd":"/home/u/myrepo","prompt":"fix the <task-notification> parser"}' running
  [ "$(jq -c 'has("tasks")' <<<"$RUST_PAYLOAD")" = false ]
}

@test "service word lists are welded between the producers" {
  # agents.rs and notify.sh must list the same service phrases, wrappers,
  # runners, value flags and description phrases, each ERE/Oniguruma-
  # metachar-free but for `.` (notify.sh escapes it in the description regex;
  # the command lists are compared as tokens, never as regex).
  local name rust bash_list p
  for name in SERVICE_PHRASES SERVICE_WRAPPERS SERVICE_RUNNERS SERVICE_VALUE_FLAGS SERVICE_DESCRIPTION_PHRASES; do
    rust="$(sed -n "/ $name: &\[&str\] = &\[/,/^];/p" "$BATS_TEST_DIRNAME/../../../crates/cli/src/agents.rs" \
      | grep -o '"[^"]*"' | tr -d '"' | sort)"
    bash_list="$(grep -m1 "^$name=" "$SCRIPT" | sed "s/^$name=\"//; s/\"\$//" | tr '|' '\n' | sort)"
    [ -n "$rust" ] || { echo "extraction of $name from agents.rs broke"; return 1; }
    echo "$name rust=[$rust]"; echo "$name bash=[$bash_list]"
    [ "$rust" = "$bash_list" ]
    while IFS= read -r p; do
      case "$p" in *[!a-z0-9\ .-]*) echo "phrase [$p] has a regex-unsafe char"; return 1;; esac
    done <<<"$rust"
  done
}

@test "parity: the shared service corpus classifies alike in both producers" {
  # The same lines agents.rs's shell_is_service_matches_the_shared_corpus
  # reads (crates/cli/src/agents/service_cases.rs), each run as a one-shell
  # Stop snapshot through BOTH producers: a service leaves the Stop done, a
  # bounded shell holds it running.
  local corpus="$BATS_TEST_DIRNAME/../../../crates/cli/src/agents/service_cases.rs"
  local cases line expect json want n=0
  cases="$(sed -n '/^pub(crate) const SERVICE_CASES: &str = r#"$/,/^"#;$/p' "$corpus" | sed '1d;$d' \
    | grep -v -e '^$' -e '^#' \
    | jq -Rr 'split("\t") as $f
        | ($f[1] // "" | gsub("\\\\t"; "\t") | gsub("\\\\n"; "\n")) as $c
        | ($f[2] // "") as $d
        | {id: "b1", type: "shell", status: "running"}
          + (if $c != "" then {command: $c} else {} end)
          + (if $d != "" then {description: $d} else {} end)
        | "\($f[0])\t" + ({hook_event_name: "Stop", cwd: "/home/u/myrepo",
                           last_assistant_message: "started", background_tasks: [.]} | tojson)')"
  while IFS= read -r line; do
    expect="${line%%$'\t'*}"
    json="${line#*$'\t'}"
    case "$expect" in
      service) want=done ;;
      bounded) want=running ;;
      *) echo "bad expectation [$expect] in the corpus"; return 1 ;;
    esac
    parity_payloads "$json" done
    [ "$(jq -r '.status' <<<"$BASH_PAYLOAD")" = "$want" ] || { echo "[$json] is not $expect"; return 1; }
    n=$((n + 1))
  done <<<"$cases"
  [ "$n" -gt 100 ] || { echo "corpus extraction broke: $n cases"; return 1; }
}

@test "parity: a held shell is labelled by its description, else its command" {
  parity_payloads '{"hook_event_name":"Stop","cwd":"/home/u/myrepo","background_tasks":[{"id":"b1","type":"shell","status":"running","command":"cargo test -p server","description":"Run the server tests"}]}' done
  [ "$(jq -r '.msg' <<<"$BASH_PAYLOAD")" = "waiting on Run the server tests" ]
  parity_payloads '{"hook_event_name":"Stop","cwd":"/home/u/myrepo","background_tasks":[{"id":"b1","type":"shell","status":"running","description":"Run the suite"}]}' done
  [ "$(jq -r '.msg' <<<"$BASH_PAYLOAD")" = "waiting on Run the suite" ]
  # A blank command is no command: no description either → no hold.
  parity_payloads '{"hook_event_name":"Stop","cwd":"/home/u/myrepo","last_assistant_message":"ok","background_tasks":[{"id":"b1","type":"shell","status":"running","command":" "}]}' done
  [ "$(jq -r '.status' <<<"$BASH_PAYLOAD")" = done ]
}

@test "parity: a subagent's tool hooks report in both (no background launch recorded)" {
  # A foreground subagent's hooks carry `agent_id` and must keep reporting —
  # its PostToolUse is the Pending-recovery edge. (parity cases run with no
  # session, so the CLI has no background-agent record: see the cli case below.)
  parity_case '{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","agent_id":"a55dd","agent_type":"general-purpose","tool_name":"Read","tool_input":{"file_path":"/x/a.rs"}}' running
  [ "$(jq -r '.msg' <<<"$BASH_PAYLOAD")" = "reading a.rs" ]
  parity_case '{"hook_event_name":"SubagentStop","cwd":"/home/u/myrepo","agent_id":"a55dd","agent_type":"general-purpose"}' running
}

@test "parity: a SubagentStop's final report never becomes the running msg" {
  # Only a Stop reads last_assistant_message; a SubagentStop carries the
  # subagent's whole report there.
  parity_payloads '{"hook_event_name":"SubagentStop","cwd":"/home/u/myrepo","agent_id":"a55dd","last_assistant_message":"## Findings\n\n- the rail is fine"}' running
  [ "$(jq -r '.status' <<<"$BASH_PAYLOAD")" = running ]
  [ "$(jq -r '.msg' <<<"$BASH_PAYLOAD")" = working ]
}
@test "cli: a background subagent's own tool hooks send nothing (bash fallback diverges)" {
  # CLI-only by design (notify.sh documents it next to the tasks note): the
  # CLI leaves a marker per async_launched agentId per (session, pane) and
  # drops that agent's Pre/PostToolUse and SubagentStop; the stateless
  # fallback reports them.
  export ZELLIJ_SESSION_NAME="bats-session" XDG_RUNTIME_DIR="$FAKEBIN/state" TMPDIR="$FAKEBIN/state"
  mkdir -p "$FAKEBIN/state"
  local launch='{"hook_event_name":"PostToolUse","cwd":"/tmp","tool_name":"Agent","tool_input":{"run_in_background":true},"tool_response":{"isAsync":true,"status":"async_launched","agentId":"a1139"}}'
  local bg='{"hook_event_name":"PreToolUse","cwd":"/tmp","agent_id":"a1139","tool_name":"Read","tool_input":{"file_path":"/x/a.rs"}}'
  local fg='{"hook_event_name":"PreToolUse","cwd":"/tmp","agent_id":"a0000","tool_name":"Read","tool_input":{"file_path":"/x/a.rs"}}'
  echo "$launch" | "$CLI" notify claude --status running
  rm -f "$RECORD"
  echo "$bg" | "$CLI" notify claude --status running
  [ ! -s "$RECORD" ] || { echo "cli reported a background agent's hook"; return 1; }
  echo "$fg" | "$CLI" notify claude --status running
  [ "$(last_payload | jq -r '.msg')" = "reading a.rs" ]
  rm -f "$RECORD"
  echo "$bg" | "$SCRIPT" running
  [ "$(last_payload | jq -r '.msg')" = "reading a.rs" ]
  # One marker file per id, under the per-user state dir.
  local markers=("$FAKEBIN"/state/zj-radar-dedup-*/bg-agents.bats-session.7.a1139)
  [ -f "${markers[0]}" ] || { echo "no marker written under the test state dir"; return 1; }
  # A dry run neither consults nor touches the markers: it prints the payload.
  run bash -c "echo '$bg' | '$CLI' notify claude --status running --dry-run"
  [[ "$output" == *"reading a.rs"* ]]
  # Its SubagentStop is dropped too (a plain running over "waiting on …")
  # and removes the marker; the id is then unknown and its hooks report.
  rm -f "$RECORD"
  echo '{"hook_event_name":"SubagentStop","cwd":"/tmp","agent_id":"a1139"}' | "$CLI" notify claude --status running
  [ ! -s "$RECORD" ] || { echo "cli reported a background agent's SubagentStop"; return 1; }
  [ ! -e "${markers[0]}" ]
  echo "$bg" | "$CLI" notify claude --status running
  [ -s "$RECORD" ]
}

@test "parity: Agent tool reads as delegating" {
  parity_case '{"hook_event_name":"PreToolUse","cwd":"/home/u/myrepo","tool_name":"Agent","tool_input":{"prompt":"x"}}' running
  [ "$(jq -r '.msg' <<<"$BASH_PAYLOAD")" = delegating ]
}

@test "parity: idle clears the message in both producers" {
  # idle is intentionally blank: both producers must agree on status=idle AND
  # an empty msg, even when a stale message rides in on the SessionStart payload.
  parity_payloads '{"hook_event_name":"SessionStart","source":"clear","cwd":"/home/u/myrepo","message":"old work in progress"}' idle
  [ "$(jq -r '.status' <<<"$BASH_PAYLOAD")" = idle ]
  [ "$(jq -r '.msg' <<<"$BASH_PAYLOAD")" = "" ]
}
