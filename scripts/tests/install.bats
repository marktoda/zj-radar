#!/usr/bin/env bats
# Tests for scripts/install.sh — the curl|sh installer published as a release
# asset. The script is pure functions behind a `main "$@"` tail; sourcing it
# with ZJ_RADAR_INSTALL_TEST=1 (the test seam) loads the functions without
# running main, so each test can stub the environment-facing pieces (uname,
# download) and exercise the rest for real: URL construction, sha256
# verification, tar extraction, install placement.

SCRIPT="$BATS_TEST_DIRNAME/../install.sh"

setup() {
  TMP="$(mktemp -d)"
}

teardown() {
  rm -rf "$TMP"
}

# Portable sha256 of $1, matching the tools install.sh itself probes for.
hash_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

# Source install.sh under the test seam in a fresh shell, then run $1.
# Stubs (uname/download/try_download as shell functions) go inside the
# snippet, after the source, so they shadow the real commands for the call.
run_installer() {
  ZJ_RADAR_INSTALL_TEST=1 bash -c ". '$SCRIPT'; $1"
}

# Fake a machine: $1/$2 become `uname -s`/`uname -m`.
fake_uname() {
  printf 'uname() { case "$1" in -s) echo %s;; -m) echo %s;; esac; }' "$1" "$2"
}

# ── detect_target: uname → release-asset triple ──────────────────────────────

@test "detect_target: Linux x86_64 → x86_64-unknown-linux-musl" {
  run run_installer "$(fake_uname Linux x86_64); detect_target"
  [ "$status" -eq 0 ]
  [ "$output" = "x86_64-unknown-linux-musl" ]
}

@test "detect_target: Linux aarch64 → aarch64-unknown-linux-musl" {
  run run_installer "$(fake_uname Linux aarch64); detect_target"
  [ "$status" -eq 0 ]
  [ "$output" = "aarch64-unknown-linux-musl" ]
}

@test "detect_target: Darwin arm64 → aarch64-apple-darwin" {
  run run_installer "$(fake_uname Darwin arm64); detect_target"
  [ "$status" -eq 0 ]
  [ "$output" = "aarch64-apple-darwin" ]
}

@test "detect_target: Intel macOS refuses with a cargo-install pointer" {
  run run_installer "$(fake_uname Darwin x86_64); detect_target"
  [ "$status" -ne 0 ]
  [[ "$output" == *"cargo install zj-radar"* ]]
}

@test "detect_target: unsupported OS names itself in the error" {
  run run_installer "$(fake_uname FreeBSD amd64); detect_target"
  [ "$status" -ne 0 ]
  [[ "$output" == *"unsupported OS 'FreeBSD'"* ]]
}

@test "detect_target: unsupported arch names itself in the error" {
  run run_installer "$(fake_uname Linux riscv64); detect_target"
  [ "$status" -ne 0 ]
  [[ "$output" == *"unsupported architecture 'riscv64'"* ]]
}

# ── asset_url: version pinning ───────────────────────────────────────────────

@test "asset_url: default version uses the /latest/download/ redirect" {
  run run_installer "asset_url x86_64-unknown-linux-musl"
  [ "$status" -eq 0 ]
  [ "$output" = "https://github.com/marktoda/zj-radar/releases/latest/download/zj-radar-x86_64-unknown-linux-musl.tar.gz" ]
}

@test "asset_url: ZJ_RADAR_VERSION pins the tag path" {
  ZJ_RADAR_VERSION=v9.9.9 run run_installer "asset_url aarch64-apple-darwin"
  [ "$status" -eq 0 ]
  [ "$output" = "https://github.com/marktoda/zj-radar/releases/download/v9.9.9/zj-radar-aarch64-apple-darwin.tar.gz" ]
}

# ── verify_sha256: the integrity gate ────────────────────────────────────────

@test "verify_sha256: accepts a matching digest" {
  echo "payload" > "$TMP/f"
  printf '%s  f\n' "$(hash_of "$TMP/f")" > "$TMP/f.sha256"
  run run_installer "verify_sha256 '$TMP/f' '$TMP/f.sha256'"
  [ "$status" -eq 0 ]
  [[ "$output" == *"checksum verified"* ]]
}

@test "verify_sha256: refuses on mismatch" {
  echo "payload" > "$TMP/f"
  printf '%s  f\n' "$(printf 'a%.0s' $(seq 64))" > "$TMP/f.sha256"
  run run_installer "verify_sha256 '$TMP/f' '$TMP/f.sha256'"
  [ "$status" -ne 0 ]
  [[ "$output" == *"refusing to install"* ]]
}

