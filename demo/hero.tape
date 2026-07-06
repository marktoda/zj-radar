# zj-radar — hero demo (vhs tape, template).
# `record.sh` substitutes __ROOT__/__WASM__/__CFG__/__LAYOUT__, seeds the plugin
# permission grant, and runs vhs on the concrete copy in target/demo/. Requires:
# vhs, zellij, ffmpeg, ttyd, and JetBrainsMono Nerd Font on the recording host.
#
# The story ("the needs-you moment"): you're working on `web` while other
# agents run; `api` blocks on approval and the RAIL tells you — bell, `1!`
# badge, footer tally, and the actual question on a `↳` line — without you
# leaving your tab. You jump over, answer, everything settles.

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

# Beat 0 — a brief title card that names the tool and pre-teaches the glyphs,
# so a cold viewer knows what they're looking at before the rail appears. It
# also gives the loop a clean boundary.
Hide
Type "export PS1='$ '" Enter
Type "clear" Enter
Type "__ROOT__/demo/banner.sh" Enter
Show
Sleep 2500ms

# Launch the session off-camera and resume recording once the rail has
# populated — so the GIF never shows the (long) launch command or a blank load.
# record.sh has pre-granted the plugin's permissions, so there's no prompt.
Hide
Type "clear" Enter
Type "zellij --config __CFG__ --layout __LAYOUT__" Enter
Wait+Screen@30s /api/
Sleep 1200ms
Show

# Beat 1 — you're working elsewhere: focus opens on `web` (two agents rolled up
# as a ├/└ tree) while api/tests/infra spin in the rail; footer reads
# `4 working`. No action — just let the scene establish until…
#
# Beat 2 — the flip: `api` goes pending off-screen and the RAIL carries it —
# bell glyph, header `1!`, footer `3 working · 1 need you`, and the question
# itself on the `↳` line. Anchoring on the question text (it appears nowhere
# else on screen) syncs the tape to the arc regardless of startup lag.
Wait+Screen@20s /migration/
Sleep 2s
Screenshot "__ROOT__/docs/media/needs-you.png"
Sleep 1s

# Beat 3 — jump and answer: over to `api` via Zellij's tab mode (Ctrl+t then
# the tab number; the digit returns to normal mode, no Escape). The pane shows
# the Claude session blocked at `Run migration? (y/n)`; the typed `y` is read
# by the arc itself, so the resume is keystroke-driven, not timed. The rail's
# question line vanishes and the badge/tally clear.
Ctrl+t
Sleep 600ms
Type "1"
Sleep 1500ms
Type "y"
Wait+Screen@10s /sqlx/
Sleep 1s

# Beat 4 — settle: tests pass, infra errors out, the web agents finish — the
# full glyph spread (● ✓ ✗) with the answered api on top. Anchor on the tests
# summary reaching the rail, give the last arcs a beat to land, then hold.
Wait+Screen@15s /48 passed/
Sleep 2500ms
Screenshot "__ROOT__/docs/media/states.png"
Sleep 2s

# Stop recording BEFORE Ctrl+q so Zellij's "Bye from Zellij!" quit screen never
# lands in the GIF.
Hide
Ctrl+q
Sleep 1s
