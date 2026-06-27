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

Add it as a pinned, borderless left column in your layout's
`default_tab_template` — **outside** `children`, so swap-layout cycling never
disturbs it:

```kdl
default_tab_template {
    pane split_direction="vertical" {
        pane size=24 borderless=true {
            plugin location="file:~/.config/zellij/plugins/zj_radar.wasm"
        }
        children
    }
    pane size=2 borderless=true { plugin location="zellij:status-bar" }
}
```

On first load the sidebar shows an onboarding face and requests three
permissions (`ReadApplicationState`, `ReadCliPipes`, `ChangeApplicationState`) —
press `y` to grant. It never runs commands; notifications stay in the producer.

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
plugin location="file:${inputs.zj-radar.packages.${system}.default}/lib/zj_radar.wasm"
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
- 📋 **Designed, not yet built** — a native `zj-radar` CLI (universal `notify` +
  idempotent `setup`) to drop the `jq`/`bash` dependency, and a Codex adapter.
  See [`docs/cli-design.md`](docs/cli-design.md) and
  [`docs/distribution.md`](docs/distribution.md).

## License

MIT — see [`LICENSE`](LICENSE).
