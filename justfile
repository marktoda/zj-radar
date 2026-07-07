# Deterministic suite (L1-L4): host target, fail on snapshot drift in CI.
test:
    cargo test --all-features

# Bash hook + installer tests (requires bats + shellcheck + jq on PATH).
# Builds the CLI first: parity.bats compares the bash producer against
# target/debug/zj-radar. Covers every shipped shell script: notify.sh (the
# Claude producer hook), install.sh (the curl|sh release asset — see
# scripts/tests/install.bats), and funnel.sh (the fresh-machine CI check).
test-bash:
    cargo build -p zj-radar
    shellcheck plugins/zj-radar-claude/scripts/notify.sh scripts/install.sh scripts/funnel.sh
    bats plugins/zj-radar-claude/tests scripts/tests

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
# uses; ZJ_RADAR_WASM force-loads the just-built plugin.
#
# ALWAYS a fresh session, NEVER a killer: each run launches a uniquely-named
# `zj-radar-dev-<hhmmss>` session, because attaching to a leftover would
# silently keep running the PREVIOUS wasm (a session loads its plugin at
# launch; a new artifact on disk never hot-swaps in). The cleanup line sweeps
# only *exited* `zj-radar-dev-*` leftovers — no fixed names, no --force — so
# it can never touch a live session, including one of yours that happens to
# be named `zj-radar-dev` from the old dev/run.sh days. Run from a plain
# terminal (`run` refuses to nest inside Zellij). The sandbox under
# target/dev/data survives across runs so the grant doesn't re-prompt;
# `rm -rf target/dev/data` for a true first-run experience.
dev:
    cargo build --release --target wasm32-wasip1 -p zj-radar-plugin
    cargo build -p zj-radar
    -zellij list-sessions --short --no-formatting 2>/dev/null | grep '^zj-radar-dev-' | xargs -I{} zellij delete-session {} 2>/dev/null
    ZJ_RADAR_DATA_DIR="{{justfile_directory()}}/target/dev/data" \
    ZJ_RADAR_WASM="{{justfile_directory()}}/target/wasm32-wasip1/release/zj_radar.wasm" \
    ./target/debug/zj-radar run "zj-radar-dev-$(date +%H%M%S)"

# Build the dev artifacts without launching (point an existing session's
# layout at the fresh wasm, or drive the CLI by hand).
dev-build:
    cargo build --release --target wasm32-wasip1 -p zj-radar-plugin
    cargo build -p zj-radar
    @echo "cli:  target/debug/zj-radar"
    @echo "wasm: target/wasm32-wasip1/release/zj_radar.wasm"

# Wasm plugin compile check (matches CI's "wasm plugin compiles" step, so a
# wasm-glue-only breakage fails locally too, not just in CI).
build-wasm:
    cargo build --target wasm32-wasip1 -p zj-radar-plugin

# Everything a PR must pass locally.
ci: test clippy build-wasm test-bash
