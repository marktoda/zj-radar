# Demo assets

Everything needed to regenerate the README's screenshots and GIF — and a
zero-dependency way to **see the sidebar in action without wiring up a real
agent**.

The trick: zj-radar's only interface is the `zj_radar.status.v1` pipe broadcast,
so [`agent.sh`](agent.sh) just *plays* a scripted status arc for its own pane.
Four tabs each run a different arc; the rail animates the rest (the spinner and
elapsed clock advance on the plugin's own 1-second timer).

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
| `docs/media/hero.gif` | The full ~15s loop |
| `docs/media/needs-you.png` | An agent blocking on a permission prompt (`◆ needs you`) |
| `docs/media/states.png` | Mixed `done` / `error` / `working` across tabs |

## Try it live (no recording)

```sh
cargo build --target wasm32-wasip1
./demo/record.sh --release   # builds + records, or just run zellij yourself:
zellij --config target/demo/config.kdl --layout target/demo/layout.kdl
```

(`record.sh` writes the concrete, path-substituted config/layout to
`target/demo/` from the templates in this folder.)

## Files

| File | What it is |
|------|------------|
| `agent.sh` | Broadcasts a timed status arc (`needs-you` / `done` / `tests` / `deploy-error`) for `$ZELLIJ_PANE_ID`. The `tests` arc also streams cargo-test output to fill the focused content pane. |
| `layout.kdl` | Four tabs, each running an `agent.sh` arc; radar sidebar pinned left via a direct `file:` plugin path (inline `glyphs`/`density`/`naming`). Template — `__WASM__`/`__ROOT__` filled at record time. |
| `config.kdl` | Zellij config: Tokyo Night theme (drives the rail's card surfaces). Template. |
| `hero.tape` | The vhs script: launches the session and plays the status story. Template. |
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

- **vhs has no mouse**, and its Alt-modifier keys don't reach Zellij reliably,
  so the demo is purely status-driven — no tab switching. Click-to-switch and
  the focus-clears-`done` behavior are described in the main README. (Live, you
  can switch tabs with `Alt+a`/`w`/`e`/`r`, bound in `config.kdl`.)
- **Timing is wall-clock.** The `Sleep`s in `hero.tape` and the `sleep`s in
  `agent.sh` share one clock, offset by the ~2–3s it takes the session to start
  and the agents to begin broadcasting. If a beat lands early/late, nudge the
  two together.
