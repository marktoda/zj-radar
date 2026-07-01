# Troubleshooting

The sidebar lives in your tab templates and exists once per tab, which bumps
into a few sharp edges in Zellij itself. Here is what they look like and why.

## Sidebar renders, but no status ever appears

**Symptom:** the rail draws your tabs, but agents work and finish invisibly —
every row stays idle.

The rail and the status feed are separate installs (sidebar vs
[producer](producers.md)), so a rendering rail says nothing about the feed.
Diagnose in order:

1. **`zj-radar setup zellij --check`** — the `producer` item says whether a
   producer (the Claude plugin or Codex hooks) is wired at all; the `grant`
   item catches a missing permission grant.
2. **Bypass the producers with the smoke test** — broadcast a fake status
   straight from a shell *inside* the session (see
   [Writing your own producer](producers.md#writing-your-own-producer) for the
   full command):

   ```sh
   zellij pipe --name zj_radar.status.v1 -- \
     '{"v":1,"source":"test","pane":{"type":"terminal","id":0},"status":"running","repo":"demo","msg":"hello"}'
   ```

   If a row lights up, the sidebar is fine and the producer is the problem; if
   nothing happens, re-check the grant and that the sidebar pane is actually
   this session's (reload after `--download` updates the wasm).
3. **Producer prerequisites.** The agent must run *inside* the Zellij session
   (the hooks no-op without `$ZELLIJ_PANE_ID`, e.g. in a plain terminal or
   over ssh without Zellij). The Claude plugin's bash fallback additionally
   needs `jq` — without it the hook silently no-ops; installing the `zj-radar`
   CLI removes that dependency.
4. **Version skew.** The sidebar requires Zellij 0.44.x (unstable plugin ABI);
   on other versions the wasm can fail to load entirely, which presents as a
   blank or missing rail rather than an idle one.

## Can't open a new tab (the two-template rule)

**Symptom:** new tabs created at runtime (`Ctrl+t n`) contain only the sidebar
and status bar — no focusable pane — so keystrokes have nowhere to land and you
"can't open a new tab."

**Why:** when you don't supply a `new_tab_template`, Zellij *derives* one from
`default_tab_template` — and that derivation **drops a `children` placeholder
nested inside a split** (upstream
[zellij-org/zellij#3247](https://github.com/zellij-org/zellij/issues/3247),
still open). A *top-level* `children`, like the stock compact layout, materializes
fine — only the nested-in-a-split case is affected, which is exactly how the
sidebar pins itself.

**Fix:** declare `new_tab_template` explicitly with a concrete `pane focus=true`
(instead of `children`), as shown in the
[layout snippet](install.md#add-the-sidebar-to-a-layout). That sidesteps the
derivation entirely.

## First-run prompt coordination

**Symptom:** on a fresh layout the sidebar requests permissions in one tab while
the others wait; occasionally a late-spawned sidebar starts empty until the next
broadcast.

**Why:** because the sidebar exists once per tab, only one instance owns the
first-run prompt when session files are writable; the others wait for Zellij's
cached answer and then continue without asking again. Session files use Zellij's
shared plugin cache when available and fall back to `/tmp/zj-radar`.

**If neither is writable:** the sidebar still runs, but late-spawned sidebars may
start empty until the next broadcast, and first-run prompt coordination may be
noisier (more than one instance may prompt). Pre-approving the plugin URL once in
a floating pane — see
[First-run permission prompt](install.md#first-run-permission-prompt) — gives
every later per-tab sidebar a cached grant to reuse.

## Cards look flat — no colored row backgrounds

**Symptom:** the rail renders and the status dots are colored, but the per-row
"card" surface tints (focused row brighter, agent rows mid, idle dimmest) are
missing — every row shares the terminal's default background.

**Why:** the card surfaces and the recessed idle/dim text are emitted as
**truecolor (24-bit)** SGR escapes. A terminal without truecolor support (e.g.
macOS Terminal.app, the Linux VT console, or a `tmux` not configured with
`Tc`/`RGB`) silently ignores those escapes — they are well-formed SGR, so
nothing breaks: the character grid, click targeting, and the ANSI-16 status
hues are all unaffected. Only the surface shading is absent.

**Fix:** use a truecolor-capable terminal (Alacritty, Kitty, WezTerm, iTerm2,
foot, most modern emulators). Inside `tmux`, enable truecolor passthrough
(`set -as terminal-features ',*:RGB'`). There is no functional loss without it.

## Rail glyphs spill past the sidebar edge

**Symptom:** on some terminals a status glyph (`●`, `◆`, `✗`, the `═` rule, the
tree connectors) pushes a row one column too wide, so its tail bleeds into the
pane next to the rail.

**Why:** the rail budgets its status glyphs as **one column** each. Several of
them are Unicode *East-Asian-Width Ambiguous* codepoints, which a terminal
configured with **ambiguous width = double** (common in CJK setups) renders as
two columns — so the layout under-counts and the line overruns. Likewise, an
agent message containing an emoji with an explicit presentation selector
(`⚠️`, base + U+FE0F) may be measured one column narrower than it draws.

**Fix:** set your terminal's ambiguous-character width to **narrow/single**
(Kitty `narrow_symbols` / default; WezTerm `treat_east_asian_ambiguous_width_as_narrow = true`;
iTerm2 *Prefs → Profiles → Text → “Treat ambiguous-width characters as double width” off*).
This is the width contract the rail assumes.

## Zellij plugin-reload quirks

**Symptom:** during development, reloading the plugin opens an extra tiled plugin
pane.

**Why:** Zellij 0.44's plugin reload actions can open an extra tiled plugin pane
when the target plugin was created by a layout and has made itself non-selectable
(as the radar sidebar does after permissions).

**Fix:** the default inside-Zellij dev loop (`./dev/run.sh`) avoids both in-place
plugin reloads and session switching for exactly this reason. See
[Development in the README](../README.md#development).
