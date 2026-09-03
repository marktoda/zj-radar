# Troubleshooting

The sidebar lives in your tab templates and runs once per tab, which meets a
few sharp edges in Zellij itself. Each entry is symptom, cause, fix.

## Sidebar shows "needs permission" (or looks blank)

**Symptom:** the rail shows ` RADAR` over an orange `⚠ needs permission` line,
or one rail pane is blank, and tab naming does not work either.

**Why:** the plugin is parked at Zellij's permission prompt. It paints that
face but receives no events, so everything looks dead. The blank case is
Zellij drawing its own y/n prompt into one plugin pane, where at rail width it
is unreadable ([zellij #4749](https://github.com/zellij-org/zellij/issues/4749)).
You get here when the install-time grant was skipped or went stale: a declined
consent prompt, an older CLI, a `permissions.kdl` the installer refused to
edit, a partial grant from an older zj-radar (Zellij ignores an entry that
does not cover every requested permission), an alias pointing at a
`/nix/store/` path, or no resolvable Zellij cache directory.

**Fix:** `zj-radar setup zellij --check`; a non-`ok` `grant` item confirms it.
Then one of:

- in a `zj-radar run` session, press **Ctrl-y** to open the grant prompt in a
  legible floating pane;
- `zj-radar setup zellij -y` to pre-authorize, then open a new tab or restart
  Zellij;
- from inside the session, `zj-radar setup zellij --grant` and press `y` in
  the floating pane it opens;
- focus the blank pane and press `y` blind.

A rail that is blank because Zellij is older than 0.44.3 fails to load the
wasm at all; no grant will wake it.

## Sidebar renders, but no status ever appears

**Symptom:** the rail draws your tabs, but agents work and finish invisibly.

The rail and the status feed are separate installs, so a rendering rail says
nothing about the [producer](producers.md). In order:

1. **`zj-radar setup zellij --check`.** The `producer` item says whether any
   producer is wired; the `grant` item catches a missing permission.
2. **Bypass the producer.** From a shell inside the session:

   ```sh
   zellij pipe --name zj_radar.status.v1 -- \
     '{"v":1,"source":"test","pane":{"type":"terminal","id":'"${ZELLIJ_PANE_ID#terminal_}"'},"status":"running","repo":"demo","msg":"hello"}'
   ```

   If a row lights up, the sidebar is fine and the producer is the problem. If
   nothing happens, re-check the grant, and reload the wasm if `--download`
   just updated it.
3. **Producer prerequisites.** The agent must run inside the Zellij session;
   the hooks no-op without `$ZELLIJ_PANE_ID`. The Claude plugin's bash
   fallback needs `bash` and `jq`; installing the `zj-radar` CLI removes that
   dependency.
4. **Repeated test sends.** The CLI does not re-send a `running` identical to
   the pane's last one within 30 seconds ([producers](producers.md#claude-code)).
   When probing by hand, change the message between sends or set
   `ZJ_RADAR_NO_DEDUP=1`.
5. **Zellij too old.** The floor is 0.44.3; `--check` flags it.

## An editor, pager, or TUI shows a spinning "Running" row

**Symptom:** opening an interactive program makes its pane spin forever.

**Why:** interactive programs are recognized by name, and this one is not in
the built-in set.

**Fix:** add its exe name to the `interactive_commands` option
([configuration](configuration.md)). It applies live; the spinning row demotes
to a muted label at once.

## A command row is stuck "Running" forever

**Symptom:** a shell command's row keeps spinning after the pane is back at its
prompt.

**Why:** Zellij dropped the `CommandChanged` exit event. The sidebar promotes
a foreground command after a short debounce, and without the matching
back-to-prompt event nothing clears it.

**Fix:** run any other command in that pane, or close the pane. Agent rows
have their own safety net: a `running` row whose pane sits at the prompt is
cleared after about 15 seconds.

## Can't open a new tab (the two-template rule)

**Symptom:** tabs created at runtime (`Ctrl+t n`) contain only the sidebar and
status bar, so keystrokes have nowhere to land.

**Why:** without an explicit `new_tab_template`, Zellij derives one from
`default_tab_template` and drops a `children` placeholder nested inside a
split ([zellij#3247](https://github.com/zellij-org/zellij/issues/3247)). A
left column is exactly that shape.

**Fix:** declare `new_tab_template` with a concrete `pane focus=true`, as in
the [layout snippet](install.md#add-the-sidebar-to-a-layout).

## Alt+] hides the rail (or stops cycling)

**Symptom:** `Alt+[` / `Alt+]` (cycle swap layouts) either makes the sidebar
vanish from the current tab or does nothing.

**Why:** any custom layout discards Zellij's built-in swap layouts, so a layout
that declares none has no cycling. And a swap layout replaces the tab's
arrangement wholesale, so an entry that does not include the rail swaps it
away.

**Fix:** the injected rail and
[`examples/radar-sidebar.kdl`](../examples/radar-sidebar.kdl) redeclare
`swap_tiled_layout` with every entry routed through a rail-carrying
`tab_template name="ui"`. Two cases need your hand:

- **A hand-written layout without swap blocks:** copy them from the example,
  or re-run `zj-radar setup zellij --inject`, which adds them when none exist.
- **A layout with its own `swap_tiled_layout` blocks:** `--inject` leaves them
  alone and adds none of its own. Route each entry through the `ui` template:

  ```kdl
  swap_tiled_layout name="vertical" {
      ui max_panes=5 {          // ← was: tab_template { … } or a bare pane tree
          pane split_direction="vertical" {
              pane
              pane { children; }
          }
      }
  }
  ```

## First-run prompt coordination

**Symptom:** on a fresh layout the sidebar asks for permissions in one tab
while the others wait; occasionally a late-spawned sidebar starts empty until
the next broadcast.

**Why:** with one instance per tab, one instance owns the first-run prompt and
the others wait for Zellij's cached answer. Coordination uses Zellij's shared
plugin cache, falling back to `/tmp/zj-radar`. If neither is writable, more
than one instance may prompt and late sidebars start empty until the next
broadcast.

**Fix:** pre-authorize at install time, or grant once in a floating pane
([Permissions](install.md#permissions)); every later instance reuses the
cached grant.

## Session crashes with "too many open files" (EMFILE)

**Symptom:** in a long-lived session, `zellij pipe` calls hang, stuck `zellij
pipe` processes pile up, and the Zellij server dies with `Too many open files`.

**Why:** Zellij holds each `zellij pipe` client until every plugin instance has
consumed the message. An instance parked at its permission prompt never does,
so each broadcast leaves one blocked client pinning two server file
descriptors.

**Fix:** grant the prompt so no instance stays wedged (above), and make sure
your producers are current. Bundled producers bound every send with a
deadline; third-party producers must too. See
[Bound your sends](producers.md#bound-your-sends).

## Cards look flat (no colored row backgrounds)

**Symptom:** status glyphs are colored but the per-row card tints are missing.

**Why:** the tints are truecolor (24-bit) escapes. A terminal without truecolor
(macOS Terminal.app, the Linux console, a `tmux` without `RGB`) ignores them.
Layout and click targeting are unaffected.

**Fix:** use a truecolor terminal (Alacritty, Kitty, WezTerm, iTerm2, foot,
Ghostty). Inside `tmux`: `set -as terminal-features ',*:RGB'`.

## Rail glyphs spill past the sidebar edge

**Symptom:** a status glyph or the `═` rule pushes a row one column too wide.

**Why:** the rail budgets each glyph as one column. Several are East-Asian-Width
Ambiguous codepoints, which a terminal set to ambiguous width = double renders
as two columns. An emoji with a presentation selector (`⚠️`) can measure one
column narrower than it draws.

**Fix:** set ambiguous-character width to narrow (Kitty: default; WezTerm:
`treat_east_asian_ambiguous_width_as_narrow = true`; iTerm2: Profiles → Text →
"Treat ambiguous-width characters as double width" off).

## Focused card's highlight stops short of the pane edge (Ghostty)

**Symptom:** in Ghostty the focused card's background band appears to end one
or two columns before the rail's right edge.

**Why:** the rail paints every column Zellij gives it (verified with real-PTY
tests). Ghostty's window padding is drawn over the trailing columns.

**Fix:** in Ghostty's config, `window-padding-color = extend`.

## Sidebar is still the old version after `zj-radar update`

A running session keeps the plugin it loaded. Restart Zellij, or open a new
session. `zj-radar update --check` confirms the installed wasm matches the
release. If it reports the wasm as a symlink managed by Nix or home-manager,
`update` left it alone; update the flake input instead.

## Zellij plugin-reload quirks

**Symptom:** during development, reloading the plugin opens an extra tiled
plugin pane.

**Why:** Zellij's reload actions misbehave for a plugin created by a layout
that has made itself non-selectable, as the sidebar does after permissions.

**Fix:** `just dev` never reloads in place; each iteration is a fresh
`zj-radar-dev-<hhmmss>` session. See [Dev loop in
CONTRIBUTING](../CONTRIBUTING.md#dev-loop).
