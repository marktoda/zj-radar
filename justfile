# Deterministic suite (L1-L4): host target, fail on snapshot drift in CI.
test:
    cargo test --all-features

# Bash hook tests (requires bats + shellcheck + jq on PATH).
test-bash:
    shellcheck plugins/zj-radar-claude/scripts/notify.sh
    bats plugins/zj-radar-claude/tests

# Live E2E (L5): builds the wasm plugin, drives a real Zellij in a PTY.
# `--test-threads=1` is REQUIRED: each test spawns its own Zellij session, and
# running them in parallel makes sessions contend at startup so the plugin
# header times out intermittently (harness.rs `wait_until_ready`). Serial is
# the reliable mode — a flaky E2E suite is worse than none.
test-e2e:
    cargo build --release --target wasm32-wasip1
    cargo test --features e2e --test e2e -- --include-ignored --test-threads=1

# Review/accept snapshot changes after intentional render edits.
review:
    cargo insta review

# Everything a PR must pass locally.
ci: test test-bash
