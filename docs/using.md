# Using the rail

What the sidebar shows, what the glyphs mean, and what clicking does. The exact
grid for every scenario is in [`rail-reference.md`](rail-reference.md); this
page is the reader's version.

## Anatomy

```
 RADAR                     ·3 1!   ← header: tab count, needs-you badge
════════════════════════════════   ← rule (sweeps a ◆ while anything is working)
▌⠋ 1 pinky                         ← focused tab: spine, status, number, name
 ◆ 2 review                    ✓   ← tab that needs you; ✓ acknowledges it
 ├ ◆ ✳ migrate schema          ✓   ← pane row: status, kind mark, task
 │   ↳ approve git push?           ← the question it is blocked on
 └ ⠋ ❉ write insta tests           ← last pane in the tab
 ○ 3 notes                         ← plain terminal, nothing tracked
─ earlier ──────────────────────   ← recent completions, newest first
2m ● pinky cargo test
                                   ← spacer
────────────────────────────────
2 working · 1 need you             ← footer tally
```

**Header.** ` RADAR` plus `·N` tabs. When a tab needs you, `N!` appears in
bold. While any bounded work is running, a `◆` marches along the rule once a
second (compact and comfortable densities only).

**Tab rows.** Column 0 is the spine: `▌` marks the focused tab. The glyph is
the tab's dominant status, most urgent first: `✗` error, `◆` needs you, `⠋`
working, `●` done, `○` idle. Then the tab number and name.

**Pane rows.** Every tracked pane in a tab gets a line under it, joined by
`├` / `└`. Each line is status glyph, kind mark, then text. The text is the
agent's current task when one is known, otherwise its latest activity. A pane
waiting on you adds a `↳` line with the question. A tab shows at most six pane
lines; the rest fold into `+N more`.

**Kind marks.** `✳` claude · `❉` codex · `✺` opencode · `✦` gemini ·
`$` command · `⚙` build · `⚗` test · `⇡` deploy · `❯` server · `⦿` other.

**Time tags.** Time appears only where it costs you something: a pending pane
shows how long it has waited (`· 4m`), and a long-running build or test shows
how long it has run. Both are whole minutes, frozen at `1h+`.

**Shell commands.** Builds, tests, and deploys spin like agents. A dev server
holds a steady `▸` instead, because it never finishes. Editors, pagers, and
other interactive programs never spin; the pane shows a muted label such as
`○ $ nvim README.md`. Add your own to the quiet set with the
`interactive_commands` option. Why each class looks the way it does is in
[`activity-model.md`](activity-model.md).

**Overflow.** When tabs outnumber the lines available, trailing idle tabs fold
into a `+N idle ▾` strip and the header count gains `▲`.

**Footer.** With two or more spare lines the rail pins a rule and a tally to the
bottom: `N working`, plus `· M need you` when M is nonzero. Above it, an
`─ earlier ─` section lists recent completions (newest first, up to ten) with
a relative age. Click one to jump to its tab if the tab still exists.

**Densities.** `cards` (the default) paints each tab as a tinted band using
your theme's colors; `comfortable` separates tabs with a blank line;
`compact` is flush. Card tints need a truecolor terminal.

**Tab names.** With `naming "managed"` (the default) the rail names tabs after
the repo an agent is working in, falling back to the pane's directory or
title. It never overwrites a name you set by hand.

## Mouse

- **Click a tab row** to switch to that tab. **Click a pane row** to switch to
  the tab and focus that pane.
- **Click `✓`** on a row that says needs-you to acknowledge it. The row becomes
  `done` in every tab's rail. Use it when an agent ends a finished turn with a
  courtesy question ("want me to also…?"). A question that actually blocks the
  agent clears on its own when you answer it.
- **Click `✕`** on a dimmed session line in the badge to dismiss a session you
  know is dead. If it turns out to be alive, its next heartbeat brings it back.
- Right-click does nothing yet: Zellij does not deliver right-clicks to
  plugins ([zellij#5350](https://github.com/zellij-org/zellij/issues/5350)).

## Keybinds

Nothing is bound by default. Four verbs are available for your own bindings,
delivered over the `zj_radar.cmd.v1` pipe:

- `attention-next` / `attention-prev` cycle focus through the tabs that need
  you, errored, or finished, in tab order.
- `session-next` / `session-prev` step the cross-session badge selection (below).

Bindings for all four, and for changing options from a key, are in
[`configuration.md`](configuration.md#binding-keys-to-commands).

## Cross-session badge

Run zj-radar in more than one Zellij session on the same machine and each rail
grows one line per session: your session first, then sessions that need
attention, then the rest. Each line shows the session name with its working
and needs-you agent-pane counts. With a single session the badge is invisible.

- **Click a line** to switch to that session, landing on its attention tab if
  it has one.
- **`session-next` / `session-prev`** move a highlight through the same order.
  The switch happens about a second after your last tap; landing back on your
  own session cancels.
- A session that stops heartbeating dims after 90 seconds and is removed after
  five minutes. Dimmed sessions are skipped by cycling and carry a `✕` to
  dismiss by hand.

Sessions find each other through Zellij's shared plugin cache directory. If no
writable shared directory exists, the badge never appears and nothing else
changes.

## Notifications

The rail sends a desktop notification when a pane changes to `done`, `error`,
or `pending`. Defaults:

- Only for panes you are not looking at. Set `notify_when_focused true` to
  include the focused pane.
- One notification per event, even though the rail runs one instance per tab.
- Acknowledging a row with `✓` never notifies.
- Delivery uses `osascript` on macOS and `notify-send` on Linux. Without either
  on `PATH`, or without the `RunCommands` permission, notifications are
  silently skipped and everything else works.

Turn them off with `notify false`, or per status with `notify_done`,
`notify_error`, and `notify_pending`. See [`configuration.md`](configuration.md).
