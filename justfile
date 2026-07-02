# Deterministic suite (L1-L4): host target, fail on snapshot drift in CI.
test:
    cargo test --all-features

# Bash hook tests (requires bats + shellcheck + jq on PATH). Builds the CLI
# first: parity.bats compares the bash producer against target/debug/zj-radar.
test-bash:
    cargo build -p zj-radar
    shellcheck plugins/zj-radar-claude/scripts/notify.sh
    bats plugins/zj-radar-claude/tests

# Live E2E (L5): builds the wasm plugin, drives a real Zellij in a PTY.
# `--test-threads=1` is REQUIRED: each test spawns its own Zellij session, and
# running them in parallel makes sessions contend at startup so the plugin
# header times out intermittently (harness.rs `wait_until_ready`). Serial is
# the reliable mode — a flaky E2E suite is worse than none.
test-e2e:
    cargo build --release --target wasm32-wasip1 -p zj-radar-plugin
    cargo test --features e2e --test e2e -- --include-ignored --test-threads=1

# Lint the whole workspace; warnings are errors (matches CI).
clippy:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Review/accept snapshot changes after intentional render edits.
review:
    cargo insta review

# Disposable dev session with the FRESH LOCAL build — wasm and CLI both from
# this checkout, fully isolated from an installed zj-radar: ZJ_RADAR_DATA_DIR
# sandboxes the run-owned config/wasm under target/dev, so it never touches
# (or downloads over) the tagged release assets your daily `zj-radar run`
# uses; ZJ_RADAR_WASM force-loads the just-built plugin. Run from a plain
# terminal (`run` refuses to nest inside Zellij); the session is named
# zj-radar-dev so it sits alongside your real sessions in the session list.
# `rm -rf target/dev/data` resets the sandbox (config + grant onboarding).
dev:
    cargo build --release --target wasm32-wasip1 -p zj-radar-plugin
    cargo build -p zj-radar
    ZJ_RADAR_DATA_DIR="{{justfile_directory()}}/target/dev/data" \
    ZJ_RADAR_WASM="{{justfile_directory()}}/target/wasm32-wasip1/release/zj_radar.wasm" \
    ./target/debug/zj-radar run zj-radar-dev

# Fresh dev iteration: discard the previous zj-radar-dev session (your real
# sessions — and the agents in them — are untouched) and relaunch on the new
# build. The sandbox under target/dev/data survives, so the grant doesn't
# re-prompt; `rm -rf target/dev/data` too for a true first-run experience.
dev-fresh:
    -zellij delete-session zj-radar-dev --force
    just dev

# Build the dev artifacts without launching (point an existing session's
# layout at the fresh wasm, or drive the CLI by hand).
dev-build:
    cargo build --release --target wasm32-wasip1 -p zj-radar-plugin
    cargo build -p zj-radar
    @echo "cli:  target/debug/zj-radar"
    @echo "wasm: target/wasm32-wasip1/release/zj_radar.wasm"

# Everything a PR must pass locally.
ci: test clippy test-bash
