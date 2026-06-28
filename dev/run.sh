#!/usr/bin/env bash
# Single dev entrypoint: build the debug wasm and open a fresh disposable dev session.
set -euo pipefail

root="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
cd "$root"

target="wasm32-wasip1"
artifact_rel="target/$target/debug/zj_radar.wasm"
artifact_abs="$root/$artifact_rel"
layout_src="$root/dev/dev.kdl"
layout_gen="$root/target/dev/dev.kdl"
session="${ZJ_RADAR_DEV_SESSION:-zj-radar-dev}"
next_session="${session}-next"
mode="${1:-start}"

usage() {
    cat <<'EOF'
usage: ./dev/run.sh [--build-only|--dry-run|--fresh-session]

Builds the debug wasm and opens or reloads the Zellij dev session.

From a normal terminal, this starts/restarts the disposable dev session. From
inside Zellij, it reloads existing zj-radar sidebar panes in the current session.

Use --fresh-session from inside Zellij to switch to a fresh disposable dev
session instead of reloading the current session.
EOF
}

target_has_std() {
    local libdir

    libdir="$(rustc --print target-libdir --target "$target" 2>/dev/null)" || return 1
    [[ -d "$libdir" ]] && compgen -G "$libdir/libstd-*.rlib" >/dev/null
}

build_wasm() {
    if target_has_std; then
        cargo build --target "$target"
        return
    fi

    if command -v nix >/dev/null 2>&1 && [[ "${ZJ_RADAR_DEV_NO_NIX:-}" != "1" ]]; then
        echo "dev: $target std is missing from the current Rust toolchain; building via nix develop" >&2
        nix develop -c cargo build --target "$target"
        return
    fi

    echo "dev: $target std is missing from the current Rust toolchain" >&2
    echo "dev: run in 'nix develop', or install it with 'rustup target add $target'" >&2
    cargo build --target "$target"
}

generate_layout() {
    mkdir -p "$(dirname "$layout_gen")"
    sed "s|file:$artifact_rel|file:$artifact_abs|g" "$layout_src" > "$layout_gen"
    echo "layout: $layout_gen"
}

session_exists() {
    zellij list-sessions --short --no-formatting 2>/dev/null | grep -Fxq "$1"
}

require_jq() {
    if ! command -v jq >/dev/null 2>&1; then
        cat >&2 <<'EOF'
dev: jq is required to reload the current Zellij session
dev: enter `nix develop`, or install jq on PATH
EOF
        exit 2
    fi
}

current_tab_id() {
    zellij action current-tab-info | awk '
        $1 == "id:" { print $2; found = 1; exit }
        END { if (!found) exit 1 }
    '
}

current_focus_pane() {
    local panes_json="$1"
    local tab_id="$2"

    jq -r --argjson tab_id "$tab_id" '
        [
            .[]
            | select(.tab_id == $tab_id and .is_focused == true and .is_selectable == true)
        ]
        | first
        | if . == null then
            ""
          elif .is_plugin then
            "plugin_" + (.id | tostring)
          else
            "terminal_" + (.id | tostring)
          end
    ' <<<"$panes_json"
}

radar_panes() {
    local panes_json="$1"
    local radar_url_abs="file:$artifact_abs"
    local radar_url_rel="file:$artifact_rel"

    jq -r --arg abs "$radar_url_abs" --arg rel "$radar_url_rel" '
        .[]
        | select(
            .is_plugin == true
            and .is_floating == false
            and (.plugin_url == $abs or .plugin_url == $rel)
        )
        | [.tab_id, ("plugin_" + (.id | tostring)), .tab_name]
        | @tsv
    ' <<<"$panes_json"
}

delete_session_if_exists() {
    local target_session="$1"

    if session_exists "$target_session"; then
        echo "dev: deleting existing Zellij session '$target_session'" >&2
        zellij delete-session "$target_session" --force
    fi
}

restart_from_terminal() {
    delete_session_if_exists "$session"

    echo "dev: starting Zellij session '$session'" >&2
    exec zellij --session "$session" --new-session-with-layout "$layout_gen"
}

current_zellij_session_name() {
    if [[ -z "${ZELLIJ_SESSION_NAME:-}" ]]; then
        cat >&2 <<'EOF'
dev: ZELLIJ is set but ZELLIJ_SESSION_NAME is missing
dev: cannot safely choose a disposable target session from inside Zellij
EOF
        exit 2
    fi

    printf '%s\n' "$ZELLIJ_SESSION_NAME"
}

