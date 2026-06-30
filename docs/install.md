# Installing the sidebar

This is the full install reference for the **sidebar** — the wasm plugin you add
to your Zellij layout. For the **producer** (whatever broadcasts agent status),
see [`producers.md`](producers.md). For a copy-paste fast path, see
[Quick start](../README.md#quick-start).

There are two jobs to get a working radar:

1. **Show the sidebar in Zellij** — install the wasm at a stable path, define a
   `radar` plugin alias in `config.kdl`, and add the sidebar templates to a
   layout. *(This page.)*
2. **Send agent status to the sidebar** — install the Claude plugin or wire an
   agent to call `zj-radar notify`. *(See [`producers.md`](producers.md).)*

## Recommended: install the CLI, then `setup zellij --download`

A tagged release ships a prebuilt `zj-radar` CLI for Linux and macOS. Install it
with the one-line script, then let it fetch the matching sidebar wasm and manage
the Zellij plugin alias:

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
- copies it to `~/.config/zellij/plugins/zj_radar.wasm`
- adds or updates a managed `radar` alias in `~/.config/zellij/config.kdl`
- prints the layout snippet to paste

It does **not** rewrite your layouts. Use `--dry-run` to preview, `--yes` for
non-interactive runs, and `--force` only if you want to replace an existing
unmanaged `radar` alias. The installer also honors `ZJ_RADAR_VERSION` (release
tag) and `ZJ_RADAR_BIN_DIR` (install directory).

## Build from source instead

No prebuilt binary for your platform, or hacking on zj-radar? Build the wasm and
install the CLI from a checkout, then point `setup zellij` at the local wasm:

```sh
git clone https://github.com/marktoda/zj-radar
cd zj-radar

# Needs the wasm32-wasip1 target; rust-toolchain.toml requests it (rustup
# auto-installs it). See docs/TOOLCHAIN.md.
cargo build --release --target wasm32-wasip1 -p zj-radar-plugin
cargo install --path . --features cli

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

Paste [`examples/radar-template-snippet.kdl`](../examples/radar-template-snippet.kdl)
into any layout:

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

Prefer a complete starting layout? Copy
[`examples/radar-sidebar.kdl`](../examples/radar-sidebar.kdl) to
`~/.config/zellij/layouts/` and run `zellij --layout radar-sidebar`. It uses the
same `plugin location="radar"` alias as the snippet.

Want the column on the **right**? Put `children` (and the runtime
`pane focus=true`) before the radar pane in each vertical split. Different
width? Change `size`.

## First-run permission prompt

On first load the sidebar shows an onboarding face and requests three
permissions (`ReadApplicationState`, `ReadCliPipes`, `ChangeApplicationState`) —
press `y` to grant. The sidebar stays focusable only for that prompt, then goes
back to passive sidebar behavior. It never runs commands; notifications stay in
the producer.

For a roomier first-run prompt, approve the same stable plugin URL once in a
floating pane before using the sidebar layout:

```sh
zellij plugin --floating --width 80 --height 24 file:~/.config/zellij/plugins/zj_radar.wasm
```

After approval, close that floating pane and start your radar layout; the per-tab
sidebars should use the cached grant. For how the per-tab instances coordinate
that single prompt (and what happens when session files aren't writable), see
[First-run prompt coordination](troubleshooting.md#first-run-prompt-coordination).

## Loading straight from a release URL (caveat)

Zellij can also load a plugin directly from an `https://` URL, downloading and
caching it (no manual `cp`) — once a release is tagged:

```kdl
plugin location="https://github.com/marktoda/zj-radar/releases/download/v0.1.0/zj_radar.wasm"
```

**Not recommended as the default for zj-radar**, though: the sidebar loads once
*per tab* (it lives in `default_tab_template`), and Zellij has a known bug where
several tabs fetching the same remote plugin at once can corrupt the download.
Prefer the `file:` path above or the Nix package below; use the URL form only
for a quick single-tab try.

## Nix / home-manager

This flake exposes the wasm as `packages.default`, so a flake-based config can
consume the exact same artifact this repo builds. Add the repo as an input:

```nix
# flake.nix
inputs.zj-radar.url = "github:marktoda/zj-radar";
```

Then reference the built wasm from your generated `config.kdl` alias:

```kdl
plugins {
    radar location="file:${inputs.zj-radar.packages.${system}.default}/bin/zj_radar.wasm" {
        naming "managed"
    }
}
```

Tagged releases also publish a prebuilt wasm artifact that can be pinned without
a Rust toolchain (mirrors the older `room`/`smart-tabs` vendoring this replaces):

```nix
zjRadarWasm = pkgs.fetchurl {
  url = "https://github.com/marktoda/zj-radar/releases/download/v0.1.0/zj_radar.wasm";
  hash = "sha256-..."; # nix-prefetch-url the asset to fill this in
};
```

The old `@smartTabs@` substitution is fully retired — zj-radar owns the rail.
