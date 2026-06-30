# zj-radar — hero demo (vhs tape, template).
# `record.sh` substitutes __ROOT__/__WASM__/__CFG__/__LAYOUT__, seeds the plugin
# permission grant, and runs vhs on the concrete copy in target/demo/. Requires:
# vhs, zellij, ffmpeg, ttyd, and JetBrainsMono Nerd Font on the recording host.

Require zellij

Output "__ROOT__/docs/media/hero.gif"

Set Shell "bash"
Set FontFamily "JetBrainsMono Nerd Font"
Set FontSize 22
Set Width 960
Set Height 760
Set Padding 20
Set Theme "TokyoNight"
Set Framerate 24
Set CursorBlink false

# Open on a brief title card that names the tool and pre-teaches the glyphs, so
# a cold viewer knows what they're looking at before the rail appears.
Hide
Type "export PS1='$ '" Enter
Type "clear" Enter
Type "__ROOT__/demo/banner.sh" Enter
Show
Sleep 3200ms

# Then launch the session off-camera and resume recording once the rail has
# populated — so the GIF never shows the (long) launch command or a blank load.
# record.sh has pre-granted the plugin's permissions, so there's no prompt.
Hide
Type "clear" Enter
Type "zellij --config __CFG__ --layout __LAYOUT__" Enter
Wait+Screen@30s /api/
Sleep 1200ms
Show

# Beats are synced to on-screen text (Wait), not fixed sleeps, so they're robust
# to startup lag and stay tight. `api` (focused) shows the Claude agent working,
# then blocking on approval; meanwhile the rail tracks web/tests/infra.
Wait+Screen@20s /migration/
Screenshot "__ROOT__/docs/media/needs-you.png"
Sleep 1s

# Tab over to `web` — a tab running two Codex agents — via Zellij's tab mode
# (Ctrl+t then the tab number; the digit returns to normal mode, no Escape).
# The rail rolls the two panes up with tree connectors; focusing shows both.
Ctrl+t
Sleep 600ms
Type "2"
# Dwell on the web tab while infra errors out and the web agents finish/keep
# spinning (Sleep, not Wait — the rail's text moves around after the tab switch,
# which makes Wait+Screen unreliable here).
Sleep 3s
Screenshot "__ROOT__/docs/media/states.png"
Sleep 1500ms

# Closing beat: open a fresh tab. The sidebar follows into it (new_tab_template),
# so the rail gains a new ○ idle tab — a quiet, on-brand sign-off that also shows
# the sidebar lives in every tab.
Ctrl+t
Sleep 400ms
Type "n"
Sleep 1200ms
Type "echo '✦ zj-radar'" Enter
Sleep 3s

# Stop recording BEFORE Ctrl+q so Zellij's "Bye from Zellij!" quit screen never
# lands in the GIF.
Hide
Ctrl+q
Sleep 1s
