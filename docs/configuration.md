# Configuration

zj-radar reads its options from the plugin alias and accepts live updates over
a pipe. For the defaults block, see [Configuration in the
README](../README.md#configuration).

## Options

Options go on the `radar` alias in `~/.config/zellij/config.kdl`. Every key is
optional. This example sets two keys to non-default values:

```kdl
plugins {
    radar location="file:~/.config/zellij/plugins/zj_radar.wasm" {
        density "comfortable"   // default is "cards"
        naming "off"            // default is "managed"
    }
}
```

If `setup zellij` wrote your alias, it sits between `// zj-radar: managed
plugin alias begin`/`end` markers, and a later `setup zellij` rewrites that
region with only `naming "managed"`. Put custom keys on a hand-owned alias
outside the markers, or set them over the `config.v1` pipe below.

Unknown keys are ignored and parsing never fails. An unrecognized value keeps
the field as it was, so a typo never resets a setting.

| Key | Values | Default | Effect |
|-----|--------|---------|--------|
| `density` | `cards` · `comfortable` · `compact` | `cards` | Tinted card bands / blank separators / flush rail. |
| `naming` | `off` · `managed` · `force` | `managed` | Auto-name tabs from the agent's repo or the pane's cwd/title. `managed` never overwrites a name you set by hand; `force` does. See note 1. |
| `header` | `true` · `false` | `true` | Show the ` RADAR` header and tab count. |
| `glyphs` | `plain` · `nerd` | `plain` | Status glyph set. `nerd` needs a Nerd Font. |
| `jump_hint` | `alt-n` · `hidden` | `hidden` | Footer advertises ` alt-[n] jump`. See note 2. |
| `notify` | `true` · `false` | `true` | Master switch for desktop notifications. |
| `notify_done` | `true` · `false` | `true` | Notify on transition to `done`. |
| `notify_error` | `true` · `false` | `true` | Notify on transition to `error`. |
| `notify_pending` | `true` · `false` | `true` | Notify on transition to `pending`. |
| `notify_when_focused` | `true` · `false` | `false` | Also notify for the focused pane. |
| `interactive_commands` | comma/space-separated exe names | *(empty)* | Extra programs to treat as interactive: never a spinning row, only a muted label. Extends the built-in set of editors, pagers, and TUIs. Applies live. See [`activity-model.md`](activity-model.md). |

Notes:

1. The "names I applied" memory lives in plugin memory. After a Zellij server
   restart, previously auto-applied names read as manual, so `managed` leaves
   them alone. Use `force`, or rename the tab back to its default `Tab #N`.
2. Only set `jump_hint` when Alt+digit actually reaches Zellij on your machine.
   Window managers often claim those chords, and macOS terminals type `¡`
   unless option-as-alt is on. `zj-radar run` binds Alt-1..9 but does not set
   this, because it cannot verify the chord arrives.

Three more keys (`role`, `grant_hint`, `defer_permission`) are set by
`zj-radar run`'s generated layouts. Don't set them by hand.

Notification behavior (transitions only, background panes by default, one per
event across tabs) is described in [`using.md`](using.md#notifications).

## Runtime config

Change options without editing the layout by broadcasting a flat JSON object
on the `zj_radar.config.v1` pipe:

```sh
zellij pipe --name zj_radar.config.v1 -- '{"density":"compact","header":false}'
```

## Binding keys to runtime config

The same payload can come from a keybind. Zellij's `MessagePlugin` action
delivers a named pipe message straight to the plugin. Add to
`~/.config/zellij/config.kdl`:

```kdl
keybinds {
    shared_except "locked" {
        // Flush/compact rail
        bind "Alt Shift c" {
            MessagePlugin "radar" {
                name "zj_radar.config.v1"
                payload "{\"density\":\"compact\"}"
            }
        }
        // Roomy cards
        bind "Alt Shift v" {
            MessagePlugin "radar" {
                name "zj_radar.config.v1"
                payload "{\"density\":\"cards\"}"
            }
        }
        // Hide the identity header
        bind "Alt Shift h" {
            MessagePlugin "radar" {
                name "zj_radar.config.v1"
                payload "{\"header\":false}"
            }
        }
    }
}
```

- `"radar"` is the plugin alias your layout uses. Without an alias, use the
  full plugin URL instead.
- `MessagePlugin` reaches every running radar instance (one per tab), so the
  whole session updates at once. If none is running it launches a headless one
  to receive the message, which is harmless.
- `config.v1` sets values; it cannot toggle them. Bind one key per value.

## Binding keys to commands

For imperative actions the plugin accepts `zj_radar.cmd.v1`, whose payload is
a single verb:

```kdl
keybinds {
    shared_except "locked" {
        // Cycle focus to the next tab needing attention (pending / error / done)
        bind "Alt n" {
            MessagePlugin "radar" { name "zj_radar.cmd.v1"; payload "attention-next"; }
        }
        bind "Alt p" {
            MessagePlugin "radar" { name "zj_radar.cmd.v1"; payload "attention-prev"; }
        }
        // Cycle the badge selection across sessions; commits ~1s after the last tap.
        bind "Alt s" {
            MessagePlugin "radar" { name "zj_radar.cmd.v1"; payload "session-next"; }
        }
        bind "Alt Shift s" {
            MessagePlugin "radar" { name "zj_radar.cmd.v1"; payload "session-prev"; }
        }
    }
}
```

- `attention-next` / `attention-prev` walk the tabs whose agents are waiting
  for you, errored, or done, in tab order, wrapping. Running and idle tabs are
  skipped.
- `session-next` / `session-prev` move the cross-session badge's highlight
  through the order the badge renders in (see [Cross-session
  badge](using.md#cross-session-badge)). Nothing switches until about a second
  after the last tap; landing back on your own session cancels. A committed
  switch lands on the target's attention tab if it has one. With only one
  session running these verbs do nothing.

Unknown verbs are ignored. Both pipes are inert until the sidebar has been
granted permissions. Mouse gestures (`✓` acknowledge, `✕` dismiss) are
described in [`using.md`](using.md#mouse).