switch_target_session() {
    local current_session="$1"

    case "$current_session" in
        "$session")
            printf '%s\n' "$next_session"
            ;;
        *)
            printf '%s\n' "$session"
            ;;
    esac
}

switch_from_zellij() {
    local current_session
    local target_session

    current_session="$(current_zellij_session_name)"
    target_session="$(switch_target_session "$current_session")"
    if [[ "$target_session" == "$current_session" ]]; then
        echo "dev: refusing to replace the current Zellij session '$target_session'" >&2
        exit 2
    fi

    delete_session_if_exists "$target_session"

    echo "dev: switching current Zellij client to fresh session '$target_session'" >&2
    zellij action switch-session --layout "$layout_gen" --cwd "$root" "$target_session"
}

restore_focus() {
    local tab_id="${1:-}"
    local pane_id="${2:-}"

    if [[ -n "$tab_id" ]]; then
        zellij action go-to-tab-by-id "$tab_id" >/dev/null 2>&1 || true
    fi
    if [[ -n "$pane_id" ]]; then
        zellij action focus-pane-id "$pane_id" >/dev/null 2>&1 || true
    fi
}

reload_current_session() {
    local panes_json
    local original_tab_id
    local original_pane_id
    local rows
    local row
    local tab_id
    local pane_id
    local tab_name
    local radar_url_abs="file:$artifact_abs"

    require_jq
    panes_json="$(zellij action list-panes --json --all --geometry --state --tab)"
    original_tab_id="$(current_tab_id)"
    original_pane_id="$(current_focus_pane "$panes_json" "$original_tab_id")"
    mapfile -t rows < <(radar_panes "$panes_json")

    if ((${#rows[@]} == 0)); then
        cat >&2 <<EOF
dev: no zj-radar sidebar panes found in the current Zellij session
dev: run ./dev/run.sh --fresh-session to open a disposable dev session
EOF
        exit 2
    fi

    trap 'restore_focus "$original_tab_id" "$original_pane_id"' RETURN

    for row in "${rows[@]}"; do
        IFS=$'\t' read -r tab_id pane_id tab_name <<<"$row"
        echo "dev: reloading $pane_id in tab '$tab_name'" >&2
        zellij action go-to-tab-by-id "$tab_id"
        zellij action focus-pane-id "$pane_id"
        zellij action launch-plugin \
            --in-place \
            --close-replaced-pane \
            --skip-plugin-cache \
            --configuration naming=force \
            "$radar_url_abs" >/dev/null
    done

    trap - RETURN
    restore_focus "$original_tab_id" "$original_pane_id"
    echo "dev: reloaded ${#rows[@]} zj-radar sidebar pane(s)" >&2
}

dry_run_current_session_reload() {
    local panes_json
    local rows
    local row
    local tab_id
    local pane_id
    local tab_name

    require_jq
    panes_json="$(zellij action list-panes --json --all --geometry --state --tab)"
    mapfile -t rows < <(radar_panes "$panes_json")

    if ((${#rows[@]} == 0)); then
        echo "dev: dry run; found no zj-radar sidebar panes in the current Zellij session"
        return
    fi

    echo "dev: dry run; would reload ${#rows[@]} zj-radar sidebar pane(s):"
    for row in "${rows[@]}"; do
        IFS=$'\t' read -r tab_id pane_id tab_name <<<"$row"
        echo "dev:   tab '$tab_name' ($tab_id): $pane_id"
    done
}

open_or_reload_session() {
    if [[ -n "${ZELLIJ:-}" ]]; then
        reload_current_session
    else
        restart_from_terminal
    fi
}

open_fresh_session() {
    if [[ -n "${ZELLIJ:-}" ]]; then
        switch_from_zellij
    else
        restart_from_terminal
    fi
}

case "$mode" in
    start)
        ;;
    --build-only)
        mode="build-only"
        ;;
    --dry-run)
        mode="dry-run"
        ;;
    --fresh-session)
        mode="fresh-session"
        ;;
    -h|--help)
        usage
        exit 0
        ;;
    *)
        usage >&2
        exit 2
        ;;
esac

case "$mode" in
    build-only)
        build_wasm
        echo "built: $artifact_rel"
        generate_layout
        ;;
    dry-run)
        generate_layout
        if [[ -n "${ZELLIJ:-}" ]]; then
            dry_run_current_session_reload
        else
            echo "dev: dry run; would restart Zellij session '$session'"
        fi
        ;;
    start)
        build_wasm
        echo "built: $artifact_rel"
        generate_layout
        open_or_reload_session
        ;;
    fresh-session)
        build_wasm
        echo "built: $artifact_rel"
        generate_layout
        open_fresh_session
        ;;
esac
