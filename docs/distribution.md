# Distribution: making the agent notifiers easy to install

**Problem:** today wiring up the notifiers means hand-editing several files
(`~/.claude/settings.json`, `~/.codex/config.toml`, shell scripts, Nix). That's
fine for Mark's own machine; it's a non-starter for anyone else adopting this.
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

2. **Every other agent** (Codex, Gemini, Aider, opencode, …) — no plugin-hooks
   system, so we need a small **`zj-radar setup` installer** that idempotently
   writes each agent's own config to call **one universal notifier**, modeled on
   `cmux hooks setup`.

Plus a third, separate surface: **the Zellij plugin itself** (the wasm + its
permission grant + the layout). That's its own install path (see §4).

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
  `Format` (`TomlNotify` | `JsonNested` | `JsonFlat` | `Yaml` | `PluginFile`),
  the events, and a unique **marker** string. Adding an agent = one row.
- **Detection = two gates:** config dir exists *and* `command -v <binary>`
  succeeds → install; else skip and report. On `--uninstall`, bypass gates to
  clean stale config even after the agent is gone.
- **Idempotent edit = "strip-own-then-re-add":** parse the config, remove only
  entries containing our marker (preserving all user entries), re-add ours,
  pretty-print + compare to detect a no-op, **atomic write**. Refuse to write if
  the file exists but doesn't parse. Per format: `serde_json` (Claude/Gemini/
  Cursor/Codex-hooks.json), `toml_edit` (Codex `config.toml` `notify`, format-
  preserving), `serde_yaml` (Aider), or a marker-tagged template file for
  agents that only support JS/TS plugins (opencode, amp, pi).
- **Clean uninstall:** remove only marker-tagged entries, collapse emptied
  containers, delete plugin files only if our marker is present; back up first.
- **Safety UX:** per-file diff preview + `Proceed? [y/N]`, `--yes` for scripts,
  `--dry-run`, atomic writes, and a per-agent disable env var so a user can mute
  one agent without uninstalling.

Per-agent specifics (verified):
| Agent | File | Insert | Event → status |
|---|---|---|---|
| Codex | `~/.codex/config.toml` `notify` | `notify = ["zj-radar","notify","codex"]` (top-level, before tables) | `agent-turn-complete` → done |
| Gemini (≥0.26) | `~/.gemini/settings.json` | `hooks.AfterAgent` + `hooks.Notification` | AfterAgent → done |
| Aider | `.aider.conf.yml` | `notifications-command` (no payload — bake pane id into the cmd) | done/waiting |
| opencode / amp / pi | a `*.ts`/`*.js` plugin file (no command-hook config) | marker-tagged template | session.idle / agent.end → done |

## 3. One universal notifier (not per-agent scripts)

Both Cmux and code-notify converge here: **a single entrypoint every agent
calls** — `zj-radar notify <agent> [event]` — which figures out the event from
its argv subcommand + the JSON the agent passes (Codex: JSON on argv; Claude/
Gemini: type on argv + JSON on stdin; Aider: bare). It maps all of them to one
`zellij pipe --name zj_radar.status.v1` broadcast and **no-ops when not running
under Zellij** (gate on `$ZELLIJ`). This collapses our two shell scripts into one
code path and keeps the "wire up with one command" promise.

**Form factor:** a small native **`zj-radar` CLI binary** (Rust) with
subcommands `notify`, `setup`, `setup --uninstall`. Native = no `jq`/`bash`
runtime deps, cross-platform, easy to vendor. It ships alongside the wasm plugin.
(Interim: the current bash scripts work for Mark; the binary is the productized
form.)

## 4. The Zellij-plugin side of install (separate but needed)

Installing the *sidebar* itself still needs: the `.wasm` at a **stable path**
(not a per-rebuild Nix store path, so the permission grant sticks), the layout
edit (a `radar` plugin **alias** in `config.kdl` plus matching
`default_tab_template` **and** `new_tab_template` — see the README "Wire it into
your layout"), and the first-run **permission grant** (selectable-until-granted
in-code + optional `permissions.kdl` pre-seed). A `zj-radar init` could: copy the
wasm to `~/.config/zellij/plugins/`, add the `radar` alias to `config.kdl`, print
**both** template snippets to paste (or patch a layout), and pre-seed the
permission. Document the one-time grant for the manual path.

## 5. Recommended rollout

1. **Phase 1 (biggest bang, least work):** package the Claude hooks as a **Claude
   Code plugin** (§1). Most users are on Claude Code; this delivers the
   "no-config install" they asked for immediately, reusing our notify logic.
2. **Phase 2:** the **`zj-radar` CLI** with the universal `notify` + the
   `setup`/`--uninstall` installer for Codex/Gemini/Aider (§2–3).
3. **Phase 3:** `zj-radar init` for the sidebar wasm + layout + permission (§4),
   and a README documenting all of it.

Net new-user story we're aiming for:
```
# Claude users:
/plugin install zj-radar-claude@zj-radar
# Other agents:
zj-radar setup            # detects installed agents, wires them up idempotently
# The sidebar:
zj-radar init             # installs the wasm + layout snippet + permission
```

---

### Sources
- Claude Code plugins/hooks: https://code.claude.com/docs/en/plugins-reference ,
  /plugins , /settings , /hooks-guide , /cli-reference
- Cmux installer (firsthand): `CLI/CMUXCLI+AgentHookDefinitions.swift`,
  `CLI/cmux.swift` (`runSetupHooks`, `installAgentHooks`, `uninstallAgentHooks`,
  `codexConfigTomlInstallingHooksFeature`) — github.com/manaflow-ai/cmux
- code-notify (firsthand): `lib/code-notify/{utils/detect.sh,core/config.sh,
  core/notifier.sh}` — github.com/mylee04/code-notify
- Codex notify/hooks: developers.openai.com/codex/config-reference , /hooks
- Gemini CLI hooks: github.com/google-gemini/gemini-cli/blob/main/docs/hooks/reference.md
- Aider notifications: aider.chat/docs/usage/notifications.html
- opencode plugins: opencode.ai/docs/plugins/
