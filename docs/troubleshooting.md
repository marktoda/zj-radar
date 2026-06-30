# Troubleshooting

The sidebar lives in your tab templates and exists once per tab, which bumps
into a few sharp edges in Zellij itself. Here is what they look like and why.

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

## Zellij plugin-reload quirks

**Symptom:** during development, reloading the plugin opens an extra tiled plugin
pane.

**Why:** Zellij 0.44's plugin reload actions can open an extra tiled plugin pane
when the target plugin was created by a layout and has made itself non-selectable
(as the radar sidebar does after permissions).

**Fix:** the default inside-Zellij dev loop (`./dev/run.sh`) avoids both in-place
plugin reloads and session switching for exactly this reason. See
[Development in the README](../README.md#development).
