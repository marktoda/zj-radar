#!/bin/sh
# zj-radar CLI installer.
#
#   curl --proto '=https' --tlsv1.2 -LsSf \
#     https://github.com/marktoda/zj-radar/releases/latest/download/install.sh | sh
#
# Downloads the prebuilt `zj-radar` binary matching this machine's OS/arch from a
# GitHub release and installs it. Pure POSIX sh. The whole script is defined as
# functions and only invoked by the final `main "$@"` line, so a truncated
# download (the classic curl|sh hazard) can never execute a partial script.
#
# Environment overrides:
#   ZJ_RADAR_VERSION   release tag to install (e.g. v0.1.2). Default: latest.
#   ZJ_RADAR_BIN_DIR   install directory. Default: ~/.local/bin.
#   ZJ_RADAR_REPO      owner/repo to download from. Default: marktoda/zj-radar.

set -eu

REPO="${ZJ_RADAR_REPO:-marktoda/zj-radar}"
VERSION="${ZJ_RADAR_VERSION:-latest}"
BIN_DIR="${ZJ_RADAR_BIN_DIR:-$HOME/.local/bin}"

say()  { printf 'zj-radar: %s\n' "$1"; }
err()  { printf 'zj-radar: error: %s\n' "$1" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || err "required command not found: $1"; }

# Map `uname` to a Rust target triple matching the release asset names.
detect_target() {
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os" in
    Linux)  suffix="unknown-linux-musl" ;;
    Darwin) suffix="apple-darwin" ;;
    *) err "unsupported OS '$os' (zj-radar ships Linux and macOS binaries; build from source for others)" ;;
  esac
  case "$arch" in
    x86_64|amd64)  cpu="x86_64" ;;
    arm64|aarch64) cpu="aarch64" ;;
    *) err "unsupported architecture '$arch'" ;;
  esac
  # Intel macOS has no prebuilt binary (Apple Silicon only); fall back to source.
  if [ "$os" = Darwin ] && [ "$cpu" = x86_64 ]; then
    err "no prebuilt binary for Intel (x86_64) macOS — install from source instead: cargo install zj-radar"
  fi
  printf '%s-%s' "$cpu" "$suffix"
}

# Build the release asset URL for a target triple.
asset_url() {
  triple="$1"
  asset="zj-radar-${triple}.tar.gz"
  if [ "$VERSION" = "latest" ]; then
    printf 'https://github.com/%s/releases/latest/download/%s' "$REPO" "$asset"
  else
    printf 'https://github.com/%s/releases/download/%s/%s' "$REPO" "$VERSION" "$asset"
  fi
}

# Download $1 to $2 over HTTPS only (redirects included), failing hard on HTTP errors.
download() {
  url="$1"; dest="$2"
  if command -v curl >/dev/null 2>&1; then
    curl --proto '=https' --proto-redir '=https' --tlsv1.2 -fL "$url" -o "$dest"
  elif command -v wget >/dev/null 2>&1; then
    wget --https-only -O "$dest" "$url"
  else
    err "need curl or wget to download"
  fi
}

# Best-effort quiet fetch of an OPTIONAL asset (the checksum sidecar): returns
# nonzero instead of erroring, since releases predating checksums lack it.
try_download() {
  url="$1"; dest="$2"
  if command -v curl >/dev/null 2>&1; then
    curl --proto '=https' --proto-redir '=https' --tlsv1.2 -fsSL "$url" -o "$dest" 2>/dev/null
  elif command -v wget >/dev/null 2>&1; then
    wget --https-only -qO "$dest" "$url" 2>/dev/null
  else
    return 1
  fi
}

# Verify $1 against its sha256 sidecar file $2 (sha256sum "<hex>  <name>" form).
# Strict on mismatch — a tarball that doesn't match its published digest must
# never be installed. A missing local sha256 tool downgrades to a warning,
# matching `zj-radar setup zellij --download`'s policy (TLS stays the floor).
verify_sha256() {
  file="$1"; sidecar="$2"
  expected=""
  read -r expected _ < "$sidecar" || true
  [ -n "$expected" ] || { say "warning: checksum file unreadable; skipping integrity check"; return 0; }
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$file")"
  elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$file")"
  else
    say "warning: no sha256 tool (sha256sum/shasum) on PATH; skipping integrity check"
    return 0
  fi
  actual="${actual%% *}" # keep the leading hex, drop the filename column
  [ "$actual" = "$expected" ] \
    || err "checksum mismatch for $file (expected $expected, got $actual) — refusing to install"
  say "checksum verified"
}

main() {
  need uname
  need tar
  need mktemp

  triple="$(detect_target)"
  url="$(asset_url "$triple")"
  say "installing $triple from $url"

  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT INT TERM

  if ! download "$url" "$tmp/zj-radar.tar.gz"; then
    err "download failed — is $VERSION released for $triple? See https://github.com/$REPO/releases"
  fi

  if try_download "$url.sha256" "$tmp/zj-radar.tar.gz.sha256"; then
    verify_sha256 "$tmp/zj-radar.tar.gz" "$tmp/zj-radar.tar.gz.sha256"
  else
    say "warning: no checksum published for this release; installing TLS-verified only"
  fi

  tar -xzf "$tmp/zj-radar.tar.gz" -C "$tmp"
  [ -f "$tmp/zj-radar" ] || err "archive did not contain a zj-radar binary"

  mkdir -p "$BIN_DIR"
  install -m 0755 "$tmp/zj-radar" "$BIN_DIR/zj-radar" 2>/dev/null \
    || { cp "$tmp/zj-radar" "$BIN_DIR/zj-radar" && chmod 0755 "$BIN_DIR/zj-radar"; }

  say "installed to $BIN_DIR/zj-radar"

  case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) say "note: $BIN_DIR is not on your PATH — add it, e.g.:"
       # shellcheck disable=SC2016  # $PATH is intentionally literal in this hint
       printf '\n  export PATH="%s:$PATH"\n\n' "$BIN_DIR" ;;
  esac

  say "next: 'zj-radar setup zellij --download' to install the sidebar, then 'zj-radar setup' to wire Codex (Claude Code: install the plugin from inside Claude — see the README)"
}

# Test seam: scripts/tests/install.bats sources this file with
# ZJ_RADAR_INSTALL_TEST=1 to exercise the functions above in isolation
# (stubbing uname/download). A curl|sh run falls through to main, and the
# guard keeps the truncation property: cut anywhere above, nothing executes.
if [ "${ZJ_RADAR_INSTALL_TEST:-0}" != 1 ]; then
  main "$@"
fi
