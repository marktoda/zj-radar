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
| `src/` | The Zellij sidebar plugin (Rust → `wasm32-wasip1`). Pure logic modules + thin host glue. |
| `plugins/zj-radar-claude/` | A **Claude Code plugin** that broadcasts agent status via hooks — no `settings.json` editing. |
| `docs/` | Design, plan, and postmortem docs. `design.md` is the canonical living design. |
| `dev/dev.kdl` | A dev layout for hot-reloading the plugin while building. |

There are two independent install surfaces: the **sidebar** (the wasm plugin you
add to your Zellij layout) and the **producer** (whatever broadcasts status —
today, the Claude Code plugin).

## Install

### 1. The sidebar plugin

There is no pre-built release yet, so build the wasm from source:

```sh
# Needs a wasm32-wasip1 toolchain — `nix develop` provides one (see docs/TOOLCHAIN.md).
nix develop -c cargo build --release --target wasm32-wasip1
```

Copy it to a **stable path** (a fixed path matters: Zellij ties the plugin's
permission grant to its location, so a per-build Nix store path would re-prompt
every rebuild):

```sh
mkdir -p ~/.config/zellij/plugins
cp target/wasm32-wasip1/release/zj_radar.wasm ~/.config/zellij/plugins/
```

#### Wire it into your layout

