# Deterministic suite (L1-L4): host target, fail on snapshot drift in CI.
test:
    cargo test --all-features

# Bash hook tests (requires bats + shellcheck + jq on PATH).
test-bash:
    shellcheck plugins/zj-radar-claude/scripts/notify.sh
    bats plugins/zj-radar-claude/tests

# Live E2E (L5): builds the wasm plugin, drives a real Zellij in a PTY.
test-e2e:
    cargo build --release --target wasm32-wasip1
    cargo test --features e2e --test e2e -- --include-ignored

# Review/accept snapshot changes after intentional render edits.
review:
    cargo insta review

# Everything a PR must pass locally.
ci: test test-bash
