# Distribution: making the agent notifiers easy to install

> **Status: shipped — historical.** The conclusions here landed as the turnkey
> `zj-radar run` command and the zero-config Claude Code producer plugin (no
> `settings.json` editing). Kept for the rationale behind those choices; for how
> to *use* the result, see [`install.md`](install.md) and the README, not this memo.

**Problem:** today wiring up the notifiers means hand-editing several files
(`~/.claude/settings.json`, `~/.codex/hooks.json`, shell scripts, Nix). That's
workable for a single developer; it's a non-starter for anyone else adopting this.
We want **install-once, no manual config, cleanly removable.**

This memo is grounded in how Cmux (`cmux hooks setup`, 17 agents) and code-notify
do it, plus the Claude Code plugin/hooks docs. Sources at the bottom.

---

## The big realization

There are **two install surfaces**, and they have *different* best answers:

1. **Claude Code** — has a first-class **plugin system that can bundle hooks**.
   Shipping a Claude Code plugin = the user runs one install command, the hooks
   auto-register, and uninstall removes them cleanly. **Zero editing of their
   settings.json.** This is the single highest-leverage win (Claude is the
   primary agent).

2. **Codex and other non-Claude agents** — Codex has first-class lifecycle hooks
   but not a marketplace-style installer for this project, so `zj-radar setup`
   manages `~/.codex/hooks.json` directly. Other agents still use the same
   installer pattern against their native config/plugin surfaces.

Plus a third, separate surface: **the Zellij plugin itself** (the wasm + its
permission grant + the layout). That's now handled by `zj-radar setup zellij`
plus an explicit layout snippet (see §4).

---

## 1. Claude Code → ship a Claude Code *plugin* (recommended, do this first)

A Claude Code plugin can bundle hooks in `hooks/hooks.json` that register
automatically when the plugin is enabled — no settings.json mutation, clean
uninstall, and hook commands can reference bundled scripts via
`${CLAUDE_PLUGIN_ROOT}`.

Plugin layout:
```
zj-radar-claude/
├── .claude-plugin/
│   └── plugin.json            # name, version, description
├── hooks/
│   └── hooks.json             # the Stop/Notification/UserPromptSubmit/... hooks
└── scripts/
    └── notify.sh              # the broadcaster (our current claude-zellij-notify logic)
```
`hooks/hooks.json` (commands call the bundled script):
```json
{
  "hooks": {
    "UserPromptSubmit": [{ "hooks": [{ "type": "command",
      "command": "\"${CLAUDE_PLUGIN_ROOT}\"/scripts/notify.sh running" }] }],
    "Notification": [{ "hooks": [{ "type": "command",
      "command": "\"${CLAUDE_PLUGIN_ROOT}\"/scripts/notify.sh pending" }] }],
    "Stop": [{ "hooks": [{ "type": "command",
      "command": "\"${CLAUDE_PLUGIN_ROOT}\"/scripts/notify.sh done" }] }]
  }
}
```

Install (any of):
- one-line marketplace: `/plugin marketplace add <gh-org/zj-radar>` then
  `/plugin install zj-radar-claude@…`,
- scriptable: `claude plugin install zj-radar-claude@…`,
- no-marketplace/dev: `claude --plugin-dir ./zj-radar-claude` (session-only).

Uninstall: `claude plugin uninstall …` — hooks vanish, no file surgery. Set
`"defaultEnabled": false` in the manifest if we want it opt-in.

**Our existing `claude-zellij-notify.sh` becomes this plugin's bundled
`scripts/notify.sh`** — the logic we just wrote is reused, not thrown away.

## 2. Other agents → a `zj-radar setup` installer (Cmux model)

Mirror Cmux/code-notify's proven shape:

- **Declarative agent table** — one entry per agent: binary name (for PATH
  detection), config dir (+ env override like `CODEX_HOME`), config file, a
  `Format` (`CodexHooksJson` | `TomlNotifyLegacy` | `JsonNested` | `JsonFlat` |
  `Yaml` | `PluginFile`),
  the events, and a unique **marker** string. Adding an agent = one row.
- **Detection = two gates:** config dir exists *and* `command -v <binary>`
  succeeds → install; else skip and report. On `--uninstall`, bypass gates to
  clean stale config even after the agent is gone.
- **Idempotent edit = "strip-own-then-re-add":** parse the config, remove only
  entries containing our marker (preserving all user entries), re-add ours,
  pretty-print + compare to detect a no-op, **atomic write**. Refuse to write if
  the file exists but doesn't parse. Per format: `serde_json` for Codex
  `hooks.json` and other JSON hook configs, `toml_edit` for legacy Codex
  `config.toml` `notify`, and equivalent format-native editors for any future
  agent's config.
