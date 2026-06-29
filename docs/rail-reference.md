# Rail rendering reference — spec by example (executable)

> **Status:** PROPOSED target design (2026-06-28), revised per review. This file
> is the **test oracle**: each scenario carries a `rail-input` block (the state)
> and a `rail-expect` block (the exact ANSI-stripped grid). A crate test parses
> them, runs the real `aggregate` + `render_rail`, and asserts the grid matches
> (see *Using this as tests* at the end). Edit a block → edit the test.

## Design rules (this revision)

1. **Width 32 columns.** (Was 24.) The layout snippet's `size=24` should bump to
   `size=32` to match. ⟦D8⟧
2. **No elapsed/timer, for now.** Removed from tab and pane lines entirely.
   Per-pane elapsed is the eventual right answer but adds width pressure; revisit
   later. ⟦D-timer⟧
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

## Vocabulary

**Status glyphs (plain):** `○` idle · `◐` working *(spins ◐◓◑◒)* · `◆` needs-you ·
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
▌◐ 2 name                          ← tab row: [spine][glyph] [num] [name]
▌ ◐ ❉ activity message             ← pane row (indent 2): [glyph] [mark] [msg]
```

`▌` = active-tab spine (focused tab only). Tab **glyph** = dominant status.
Single-pane tabs put the one pane's message on line 2 with just its mark.

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
════════════════════════════════
◐ 1 pinky
  ✳ running tests…
```

## D. Single agent — needs you

```rail-input
width 32
tab 3 "api"
  claude pending "approve edit?"
```
```rail-expect
 RADAR                        ·1
════════════════════════════════
◆ 3 api
  ✳ approve edit?
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
  ✳ refactored the dotfiles
```

## F. Single agent — error

```rail-input
width 32
tab 2 "build-svc"
  claude error "exit 1: cargo test failed"
```
```rail-expect
 RADAR                        ·1
════════════════════════════════
✗ 2 build-svc
  ✳ exit 1: cargo test failed
```

## G. Single command — build running

```rail-input
width 32
tab 1 "web"
  build running "cargo build"
```
```rail-expect
 RADAR                        ·1
════════════════════════════════
◐ 1 web
  ⚙ cargo build
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
════════════════════════════════
◐ 2 af
  ◐ ❉ exploring render
  ● ⚙ cargo build
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
 RADAR                        ·1
════════════════════════════════
◆ 4 review
  ◆ ✳ approve diff?
  ◐ ❉ writing tests
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
════════════════════════════════
◐ 2 af
  ◐ ❉ exploring render
  ○ $ ./deploy.sh
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
════════════════════════════════
◐ 2 swarm
  ◐ ❉ planning api
  ◐ ❉ writing tests
  ◐ ❉ refactoring
  ◐ ❉ reviewing pr
  ◐ ❉ docs pass
  ◐ ❉ benchmarks
  +1 more
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
 RADAR                        6▲
════════════════════════════════
◆ 1 review
  ✳ approve diff?
◐ 2 af
  ❉ exploring render
● 3 dotfiles
  ✳ refactored auth
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
════════════════════════════════
▌◐ 2 af
▌ ◐ ❉ exploring render
▌ ● ⚙ cargo build
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
════════════════════════════════
◐ 1 swarm
  ◐ ❉ pane one
  ◐ ❉ pane two
  ◐ ❉ pane three
  ◐ ❉ pane four
  ◐ ❉ pane five
  ◐ ❉ pane six
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
════════════════════════════════
◐ 1 swarm
  ◐ ❉ pane one
  ◐ ❉ pane two
  ◐ ❉ pane three
  ◐ ❉ pane four
  ◐ ❉ pane five
  ◐ ❉ pane six
  +2 more
```

> 8 panes, cap 6: `8 - 6 = 2` remainder → `+2 more`. ⟦D6⟧

## P. Truncation at width 32

**Author-from-intent.** Multi-pane tab: pane line prefix is `"  " + glyph(1) + " " + mark(1) + " "` = 6 visible cols; budget = 26; truncate at 25 chars + `…`. The 51-char msg is clipped to `this message is quite lon…`.

```rail-input
width 32
tab 1 "work"
  claude running "this message is quite long and will be truncated here"
  build done "ok"
```
```rail-expect
 RADAR                        ·1
════════════════════════════════
◐ 1 work
  ◐ ✳ this message is quite lon…
  ● ⚙ ok
```

> Prefix = 6 cols; avail = 26; budget = 25 + `…`. Exercises `emit_pane_line` truncation. ⟦D8: width=32⟧

## Q. CJK / wide-char message at width 32

**Author-from-intent.** Multi-pane tab with a CJK message. CJK chars are 2 display cols each; prefix = 6 cols; avail = 26; budget = 25 display cols. "処理中のメッセージが長すぎるケース" (17 chars, 34 display cols) → first 12 chars (24 display cols) fit; 13th would exceed, so result = "処理中のメッセージが長す" + `…` (25 display cols). No rendered line exceeds width=32.

```rail-input
width 32
tab 1 "cjk"
  claude running "処理中のメッセージが長すぎるケース"
  build done "ok"
```
```rail-expect
 RADAR                        ·1
════════════════════════════════
◐ 1 cjk
  ◐ ✳ 処理中のメッセージが長す…
  ● ⚙ ok
```

> CJK chars are width-2; unicode-width truncation keeps the line at ≤32 display cols.

## R. Bell marker in tab line

