# Demo assets

Everything needed to regenerate the README's screenshots and GIF — and a
zero-dependency way to **see the sidebar in action without wiring up a real
agent**.

The trick: zj-radar's only interface is the `zj_radar.status.v1` pipe broadcast,
so [`agent.sh`](agent.sh) just *plays* a scripted status arc for its own pane.
Each tab runs a different arc; the rail animates the rest (the spinner and
elapsed clock advance on the plugin's own 1-second timer).

The loop: a title card → a focused Claude agent works and then blocks on
approval → the demo tabs over to a second tab running **two** Claude agents
(rolled up in the rail as a `├`/`└` tree) → it closes by opening a fresh tab,
showing the sidebar follows into every tab.

## Regenerate the assets

```sh
./demo/record.sh            # build the debug wasm + record docs/media/hero.gif
./demo/record.sh --release  # use the release wasm instead
```

Requirements: [`vhs`](https://github.com/charmbracelet/vhs) (with `ffmpeg` +
`ttyd`), `zellij`, and a **Nerd Font** (`JetBrainsMono Nerd Font`) installed.
`gifsicle` is used to shrink the GIF if present.

Outputs:

| File | Beat |
|------|------|
| `docs/media/hero.gif` | The full loop |
| `docs/media/needs-you.png` | The focused Claude agent blocking on a permission prompt |
| `docs/media/states.png` | The multi-pane `web` tab + the `done`/`error`/`working` spread |

## Try it live (no recording)

```sh
cargo build --target wasm32-wasip1 -p zj-radar-plugin
./demo/record.sh --release   # builds + records, or just run zellij yourself:
zellij --config target/demo/config.kdl --layout target/demo/layout.kdl
```

(`record.sh` writes the concrete, path-substituted config/layout to
`target/demo/` from the templates in this folder.)

## Files

| File | What it is |
|------|------------|
| `agent.sh` | Broadcasts a timed status arc for `$ZELLIJ_PANE_ID`; the focused arcs also print agent-style output to fill the content pane. |
| `banner.sh` | The intro title card (tool name + glyph legend) shown for the first ~3s. |
| `demorc.sh` | A minimal bash rcfile giving the closing tab a clean `$ ` prompt instead of the recording host's shell config. |
| `layout.kdl` | Tabs running `agent.sh` arcs (one with two panes → a roll-up tree); radar sidebar pinned left via a direct `file:` plugin path. Template — `__WASM__`/`__ROOT__` filled at record time. |
| `config.kdl` | Zellij config: Tokyo Night theme (drives the rail's card surfaces). Template. |
| `hero.tape` | The vhs script: title card → status story → tab-over → fresh-tab sign-off. Template. |
| `record.sh` | Builds the wasm, pre-grants the plugin permission, strips inherited `ZELLIJ*` env, substitutes paths, runs vhs, optimizes the GIF. |

## How it stays reproducible

`record.sh` handles the three things that otherwise make a Zellij-in-vhs
recording flaky:

- **Permissions** — seeds the grant into Zellij's `permissions.kdl` (keyed by
  the wasm's filesystem path), so the recording shows no first-run prompt.
- **Nested Zellij** — unsets `ZELLIJ*` before launching vhs, so the inner
  session starts fresh even when you run `record.sh` from inside Zellij.
- **Theme/glyphs** — Tokyo Night + `JetBrainsMono Nerd Font`, pinned in the
  config and tape, so the look doesn't depend on your personal setup.

## Tuning notes

- **vhs has no mouse**, and its `Alt`-modifier keys don't reach Zellij reliably
  — but `Ctrl` does, so tab navigation uses Zellij's tab mode (`Ctrl+t` then the
  tab number / `n` for a new tab). Click-to-switch is described in the main
  README. (Live, you can also switch tabs with `Alt+a`/`w`/`e`/`r`, bound in
  `config.kdl`.)
- **Timing is wall-clock.** The `Sleep`s in `hero.tape` and the `sleep`s in
  `agent.sh` share one clock, offset by the ~2–3s it takes the session to start
  and the agents to begin broadcasting. If a beat lands early/late, nudge the
  two together.
