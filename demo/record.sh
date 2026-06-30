#!/usr/bin/env bash
# Regenerate the zj-radar demo assets (docs/media/*) with vhs.
#
#   ./demo/record.sh            # build debug wasm + record the hero GIF
#   ./demo/record.sh --release  # use the release wasm (smaller, slower build)
#
# Requires: vhs, zellij, ffmpeg, ttyd on PATH, and a Nerd Font
# (JetBrainsMono Nerd Font) installed. gifsicle is used to optimize the GIF if
# present. The tape/layout/config under demo/ are templates; this script writes
# concrete copies (with absolute paths) to target/demo/ and records from there.
set -euo pipefail

root="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
cd "$root"

profile="debug"
cargo_flags=()
if [[ "${1:-}" == "--release" ]]; then
    profile="release"
    cargo_flags=(--release)
fi
wasm="$root/target/wasm32-wasip1/$profile/zj_radar.wasm"

for bin in vhs zellij; do
    command -v "$bin" >/dev/null 2>&1 || { echo "record.sh: '$bin' not found on PATH" >&2; exit 1; }
done

echo "==> building $profile wasm"
if rustup target list --installed 2>/dev/null | grep -q wasm32-wasip1; then
    cargo build "${cargo_flags[@]}" --target wasm32-wasip1 -p zj-radar-plugin
elif command -v nix >/dev/null 2>&1; then
    echo "    (wasm32-wasip1 target missing; building via nix)"
    nix build .#zj-radar -L && wasm="$root/result/bin/zj_radar.wasm"
else
    echo "record.sh: install the wasm32-wasip1 target (rustup target add wasm32-wasip1) or nix" >&2
    exit 1
fi

gen="$root/target/demo"
mkdir -p "$gen" "$root/docs/media"
chmod +x "$root/demo/agent.sh" "$root/demo/banner.sh"

# Pre-grant the sidebar's permissions so the recording shows no prompt. Zellij
# keys grants by the plugin's filesystem path (no `file:` prefix) in
# permissions.kdl; seed our wasm path if it isn't already granted.
case "$(uname)" in
    Darwin) zcache="$HOME/Library/Caches/org.Zellij-Contributors.Zellij" ;;
    *)      zcache="${XDG_CACHE_HOME:-$HOME/.cache}/zellij" ;;
esac
perm="$zcache/permissions.kdl"
mkdir -p "$zcache"
if ! { [ -f "$perm" ] && grep -qF "\"$wasm\"" "$perm"; }; then
    printf '\n"%s" {\n    ReadApplicationState\n    ChangeApplicationState\n    ReadCliPipes\n}\n' "$wasm" >>"$perm"
    echo "==> seeded permission grant for $wasm"
fi

subst() { # <src-template> <dest> — fill the absolute-path placeholders
    sed -e "s#__ROOT__#$root#g" \
        -e "s#__WASM__#$wasm#g" \
        -e "s#__CFG__#$gen/config.kdl#g" \
        -e "s#__LAYOUT__#$gen/layout.kdl#g" \
        "$1" >"$2"
}
subst "$root/demo/config.kdl" "$gen/config.kdl"
subst "$root/demo/layout.kdl" "$gen/layout.kdl"
subst "$root/demo/hero.tape"  "$gen/hero.tape"

echo "==> recording (vhs)"
# Strip inherited Zellij env so the Zellij we launch inside vhs starts a fresh
# session instead of panicking on a nested one (this script is often run from
# inside a Zellij session).
unset ZELLIJ ZELLIJ_SESSION_NAME ZELLIJ_PANE_ID
vhs "$gen/hero.tape"

gif="$root/docs/media/hero.gif"
if command -v gifsicle >/dev/null 2>&1 && [[ -f "$gif" ]]; then
    echo "==> optimizing $gif"
    gifsicle -O3 --lossy=80 -o "$gif" "$gif"
fi

echo "==> done"
[[ -f "$gif" ]] && ls -lh "$gif"
