# zj-radar-claude

A Claude Code **plugin** that broadcasts agent status (working / waiting / done)
to the [zj-radar](../../) Zellij sidebar — with **no `settings.json` editing**.
Installing the plugin auto-registers the hooks; uninstalling removes them cleanly.

This plugin only sends status. Install the Zellij sidebar first with the
[main install guide](../../docs/install.md), then add this producer.

## Install

One-time (from the zj-radar marketplace repo):

```
/plugin marketplace add marktoda/zj-radar
/plugin install zj-radar-claude@zj-radar
```

Or scriptable / local dev (no marketplace):

```
claude plugin install zj-radar-claude@zj-radar      # after adding the marketplace
claude --plugin-dir /path/to/zj-radar/plugins/zj-radar-claude   # session-only
```

## What it does

Registers these hooks (all calling the bundled `scripts/notify.sh`):

| Hook | Sidebar status |
|------|----------------|
| `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `SubagentStop` | `running` |
| `Notification` (`permission_prompt` / `elicitation_dialog` matchers) | `pending` |
| `Stop` | `done` (clears when the pane returns to its shell prompt, or on the next broadcast) |
| `SessionStart` (`matcher: clear` only) | `idle` (resets the row on `/clear`) |
| `SessionEnd` | `idle` (clears the row when the Claude session exits) |

Each fires a `zellij pipe --name zj_radar.status.v1` broadcast. It is a **no-op
outside Zellij**, so it's safe to leave enabled everywhere.

`notify.sh` is a POSIX `sh` dispatcher. If the native
[`zj-radar`](../../docs/install.md) CLI is on PATH it hands the hook straight
to it (`exec zj-radar notify claude`), which needs neither `jq` nor `bash`
and skips a repeated `running` broadcast producer-side (see
[producers](../../docs/producers.md#claude-code)). Only when the binary is
absent does it re-enter itself under `bash` and run the bundled fallback,
which needs `bash`, `jq`, and `git` on PATH (to parse the payload and derive
repo/branch).

## Uninstall

```
/plugin uninstall zj-radar-claude@zj-radar
```

Hooks are removed automatically — nothing to clean up by hand.