@test "verify_sha256: unreadable sidecar downgrades to a warning" {
  echo "payload" > "$TMP/f"
  : > "$TMP/f.sha256"
  run run_installer "verify_sha256 '$TMP/f' '$TMP/f.sha256'"
  [ "$status" -eq 0 ]
  [[ "$output" == *"skipping integrity check"* ]]
}

# ── main, end to end with stubbed downloads ──────────────────────────────────
# Everything but the network is real: tar, checksum tooling, install/cp.

make_release_fixture() {
  mkdir -p "$TMP/stage"
  printf '#!/bin/sh\necho fake-zj-radar\n' > "$TMP/stage/zj-radar"
  chmod +x "$TMP/stage/zj-radar"
  tar -czf "$TMP/asset.tar.gz" -C "$TMP/stage" zj-radar
  printf '%s  zj-radar.tar.gz\n' "$(hash_of "$TMP/asset.tar.gz")" > "$TMP/asset.tar.gz.sha256"
}

# Stubs pointing main's two fetches at the fixture files.
STUB_DOWNLOADS='download() { cp "$FIXTURE_TGZ" "$2"; }; try_download() { cp "$FIXTURE_SHA" "$2"; }'

@test "main: installs a verified binary into ZJ_RADAR_BIN_DIR" {
  make_release_fixture
  export FIXTURE_TGZ="$TMP/asset.tar.gz" FIXTURE_SHA="$TMP/asset.tar.gz.sha256"
  ZJ_RADAR_BIN_DIR="$TMP/bin" run run_installer "$(fake_uname Linux x86_64); $STUB_DOWNLOADS; main"
  [ "$status" -eq 0 ]
  [[ "$output" == *"checksum verified"* ]]
  [[ "$output" == *"installed to $TMP/bin/zj-radar"* ]]
  [ -x "$TMP/bin/zj-radar" ]
  [ "$("$TMP/bin/zj-radar")" = "fake-zj-radar" ]
}

@test "main: refuses a tarball that fails checksum and installs nothing" {
  make_release_fixture
  printf '%s  zj-radar.tar.gz\n' "$(printf 'b%.0s' $(seq 64))" > "$TMP/asset.tar.gz.sha256"
  export FIXTURE_TGZ="$TMP/asset.tar.gz" FIXTURE_SHA="$TMP/asset.tar.gz.sha256"
  ZJ_RADAR_BIN_DIR="$TMP/bin" run run_installer "$(fake_uname Linux x86_64); $STUB_DOWNLOADS; main"
  [ "$status" -ne 0 ]
  [[ "$output" == *"refusing to install"* ]]
  [ ! -e "$TMP/bin/zj-radar" ]
}

@test "main: missing checksum sidecar warns but installs (TLS floor)" {
  make_release_fixture
  export FIXTURE_TGZ="$TMP/asset.tar.gz"
  ZJ_RADAR_BIN_DIR="$TMP/bin" run run_installer \
    "$(fake_uname Linux x86_64); download() { cp \"\$FIXTURE_TGZ\" \"\$2\"; }; try_download() { return 1; }; main"
  [ "$status" -eq 0 ]
  [[ "$output" == *"no checksum published"* ]]
  [ -x "$TMP/bin/zj-radar" ]
}

@test "main: archive without a zj-radar binary is rejected" {
  mkdir -p "$TMP/stage"
  echo junk > "$TMP/stage/README"
  tar -czf "$TMP/asset.tar.gz" -C "$TMP/stage" README
  printf '%s  zj-radar.tar.gz\n' "$(hash_of "$TMP/asset.tar.gz")" > "$TMP/asset.tar.gz.sha256"
  export FIXTURE_TGZ="$TMP/asset.tar.gz" FIXTURE_SHA="$TMP/asset.tar.gz.sha256"
  ZJ_RADAR_BIN_DIR="$TMP/bin" run run_installer "$(fake_uname Linux x86_64); $STUB_DOWNLOADS; main"
  [ "$status" -ne 0 ]
  [[ "$output" == *"did not contain a zj-radar binary"* ]]
  [ ! -e "$TMP/bin/zj-radar" ]
}

@test "executing install.sh with the test seam unset still reaches main" {
  # Guard the guard: if the seam ever defaults wrong, curl|sh would silently
  # no-op. An empty PATH-probe environment makes main fail fast at `need`,
  # proving it RAN (a no-op would exit 0 with no output).
  run bash -c "PATH=/nonexistent '$SCRIPT'"
  [ "$status" -ne 0 ]
  [[ "$output" == *"required command not found"* ]]
}
