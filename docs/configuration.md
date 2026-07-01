# Configuration

zj-radar reads its options from the plugin alias and accepts live updates over a
pipe. For a minimal example, see [Configuration in the README](../README.md#configuration).

## Options

With the recommended alias setup, options go on the `radar` alias in
`~/.config/zellij/config.kdl`. Every key is optional and takes the default from
the table below when omitted; the block below shows two keys set to **non-default**
values purely to illustrate the syntax:

```kdl
plugins {
    radar location="file:~/.config/zellij/plugins/zj_radar.wasm" {
        density "comfortable"   // default is "cards"
        naming "off"            // default is "managed"
    }
}
```

Layouts should continue to reference `plugin location="radar"`. Unknown keys are
ignored and invalid values fall back to the default (parsing never fails):

| Key | Values | Default | Effect |
|-----|--------|---------|--------|
| `density` | `cards` · `comfortable` · `compact` | `cards` | Card surface bands / blank separators / flush rail. |
| `naming` | `off` · `managed` · `force` | `managed` | Auto-rename tabs from agent repo / pane title. `managed` only touches default or self-applied names; `force` overrides manual names. |
| `header` | `true` · `false` | `true` | Show the ` RADAR` identity header + tab count. |
| `glyphs` | `plain` · `nerd` | `plain` | Status glyph set (`nerd` needs a Nerd Font). |
| `notify` | `true` · `false` | `true` | Master switch for OS desktop notifications (macOS `osascript`, Linux `notify-send`). |
| `notify_done` | `true` · `false` | `true` | Notify when a pane transitions into `done`. |
| `notify_error` | `true` · `false` | `true` | Notify when a pane transitions into `error`. |
| `notify_pending` | `true` · `false` | `true` | Notify when a pane transitions into `pending` (needs input). |
| `notify_when_focused` | `true` · `false` | `false` | When `false`, suppress notifications for the focused pane (background panes only). |

Notifications fire only on transitions **into** an attention status and, by
default, only for **background** panes — the focused pane is suppressed unless
`notify_when_focused` is `true`. Delivery is best-effort: the plugin runs
`osascript` on macOS, else `notify-send` (libnotify) on Linux; if neither is on
`PATH` it is a silent no-op. This is why the plugin requests Zellij's
`RunCommands` permission — solely to hand the notification to the OS.

## Runtime config

These can also be changed **at runtime** without editing the layout, by
broadcasting a flat JSON object on the `zj_radar.config.v1` pipe:

```sh
zellij pipe --name zj_radar.config.v1 -- '{"density":"compact","header":false}'
```

## Binding keys to runtime config

The same payload can be driven from a keybind — no shell, no `zellij pipe`
subprocess. Zellij's `MessagePlugin` action delivers a named pipe message
straight to the plugin's `pipe()` entrypoint, exactly like the broadcast above.
Add bindings to `~/.config/zellij/config.kdl`:

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

Notes:

- `"radar"` is the same plugin alias your layout uses (`plugin location="radar"`).
  If you have no alias, use the full URL instead, e.g.
  `MessagePlugin "https://github.com/.../zj_radar.wasm" { … }`.
- `MessagePlugin` broadcasts to **every** running radar instance (the sidebar is
  one instance per tab), so the whole session re-renders at once — which is what
  you want for a config change. If no instance is running it launches a headless
  one to receive the message, which is harmless.
- The `config.v1` pipe only **sets** a value; it can't *toggle* one (the payload
  is stateless). So bind one key per discrete value, as above. A future
  imperative command pipe could add `toggle`/`cycle` verbs.

## Binding keys to commands

`config.v1` only *sets* state. For *imperative* actions — like jumping to the
next agent that needs you — the plugin also accepts `zj_radar.cmd.v1`, whose
payload is a single bare verb string:

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
    }
}
```

`attention-next` / `attention-prev` walk the tabs whose agents are *waiting for
you*, *errored*, or *done* — in tab order, wrapping around — and switch focus to
each. Tabs that are merely *running* or *idle* are skipped. Repeated presses
sweep every attention tab and cycle. Like every command pipe, an unknown verb is
ignored, and the action is inert until the sidebar has been granted permissions.
