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

---

## Open decisions

- **⟦D1⟧** right-slot: keep dropped, or re-add `done/total` for multi-pane?
- **⟦D3⟧** idle-but-tracked panes (J) — drop their line after a while, or keep?
  Tied to the lingering-`done`/ghost-row question.
- **⟦D8⟧** bump the layout `size=24` → `size=32` (and README).
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