**Author-from-intent.** A tab with `bell` renders `⚑` at the right side of the tab line (2-col slot: `⚑` + trailing space, which is trimmed). For `"alerts"` (6 chars) at width=32: prefix=4, bell_len=2, name_budget=26, gap=32-4-6-2=20 → `○ 1 alerts` + 20 spaces + `⚑`.

```rail-input
width 32
tab 1 "alerts" bell
```
```rail-expect
 RADAR                        ·1
════════════════════════════════
○ 1 alerts                    ⚑
```

> Bell token on the `tab` line sets `has_bell=true`; the `⚑` glyph appears right-aligned. Tab-line trailing space after `⚑` is trimmed by the vt100 grid helper.

## S. Bell with running agent

**Author-from-intent.** Bell + single tracked pane — exercises bell on a non-idle tab. `◐ 1 pinky` + spaces + `⚑`, then the pane line (no bell on pane lines).

```rail-input
width 32
tab 1 "pinky" bell
  claude running "running tests"
```
```rail-expect
 RADAR                        ·1
════════════════════════════════
◐ 1 pinky                     ⚑
  ✳ running tests
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

**Author-from-intent.** One tracked pane (claude running) + one untracked pane (zsh). Only the tracked pane appears; untracked is suppressed. Single-pane path (1 tracked): shows `✳ exploring render` on line 2.

```rail-input
width 32
tab 1 "af"
  claude running "exploring render"
  untracked "zsh"
```
```rail-expect
 RADAR                        ·1
════════════════════════════════
◐ 1 af
  ✳ exploring render
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
◐ 1 pinky
  ✳ running tests
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
▌◐ 1 af
▌ ◐ ❉ exploring render
▌ ● ⚙ cargo build
```

> Cards density + active: spine `▌` on all rows; active-child bg path exercised (not visible after ANSI strip). ⟦Cards density⟧

---

## X. Command-origin end-result tag — done (`✓`)

**Author-from-intent.** A single build pane that exited successfully (exit 0) via the
Command-origin path. The `pane_outcome()` function fires only for Command-origin panes;
status-pipe panes never get an outcome tag. Single-pane path: tab glyph `●` (Done),
pane line 2 = `  ⚙ cargo build ✓`.

Grid reasoning at width 32, prefix 6 (`"  ⚙ "` = indent 2 + mark 1 + space 1 = 4 cols;
pane line 2 uses single-pane path with `prefix_vis = 2 + 1 + 1 = 4` cols):
- Tab line: `● 1 work` (3 chars prefix + `1 work` = 8 chars total).
- Pane line 2: `  ⚙ cargo build ✓` = 4-col prefix + `cargo build ✓` (13 chars) = 17 cols total.
  No truncation (17 ≤ 32).

```rail-input
width 32
tab 1 "work"
  build done "cargo build" exit 0
```
```rail-expect
 RADAR                        ·1
════════════════════════════════
● 1 work
  ⚙ cargo build ✓
```

> `exit 0` routes through `command_changed` → `timer` → `panes_changed(exits)` so the
> CommandStore sets `origin = Command` and `status = Done`. `pane_outcome()` returns
> `Outcome::Ok` → `✓` tag appended after the command string.

## Y. Command-origin end-result tag — failed (`(exit 1)`)

**Author-from-intent.** A single build pane that exited with code 1 via the Command-origin
path. `on_exit(Some(1))` sets `status = Error` and `exit_code = Some(1)`; `pane_outcome()`
returns `Outcome::Failed(Some(1))` → tag `(exit 1)`. Tab glyph `✗` (Error); pane line 2
= `  ⚙ cargo build (exit 1)`.

Grid reasoning at width 32:
- Tab line: `✗ 1 work`.
- Pane line 2 prefix (single-pane path): `  ⚙ ` = 4 cols; avail = 28.
  `cargo build (exit 1)` = 21 chars, fits without truncation.

```rail-input
width 32
tab 1 "work"
  build error "cargo build" exit 1
```
```rail-expect
 RADAR                        ·1
════════════════════════════════
✗ 1 work
  ⚙ cargo build (exit 1)
```

> `exit 1` → `status = Error`, `exit_code = Some(1)` → `Outcome::Failed(Some(1))` → `(exit 1)`.
> The full form is shown because 21 cols fits within the 28-col avail budget.

---

## Open decisions

- **⟦D1⟧** right-slot: keep dropped, or re-add `done/total` for multi-pane?
- **⟦D3⟧** idle-but-tracked panes (J) — drop their line after a while, or keep?
  Tied to the lingering-`done`/ghost-row question.
- **⟦D8 ✓ done⟧** layout `size=24`→`size=32` applied (README, examples, e2e harness, design.md).
- **⟦D9⟧** placeholder name for an unnamed/first tab ("shell"? layout name? "—"?).
- **⟦D-timer⟧** if/when elapsed returns, per-pane (on the pane line) not per-tab.

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
tab <pos> "<name>" [active]
  <kind> <status> "<msg>"     # one line per pane, indented
  ...
```

- `kind` ∈ claude·codex·gemini·command·build·test·deploy·server·other
- `status` ∈ running·pending·done·error·idle
- Omit a tab's panes for a plain/idle tab. Prompt-only panes are never listed
  (they're untracked by rule 4).

### Test sketch (`src/reference_tests.rs`, `#[cfg(test)]`)

```rust
// pseudocode — lives in-crate so it can call aggregate()/render_rail()
const DOC: &str = include_str!("../docs/rail-reference.md");

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
