# Rail readability: bolder glyphs/titles, Nerd kind-marks, robust end-result tags

**Date:** 2026-06-29
**Status:** Implemented on `rail-readability` (re-built on the post-"Line-as-unit" `render.rs`).

## Goals (sidebar feedback)

1. Glyphs and tab titles read too light — make state legible at a glance.
2. The Claude mark `✳` reads like a footnote next to `⚙`/`$` (it was already
   `✳`, just painted faint + non-bold).
3. A finished (green) tab still shows present-tense activity ("cargo build")
   with no result — `done` should say *what happened*.

## Design

### Bolder glyphs & titles (`render.rs`)
- `label_bold` fires for **all non-idle** rows (was Pending-only): the line-1
  glyph + number + name go bold; idle stays light. Bold encodes *activity*;
  focus stays the accent spine + brighter card (independent cues).
- Kind marks render **bold + `dim_strong`** (not the faint `idle_text`) at both
  mark sites; the pane-line status glyph is bold on non-idle too.

### Nerd-Font kind marks (`kind.rs`)
- `Kind::mark(GlyphSet)` — the Nerd set upgrades the three thin *agent* marks
  (Claude `\u{f06a9}` robot, Codex `\u{f167a}`, Gemini `\u{f0eb9}` sparkle);
  task marks (`⚙ ⚗ ⇡ ❯ $ ⦿`) are shared. Plain set unchanged.

### End-result outcome tag — robust (structured, not baked)
- `enum Outcome { Ok, Failed(Option<i32>) }`, carried on `PrimaryDetail` and
  `PaneDisplay::Tracked`. Built in `radar_state::pane_outcome` from
  `(origin==Command, status, exit_code)`. **`msg` stays the pure command** —
  only the display DTO carries the outcome, so the data layer is untouched.
- `exit_code: Option<i32>` added to `TrackedObservation`, set by
  `CommandStore::on_exit`, persisted in the v2 snapshot (`#[serde(default)]`).
- **Reserve-and-tier truncation** (`compose_activity`): the tag is reserved
  *first*, so it survives a width squeeze — the command absorbs truncation
  (→ `…` → gone) while the tag degrades only from its full form (`(exit 1)`)
  to the irreducible 1-column glyph (`✓`/`✗`). Guarantees the outcome is visible
  down to the last column.
- **Tag-only coloring:** the command stays calm (`dim_strong`, or attention for
  Pending); only the outcome tag pops in its role hue (green `✓` / red
  `(exit N)`). Narrower than the first cut (which recolored whole Done/Error
  lines, including agents).

Scope: only command-origin panes get a tag. Agents keep their hook msg.

## Architecture fit
Re-built on the post-refactor `render.rs` (`render_row -> Vec<Line>`, no
`row_lines` predictor). The line-2 presence test is now `msg non-empty OR
outcome present`, and footprint derives from the rendered block — so the
"does the tag get a line?" decision can't drift from what's emitted.

## Known limits (unchanged, out of scope)
- A command **typed at the shell** finishes with no exit code → shows `✓`
  ("finished", not "verified passed"). Only `zellij run`-style exits yield
  `(exit N)`. (Pre-existing: such a failed build already shows green.)
- No pane-output access → no real test/build counts.
