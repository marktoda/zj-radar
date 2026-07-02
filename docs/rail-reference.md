# Rail rendering reference — spec by example (executable)

> **Status:** SHIPPED — the executable spec for the rendered rail (originally
> proposed 2026-06-28). This file is the **test oracle**: each scenario carries
> a `rail-input` block (the state) and a `rail-expect` block (the exact
> ANSI-stripped grid). A crate test (`crates/plugin/src/reference_tests.rs`)
> parses them, runs the real `aggregate` + `render_rail`, and asserts the grid
> matches (see *Using this as tests* at the end). Edit a block → edit the test.

## Design rules (this revision)

1. **Width 32 columns.** The layout snippets all use `size=32` to match. ⟦D8⟧
2. **Elapsed is pending-only.** No elapsed/timer on tab lines or calm pane
   lines. The one exception (the ⟦D-timer⟧ "per-pane, not per-tab" answer,
   scoped to where waiting is *costly*): a `pending` pane's identity line
   carries a `· Nm` wait tag once it has waited ≥ 1 minute — whole minutes,
   frozen at `1h+` (the ledger's saturate window, so the Slow heartbeat can
   disarm). Under a minute, and for every other status, lines stay bit-identical
   to the tagless rail. See scenario T5. ⟦D-timer ✓ pending-only⟧
3. **One line per real pane — no collapsing.** Every *tracked* pane (an agent or
   a real command) gets its own line regardless of status or tab focus. No
   tally, no `+N verb` — those were hard to parse.
4. **Prompt programs are not panes.** `starship`/`zsh`/`bash`/… never surface as
   a pane line (the `$ (starship)` phantom is gone). A pane that has only ever
   run the shell prompt is untracked → no line. ⟦D4 ✓ locked⟧
5. **Safety cap 6.** At most 6 pane lines per tab; beyond that, a final
   `+N more` line. (High on purpose — the common case never folds.) ⟦D6 ✓ = 6⟧
6. **Position order.** Tabs and panes render in position order — no
   float-to-top. ⟦D7 ✓ locked⟧
7. **No right-slot for now.** Status is carried by the glyph; we dropped the
   count/elapsed slot to keep lines clean. `done/total` may return later. ⟦D1⟧
8. **Empty/initial is not a marketing screen.** No "AI agent activity" legend —
   just render the tab list; an unnamed/first tab shows a placeholder name.
   Now true end-to-end (Task 14): the zero-tab onboarding face itself dropped
   the status-glyph legend down to a one-line ` scanning… no agents yet`. That
   face is a separate code path from this doc's harness (see §A's note below)
   — `render_rail` and `aggregate` are what this file pins.

## Vocabulary

**Status glyphs (plain):** `○` idle · `⠋` working *(spins ⠋⠙⠹⠸⠼⠴⠦⠧⠋⠏)* · `◆` needs-you ·
`●` done · `✗` error.
**Kind marks:** `✳` claude · `❉` codex · `✦` gemini · `$` command · `⚙` build ·
`⚗` test · `⇡` deploy · `❯` server · `⦿` other.

**Width ruler (32):**

```
0         1         2         3
01234567890123456789012345678901
```

**Line anatomy:**

```
 RADAR                        ·N   ← title + tab count (·N; "N▲" when overflowing)
════════════════════════════════   ← rule (32 wide)
 RADAR                     ·N n!   ← same, plus the needs-you badge when n>0
════════════════════════════════   ← rule (32 wide)
▌⠋ 2 name                          ← tab row: [spine][glyph] [num] [name]
▌├ ⠋ ❉ activity message            ← pane row: [spine][conn] [glyph] [mark] [msg]
▌└ ● ⚙ activity message            ← last pane row uses the └ elbow connector
```

**Needs-you badge (Task 16).** The header's right slot appends `{n}!` — bold,
loud (`Attention` role) — space-joined after the census whenever `n =
rows.iter().filter(|r| r.display.status.needs_you()).count()` is nonzero (i.e.
any tab is `Pending` or `Error`). At narrow widths, priority to keep is
overflow marker > badge > plain census: a tight budget drops the census
first and shows the bare badge (`n!` with no leading `·N`); the overflow
marker itself is never dropped for the badge's sake.

