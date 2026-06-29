# zj-radar — hero demo (vhs tape, template).
# `record.sh` substitutes __ROOT__/__WASM__/__CFG__/__LAYOUT__, seeds the plugin
# permission grant, and runs vhs on the concrete copy in target/demo/. Requires:
# vhs, zellij, ffmpeg, ttyd, and JetBrainsMono Nerd Font on the recording host.

Require zellij

Output "__ROOT__/docs/media/hero.gif"

Set Shell "bash"
Set FontFamily "JetBrainsMono Nerd Font"
Set FontSize 22
Set Width 1000
Set Height 720
Set Padding 24
Set Theme "TokyoNight"
Set Framerate 24
Set CursorBlink false
# Skip the launch/blank-load frames in the looping GIF; the file still plays
# them once from the top. ~15% lands just as the first rows populate.
Set LoopOffset 15%

# Quiet prompt, then clear the screen for a clean launch.
Hide
Type "export PS1='$ '" Enter
Type "clear" Enter
Show
Sleep 500ms

# Launch a fresh (auto-named) session. No --session name, so reruns never
# collide with a stale/exited session. record.sh has pre-granted the plugin's
# permissions, so there's no prompt — the four sidebars come up and the agents
# start broadcasting their scripted arcs.
Type "zellij --config __CFG__ --layout __LAYOUT__" Enter

# t≈0..7: all four rows working — spinners + elapsed animate on the 1s timer,
# and the focused `tests` pane streams cargo-test output beside the rail.
Sleep 7s
# `api` blocks on a permission prompt → needs you (pink bell). The money shot.
Screenshot "__ROOT__/docs/media/needs-you.png"
Sleep 4s
# `web` ● done, `tests` ● done, `infra` ✗ error — the full status spread.
Screenshot "__ROOT__/docs/media/states.png"
Sleep 3s

# Quit cleanly so vhs stops.
Ctrl+q
Sleep 1s
