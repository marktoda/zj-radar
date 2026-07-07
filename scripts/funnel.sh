#!/usr/bin/env bash
# The fresh-machine funnel test: run the README quickstart verbatim on a
# pristine box and assert a stranger's first five minutes actually work.
#
#   installer -> setup zellij --download --inject --yes -> zellij
#
# Asserts: the release assets install, the doctor is healthy (producer is the
# only expected gap), the rail renders GRANTED on first launch (no permission
# prompt — the pre-seeded grant), and smart tab naming follows a `cd`.
#
# Expects a pristine environment (a fresh ubuntu:24.04 container; CI runs it
# that way) — it installs into $HOME and /usr/local/bin. ZJ_RADAR_VERSION pins
# the release tag under test (default: latest); the same pin flows into
# install.sh and `setup zellij --download`, so a just-pushed tag is testable
# before (or regardless of) what `latest` points at.
set -uo pipefail

ZELLIJ_VERSION="v0.44.3"
fail=0
check() { # check <ok-message> <cmd...>
    local msg="$1"; shift
    if "$@"; then echo "PASS: $msg"; else echo "FAIL: $msg"; fail=1; fi
}

export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null
apt-get install -y -qq curl ca-certificates python3 >/dev/null 2>&1

case "$(uname -m)" in
    aarch64|arm64) zellij_triple="aarch64-unknown-linux-musl" ;;
    x86_64)        zellij_triple="x86_64-unknown-linux-musl" ;;
    *) echo "FAIL: unsupported arch $(uname -m)"; exit 1 ;;
esac
curl -LsSf "https://github.com/zellij-org/zellij/releases/download/${ZELLIJ_VERSION}/zellij-${zellij_triple}.tar.gz" \
    | tar xz -C /usr/local/bin

# ── README step 1: the installer ─────────────────────────────────────────────
if [ -n "${ZJ_RADAR_VERSION:-}" ]; then
    installer_url="https://github.com/marktoda/zj-radar/releases/download/${ZJ_RADAR_VERSION}/install.sh"
else
    installer_url="https://github.com/marktoda/zj-radar/releases/latest/download/install.sh"
fi
curl --proto '=https' --tlsv1.2 -LsSf "$installer_url" | sh
export PATH="$HOME/.local/bin:$PATH"
installed="$(zj-radar --version 2>/dev/null)"
echo "installed: ${installed:-NONE}"
if [ -n "${ZJ_RADAR_VERSION:-}" ]; then
    check "installed CLI matches ${ZJ_RADAR_VERSION}" [ "zj-radar ${ZJ_RADAR_VERSION#v}" = "$installed" ]
else
    check "CLI installed" [ -n "$installed" ]
fi

# ── README step 2: sidebar install (consented non-interactively) ─────────────
zj-radar setup zellij --download --inject --yes
doctor="$(zj-radar setup zellij --check 2>&1)"; echo "$doctor"
check "doctor: grant ok"  grep -q 'ok grant'  <<<"$doctor"
check "doctor: layout ok" grep -q 'ok layout' <<<"$doctor"
check "doctor: producer is the only missing item" \
    bash -c '! grep -E "missing (grant|wasm|alias|layout|zellij)" <<<"$1"' _ "$doctor"

# ── README step 3: launch — rail must come up live, naming must follow cd ────
printf 'show_release_notes false\nshow_startup_tips false\n' >> "$HOME/.config/zellij/config.kdl"
python3 "$(dirname "$0")/drive_zellij.py"

check "rail rendered (RADAR header)" grep -aq 'RADAR' /tmp/typescript
check "no native permission prompt"  bash -c "! grep -aq 'Allow?' /tmp/typescript"
check "not parked pre-grant"         bash -c "! grep -aq 'needs permission' /tmp/typescript"

names=""
for _ in $(seq 1 20); do
    names="$(zellij --session funnel action query-tab-names 2>/dev/null)"
    case "$names" in *srv*) break ;; esac
    sleep 1
done
check "smart naming followed cd (want srv, got: ${names:-none})" \
    bash -c 'case "$1" in *srv*) exit 0 ;; *) exit 1 ;; esac' _ "$names"

zellij kill-session funnel >/dev/null 2>&1
[ "$fail" = 0 ] && echo "FUNNEL: all checks passed" || echo "FUNNEL: FAILURES above"
exit "$fail"
