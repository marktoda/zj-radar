#!/usr/bin/env bash
# Shared bats setup: a temp dir of fake binaries on PATH that record calls.

setup_fakes() {
  FAKEBIN="$(mktemp -d)"
  RECORD="$FAKEBIN/zellij.log"
  # The fake zellij records: "<all args>\t<stdin>" on each call.
  # notify.sh passes the JSON payload as a positional arg after `--`, so
  # last_payload must extract it from field 1 (the args), not field 2 (stdin).
  cat >"$FAKEBIN/zellij" <<EOF
#!/usr/bin/env bash
stdin="\$(cat 2>/dev/null || true)"
printf '%s\t%s\n' "\$*" "\$stdin" >> "$RECORD"
exit 0
EOF
  cat >"$FAKEBIN/git" <<'EOF'
#!/usr/bin/env bash
case "$1 $2" in
  'rev-parse --show-toplevel') echo /home/u/myrepo;;
  'branch --show-current') echo main;;
  *) exit 0;;
esac
EOF
  chmod +x "$FAKEBIN/zellij" "$FAKEBIN/git"
  # Keep the REAL jq (we want real JSON building); only fake zellij/git.
  # A real zj-radar on PATH would make notify.sh `exec zj-radar notify …` and
  # bypass the fake zellij recorder — every $RECORD assertion would then pass
  # vacuously. Dogfooding machines DO have one installed, so don't refuse: drop
  # any PATH dir that resolves it, keeping every other real tool available.
  local orig_path="$PATH" filtered="" dir
  local IFS=':'
  for dir in $PATH; do
    [[ -z "$dir" || -x "$dir/zj-radar" ]] && continue
    filtered="${filtered:+$filtered:}$dir"
  done
  unset IFS
  export PATH="$FAKEBIN:$filtered"
  # A filtered dir may have ALSO held a tool the suite needs (e.g. jq next to
  # zj-radar) — re-link any that went missing into the fake bin.
  local tool src
  for tool in jq timeout; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      src="$(PATH="$orig_path" command -v "$tool" 2>/dev/null || true)"
      [[ -n "$src" && "$src" != */zj-radar ]] && ln -s "$src" "$FAKEBIN/$tool"
    fi
  done
  # Belt-and-braces: if one is somehow still resolvable, fail loudly rather
  # than let the suite go vacuous.
  if command -v zj-radar >/dev/null 2>&1; then
    echo "ERROR: a real zj-radar is still on PATH; bats must test the bash fallback" >&2
    return 1
  fi
  export ZELLIJ=1 ZELLIJ_PANE_ID=terminal_7
}

teardown_fakes() { rm -rf "$FAKEBIN"; }

# Extract the JSON payload from the last zellij call.
# notify.sh invokes: zellij pipe --name zj_radar.status.v1 -- "$payload"
# The payload is passed as a positional arg (after --), so it appears in
# field 1 of the tab-separated log record.  We extract everything after "-- ".
# The script backgrounds the zellij call with `&`, so we poll for the record to
# appear. The budget is generous (up to ~3s) because under load — e.g. `just ci`
# runs this right after cargo test+clippy — the backgrounded pipe can take well
# over the old 0.5s, which surfaced as a flaky empty `$output` and a jq parse
# error downstream. We return the instant the record is written, so a fast run
# pays nothing for the larger ceiling. Callers that expect NO broadcast assert on
# `[ ! -s "$RECORD" ]` directly and never call this.
last_payload() {
  local i
  for i in $(seq 1 30); do
    [ -s "$RECORD" ] && break
    sleep 0.1
  done
  tail -n1 "$RECORD" | cut -f1 | sed 's/.*-- //'
}