**Header heartbeat sweep (Task 20).** In Compact/Comfortable density only (the
densities with the `═` rule — Cards drops the rule entirely, so it never
carries the heartbeat), whenever any row's `display.status == Status::Running`
the rule line swaps one `═` character for a `◆` (`Role::Accent`, bold) at
column `now_tick % width`, wrapping around as the tick advances — a pure,
stateless function of `now_tick` that marches one column per render tick
while any tab is actively working, and disappears the instant no row is
`Running` (idle, or every row settled to Done/Error/Pending). Every fixture
below is captured at the doc harness's fixed `now_tick = 0`, so a Compact/
Comfortable scenario with a `Running` row shows the `◆` at column 0 (the
rule's leftmost `═` is replaced): `◆═══════════════════════════════`.

**Col 0 is always the spine column** — reserved on every line, active or not:
`▌` for the focused tab, a plain space otherwise. This holds line 1 (the tab
row) and every pane/child row to the same fixed columns regardless of focus,
so the glyph/number/name never shift left by a column just because a row is
inactive. Tab **glyph** = dominant status.
**Multi-pane** tabs (>1 tracked pane) join their pane lines to the tab with a
tree connector at column 1: `├` for every child that has a sibling (or a
`+N more` line) below it, `└` for the last visible child. The connector sits one
column right of the spine, so the prefix is 3 cols (`[spine/space][conn][space]`)
and the status glyph aligns at column 3. **Single-pane** tabs are connector-free:
they put the one pane's message on line 2 with just its mark (`  ‹mark› ‹msg›`).

---

## A. Empty / initial (no agents yet)

```rail-input
width 32
tab 1 "shell"
```
```rail-expect
 RADAR                        ·1
════════════════════════════════
 ○ 1 shell
```

> No legend, no permission marketing. Just the tab list. An unnamed tab renders
> a placeholder ("shell"/the layout name). ⟦D9: placeholder text⟧
>
> This is the *one-tab* placeholder case, not the *zero-tab* one — with no
> tracked tabs at all (cold start, or every tab closed with no completion
> history), `PluginRuntime::render` routes to a distinct `onboarding()` face
> instead of `render_rail`: ` RADAR` + the `═` rule + a blank line + one muted
> ` scanning… no agents yet`. That face bypasses `render_rail` entirely, so it
> isn't a `rail-input`/`rail-expect` case here — see
> `render::tests::zero_state_is_a_scanning_one_liner`. (Zero tabs WITH
> completion history still goes through `render_rail`, header `·0` and all —
> see §AB's ledger scenario.)

## B. Single plain tab (idle)

```rail-input
width 32
tab 4 "notes"
```
```rail-expect
 RADAR                        ·1
════════════════════════════════
 ○ 4 notes
```

## C. Single agent — working

```rail-input
width 32
tab 1 "pinky"
  claude running "running tests…"
```
```rail-expect
 RADAR                        ·1
◆═══════════════════════════════
 ⠋ 1 pinky
 └ ⠋ ✳ running tests…
```

> Single-pane tabs render their one tracked pane through the SAME tree
> machinery as multi-pane children (§H): `└` elbow, status glyph, identity
> mark, activity. One layout to learn — a tab with one pane and a tab with
> three scan identically.

## D. Single agent — needs you

```rail-input
width 32
tab 3 "api"
  claude pending "approve edit?"
```
```rail-expect
 RADAR                     ·1 1!
════════════════════════════════
 ◆ 3 api
 └ ◆ ✳ approve edit?
```

## E. Single agent — done

```rail-input
width 32
tab 1 "dotfiles"
  claude done "refactored the dotfiles"
```
```rail-expect
 RADAR                        ·1
════════════════════════════════
 ● 1 dotfiles
 └ ● ✳ refactored the dotfiles
```

## F. Single agent — error

```rail-input
width 32
tab 2 "build-svc"
  claude error "exit 1: cargo test failed"
```
```rail-expect
 RADAR                     ·1 1!
════════════════════════════════
 ✗ 2 build-svc
 └ ✗ ✳ exit 1: cargo test failed
```

## G. Single command — build running

```rail-input
width 32
tab 1 "web"
  build running "cargo build"
```
```rail-expect
 RADAR                        ·1
◆═══════════════════════════════
 ⠋ 1 web
 └ ⠋ ⚙ cargo build
```

## H. Multi-pane — the `af` case (line per pane)

**Codex working + a finished build. The idle shell prompt is excluded (D4).**

```rail-input
width 32
tab 2 "af"
  codex running "exploring render"
  build done "cargo build"
```
```rail-expect
 RADAR                        ·1
◆═══════════════════════════════
 ⠋ 2 af
 ├ ⠋ ❉ exploring render
 └ ● ⚙ cargo build
```

> **Today** an unfocused `af` shows only `+1 working`. **Target:** both real
> panes are visible as their own lines; the idle prompt simply isn't a pane.

## I. Multi-pane — needs-you + working

```rail-input
width 32
tab 4 "review"
  claude pending "approve diff?"
  codex running "writing tests"
```
```rail-expect
 RADAR                     ·1 1!
════════════════════════════════
 ◆ 4 review
 ├ ◆ ✳ approve diff?
 └ ⠋ ❉ writing tests
```

> Tab glyph is `◆` (dominant = needs-you). Panes in position order.

## J. Multi-pane — mixed working + idle (idle pane IS tracked)

**A pane that ran something and is now idle still gets a line (line-per-pane).**

```rail-input
width 32
tab 2 "af"
  codex running "exploring render"
  command idle "./deploy.sh"
```
```rail-expect
 RADAR                        ·1
◆═══════════════════════════════
 ⠋ 2 af
 ├ ⠋ ❉ exploring render
 └ ○ $ ./deploy.sh
```

> Contrast J with H: in H the second pane finished (`● done`); here it went
> idle (`○`). Either way it's a line — no collapse. (Whether idle panes should
> eventually drop is ⟦D3⟧.)

## K. Many panes — safety cap (7 working → 6 + "+1 more")

```rail-input
width 32
tab 2 "swarm"
  codex running "planning api"
  codex running "writing tests"
  codex running "refactoring"
  codex running "reviewing pr"
  codex running "docs pass"
  codex running "benchmarks"
  codex running "fuzzing"
```
```rail-expect
 RADAR                        ·1
◆═══════════════════════════════
 ⠋ 2 swarm
 ├ ⠋ ❉ planning api
 ├ ⠋ ❉ writing tests
 ├ ⠋ ❉ refactoring
 ├ ⠋ ❉ reviewing pr
 ├ ⠋ ❉ docs pass
 ├ ⠋ ❉ benchmarks
 └ +1 more
```

> Cap = 6 pane lines (the tab line + 6 = 7 rows), then `+N more`. ⟦D6⟧

## L. Many tabs — overflow folding into the idle strip

```rail-input
width 32
height 9
tab 1 "review"
  claude pending "approve diff?"
tab 2 "af"
  codex running "exploring render"
tab 3 "dotfiles"
  claude done "refactored auth"
tab 4 "notes"
tab 5 "scratch"
tab 6 "logs"
```
```rail-expect
 RADAR                     6▲ 1!
◆═══════════════════════════════
 ◆ 1 review
 └ ◆ ✳ approve diff?
 ⠋ 2 af
 └ ⠋ ❉ exploring render
 ● 3 dotfiles
 └ ● ✳ refactored auth
+3 idle ▾
```

> Header shows `6▲` (overflow). Trailing plain-idle tabs fold into `+N idle ▾`.
> Position order preserved.

## M. Focused tab — spine only (focus ≠ content)

```rail-input
width 32
tab 2 "af" active
  codex running "exploring render"
  build done "cargo build"
```
```rail-expect
 RADAR                        ·1
◆═══════════════════════════════
▌⠋ 2 af
▌├ ⠋ ❉ exploring render
▌└ ● ⚙ cargo build
```

> Identical content to H; focus only adds the `▌` spine (and a bg highlight, not
> visible in the stripped grid). This is the core fix: focus changes decoration,
> not which panes show.

## N. Safety cap — exactly 6 panes (no overflow)

**Author-from-intent.** Exactly at the cap: 6 tracked panes → 6 lines, NO `+N more`.

```rail-input
width 32
tab 1 "swarm"
  codex running "pane one"
  codex running "pane two"
  codex running "pane three"
  codex running "pane four"
  codex running "pane five"
  codex running "pane six"
```
```rail-expect
 RADAR                        ·1
◆═══════════════════════════════
 ⠋ 1 swarm
 ├ ⠋ ❉ pane one
 ├ ⠋ ❉ pane two
 ├ ⠋ ❉ pane three
 ├ ⠋ ❉ pane four
 ├ ⠋ ❉ pane five
 └ ⠋ ❉ pane six
```

> 6 panes = exactly the cap; the `+N more` line does not appear. ⟦D6: cap=6⟧

## O. Safety cap — 8 panes (`+2 more`)

**Author-from-intent.** Two over the cap: 8 tracked panes → 6 lines + `+2 more`.

```rail-input
width 32
tab 1 "swarm"
  codex running "pane one"
  codex running "pane two"
  codex running "pane three"
  codex running "pane four"
  codex running "pane five"
  codex running "pane six"
  codex running "pane seven"
  codex running "pane eight"
```
```rail-expect
 RADAR                        ·1
◆═══════════════════════════════
 ⠋ 1 swarm
 ├ ⠋ ❉ pane one
 ├ ⠋ ❉ pane two
 ├ ⠋ ❉ pane three
 ├ ⠋ ❉ pane four
 ├ ⠋ ❉ pane five
 ├ ⠋ ❉ pane six
 └ +2 more
```

> 8 panes, cap 6: `8 - 6 = 2` remainder → `+2 more`. ⟦D6⟧

## P. Truncation at width 32

**Author-from-intent.** Multi-pane tab: pane line prefix is `" " + conn(1) + " " + glyph(1) + " " + mark(1) + " "` = 7 visible cols; avail = 25; truncate at 24 chars + `…`. The 51-char msg is clipped to `this message is quite lo…`.

```rail-input
width 32
tab 1 "work"
  claude running "this message is quite long and will be truncated here"
  build done "ok"
```
```rail-expect
 RADAR                        ·1
◆═══════════════════════════════
 ⠋ 1 work
 ├ ⠋ ✳ this message is quite lo…
 └ ● ⚙ ok
```

> Prefix = 7 cols; avail = 25; budget = 24 + `…`. Exercises `emit_pane_line` truncation. ⟦D8: width=32⟧

## Q. CJK / wide-char message at width 32

**Author-from-intent.** Multi-pane tab with a CJK message. CJK chars are 2 display cols each; prefix = 7 cols; avail = 25; budget = 24 display cols. "処理中のメッセージが長すぎるケース" (17 chars, 34 display cols) → first 12 chars (24 display cols) fit; 13th would exceed, so result = "処理中のメッセージが長す" + `…` (25 display cols incl. ellipsis; prefix 7 + 25 = 32 = width). No rendered line exceeds width=32.

```rail-input
width 32
tab 1 "cjk"
  claude running "処理中のメッセージが長すぎるケース"
  build done "ok"
```
```rail-expect
 RADAR                        ·1
◆═══════════════════════════════
 ⠋ 1 cjk
 ├ ⠋ ✳ 処理中のメッセージが長す…
 └ ● ⚙ ok
```

> CJK chars are width-2; unicode-width truncation keeps the line at ≤32 display cols.

## R. Bell marker in tab line

**Author-from-intent.** A tab with `bell` renders `⚑` at the right side of the tab line (2-col slot: `⚑` + trailing space, which is trimmed). For `"alerts"` (6 chars) at width=32: prefix=5 (col-0 spine/space + glyph + sp + num + sp), bell_len=2, name_budget=25, gap=32-5-6-2=19 → ` ○ 1 alerts` + 19 spaces + `⚑`.

```rail-input
width 32
tab 1 "alerts" bell
```
```rail-expect
 RADAR                        ·1
════════════════════════════════
 ○ 1 alerts                   ⚑
```

> Bell token on the `tab` line sets `has_bell=true`; the `⚑` glyph appears right-aligned. Tab-line trailing space after `⚑` is trimmed by the vt100 grid helper.

## S. Bell with running agent

**Author-from-intent.** Bell + single tracked pane — exercises bell on a non-idle tab. `⠋ 1 pinky` + spaces + `⚑`, then the pane line (no bell on pane lines).

```rail-input
width 32
tab 1 "pinky" bell
  claude running "running tests"
```
```rail-expect
 RADAR                        ·1
◆═══════════════════════════════
 ⠋ 1 pinky                    ⚑
 └ ⠋ ✳ running tests
```

## T. Untracked-only tab (D4 prompt-exclusion)

**Author-from-intent.** A tab whose only pane is untracked (never sent a status → `PaneDisplay::Untracked`). The aggregator produces zero tracked panes → the tab renders as idle with no pane line, just like a plain tab.

```rail-input
width 32
tab 1 "shell"
  untracked "zsh"
```
```rail-expect
 RADAR                        ·1
════════════════════════════════
 ○ 1 shell
```

> Untracked pane gets no line. Tab status = Idle (no tracked panes). ⟦D4 ✓⟧

## U. Mixed tracked + untracked (one tab)

**Author-from-intent.** One tracked pane (claude running) + one untracked pane (zsh). Only the tracked pane appears; untracked is suppressed. Single-pane path (1 tracked): shows ` └ ⠋ ✳ exploring render` as the child line.

```rail-input
width 32
tab 1 "af"
  claude running "exploring render"
  untracked "zsh"
```
```rail-expect
 RADAR                        ·1
◆═══════════════════════════════
 ⠋ 1 af
 └ ⠋ ✳ exploring render
```

> Only tracked panes produce pane lines. Untracked (prompt programs, idle shell) are invisible. ⟦D4 ✓⟧

## V. Cards density — single working agent

**Render-derived.** A single tracked pane at `density cards`. Cards mode: no `═` rule (header_lines=1); the card body appears immediately under the title. Verified: card surface structure present (title → tab row → pane line), correct glyphs, ≤32 width.

<!-- render-derived: grid captured from the real renderer, sanity-checked for card structure -->
```rail-input
width 32
density cards
tab 1 "pinky"
  claude running "running tests"
```
```rail-expect
 RADAR                        ·1
 ⠋ 1 pinky
 └ ⠋ ✳ running tests
```

> Cards density: header is title-only (no `═` rule). Content identical to compact; only the surface bg changes (not visible in the stripped grid).

## W. Cards density — multi-pane `af` tab (active, exercises active-child bg path)

**Render-derived.** Multi-pane tab at `density cards` with `active`. The focused tab gets the `▌` spine on all rows (identical content to M but in Cards density). Active child rows use the `surface_agent` bg tint (not visible in stripped grid). Sanity-checked: spine + glyphs + marks + msgs correct.

<!-- render-derived: grid captured from the real renderer, sanity-checked -->
```rail-input
width 32
density cards
tab 1 "af" active
  codex running "exploring render"
  build done "cargo build"
```
```rail-expect
 RADAR                        ·1
▌⠋ 1 af
▌├ ⠋ ❉ exploring render
▌└ ● ⚙ cargo build
```

> Cards density + active: spine `▌` on all rows; active-child bg path exercised (not visible after ANSI strip). ⟦Cards density⟧

---

## X. Command-origin end-result tag — done (no tag)

**Author-from-intent.** A single build pane that exited successfully (exit 0) via the
Command-origin path. The `pane_outcome()` function fires only for Command-origin panes;
status-pipe panes never get an outcome tag. `Outcome::Ok` renders as no tag at all — the
line-1 status glyph (`●`, green) is the one done signal, so a second `✓` on line 2 would
double-mark the same fact. Single-pane path: tab glyph `●` (Done), child line =
` └ ● ⚙ cargo build` (same tree shape as multi-pane — §C's note).

Grid reasoning at width 32 (`emit_pane_line` prefix: spine/space + `└` + space
+ glyph + space + mark + space = 7 cols):
- Tab line: ` ● 1 work` (5 chars prefix — col-0 spine/space + glyph + sp + num + sp —
  + `work` = 9 chars total).
- Child line: ` └ ● ⚙ cargo build` = 7-col prefix + `cargo build` (11 chars) = 18 cols total.
  No truncation (18 ≤ 32).

```rail-input
width 32
tab 1 "work"
  build done "cargo build" exit 0
```
```rail-expect
 RADAR                        ·1
════════════════════════════════
 ● 1 work
 └ ● ⚙ cargo build
```

> `exit 0` routes through `command_changed` → `timer` → `panes_changed(exits)` so the
> CommandStore sets `origin = Command` and `status = Done`. `pane_outcome()` returns
> `Outcome::Ok`, whose `full()`/`minimal()` forms are both empty — `compose_activity`
> treats an empty tag as no tag: no separator space, no empty SGR pair.

## Y. Command-origin end-result tag — failed (`exit 1`)

**Author-from-intent.** A single build pane that exited with code 1 via the Command-origin
path. `on_exit(Some(1))` sets `status = Error` and `exit_code = Some(1)`; `pane_outcome()`
returns `Outcome::Failed(Some(1))` → tag `exit 1`. Tab glyph `✗` (Error) already carries the
failure signal; the tag adds the one thing the glyph can't: the exit code. Child line =
` └ ✗ ⚙ cargo build exit 1`.

Grid reasoning at width 32:
- Tab line: ` ✗ 1 work` (col 0 is the reserved, blank spine column since this tab isn't active).
- Child line prefix (`emit_pane_line`): ` └ ✗ ⚙ ` = 7 cols; avail = 25.
  `cargo build exit 1` = 18 chars, fits without truncation.

```rail-input
width 32
tab 1 "work"
  build error "cargo build" exit 1
```
```rail-expect
 RADAR                     ·1 1!
════════════════════════════════
 ✗ 1 work
 └ ✗ ⚙ cargo build exit 1
```

> `exit 1` → `status = Error`, `exit_code = Some(1)` → `Outcome::Failed(Some(1))` → `exit 1`
> (no parens — the exit code is the whole tag now that `Ok` carries none). The full form is
> shown because 19 cols fits within the 28-col avail budget.

---

## Z. Comfortable density — blank gap row between tabs

**Render-derived.** Multiple tabs at `density comfortable`. Comfortable keeps the
`═` header rule (like compact) but inserts ONE blank gap row after each tab so the
list breathes. The trailing gap after the last tab is stripped. This is the sole
behavior that distinguishes comfortable from compact in the stripped grid.

```rail-input
width 32
density comfortable
tab 1 "web"
  claude running "building"
tab 2 "notes"
```
```rail-expect
 RADAR                        ·2
◆═══════════════════════════════
 ⠋ 1 web
 └ ⠋ ✳ building

 ○ 2 notes
```

> Comfortable density: header rule present; a blank line separates each tab. ⟦Comfortable density⟧

## AA. Cards density — multi-tab card stack (idle / working / done)

**Render-derived.** Three tabs at `density cards`, none focused. Cards drops the `═`
rule (title-only header) and paints every tab as a card followed by a trailing gap
row — which appears as a blank line in the stripped grid. Pins the card-stack text
layout across states; the per-row surface tints (idle/agent) are covered by the
`canonical_tint_map` / `cards_3tint_layout_snapshot` snapshots.

```rail-input
width 32
density cards
tab 1 "web"
  claude running "building"
tab 2 "worker"
  claude done "shipped"
tab 3 "notes"
```
```rail-expect
 RADAR                        ·3
 ⠋ 1 web
 └ ⠋ ✳ building

 ● 2 worker
 └ ● ✳ shipped

 ○ 3 notes
```

> Cards density: title-only header (no `═`); each card followed by a blank gap row. ⟦Cards density⟧

---

## T1 — sticky task: identity line + pending question line

A pane with a known task shows the task as its line text in every state; the
pending pane spends one extra `↳` line on the actionable question. Calm panes
(running) stay single-line.

```rail-input
width 32
tab 1 "review"
  claude pending "approve git push?" task "migrate schema"
  codex running "editing retry.rs" task "write insta tests"
```

```rail-expect
 RADAR                     ·1 1!
════════════════════════════════
 ◆ 1 review
 ├ ◆ ✳ migrate schema
 │   ↳ approve git push?
 └ ⠋ ❉ write insta tests
```

## T2 — sticky task: fallback and calm done

No task ⇒ exactly the pre-task rail (msg as the line text, no `↳` line even
when pending). A done pane with a task shows the task alone — no second line.

```rail-input
width 32
tab 1 "work"
  claude pending "approve?"
  claude done "All 47 tests pass" task "fix flaky e2e"
```

```rail-expect
 RADAR                     ·1 1!
════════════════════════════════
 ◆ 1 work
 ├ ◆ ✳ approve?
 └ ● ✳ fix flaky e2e
```

## T3 — sticky task: single-pane question line

A single-tracked-pane tab keeps its 2-line block (tab row + identity, in the
same `└` tree shape as multi-pane children) and adds the `↳` question line
when pending — the same subordinate-line layout T1 shows under `├`.

```rail-input
width 32
tab 1 "review"
  claude pending "approve git push?" task "migrate schema"
```

```rail-expect
 RADAR                     ·1 1!
════════════════════════════════
 ◆ 1 review
 └ ◆ ✳ migrate schema
     ↳ approve git push?
```

## T4 — sticky task: error pane with `↳` and a sibling running pane

An error-status pane with a task shows the task as identity plus the `↳`
line for its error message; a sibling running pane keeps the multi-pane tree
connectors correct (the `│` continuation under the error pane's `↳` line).

```rail-input
width 32
tab 1 "release"
  claude error "boom" task "fix the deploy"
  codex running "editing retry.rs" task "write insta tests"
```

```rail-expect
 RADAR                     ·1 1!
════════════════════════════════
 ✗ 1 release
 ├ ✗ ✳ fix the deploy
 │   ↳ boom
 └ ⠋ ❉ write insta tests
```

## T5 — pending wait tag: `· Nm` on the identity line (pending only)

A pane blocked on the user for ≥ 1 minute carries a `· Nm` wait tag on its
identity line (rule 2) — the cost of ignoring it, at a glance. The sibling
running pane shows the tag is pending-only, and the `↳` question line is
unchanged. The `waiting <N>m` input trailer backdates the waiting-on-you edge;
a pending pane without it (T1/T3) renders tagless — fresh asks need no clock.

```rail-input
width 32
tab 1 "review"
  claude pending "approve git push?" task "migrate schema" waiting 12m
  codex running "editing retry.rs" task "write insta tests"
```

```rail-expect
 RADAR                     ·1 1!
════════════════════════════════
 ◆ 1 review
 ├ ◆ ✳ migrate schema · 12m
 │   ↳ approve git push?
 └ ⠋ ❉ write insta tests
```

---

## AB. The floor: footer + earlier-ledger

**Render-derived.** The bottom region (spec §9): once a session's content
leaves ≥2 spare lines, a footer pins to the floor of the pane — a `─` rule and
the tally line (`{n} working`, gaining ` · {m} need you` only when `m > 0`:
a zero need-you count is noise, not signal). The `alt-[n] jump` hint is a
THIRD footer line that renders only under `jump_hint` (the `JumpHint` config —
same honesty contract as `grant_hint`, but no in-tree config sets it: `run`
bakes Alt-1..9 → GoToTab binds, yet Alt+digit is commonly claimed upstream of
Zellij — WM workspace hotkeys, macOS Option typing `¡` — and the rail can't
detect interception, so the hint is opt-in for setups that truly deliver the
chord; see §AC). With enough
spare and a non-empty completion ledger, an `─ earlier` section of receded
completions (newest first, per the `ledger` directive below) fills the space
between the content and the footer, always followed by ONE blank spacer line
so history gets air above the pinned floor; a tab name past 12 columns
truncates with `…`. The section shows at most **10** entries regardless of
pane height (`LEDGER_DISPLAY_CAP`) — the rail is a status surface, not a log,
so spare height past that stays blank filler above the rule rather than
ever-deeper history (the ring still *stores* 32 for cross-instance merging).

```rail-input
width 32
height 9
tab 1 "web"
ledger 90 done "web" "deploying"
ledger 300 error "workspace-ci-runner" "tests failed"
```
```rail-expect
 RADAR                        ·1
════════════════════════════════
 ○ 1 web
─ earlier ──────────────────────
1m ● web deploying
5m ✗ workspace-c… tests failed

────────────────────────────────
0 working
```

> `height 9` leaves 6 spare lines past the 3-line body: the `─ earlier` rule +
> 2 ledger rows (newest first) + the spacer + the pinned 2-line footer
> (rule/tally — no hint line: this scenario doesn't set `jump_hint`), with no
> filler needed. The tally reads just `0 working` — nothing needs you, so no
> segment says so. `ledger <age_secs> done|error "<tab>" "<label>"` seeds a
> row directly (age is wall-clock via `now_epoch_s`, independent of
> `now_tick`); a row's tab is looked up live in production, so a closed tab's
> row just becomes click-inert rather than disappearing. ⟦bottom region /
> spec §9⟧

---

## AC. Zero tabs, non-empty ledger (history outlives the tab)

**Render-derived.** Task 14: every tab can close while completion history
remains — `render_rail` still renders (header + bottom region, no cards) as
long as `opts.ledger` is non-empty, even with zero rows. The header's `·N`
count is honest about what it counts: `·0` tabs, not 0 history.

```rail-input
width 32
height 8
jump_hint
ledger 90 done "web" "deploying"
```
```rail-expect
 RADAR                        ·0
════════════════════════════════
─ earlier ──────────────────────
1m ● web deploying

────────────────────────────────
0 working
alt-[n] jump
```

> No `tab` line at all — zero rows. The header still renders (·0) because
> `has_content` is `!rows.is_empty() || !ledger.is_empty()`. Only when BOTH are
> empty does `render_rail` return nothing, and `PluginRuntime::render` routes
> to the `onboarding()` scanning face instead (see §A's note above). This
> scenario also opts into `jump_hint`, so the footer is the full 3 lines —
> rule, tally, `alt-[n] jump` (column 0, aligned with the tally). No in-tree
> config opts in (see §AB — interception upstream of Zellij makes the chord
> machine-dependent); the directive keeps the mechanism pinned for users who
> set `jump_hint "alt-n"` themselves. Without it the hint line simply doesn't
> exist — the rail never advertises a chord that isn't bound. ⟦zero state +
> jump_hint / spec §7,§9⟧

---

## Open decisions

- **⟦D1⟧** right-slot: keep dropped, or re-add `done/total` for multi-pane?
- **⟦D3⟧** idle-but-tracked panes (J) — drop their line after a while, or keep?
  Tied to the lingering-`done`/ghost-row question.
- **⟦D8 ✓ done⟧** layout `size=24`→`size=32` applied (README, examples, e2e harness, design.md).
- **⟦D9⟧** placeholder name for an unnamed/first tab ("shell"? layout name? "—"?).
- **⟦D-timer ✓ pending-only⟧** resolved: elapsed returned per-pane (on the pane
  line, as decided), scoped to `pending` rows only — the `· Nm` wait tag of
  rule 2 / scenario T5. Calm statuses stay tagless; widening beyond pending
  would reopen the width-pressure concern that removed elapsed originally.

---

## Using this as tests

**Recommendation: the doc is the oracle.** A `#[cfg(test)]` module in the crate
`include_str!`s this file, parses every `rail-input`/`rail-expect` pair, builds
the state, runs the **real** pipeline (`aggregate` → `render_rail`), strips ANSI
to a grid, and asserts equality. One source of truth; editing a block edits the
test; no drift between doc and behavior.

### The `rail-input` mini-DSL

```
width <n>            # optional, default 32
height <n>           # optional, default = enough to fit (no overflow)
glyphs plain|nerd    # optional, default plain
jump_hint            # optional; footer advertises `alt-[n] jump` (default hidden)
tab <pos> "<name>" [active]
  <kind> <status> "<msg>" [task "<text>"] [waiting <N>m] [exit <N>|?]   # one line per pane, indented
  ...
```

- `kind` ∈ claude·codex·gemini·command·build·test·deploy·server·other
- `status` ∈ running·pending·done·error·idle
- `waiting <N>m` backdates the pane's waiting-on-you edge by N minutes so the
  `· Nm` wait tag renders (pending panes only; without it the pane applied
  "now" and rule 2's under-a-minute case keeps the line tagless).
- Omit a tab's panes for a plain/idle tab. Prompt-only panes are never listed
  (they're untracked by rule 4).

### Test sketch (`crates/plugin/src/reference_tests.rs`, `#[cfg(test)]`)

```rust
// pseudocode — lives in-crate so it can call aggregate()/render_rail()
const DOC: &str = include_str!("../../../docs/rail-reference.md");

#[test]
fn rail_reference_matches() {
    for case in parse_cases(DOC) {                 // (id, input_dsl, expect_grid)
        let (rows, opts) = build_state(&case.input);   // DSL → StateStore/CommandStore
                                                        //      → aggregate() per tab → TabRow[]
        let rail = render::render_rail(&rows, &opts);
        let got = grid(&rail.ansi, opts.width);         // existing vt100 grid() helper
        assert_eq!(got.trim_end(), case.expect.trim_end(),
            "scenario {} mismatch:\n--- expected ---\n{}\n--- got ---\n{}",
            case.id, case.expect, got);
    }
}
```

`parse_cases` is a ~40-line line-scanner (find fenced ```` ```rail-input ````
/ ```` ```rail-expect ```` blocks; the nearest preceding `## <id>` heading names
the case). `build_state` maps each `<kind> <status> "<msg>"` into a pane
observation, assigns sequential pane ids, and calls the real `aggregate` — so
the test exercises aggregation *and* rendering, exactly the path the bug lived in.

### Why this over plain `insta`

`insta` snapshots (the `canonical_*` tests) stay for the pixel-exact card/tint
cases. The reference is different: it's **human-authored intent** that should be
reviewable as a doc *and* enforced as a test. Doc-as-oracle gives both. (If the
DSL parser feels like too much machinery, the fallback is fixtures: one
`tests/reference/<id>.txt` per case + inputs in code — but then the doc mirrors
the fixtures instead of being them.)

### Build workflow (TDD)

1. These `rail-expect` blocks are the **target** (hand-authored now).
2. Implement the renderer changes until `rail_reference_matches` passes.
3. From then on the blocks are the regression guard; an intentional change edits
   the block (a visible, reviewable diff in the doc) — never a silent snapshot
   accept. E2E (real Zellij, sidebar-region assertions) covers that the live
   event stream actually drives these states.