The sidebar is a pinned, borderless **left column** that lives in every tab.
Zellij has no "pin a pane across all tabs" mechanism other than the tab
templates — the same place its own tab-bar/status-bar live — so, like
[zjstatus](https://github.com/dj95/zjstatus), radar integrates by adding a pane
to your layout's templates. You compose the radar node into *your* layout
(left or right, any width); we don't hand you a layout to adopt wholesale.

**First, alias the plugin once** in `config.kdl` so the path + default config
live in one place and every layout just refers to the name `radar`:

```kdl
// ~/.config/zellij/config.kdl
plugins {
    radar location="file:~/.config/zellij/plugins/zj_radar.wasm" {
        naming "managed"
    }
}
```

**Then reference `radar` in two tab templates.** A left column forces `children`
to be *nested inside a vertical split*, and that needs **both** templates:

```kdl
// Tabs defined in the layout file get their panes via `children`.
default_tab_template {
    pane split_direction="vertical" {
        pane size=24 borderless=true { plugin location="radar" }   // ← alias
        children
    }
    pane size=2 borderless=true { plugin location="zellij:status-bar" }
}

// Tabs created at runtime (Ctrl+t n) get a CONCRETE focused pane, not `children`.
new_tab_template {
    pane split_direction="vertical" {
        pane size=24 borderless=true { plugin location="radar" }
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

A complete, runnable starting point lives in
[`examples/radar-sidebar.kdl`](examples/radar-sidebar.kdl) — copy it to
`~/.config/zellij/layouts/` and tweak. Want the column on the **right**? Put
`children` (and the runtime `pane`) *before* the radar pane in the split. Want a
different width? Change `size`. The node composes; the layout is yours.

On first load the sidebar shows an onboarding face and requests three
permissions (`ReadApplicationState`, `ReadCliPipes`, `ChangeApplicationState`) —
press `y` to grant. It never runs commands; notifications stay in the producer.

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

#### Installing via Nix / home-manager

This flake exposes the wasm as `packages.default`, so a flake-based config can
consume the exact same artifact this repo builds. Add the repo as an input:

```nix
# flake.nix
inputs.zj-radar.url = "github:mark-toda/zj-radar";
```

Then reference the built wasm at a stable store path in your Zellij layout
derivation (build-from-source, works today):

```nix
plugin location="file:${inputs.zj-radar.packages.${system}.default}/bin/zj_radar.wasm"
```

Once tagged releases exist, you can instead pin a prebuilt artifact without a
Rust toolchain (mirrors the older `room`/`smart-tabs` vendoring this replaces):

```nix
zjRadarWasm = pkgs.fetchurl {
  url = "https://github.com/mark-toda/zj-radar/releases/download/v0.1.0/zj_radar.wasm";
  hash = "sha256-..."; # nix-prefetch-url the asset to fill this in
};
```

The old `@smartTabs@` substitution is fully retired — zj-radar owns the rail.

### 2. The Claude Code producer

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

### Optional: the `zj-radar` CLI

A native binary that drops the `jq`/`bash` dependency and wires non-plugin agents.

```sh
# Nix:
nix build github:mark-toda/zj-radar#zj-radar-cli   # -> result/bin/zj-radar
# Cargo:
cargo install --git https://github.com/mark-toda/zj-radar --features cli
```

- **`zj-radar notify <claude|codex>`** — broadcasts agent status. The Claude
  plugin's hook script automatically prefers it when it's on `PATH` (jq-free);
  otherwise the plugin falls back to its bundled `bash`+`jq` script.
- **`zj-radar setup [codex]`** — idempotently wires Codex's `~/.codex/config.toml`
  `notify` to call `zj-radar notify codex`. It **never overwrites** an existing
  `notify` program (e.g. a Computer Use notifier); pass `--force` to replace it,
  `--dry-run` to preview, `--uninstall` to remove. (Claude needs no `setup` — use
  the plugin in §2.)

Codex reports only turn-completion, so it shows as `done` only (no `working`).

## Configuration

Pass options in the layout's `plugin { ... }` block. Unknown keys are ignored
and invalid values fall back to the default (parsing never fails):

| Key | Values | Default | Effect |
|-----|--------|---------|--------|
| `density` | `cards` · `comfortable` · `compact` | `cards` | Card surface bands / blank separators / flush rail. |
| `naming` | `off` · `managed` · `force` | `managed` | Auto-rename tabs from agent repo / pane title. `managed` only touches default or self-applied names; `force` overrides manual names. |
| `header` | `true` · `false` | `true` | Show the ` RADAR` identity header + tab count. |
| `glyphs` | `plain` · `nerd` | `plain` | Status glyph set (`nerd` needs a Nerd Font). |

```kdl
plugin location="file:~/.config/zellij/plugins/zj_radar.wasm" {
    density "comfortable"
    naming "off"
}
```

These can also be changed **at runtime** without editing the layout, by
broadcasting a flat JSON object on the `zj_radar.config.v1` pipe:

```sh
zellij pipe --name zj_radar.config.v1 -- '{"density":"compact","header":false}'
```

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
nix develop                              # host Rust + wasm32-wasip1 std + zellij
cargo test                               # 135 pure-logic tests, no wasm needed
nix develop -c cargo build --target wasm32-wasip1
zellij --layout dev/dev.kdl              # hot-reload dev session (edit the path inside first)
```

The pure modules (`status`, `payload`, `state`, `model`, `render`, `naming`,
`config`, `theme`) carry no `zellij-tile` dependency and are fully unit-tested
on the host target. Only `lib.rs`/`main.rs` touch the Zellij host API and are
gated behind `#[cfg(target_arch = "wasm32")]`. See
[`docs/TOOLCHAIN.md`](docs/TOOLCHAIN.md).

## Status & roadmap

- ✅ **Sidebar plugin** — tab list, click-to-switch, per-tab agent aggregation,
  overflow folding, theme-derived card surfaces, runtime config.
- ✅ **Claude Code producer** — ships as a Claude plugin (`plugins/zj-radar-claude`).
- ✅ **`zj-radar` CLI** — native, jq-free `notify` (Claude + Codex) and
  conflict-aware `setup`; see [Optional: the `zj-radar` CLI](#optional-the-zj-radar-cli).
- 📋 **Not yet built** — cross-platform prebuilt release binaries and a
  `zj-radar init` sidebar installer. See [`docs/distribution.md`](docs/distribution.md).

## License

MIT — see [`LICENSE`](LICENSE).
