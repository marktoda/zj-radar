# zj-agents-claude

A Claude Code **plugin** that broadcasts agent status (working / waiting / done)
to the [zj-agents](../../) Zellij sidebar — with **no `settings.json` editing**.
Installing the plugin auto-registers the hooks; uninstalling removes them cleanly.

## Install

One-time (from the zj-agents marketplace repo):

```
/plugin marketplace add mark-toda/zj-agents
/plugin install zj-agents-claude@zj-agents
```

Or scriptable / local dev (no marketplace):

```
claude plugin install zj-agents-claude@zj-agents      # after adding the marketplace
claude --plugin-dir /path/to/zj-agents/plugins/zj-agents-claude   # session-only
```

## What it does

Registers these hooks (all calling the bundled `scripts/notify.sh`):

| Hook | Sidebar status |
|------|----------------|
| `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `SubagentStop` | `running` |
| `Notification` (permission / idle / input) | `pending` |
| `Stop` | `done` (clears when you focus the tab) |

Each fires a `zellij pipe --name zj_agents.status.v1` broadcast. It is a **no-op
outside Zellij**, so it's safe to leave enabled everywhere.

Requires `jq` and `git` on PATH (used to parse the payload and derive
repo/branch). The forthcoming native `zj-agents notify` binary will drop the `jq`
dependency.

## Uninstall

```
/plugin uninstall zj-agents-claude@zj-agents
```

Hooks are removed automatically — nothing to clean up by hand.
