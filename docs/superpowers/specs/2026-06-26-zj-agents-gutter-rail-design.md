# zj-radar sidebar — "the gutter rail" (Direction C)

**Status:** design / approved for spec-review
**Date:** 2026-06-26
**Author:** Mark Toda (with Claude)
**Source design:** claude.ai/design `zj-agents sidebar.dc.html` ("Direction C, consolidated")
**Supersedes:** the visual layer of `docs/design.md` §3 and `docs/ui-design-brief.md`. The
data model, pipe contract, aggregation, and wiring in `docs/design.md` are unchanged.

---

## 1. Goal

Translate the finalized "gutter rail" mock into the renderer. The rail is a left,
character-grid sidebar (~24 cols) that lists every tab and decorates agent tabs with
status. The mock's thesis: a fixed **2-column status gutter** (active/attention bar +
status glyph) forms a vertical stripe so the eye finds "needs me" without reading words;
rows stay calm at rest and earn extra detail lines only when working / waiting / errored.

This is a **render/layout/state-vocabulary** change only. Agent detection, the pipe
contract (`zj_radar.status.v1`), per-pane→per-tab aggregation, naming, and host wiring are
**not** changed.

## 2. Decisions (locked)

| # | Decision | Choice |
|---|---|---|
| 1 | Row sort order | **Keep tab-position order.** The aligned gutter does the scanning; tab N stays at row ~N (muscle memory). No priority re-sort. |
| 2 | Color strategy | **ANSI-16 role mapping** — named ANSI colors mapped to roles, so the user's terminal theme styles them. No fixed hex. |
| 3 | Glyph set | **Plain default; Nerd opt-in** via `glyphs = "nerd"`. Plain glyphs: `○ ◐ ◆ ● ✗`. |
| 4 | Scope — in | Core rail, **overflow folding**, **onboarding/empty-state panel**, **light-theme legibility**. |
| 5 | Scope — out | Collapsed/keybind mode; the per-agent **roster line** in multi-agent tabs (show aggregate only, opt-in later). |

## 3. Color roles (ANSI-16, theme-adaptive)

Six roles. The renderer emits only role colors — never hardcoded hex — so dark/light
legibility is the active theme's job.

| Role | Used for | ANSI |
|---|---|---|
| `error` | error glyph, `failed` | red (`31`) |
| `attention` | waiting glyph, waiting right-slot, active-bar-when-urgent | bright red (`91`) |
| `working` | working glyph, working spinner | yellow (`33`) |
| `success` | done glyph, `done` | green (`32`) |
| `muted` | idle glyph/name, dim detail lines | bright black / dim (`90`) |
| `accent` | `AGENTS` title, rule, active-bar (normal), `+N idle ▾`, overflow `▲` | magenta (`35`, ≈ mauve) |

Notes:
- ANSI-16 has no "peach." `attention` borrows the **bright-red** slot; the **diamond glyph
  ◆** plus **active-bar + bold name** carry the rest of the "this is the loud one"
  distinction. This intentionally flips the current bug where `pending` (`33`) rendered
  *quieter* than `running` (`93`).
- `accent` and `attention` are distinct hues (magenta vs red), so the active bar's
  normal→urgent tint is visible.

## 4. Glyph set (`GlyphSet`: Plain | Nerd)

Selected once in `load()` from config (`glyphs`), default **Plain** (Nerd opt-in via
`glyphs = "nerd"`). A small lookup, not a new module — extend `status.rs`.

> **Rationale:** Nerd Font presence cannot be detected at runtime. Defaulting to Nerd would
> show broken box characters on non-Nerd terminals, so Plain is the safe default.

| State | Nerd | Plain |
|---|---|---|
| idle | `EB83` | `○` |
| working | `F110` | `◐` |
| waiting | `F0F3` | `◆` |
| done | `F058` | `●` |
| error | `F057` | `✗` |