- **Clean uninstall:** remove only marker-tagged entries, collapse emptied
  containers, delete plugin files only if our marker is present; back up first.
- **Safety UX:** per-file diff preview + `Proceed? [y/N]`, `--yes` for scripts,
  `--dry-run`, atomic writes, and a per-agent disable env var so a user can mute
  one agent without uninstalling.

Shipped agent wiring:
| Agent | File | Insert | Event → status |
|---|---|---|---|
| Codex | `~/.codex/hooks.json` | command hooks with `ZJ_RADAR_CODEX_HOOK=v1 zj-radar notify codex` marker | `UserPromptSubmit` / tool hooks / subagents → running; `PermissionRequest` → pending; `Stop` → done |
| Codex legacy | `~/.codex/config.toml` `notify` | `notify = ["zj-radar","notify","codex"]` only with `--legacy-notify` | `agent-turn-complete` → done |

Other agents follow the same strip-own-then-re-add installer pattern against
their native hook/plugin config; none beyond Codex (and the Claude Code plugin
in §1) ship today.

## 3. One universal notifier (not per-agent scripts)

Both Cmux and code-notify converge here: **a single entrypoint every agent
calls** — `zj-radar notify <agent> [event]` — which figures out the event from
its argv subcommand + the JSON the agent passes (Codex hooks: JSON on stdin;
Codex legacy notify: JSON on argv; Claude: type on argv + JSON on stdin).
It maps all of them to one
`zellij pipe --name zj_radar.status.v1` broadcast and **no-ops when not running
under Zellij** (gate on `$ZELLIJ`). This collapses our two shell scripts into one
code path and keeps the "wire up with one command" promise.

**Form factor:** a small native **`zj-radar` CLI binary** (Rust) with
subcommands `notify`, `setup`, `setup --check`, and `setup --uninstall`. Native =
no `jq`/`bash` runtime deps, cross-platform, easy to vendor. It ships alongside
the wasm plugin.

## 4. The Zellij-plugin side of install (separate but needed)

Installing the *sidebar* itself needs three things:

1. the `.wasm` at a **stable path** (`~/.config/zellij/plugins/zj_radar.wasm`)
   so Zellij's permission grant sticks,
2. a `radar` plugin **alias** in `~/.config/zellij/config.kdl`, and
3. matching `default_tab_template` **and** `new_tab_template` blocks in the
   user's layout.

`zj-radar setup zellij --wasm <path>` now owns the first two: it copies the wasm,
adds or updates a marker-managed alias in `config.kdl`, and prints the layout
snippet. It deliberately leaves layouts user-owned because real Zellij layouts
vary too much to patch blindly. A future layout patcher can build on the same
snippet, but it should be opt-in and previewable.

The first-run **permission grant** remains a Zellij prompt. The plugin stays
selectable only while the prompt is pending; because the sidebar is instantiated
once per tab, per-tab prompt coordination elects one instance to request the
uncached grant and peers reuse Zellij's cached answer.

## 5. Recommended rollout

1. **Phase 1 (biggest bang, least work):** package the Claude hooks as a **Claude
   Code plugin** (§1). Most users are on Claude Code; this delivers the
   "no-config install" they asked for immediately, reusing our notify logic.
2. **Phase 2:** the **`zj-radar` CLI** with the universal `notify`,
   hook-first Codex setup, `setup --check`, native CLI release artifacts, and
   `setup zellij --wasm` for the sidebar alias (§2–4).
3. **Phase 3:** add more agents and, if still useful, a previewable layout
   patcher. The current setup command should keep printing the snippet rather
   than silently rewriting layouts.

Net new-user story we're aiming for:
```
# Claude users:
/plugin install zj-radar-claude@zj-radar
# Other agents:
zj-radar setup            # detects installed agents, wires them up idempotently
zj-radar setup --check    # verifies PATH/config/hook state
# Codex users then run /hooks once inside Codex to trust the command hook
# The sidebar:
zj-radar setup zellij --wasm target/wasm32-wasip1/release/zj_radar.wasm
# prompts to inject into the default layout, or use --inject to skip the prompt
```

---

### Sources
- Claude Code plugins/hooks: https://code.claude.com/docs/en/plugins-reference
- Cmux installer: https://github.com/manaflow-ai/cmux
- code-notify: https://github.com/mylee04/code-notify
- Codex notify/hooks: https://developers.openai.com/codex/config-reference
