# zj-radar

A native [Zellij](https://zellij.dev) **sidebar** that shows live AI-agent
status for every tab — *working*, *waiting for you*, *done*, or *error* — with
repo·branch, elapsed time, and the last message. Click a row to jump to that
tab.

```
╔═ RADAR ════════════╗┌─ your panes ──────────────┐
║● 1 dotfiles         ║│                            │
║  main · done 2m     ║│   focused tab content      │
║  "refactored the…"  ║│                            │
║◐ 2 pinky      2/4   ║│                            │
║  fix/x · 0:14       ║│                            │
║  "running tests…"   ║│                            │
║◆ 3 api              ║│                            │
║  feat/y · needs you ║│                            │
║○ 4 notes            ║│                            │
╚════════════════════╝└────────────────────────────┘
 NORMAL  <p>ane <t>ab …   ← existing status-bar, untouched
```

`◆ needs you` · `◐ working` · `● done` · `✗ error` · `○ idle / plain terminal`

## Why it exists

Agents like Claude Code spend long stretches working, then quietly block on a
permission prompt or finish. In a many-tab Zellij session it's easy to lose
track of which agent needs you. zj-radar surfaces that at a glance, in a pinned
left column that survives swap-layout cycling.

It is **push-driven, not poll-driven**: status arrives via an explicit
`zellij pipe` broadcast from per-agent hooks. The plugin never issues blocking
host queries (`get_pane_running_command`, etc.). This is a deliberate, hard
constraint — the predecessor plugin (`smart-tabs`) melted a many-agent session
by polling every pane on every output event; see
[`docs/smart-tabs-postmortem.md`](docs/smart-tabs-postmortem.md).

## Repo layout

| Path | What it is |
|------|------------|
| `src/` | The Zellij sidebar plugin (Rust → `wasm32-wasip1`). Thin Zellij adapter, pure runtime, stores, model, and renderer. |
| `plugins/zj-radar-claude/` | A **Claude Code plugin** that broadcasts agent status via hooks — no `settings.json` editing. |
| `docs/` | Design, plan, and postmortem docs. `design.md` is the canonical living design. |
| `dev/dev.kdl` | A dev layout for dogfooding the debug plugin while building. |

There are two independent install surfaces: the **sidebar** (the wasm plugin you
add to your Zellij layout) and the **producer** (whatever broadcasts status —
today, the Claude Code plugin).

## Install

There are two jobs:

1. **Show the sidebar in Zellij** — install the wasm at a stable path, define a
   `radar` plugin alias in `config.kdl`, and add the sidebar templates to a
   layout.
2. **Send agent status to the sidebar** — install the Claude plugin or wire an
   agent to call `zj-radar notify`.

### 1. Show the sidebar in Zellij

There is no pre-built release yet, so build the wasm from source:

```sh
# Needs the wasm32-wasip1 target; rust-toolchain.toml requests it (rustup auto-installs it). See docs/TOOLCHAIN.md.
cargo build --release --target wasm32-wasip1
```

#### Recommended: use the CLI

Install the native CLI from this checkout, then let it copy the wasm and manage
the Zellij plugin alias:

```sh
cargo install --path . --features cli
zj-radar setup zellij --wasm target/wasm32-wasip1/release/zj_radar.wasm
```

`setup zellij`:

- copies the wasm to `~/.config/zellij/plugins/zj_radar.wasm`
- adds or updates a managed `radar` alias in `~/.config/zellij/config.kdl`
- prints the layout snippet to paste

It does **not** rewrite your layouts. Use `--dry-run` to preview, `--yes` for
non-interactive runs, and `--force` only if you want to replace an existing
unmanaged `radar` alias.

#### Manual setup

If you are not using the CLI, copy the wasm to the same stable path yourself:

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

#### Add the sidebar to a layout

The sidebar is a pinned, borderless **left column** that lives in every tab.
Zellij has no "pin a pane across all tabs" mechanism other than the tab
templates — the same place its own tab-bar/status-bar live — so radar integrates
like [zjstatus](https://github.com/dj95/zjstatus): add one pane to your
templates, and keep the rest of your layout yours.

Paste [`examples/radar-template-snippet.kdl`](examples/radar-template-snippet.kdl)
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

> **Why two templates?** When you don't supply a `new_tab_template`, Zellij
> *derives* one from `default_tab_template` — and that derivation **drops a
> `children` placeholder nested inside a split** (upstream
> [zellij-org/zellij#3247](https://github.com/zellij-org/zellij/issues/3247),
> still open). New tabs then contain only the borderless sidebar + status-bar —
> no focusable pane — so keystrokes have nowhere to land and you "can't open a
> new tab." Declaring `new_tab_template` explicitly with a concrete
> `pane focus=true` (instead of `children`) sidesteps the derivation. A
> *top-level* `children`, like the stock compact layout, materializes fine —
> only the nested-in-a-split case is affected.

Prefer a complete starting layout? Copy
[`examples/radar-sidebar.kdl`](examples/radar-sidebar.kdl) to
`~/.config/zellij/layouts/` and run `zellij --layout radar-sidebar`. It uses the
same `plugin location="radar"` alias as the snippet.

Want the column on the **right**? Put `children` (and the runtime
`pane focus=true`) before the radar pane in each vertical split. Different
width? Change `size`.

On first load the sidebar shows an onboarding face and requests three
permissions (`ReadApplicationState`, `ReadCliPipes`, `ChangeApplicationState`) —
press `y` to grant. Because the sidebar exists once per tab, only one instance
owns the first-run prompt when session files are writable; the others wait for
Zellij's cached answer and then continue without asking again. Session files use
Zellij's shared plugin cache when available and fall back to `/tmp/zj-radar`; if
neither is writable, the sidebar still runs, but late-spawned sidebars may start
empty until the next broadcast and first-run prompt coordination may be noisier.
The sidebar stays focusable only for that prompt, then goes back to passive
sidebar behavior. It never runs commands; notifications stay in the producer.

For a roomier first-run prompt, approve the same stable plugin URL once in a
floating pane before using the sidebar layout:

```sh
zellij plugin --floating --width 80 --height 24 file:~/.config/zellij/plugins/zj_radar.wasm
```

After approval, close that floating pane and start your radar layout; the per-tab
sidebars should use the cached grant.

#### Loading straight from a release URL (caveat)

Zellij can also load a plugin directly from an `https://` URL, downloading and
caching it (no manual `cp`) — once a release is tagged:

```kdl
plugin location="https://github.com/mark-toda/zj-radar/releases/download/v0.1.0/zj_radar.wasm"
```

**Not recommended as the default for zj-radar**, though: the sidebar loads once
*per tab* (it lives in `default_tab_template`), and Zellij has a known bug where
several tabs fetching the same remote plugin at once can corrupt the download.
Prefer the `file:` path above or the Nix package below; use the URL form only
for a quick single-tab try.

#### Nix / home-manager

This flake exposes the wasm as `packages.default`, so a flake-based config can
consume the exact same artifact this repo builds. Add the repo as an input:

```nix
# flake.nix
inputs.zj-radar.url = "github:mark-toda/zj-radar";
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
  url = "https://github.com/mark-toda/zj-radar/releases/download/v0.1.0/zj_radar.wasm";
  hash = "sha256-..."; # nix-prefetch-url the asset to fill this in
};
```

The old `@smartTabs@` substitution is fully retired — zj-radar owns the rail.

### 2. Send agent status to the sidebar

#### Claude Code

Installing this plugin auto-registers the status hooks — **no `settings.json`
editing**, clean uninstall.

```sh
/plugin marketplace add mark-toda/zj-radar
/plugin install zj-radar-claude@zj-radar
```

Requires `jq` and `git` on `PATH` (used to parse the hook payload and derive
repo/branch). See [`plugins/zj-radar-claude/README.md`](plugins/zj-radar-claude/README.md)
for details. It's a no-op outside Zellij, so it's safe to leave enabled
everywhere.

#### Codex and the native CLI

A native binary that drops the `jq`/`bash` dependency and wires non-plugin agents.

> **Before the first tagged release**, the prebuilt tarballs and the
> `#zj-radar-cli` Nix output aren't published yet — use the `cargo install --git`
> form below (or build from source). The release workflow produces all three on
> the first `v*` tag.

```sh
# Release tarballs (published on tagged releases):
#   zj-radar-linux-x86_64.tar.gz
#   zj-radar-macos-aarch64.tar.gz
# Nix:
nix build github:mark-toda/zj-radar#zj-radar-cli   # -> result/bin/zj-radar
# Cargo:
cargo install --git https://github.com/mark-toda/zj-radar --features cli
```

- **`zj-radar notify <claude|codex>`** — broadcasts agent status. The Claude
  plugin's hook script automatically prefers it when it's on `PATH` (jq-free);
  otherwise the plugin falls back to its bundled `bash`+`jq` script.
- **`zj-radar setup [codex]`** — idempotently wires Codex's
  `~/.codex/hooks.json` to call `zj-radar notify codex`. This preserves any
  existing Codex `notify` program (e.g. a Computer Use notifier), because hooks
  are additive. Use `--dry-run` to preview, `--uninstall` to remove only
  zj-radar's hooks, and `--check` to diagnose the current setup. After installing
  or changing hooks, run `/hooks` inside Codex once to review and trust the
  command hook. (Claude needs no `setup` — use the plugin in §2.)
- **`zj-radar setup codex --legacy-notify`** — opt-in fallback for older Codex
  setups that only support the single `notify` program. It refuses to replace a
  foreign notifier unless `--force` is also passed.
- **`zj-radar setup zellij --wasm <path>`** — copies the sidebar wasm to
  `~/.config/zellij/plugins/zj_radar.wasm`, manages the `radar` alias in
  `config.kdl`, and prints the layout snippet. It leaves layouts user-owned.

Codex hooks report turn start, tool use, permission requests, subagents, and
turn stop. zj-radar maps those to `running`, `pending`, and `done`.

## Configuration

With the recommended alias setup, defaults live in
`~/.config/zellij/config.kdl`:

```kdl
plugins {
    radar location="file:~/.config/zellij/plugins/zj_radar.wasm" {
        density "comfortable"
        naming "off"
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

These can also be changed **at runtime** without editing the layout, by
broadcasting a flat JSON object on the `zj_radar.config.v1` pipe:

```sh
zellij pipe --name zj_radar.config.v1 -- '{"density":"compact","header":false}'
```

### Binding keys to runtime config

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

### Binding keys to commands

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

## Writing your own producer

The plugin's only real interface is the versioned pipe payload. Broadcast (by
name, never `--plugin`) a `zj_radar.status.v1` message:

```json
{ "v": 1,
  "source": "claude",
  "pane": { "type": "terminal", "id": 12 },
  "status": "running",
  "repo": "pinky",
  "branch": "fix/x",
  "msg": "running tests…",
  "on_focus": "idle",
  "seq": 42 }
```

- `status`: `running` → working · `pending` → needs-you · `done` · `error` ·
  `idle`/unknown → plain.
- `pane.id`: strip any `terminal_` prefix from `$ZELLIJ_PANE_ID`.
- `on_focus` (optional): the status to apply when you next focus that exact pane
  (lets `done` persist on other tabs, then auto-clear).
- `seq` (optional): monotonic per-pane counter; a `seq` ≤ the stored one is
  dropped (hook-race guard).

The plugin defends itself: it ignores oversized payloads, strips ANSI/control
chars, folds newlines to spaces, and truncates. Adapters can stay simple.

Quick smoke test (a "fake agent" — broadcast straight from your shell):

```sh
zellij pipe --name zj_radar.status.v1 -- \
  '{"v":1,"source":"test","pane":{"type":"terminal","id":12},"status":"running","repo":"demo","branch":"main","msg":"hello"}'
```

## Develop

```sh
cargo test                                # host tests, no wasm needed
./dev/run.sh                              # build + open the dev session
```

Run `./dev/run.sh` from either a normal terminal or inside Zellij. It builds
`target/wasm32-wasip1/debug/zj_radar.wasm` and generates `target/dev/dev.kdl`
with an absolute plugin path. Outside Zellij it restarts the disposable
`zj-radar-dev` session. Inside Zellij it leaves the current session unchanged;
use `./dev/run.sh --fresh-session` when you explicitly want to switch the
current client to a fresh disposable dev session. If the current Rust toolchain
is missing `wasm32-wasip1`, the script uses the repo's Nix flake automatically.

Zellij 0.44's plugin reload actions can open an extra tiled plugin pane when the
target plugin was created by a layout and has made itself non-selectable (as the
radar sidebar does after permissions). The default inside-Zellij dev loop avoids
both in-place plugin reloads and session switching.

The host-testable modules (`status`, `payload`, `radar_state`, `rollup`, `render`,
`tab_namer`, `command`, `config`, `theme`, `session_files`) carry no `zellij-tile` dependency
and are covered on the host target. Only `lib.rs`/`main.rs` touch the Zellij host
API and are gated behind `#[cfg(target_arch = "wasm32")]`. See
[`docs/TOOLCHAIN.md`](docs/TOOLCHAIN.md).

## Status & roadmap

- ✅ **Sidebar plugin** — tab list, click-to-switch, per-tab agent aggregation,
  overflow folding, theme-derived card surfaces, runtime config.
- ✅ **Claude Code producer** — ships as a Claude plugin (`plugins/zj-radar-claude`).
- ✅ **`zj-radar` CLI** — native, jq-free `notify` (Claude + Codex) and
  conflict-aware `setup`; see [Codex and the native CLI](#codex-and-the-native-cli).
- 📋 **Not yet built** — cross-platform prebuilt release binaries and a
  fully automatic layout patcher. See [`docs/distribution.md`](docs/distribution.md).

## Contributing

Issues and PRs welcome. See [`CONTRIBUTING.md`](CONTRIBUTING.md) for build/test
layers, the no-`rustfmt` rule, and the two load-bearing invariants
(push-driven, rail lockstep). [`CONTEXT.md`](CONTEXT.md) is the domain glossary —
the fastest way to orient before touching the core.

## License

MIT — see [`LICENSE`](LICENSE).