- **Working spinner:** the *status glyph* spins through the geometric quarter-circle
  sequence `◐ ◓ ◑ ◒` in **both** sets (the Nerd static icon doesn't spin cleanly), advanced
  by the existing 1 Hz `tick` (`frame = tick % 4`).
- **In-message spinner:** braille `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏` on the working detail line.
- Other literals used in both sets: bar `▌`, waiting caret `⏵`, fold marker `▾`, overflow
  `▲`, rule `═`, ellipsis `…`. These are plain Unicode regardless of set.

## 5. Header

Replaces the bare glyph-count summary with two lines:

```
 AGENTS                ·14      ← accent title, right-aligned ·N tab count
══════════════════════         ← accent rule, full width
```

- `·N` = total tab count. When overflow folding is active, show `N ▲` (accent) to signal
  "more than fits."
- `header_lines()` returns **2** when any rows exist (it's the rail's identity, always on),
  **0** only for the onboarding/empty state (§9, which paints its own panel). Click-mapping
  (`tab_position_at_line`) already consumes `header_lines()`, so click offsets stay correct.

## 6. Row anatomy — the gutter + right slot

Every row: `<col0><col1> <num> <name> … <rightslot>`

- **col 0** — active bar `▌`, else a space. Color: `accent` normally; `attention` when the
  active row is *also* waiting/error (the alarm wins). Only the **active** tab gets a bar.
- **col 1** — the status glyph, vertically aligned across all rows (the scan stripe).
- **num** — `position + 1` (unchanged; the stable label).
- **name** — bold when active.
- **right slot** — fixed-width, right-aligned, **reserved even when empty** so columns never
  jump:

| state | right slot |
|---|---|
| idle | (empty) |
| working | elapsed `0:14` |
| waiting | `⏵ 0:02` · multi-agent `2/4 ⏵0:18` |
| done | `done` |
| error | `failed` (→ `err` when narrow) |

The bell marker (`⚑`, `has_bell`) is retained, rendered just before the right slot.

## 7. Adaptive density (drives `row_lines` + click mapping)

`row_lines(agg)` is the single source of truth; `tab_position_at_line` replays it, so the
two stay in lockstep.

| state | lines | line 2 / 3 |
|---|---|---|
| idle | 1 | — |
| done | 1 | — (right slot carries `done`) |
| working | 2 | `repo/branch ⟨spin⟩ msg…` (muted) |
| error | 2 | `repo · detail` (muted) |
| waiting | 2 (no msg) / 3 (msg) | `branch · needs you`, then `"msg"` |

Multi-agent tabs use the **aggregate** glyph (most-urgent member, per existing
aggregation), the `done/total` count in the right slot, and "N needs you" on the detail
line when >1 member is pending. No per-agent roster line in this build.

## 8. Truncation order (narrow widths)

Apply in order until the line fits: **(1)** drop branch (repo only) → **(2)** drop message
→ **(3)** truncate name with `…`. The status word shortens (`failed → err`). The gutter,
number, name (≥1 char), and right slot always survive. A test asserts no emitted line
(ANSI-stripped) exceeds the width, across states and widths (extends the existing
`no_emitted_line_exceeds_width`).

## 9. Overflow folding (reconciled with position order)

When total row-lines exceed the rail height (`render` receives `rows`/height), fold to fit
while honoring position order:

1. **Compress** calm rows — working & done collapse to their single line 1.
2. **Fold idle** tabs (wherever they sit in position order) into one strip:
   `○ ○ ○ … ─── +N idle ▾` (`accent` footer).
3. **Non-idle rows always keep their right slot and never fold.** An urgent tab at any
   position stays visible with detail — this delivers the mock's "urgent never scrolls off"
   guarantee without a priority sort.

If even compressed non-idle rows overflow (pathological), idle strip is dropped first, then
the lowest-position calm rows compress hardest; urgent (waiting/error) rows are last to
lose detail. The header always shows `N ▲` in this mode.

`render` must therefore know the available **height** (the `_rows` arg already passed to the
wasm `render(rows, cols)` — plumb it into the pure `render()`).

## 10. Onboarding / empty-state panel

Shown when permission is not yet granted **or** no tabs are known yet (cold start). Once
tabs are known, the normal rail renders even if every tab is idle — idle tabs stay visible
(per the mock), so the panel never hides the tab list. Not a permission interceptor (Zellij
paints its own permission modal); this is the rail's resting "hello / how it works" face.

Contents (within rail width):
- `AGENTS` title + rule (same header).
- A short hello + one-line "what this is."
- A compact **legend**: each glyph + its meaning (`◆ needs you`, `◐ working`, `● done`,
  `✗ error`, `○ idle`) in role colors.
- A **click hint**: "click a row to jump to that tab."

Plumbed via a `permission_granted: bool` flag in `State` set from
`PermissionRequestResult`; the panel also doubles as the all-idle empty state.

## 11. Light theme

Free with ANSI roles — the user's light terminal theme supplies legible variants. Guard
with a test asserting the renderer emits only the role SGR codes from §3 (no raw hex, no
truecolor `38;2;` sequences).

## 12. Config surface

`load()` already receives `BTreeMap<String,String>`. New keys (all optional, safe defaults):
- `glyphs` = `plain` (default) | `nerd`.
- `width` — optional render-width hint (else use the pane `cols`).

No new permissions; no new host calls.

## 13. Module / structure impact

Preserve the pure / host-testable split (`status`, `model`, `render`, `naming` pure;
`lib.rs` wasm glue).

- **`status.rs`** — add `GlyphSet` + role/glyph/spinner lookups; remap colors to roles;
  `waiting` glyph `◑ → ◆`; keep `severity()` (aggregation) unchanged.
- **`render.rs`** — gutter (col0 bar + col1 glyph), right slot, per-state density,
  two-line header with count, truncation order, overflow folding, onboarding panel.
  `row_lines`/`header_lines` updated as the single sources of truth.
- **`lib.rs`** — read config in `load()`; track `permission_granted`; plumb height + glyph
  set into `render()`; click-mapping unchanged in logic (still replays `row_lines`).
- `model.rs`, `state.rs`, `payload.rs`, `naming.rs` — unchanged.

## 14. Testing (pure `cargo test`)

Extend existing snapshot-style tests:
1. Header is two lines (title + rule) with correct `·N`; `header_lines() == 2`.
2. Gutter alignment: col1 glyph at the same column across mixed rows.
3. Active bar present only on active row; tints `attention` when active row is waiting/error.
4. Right slot per state; reserved (present) even when empty; columns don't shift.
5. `row_lines` per state matches §7 (idle/done 1, working/error 2, waiting 2–3).
6. Click mapping (`tab_position_at_line`) matches the new `row_lines` across mixed states.
7. Truncation order (branch → msg → name); `failed → err`; no line exceeds width.
8. Overflow folding: idle strip + `+N idle ▾`; non-idle rows retained with slot; `N ▲`.
9. Working spinner advances with `tick` (`◐◓◑◒`); not present for non-working.
10. Role-color-only assertion (no hex / truecolor) — guards light-theme legibility.
11. Onboarding/empty-state panel renders legend + hint when not granted / all idle.
12. Glyph set toggle: `plain` emits `◆` for waiting; `nerd` emits the Nerd codepoint.

## 15. Non-goals (this build)

- Collapsed/keybind mode (parked; `MessagePlugin` wiring later).
- Per-agent roster line within a multi-agent tab (aggregate only for now).
- Any change to detection, pipe contract, aggregation, naming, notifications.
- Intercepting Zellij's native permission prompt.

## 16. Risks

- **Height plumbing** — `render()` becomes height-aware for overflow; the wasm `render`
  already has `_rows`, so this is mechanical. Tests cover the fold math.
- **Glyph width** — Nerd Font icons and `◆/●` are single-cell in monospace Nerd builds;
  truncation tests use char counts. If a terminal renders a glyph double-wide, the right
  slot could shift — mitigated by the plain fallback and by reserving the slot.
- **Onboarding visibility** — if Zellij grants permission instantly (already-trusted
  plugin), the panel may flash only as the all-idle empty state; acceptable.
