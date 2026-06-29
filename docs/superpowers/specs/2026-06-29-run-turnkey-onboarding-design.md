# `zj-radar run` — turnkey onboarding

**Status:** design approved (2026-06-29)
**Scope:** one implementation plan

## Problem

zj-radar is a *pinned-in-every-tab* rail, which Zellij only supports through tab
templates. That single requirement forces three burdens on every installer that
ordinary (launch-on-demand) plugins never impose:

1. **Template surgery in three places** — `default_tab_template`,
   `new_tab_template` (the zellij#3247 two-template dance), and a `ui` template
   for swap layouts.
2. **Swap-layout participation** — a custom layout silently discards Zellij's
   built-in swaps (`zellij setup --dump-swap-layout default`), so `Alt+]` /
   `Alt+[` no-op unless the layout redeclares them.
3. **An interactive permission grant on a borderless, unfocused rail** — and the
   rail's render historically showed the *same* "watching your tabs" face whether
   it was blocked-on-permission or granted-and-idle, so a blocked install looked
   like a working one.

Every failure observed in practice (dead `Alt+]`, redundant top bar, re-grant on
every rebuild, clobbered home-manager config, blank rail) is a child of one of
these. The integration path can be *documented* better, but it can't be made
foolproof because the layout is user-owned and infinitely varied.

## Approach

Add a turnkey front door, **`zj-radar run`**, that sidesteps integration
entirely: zj-radar owns a complete Zellij config and launches it. When zj-radar
owns the environment, the layout-merge / swap / stable-path / grant-discovery
problems dissolve. The existing `setup zellij` integration path remains for users
who want the rail in their *own* Zellij, and shares the same bundled assets so
the two can't drift.

Decisions settled during brainstorming:

- **Front door:** `run` is the hero path; `setup`/Nix are the "make it permanent
  / integrate into my own Zellij" secondary paths.
- **Config:** `run` uses a zj-radar-**owned** `--config-dir` (Zellij default
  keybinds + radar alias + rail layout + swaps). It never touches
  `~/.config/zellij`.
- **Session model:** per-directory **attach-or-create** (session named for the
  cwd basename; `run <name>` overrides).
- **Wasm delivery:** **embedded** in the CLI via `include_bytes!`, materialized
  to the owned dir. Single self-contained binary.
- **Producer:** **detect + guide** — `run` reports whether a producer is wired
  and prints a one-line hint if not; it never auto-edits agent configs.
- **Grant:** **clean first-run grant, never pre-seed.** (See Grant flow.)

### Rejected: pre-seeding `permissions.kdl`

Zellij records permission grants in a top-level `permissions.kdl` (keyed by the
literal plugin-URL string, no integrity check), so a tool *could* write its own
grant and launch promptlessly. We reject this as an antipattern: it bypasses
Zellij's trust-on-first-use consent UX, couples zj-radar to Zellij's
version/OS-specific internal cache (which isn't relocatable via `--config-dir`),
and writes a *global* grant beyond `run`'s isolated environment. The legitimate
promptless path — a user-authored "trusted plugins" declaration in `config.kdl`
— does not exist in Zellij 0.44; pursuing it is an upstream feature request, not
a cache poke.

## Architecture

`run` is a thin **orchestrator** over Zellij; it renders no UI itself. Five
units, each understandable and testable in isolation:

1. **Embedded assets** — the release wasm (`include_bytes!`), a `config.kdl`
   template (Zellij default keybinds + the `radar` plugin alias), and the
   `radar.kdl` layout (the rail's 3 templates + swap layouts). A build step
   produces the wasm before the CLI embeds it.
2. **Materializer** (the deep module) — given embedded assets + a target dir +
   the CLI version, writes the owned config dir idempotently and returns the
   resolved paths. Re-materializes only on version-marker mismatch. Carries the
   bulk of unit coverage.
3. **Config-dir locator** — computes the owned dir via the `dirs` crate
   (`<data_dir>/zj-radar/zellij/`), matching how Zellij resolves OS paths.
4. **Grant checker** — reads Zellij's `permissions.kdl` (OS cache path) to decide
   whether to print the first-run hint. Read-only; never writes.
5. **Launcher** — derives a sanitized session name from cwd, then execs
   `zellij --config-dir <owned> --layout radar --session <name>`
   (attach-or-create), inheriting the terminal.

The wasm lives at a **version-stable path** (`<owned>/plugins/zj_radar.wasm`):
its bytes are overwritten on upgrade but the path string never changes, so the
one-time grant persists across reruns *and* zj-radar upgrades.

## Materialization lifecycle

Target dir `<data_dir>/zj-radar/zellij/`
(`~/Library/Application Support/zj-radar/zellij/` on macOS,
`~/.local/share/zj-radar/zellij/` on Linux):

```
config.kdl            # zellij default keybinds + radar alias -> ./plugins/zj_radar.wasm
layouts/radar.kdl     # the rail layout (3 templates + swaps)
plugins/zj_radar.wasm # embedded wasm, written verbatim
.zj-radar-version     # marker: CLI version that last materialized
```

On each `run`: compare `.zj-radar-version` to the running CLI version. Match →
no-op fast path, just launch. Mismatch / absent / incomplete dir → rewrite all
four files and update the marker. The `radar` alias references the wasm by
**absolute** materialized path (resolved at materialize-time). If the dir isn't
writable, `run` errors clearly rather than degrading silently — it owns this dir,
so unwritable is a real, reportable problem.

## Grant flow

Two cooperating pieces, no cache-poking:

1. **Honest rail state (plugin code, the backstop).** Split the conflated
   `runtime.rs` render condition (`!permission_granted || tabrows.is_empty()`) so
   the ungranted case renders a distinct, loud face — `⚠ RADAR needs permission —
   focus this pane and press y` — rather than the idle "watching your tabs" copy.
   The plugin already makes the rail selectable during the request, so once the
   instruction is *visible*, the grant is reachable in-session. Ships
   independently of `run` and also fixes the integration path.
2. **`run`'s first-run nudge (CLI).** Before exec, the grant checker reads
   `permissions.kdl`; if the owned wasm path isn't granted, `run` prints one line:
   `First run: focus the RADAR rail (left) and press y to enable agent status.`
   Then it launches normally — the now-honest rail guides the rest.

First `run`: session opens, rail says "press y," user grants once → persists
forever at the stable path. Subsequent runs: silent, rail populated.

`setup zellij` gains a `--grant` helper (the documented
`zellij plugin --floating … file:<wasm>` one-liner) for the integration path,
where the rail lives in the user's own layout.

## Session model & producer detection

**Session naming.** Derived from the cwd basename, sanitized for Zellij
(alphanumerics, `-`, `_`; other chars → `-`; empty/edge cases → `radar`).
`zj-radar run <name>` overrides. Launch is attach-or-create.

**Producer detection.** Before exec, a cheap read-only check reusing `setup`'s
marker logic: Codex (`~/.codex/hooks.json` has the `ZJ_RADAR_CODEX_HOOK` marker),
Claude (marketplace plugin present), native (`zj-radar` on PATH). If none, print
`Agent status off — no producer wired. Run \`zj-radar setup\` to enable.` Never
edits those configs.

## Relationship to `setup` / Nix

`run` and `setup` share the same embedded assets (one source of truth in the
crate) so the turnkey and integration paths can't drift. `setup zellij` is
unchanged except for the new `--grant` helper. The Nix `home-manager` module and
the README Nix-path correction are a **separate follow-up spec**, not built here.

## Testing

- **Materializer** — unit tests over a temp dir: fresh write creates all four
  files; matching marker is a no-op; mismatched/absent marker rewrites;
  unwritable dir errors. The deep module, so the most coverage.
- **Config-dir locator & session-name sanitizer** — pure functions, table-driven
  unit tests (path edge cases, name sanitization).
- **Grant checker** — unit tests over sample `permissions.kdl` content (granted /
  not-granted / missing file / malformed).
- **Honest rail state** — extend the render/insta suite: a snapshot for the
  `!permission_granted` face, distinct from the idle face.
- **`run` end-to-end** — a bats-level smoke test that `zj-radar run --print-cmd`
  (a dry-run flag) materializes the dir and emits the expected
  `zellij --config-dir … --layout radar --session …` invocation without execing
  Zellij (CI-safe, matching the existing test-harness layering).

## Future (separate specs)

- Nix `home-manager` module (`programs.zj-radar.enable`) + README Nix-path fix
  (stable path, not the `/nix/store` path).
- Upstream request to Zellij for a config-declared trusted-plugins mechanism (the
  only legitimate promptless grant).
- `setup zellij --emit-layout` and a `--check`/doctor that flags missing swaps,
  `/nix/store` aliases, and read-only (home-manager) configs before clobbering.
