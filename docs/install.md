# Installing the sidebar

This page installs the sidebar: the wasm plugin, its `radar` alias, the rail in
your layout, and the permission grant. Agent status comes from a separate
install, the [producer](producers.md). The copy-paste path is the README's
[Quick start](../README.md#quick-start).

**Requirements:** Zellij 0.44.3 or newer (`zellij --version`). Earlier 0.44
patches let the sidebar pop out of its column during layout swaps; newer
releases keep compiled plugins working. `--download` needs `curl` or `wget`.

## Recommended: install the CLI, then `setup zellij --download`

Tagged releases ship a prebuilt `zj-radar` CLI for Linux (x86_64 and aarch64,
static musl) and Apple Silicon macOS. Intel macOS has no prebuilt binary; the
installer detects it and points you at `cargo install zj-radar` (see
[Build from source](#build-from-source)).

```sh
# Installs to ~/.local/bin by default.
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/marktoda/zj-radar/releases/latest/download/install.sh | sh

zj-radar setup zellij --download
```

`setup zellij --download` does five things:

1. Downloads the wasm built for this CLI's version, so the two never drift
   apart on the status contract. `ZJ_RADAR_VERSION` pins a different tag.
2. Verifies it against the release's `.sha256` checksum. A mismatch aborts; a
   missing checksum or no `sha256sum`/`shasum` on `PATH` falls back to TLS-only
   with a warning.
3. Copies it to `~/.config/zellij/plugins/zj_radar.wasm`.
4. Adds or updates a managed `radar` alias in `~/.config/zellij/config.kdl`.
5. Asks `Inject the rail into <layout>? [y/N]`. `y` splices the rail in place
   (backup at `<layout>.zj-radar.bak`); `N` prints the snippet to paste.

Flags:

| Flag | Effect |
|---|---|
| `--inject` | Non-interactive yes to the layout prompt. |
| `--layout <name>` | Target `~/.config/zellij/layouts/<name>.kdl` instead of the default layout. |
| `--dry-run` | Preview every change; write nothing. |
| `--yes` / `-y` | Fully non-interactive. Takes the safe default at each prompt: pre-seeds the grant, prints the layout snippet instead of injecting. |
| `--force` | Replace an existing unmanaged `radar` alias. |

The installer script honors `ZJ_RADAR_VERSION` (release tag) and
`ZJ_RADAR_BIN_DIR` (install directory).

## Try it without touching your config (`zj-radar run`)

`zj-radar run` launches a throwaway Zellij session with the rail wired in. It
owns that session's config, so nothing under `~/.config/zellij` is read or
edited. Consequences:

- Your Zellij keybinds and theme do not apply inside `run` sessions.
- Attaching to a session `run` did not create asks first.
- On top of Zellij's defaults it binds `Ctrl y` (open the permission-grant
  float) and `Alt 1`–`Alt 9` (tab jumps), so those chords do not reach
  programs in the panes.
- It uses the wasm bundled into the binary. A `cargo install` build has no
  bundled wasm and downloads the matching one on first use.

## Build from source

```sh
git clone https://github.com/marktoda/zj-radar
cd zj-radar

# rust-toolchain.toml requests the wasm32-wasip1 target; rustup installs it.
cargo build --release --target wasm32-wasip1 -p zj-radar-plugin
cargo install --path crates/cli

zj-radar setup zellij --wasm target/wasm32-wasip1/release/zj_radar.wasm
```

See [`TOOLCHAIN.md`](TOOLCHAIN.md) if your toolchain lacks the wasm target.

## Manual setup

Without the CLI, copy the wasm to the stable path yourself, from a source build
or a release download:

```sh
mkdir -p ~/.config/zellij/plugins
cp target/wasm32-wasip1/release/zj_radar.wasm ~/.config/zellij/plugins/
```

Define the alias once in `~/.config/zellij/config.kdl`:

```kdl
plugins {
    radar location="file:~/.config/zellij/plugins/zj_radar.wasm" {
        naming "managed"
    }
}
```

The path must be stable. Zellij ties a plugin's permission grant to its
location string, so a path that changes on every rebuild re-prompts every time.

## Add the sidebar to a layout

The sidebar is a pinned, borderless left column in every tab. Zellij's only
mechanism for that is the tab templates, where its own tab bar and status bar
live, so zj-radar integrates the way [zjstatus](https://github.com/dj95/zjstatus)
does: one pane in your templates, the rest of the layout stays yours.

With the CLI, inject or re-inject at any time:

```sh
zj-radar setup zellij --inject              # into the default layout
zj-radar setup zellij --inject --layout my  # into layouts/my.kdl
zj-radar setup zellij --uninstall           # strip the injected rail
```

If no layout file exists (stock Zellij ships none), `--inject` creates one with
the full rail layout.

By hand, add this to any layout file:

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

Both templates are required. Zellij derives `new_tab_template` from
`default_tab_template` when you omit it and drops a `children` nested inside a
split, leaving new tabs with no focusable pane. See
[Can't open a new tab](troubleshooting.md#cant-open-a-new-tab-the-two-template-rule).

Any custom layout also discards Zellij's built-in swap layouts, so `Alt+[` /
`Alt+]` cycling stops working. The injected rail and the example layout below
redeclare `swap_tiled_layout` through a rail-carrying `tab_template name="ui"`.
If you write the layout by hand, copy those blocks; if your layout already has
its own swaps, see
[Alt+] hides the rail](troubleshooting.md#alt-hides-the-rail-or-stops-cycling).

For a complete starting layout, copy
[`examples/radar-sidebar.kdl`](../examples/radar-sidebar.kdl) to
`~/.config/zellij/layouts/` and run `zellij --layout radar-sidebar`.

To put the column on the right, move `children` (and the runtime
`pane focus=true`) before the radar pane in each split. Change `size` for a
different width.

## Permissions

Zellij asks for an explicit grant the first time a plugin loads from a given
path. The sidebar requests four:

- `ReadApplicationState`: read tab and pane state to draw the rail.
- `ReadCliPipes`: receive `zj_radar.status.v1` broadcasts.
- `ChangeApplicationState`: switch tabs and panes on click, switch sessions
  from the badge, apply managed tab names.
- `RunCommands`: deliver desktop notifications (`osascript` / `notify-send`)
  and re-broadcast the `✓` acknowledge gesture over `zellij pipe`. Nothing
  else is ever run. Without it both are skipped and the rest works.

You normally never see the prompt. `setup zellij` asks for consent at install
time and writes the grant into Zellij's `permissions.kdl` itself: merge-safe,
other plugins' entries untouched, a `.zj-radar.bak` left beside the file, and
an unparseable file refused rather than edited. Zellij re-reads the file on
every plugin load, so the sidebar comes up live on the next launch, or on the
next new tab in a running session. To revoke, delete the `zj_radar.wasm`
block from `permissions.kdl`.

If you declined, the sidebar shows Zellij's own y/n prompt on first load, and
at rail width that prompt looks blank. Press `y` in the rail pane, or run
`zj-radar setup zellij --grant` from inside the session to get the prompt in a
readable floating pane. Details in
[Sidebar shows "needs permission"](troubleshooting.md#sidebar-shows-needs-permission-or-looks-blank).

## Check your setup (`--check`)

`zj-radar setup zellij --check` is a read-only diagnostic:

```
zj-radar setup zellij --check
zellij:
  ok zellij binary: found on PATH (zellij 0.44.3)
  ok alias: radar plugin alias present in config.kdl
  ok wasm: wasm plugin file present
  missing layout: default layout does not have the radar rail — run `zj-radar setup zellij` or paste the snippet
  missing grant: wasm not granted — run `zj-radar setup zellij -y` to pre-authorize (or `--grant` from inside Zellij)
  ok producer: Claude plugin wired
```

Items are `ok`, `warn`, or `missing`:

- **zellij binary**: on `PATH`; warns below 0.44.3.
- **alias**: `radar` alias in `config.kdl`; warns if it points at a
  `/nix/store/` path, since that grant dies on the next rebuild.
- **wasm**: file present at the stable path.
- **layout**: the default layout contains the rail.
- **grant**: `permissions.kdl` grants the wasm path.
- **producer**: which of the Claude plugin, Codex hooks, and Opencode bridge
  are wired.
- **managed config** (only when `config.kdl` is a symlink, as under
  home-manager): direct edits may be overwritten.
- **config env** (only when `$ZELLIJ_CONFIG_FILE` points elsewhere): Zellij
  reads that file, not the one setup edits.

## Upgrade (`zj-radar update`)

```sh
zj-radar update            # move the CLI and the sidebar to the latest release
zj-radar update --check    # report only; exit 1 when an update is available
```

`update` does four things:

1. Finds the latest release (`ZJ_RADAR_VERSION` pins a tag) and compares it
   to this CLI. If newer, it downloads the binary for this platform, verifies
   the release's `.sha256` checksum, and swaps it in place. The running
   process is unaffected; the next `zj-radar` is the new one.
2. Runs `setup zellij --download` through the new binary, so the wasm at
   `~/.config/zellij/plugins/zj_radar.wasm` comes from the same release. The
   file is rewritten only when its bytes differ.
3. Runs `setup` for the producers already wired (Codex hooks, Opencode
   bridge), never for one you have not set up. The Claude Code plugin updates
   from inside Claude: `/plugin update zj-radar-claude@zj-radar`.
4. Runs the doctor (`setup --check`).

Restart Zellij, or open a new session, to load the new sidebar. A running
session keeps the old plugin.

`--check` compares both halves: the CLI version against the release tag, and
the installed wasm's checksum against the release's published `.sha256`. A
wasm built from source reads as differing, since it is not byte-identical to
the release artifact.

What `update` does with each kind of install:

| Install | Behavior |
|---|---|
| `curl \| sh` installer (`~/.local/bin`) | Replaced in place. |
| `cargo install` (`~/.cargo/bin` or `$CARGO_HOME/bin`) | Left alone. Run `cargo install zj-radar` (or `cargo binstall zj-radar`), then `zj-radar setup zellij --download`. |
| Nix / home-manager (`/nix/store` binary, or a symlinked wasm) | Left alone. Update the flake input. |
| No sidebar installed | Reported; run `zj-radar setup zellij --download` to add one. |
| No prebuilt binary (Intel macOS) | Build from source, then `zj-radar setup zellij --download`. |
| `ZJ_RADAR_VERSION` older than this CLI | Refused. Downgrade with the installer and that tag. |

Re-running `zj-radar setup zellij --download` by hand refreshes the wasm alone.

## Loading straight from a release URL

Zellij can load a plugin from an `https://` URL and cache it:

```kdl
plugin location="https://github.com/marktoda/zj-radar/releases/latest/download/zj_radar.wasm"
```

Not recommended here. The sidebar loads once per tab, and Zellij has a known
bug where several tabs fetching the same remote plugin at once corrupt the
download. Use the `file:` path or the Nix package; keep the URL form for a
single-tab try.

## Nix / home-manager

The flake exposes the wasm as `packages.default` and the CLI as
`packages.zj-radar-cli`:

```nix
# flake.nix
inputs.zj-radar.url = "github:marktoda/zj-radar";
```

Install both from the same pin. The producer hooks prefer `zj-radar notify`
from `PATH`, so the CLI has to ride along:

```nix
# home-manager module
home.packages = [inputs.zj-radar.packages.${pkgs.system}.zj-radar-cli];

# Symlink the wasm to a STABLE path instead of pointing the alias at the
# /nix/store path: Zellij keys grants by the location string, so a per-build
# store path re-prompts after every rebuild (`--check` warns about this).
home.file.".config/zellij/plugins/zj_radar.wasm".source =
  "${inputs.zj-radar.packages.${pkgs.system}.default}/bin/zj_radar.wasm";
```

Then point the alias in your generated `config.kdl` at that path:

```kdl
plugins {
    radar location="file:~/.config/zellij/plugins/zj_radar.wasm" {
        naming "managed"
    }
}
```

To pin the release wasm without a Rust toolchain:

```nix
zjRadarWasm = pkgs.fetchurl {
  url = "https://github.com/marktoda/zj-radar/releases/latest/download/zj_radar.wasm";
  hash = "sha256-..."; # nix-prefetch-url the asset to fill this in
};
```

## Files zj-radar creates (and how to remove them)

Paths are the defaults; `ZELLIJ_CONFIG_DIR` / `XDG_CONFIG_HOME` move the
config-dir entries with them.

| File | Created by | `--uninstall` |
|---|---|---|
| `~/.config/zellij/config.kdl`: managed `radar` alias between `// zj-radar:` markers | `setup zellij` | **reversed**: strips only the fenced block |
| `~/.config/zellij/layouts/<name>.kdl`: rail spliced between markers | `setup zellij --inject` into an existing layout | **reversed**: exact inverse of the splice |
| `~/.config/zellij/layouts/<name>.kdl`: whole file | `setup zellij --inject` when no layout existed | left in place; delete the file |
| `<edited file>.zj-radar.bak` | every config/layout edit | left in place; they are your restore points |
| `~/.config/zellij/plugins/zj_radar.wasm` | `setup zellij --wasm/--download` | left in place; `rm` it |
| `permissions.kdl` grant entry (macOS `~/Library/Caches/org.Zellij-Contributors.Zellij/`, Linux `~/.cache/zellij/`) | `setup zellij` with your consent, or Zellij when you answer `y` | left in place (Zellij also writes this file); delete the `zj_radar.wasm` block |
| `run`'s config dir (macOS `~/Library/Application Support/zj-radar/`, Linux `~/.local/share/zj-radar/`) | `zj-radar run` | not touched by `setup`; `rm -r` it, it holds only re-materializable assets and session markers |
| Per-session plugin state under Zellij's cache, `/tmp/zj-radar` fallback | the running plugin | self-pruning after 24 h; safe to delete anytime |
| `$CODEX_HOME/hooks.json` entries (+ optional `notify` slot in `config.toml`) | `setup codex` | **reversed** by `setup codex --uninstall` |
| `zj-radar-claude` plugin + `zj-radar` marketplace entry in Claude Code's plugin store | `setup claude` | plugin **reversed** by `setup claude --uninstall`; marketplace entry stays: `claude plugin marketplace remove zj-radar` |
| `$XDG_CONFIG_HOME/opencode/plugins/zj-radar.js` (or `~/.config/opencode/plugins/zj-radar.js`) | `setup opencode` | **reversed** by `setup opencode --uninstall` (only when the marker is present) |

Complete removal:

```sh
zj-radar setup zellij --uninstall
zj-radar setup claude --uninstall && claude plugin marketplace remove zj-radar
zj-radar setup codex --uninstall
zj-radar setup opencode --uninstall
```

then delete the wasm, the `run` data dir, the grant block, and the binary.
