# Installing the sidebar

This is the full install reference for the **sidebar** — the wasm plugin you add
to your Zellij layout. For the **producer** (whatever broadcasts agent status),
see [`producers.md`](producers.md). For a copy-paste fast path, see
[Quick start](../README.md#quick-start).

**Requirements:** Zellij **0.44.x** — the plugin ABI is not yet stable across
Zellij versions, so a sidebar built against 0.44 may fail to load elsewhere
(check with `zellij --version`). `--download` additionally needs `curl` or
`wget` on PATH.

There are two jobs to get a working radar:

1. **Show the sidebar in Zellij** — install the wasm at a stable path, define a
   `radar` plugin alias in `config.kdl`, and add the sidebar templates to a
   layout. *(This page.)*
2. **Send agent status to the sidebar** — install the Claude plugin or wire an
   agent to call `zj-radar notify`. *(See [`producers.md`](producers.md).)*

## Recommended: install the CLI, then `setup zellij --download`

A tagged release ships a prebuilt `zj-radar` CLI for Linux (x86_64 and aarch64,
static musl) and **Apple Silicon** macOS (aarch64). Intel (x86_64) macOS has no
prebuilt binary — the installer detects it and points you at the source install
(`cargo install zj-radar`); see [Build from source](#build-from-source-instead).
Install with the one-line script, then let it fetch the matching sidebar wasm and
manage the Zellij plugin alias:

```sh
# Static Linux + macOS binary; installs to ~/.local/bin by default.
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/marktoda/zj-radar/releases/latest/download/install.sh | sh

zj-radar setup zellij --download
```

`setup zellij --download`:

- downloads the wasm **built from this CLI's own version** (set `ZJ_RADAR_VERSION`
  to pin a different release tag) — so the CLI and wasm can't drift apart across
  Zellij's unstable plugin ABI
- verifies it against the release's published `.sha256` checksum before installing
  (a mismatch aborts; releases without a checksum fall back to TLS-only with a
  warning) — needs `sha256sum` or `shasum` on `PATH`
- copies it to `~/.config/zellij/plugins/zj_radar.wasm`
- adds or updates a managed `radar` alias in `~/.config/zellij/config.kdl`
- reads your default layout, then **prompts** `Inject the rail into <layout>? [y/N]`:
  answer **y** to splice the rail in-place (backup saved as `.zj-radar.bak`), or
  **N** (default) to print the tailored snippet to paste yourself

Pass `--inject` for a non-interactive yes, `--layout <name>` to target a specific
layout (`~/.config/zellij/layouts/<name>.kdl`), `--dry-run` to preview without
writing, `--yes` for a fully non-interactive run (always takes the safe default —
prints the snippet, never mutates a layout), and `--force` only if you want to
replace an existing unmanaged `radar` alias. The installer also honors
`ZJ_RADAR_VERSION` (release tag) and `ZJ_RADAR_BIN_DIR` (install directory).

## Build from source instead

No prebuilt binary for your platform, or hacking on zj-radar? Build the wasm and
install the CLI from a checkout, then point `setup zellij` at the local wasm:

```sh
git clone https://github.com/marktoda/zj-radar
cd zj-radar

# Needs the wasm32-wasip1 target; rust-toolchain.toml requests it (rustup
# auto-installs it). See docs/TOOLCHAIN.md.
cargo build --release --target wasm32-wasip1 -p zj-radar-plugin
cargo install --path crates/cli

zj-radar setup zellij --wasm target/wasm32-wasip1/release/zj_radar.wasm
```

## Manual setup

If you are not using the CLI, copy the wasm to the same stable path yourself
(from a source build, or a `zj_radar.wasm` downloaded from a release):

```sh
mkdir -p ~/.config/zellij/plugins
cp target/wasm32-wasip1/release/zj_radar.wasm ~/.config/zellij/plugins/
```

Then define the alias once in `~/.config/zellij/config.kdl`:

```kdl
// ~/.config/zellij/config.kdl
plugins {
    radar location="file:~/.config/zellij/plugins/zj_radar.wasm" {
        naming "managed"
    }
}
```

The fixed path matters: Zellij ties a plugin's permission grant to its location.
If the location changes on every rebuild, Zellij asks again.

## Add the sidebar to a layout

The sidebar is a pinned, borderless **left column** that lives in every tab.
Zellij has no "pin a pane across all tabs" mechanism other than the tab
templates — the same place its own tab-bar/status-bar live — so radar integrates
like [zjstatus](https://github.com/dj95/zjstatus): add one pane to your
templates, and keep the rest of your layout yours.

`setup zellij` with `--wasm <path>` or `--download` installs the wasm and alias,
then prompts to inject the rail automatically. You can also inject or re-inject
at any time (no wasm/alias step needed):

```sh
zj-radar setup zellij --inject              # inject into the default layout
zj-radar setup zellij --inject --layout my  # inject into layouts/my.kdl
zj-radar setup zellij --uninstall           # strip the injected rail
```

To do it manually, add this snippet to any layout file:

```kdl
// Tabs defined in the layout file get their panes via `children`.
default_tab_template {
    pane split_direction="vertical" {
        pane size=32 borderless=true { plugin location="radar" }   // ← alias
        children
    }
    pane size=2 borderless=true { plugin location="zellij:status-bar" }
}

// Tabs created at runtime (Ctrl+t n) get a CONCRETE focused pane, not `children`.
new_tab_template {
    pane split_direction="vertical" {
        pane size=32 borderless=true { plugin location="radar" }
        pane focus=true
    }
    pane size=2 borderless=true { plugin location="zellij:status-bar" }
}
```

Why two templates? It works around an upstream Zellij derivation bug — see
[Can't open a new tab](troubleshooting.md#cant-open-a-new-tab-the-two-template-rule).

One more thing the snippet above does **not** cover: any custom layout makes
Zellij discard its built-in swap layouts, so `Alt+[` / `Alt+]` cycling stops
working (and a swap that doesn't include the rail would swap it away). Copy the
`tab_template name="ui"` + `swap_tiled_layout` blocks from the example layout
below, or let `--inject` add them — and if your layout already has its own
swaps, see [Alt+] hides the rail](troubleshooting.md#alt-hides-the-rail-or-stops-cycling).

Prefer a complete starting layout? Copy
[`examples/radar-sidebar.kdl`](../examples/radar-sidebar.kdl) to
`~/.config/zellij/layouts/` and run `zellij --layout radar-sidebar`. It uses the
same `plugin location="radar"` alias as the snippet.

Want the column on the **right**? Put `children` (and the runtime
`pane focus=true`) before the radar pane in each vertical split. Different
width? Change `size`.

## Grant permissions (`--grant`)

Zellij requires an explicit permission grant the first time a plugin loads from a
given path. `--grant` opens the wasm in a focused floating pane so Zellij surfaces
the prompt without you having to open a full layout first:

```sh
zj-radar setup zellij --grant
```

This is a standalone action: it skips wasm copy, alias edit, and layout injection,
and exits after launching the floating pane. After you approve inside the pane,
close it and subsequent sidebar instances at the same path will use Zellij's
cached grant.

## First-run permission prompt

On first load the sidebar shows an onboarding face and requests four
permissions — press `y` to grant:

- `ReadApplicationState` — read tab/pane state to draw the rail.
- `ReadCliPipes` — receive the `zj_radar.status.v1` broadcasts from producers.
- `ChangeApplicationState` — switch tabs on click and apply managed tab names.
- `RunCommands` — deliver desktop notifications (`osascript` on macOS,
  `notify-send` on Linux). This is the only thing the plugin runs commands
  for; turn notifications off with `notify false` (see
  [configuration](configuration.md)), and without this grant they are
  silently skipped while everything else keeps working.

The sidebar stays focusable only for that prompt, then goes back to passive
sidebar behavior.

After approval, the per-tab sidebars should use the cached grant. For how the
per-tab instances coordinate that single prompt (and what happens when session
files aren't writable), see
[First-run prompt coordination](troubleshooting.md#first-run-prompt-coordination).

## Check your setup (`--check`)

Run `zj-radar setup zellij --check` to get a diagnostic summary of every
component:

```
zj-radar setup zellij --check
zellij:
  ok alias: radar plugin alias present in config.kdl
  ok wasm: wasm plugin file present
  missing layout: default layout does not have the radar rail — run `zj-radar setup zellij` or paste the snippet
  missing grant: wasm not granted — run `zj-radar setup zellij --grant`
  ok producer: a producer is wired (Codex hooks or Claude plugin)
```

Each item is `ok`, `warn`, or `missing`. The check is read-only — it never
modifies any file. Reported items (five always; a sixth only when applicable):

- **alias** — `radar` plugin alias present in `config.kdl`; warns if it points at
  a `/nix/store/` path (grant won't survive a rebuild).
- **wasm** — plugin file exists at the expected stable path.
- **layout** — default layout contains the injected radar rail.
- **grant** — `permissions.kdl` records a grant for the wasm path.
- **producer** — Codex hooks or Claude plugin is wired up.
- **managed config** — emitted only when `config.kdl` is a symlink
  (home-manager); warns that direct edits may be overwritten.

## Loading straight from a release URL (caveat)

Zellij can also load a plugin directly from an `https://` URL, downloading and
caching it (no manual `cp`) — once a release is tagged:

```kdl
plugin location="https://github.com/marktoda/zj-radar/releases/latest/download/zj_radar.wasm"
```

**Not recommended as the default for zj-radar**, though: the sidebar loads once
*per tab* (it lives in `default_tab_template`), and Zellij has a known bug where
several tabs fetching the same remote plugin at once can corrupt the download.
Prefer the `file:` path above or the Nix package below; use the URL form only
for a quick single-tab try.

## Nix / home-manager

This flake exposes the wasm as `packages.default` and the CLI as
`packages.zj-radar-cli`, so a flake-based config consumes the exact artifacts
this repo builds. Add the repo as an input:

```nix
# flake.nix
inputs.zj-radar.url = "github:marktoda/zj-radar";
```

Install both halves from the same pin — the CLI must ride along because the
producer hooks prefer `zj-radar notify` from PATH:

```nix
# home-manager module
home.packages = [inputs.zj-radar.packages.${pkgs.system}.zj-radar-cli];

# Symlink the wasm to a STABLE path rather than pointing the alias at the
# /nix/store path directly: Zellij keys permission grants by the configured
# location string, so a per-build store path re-prompts after every rebuild
# (`zj-radar setup zellij --check` warns about exactly this). Rebuilds swap
# the symlink target; the granted path never changes.
home.file.".config/zellij/plugins/zj_radar.wasm".source =
  "${inputs.zj-radar.packages.${pkgs.system}.default}/bin/zj_radar.wasm";
```

Then point the alias in your generated `config.kdl` at the stable path:

```kdl
plugins {
    radar location="file:~/.config/zellij/plugins/zj_radar.wasm" {
        naming "managed"
    }
}
```

Tagged releases also publish a prebuilt wasm artifact that can be pinned without
a Rust toolchain (mirrors the older `room`/`smart-tabs` vendoring this replaces):

```nix
zjRadarWasm = pkgs.fetchurl {
  url = "https://github.com/marktoda/zj-radar/releases/latest/download/zj_radar.wasm";
  hash = "sha256-..."; # nix-prefetch-url the asset to fill this in
};
```

The old `@smartTabs@` substitution is fully retired — zj-radar owns the rail.
