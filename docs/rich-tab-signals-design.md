# zj-agents — rich tab signals sprint

**Status:** approved design (brainstormed 2026-06-26)
**Depends on:** `design.md` (base sidebar), `smart-tabs-postmortem.md` (the no-blocking-host-calls rule)

## Goal

Add four push-only "richer tab signal" features to the existing sidebar so a many-agent
Zellij session is triageable at a glance. All four obey the standing rule from the postmortem:
**no blocking host queries (`get_pane_running_command`/`get_pane_cwd`/…) on any path.** Signals
come only from pushed events (`TabUpdate`, `PaneUpdate`, `pipe`, `Timer`) or fire-and-forget
actions (`rename_tab`, `switch_tab_to`).

Three features (A, B, C) are pure-core/render additions. One (D) mutates Zellij tab state via
`rename_tab`, with its decision logic isolated in a pure, testable function.

## A. Bell / attention signal

**What:** surface Zellij's native per-tab bell so non-agent activity (a build finishing, a test
run beeping) is visible even when no agent hook reports it.

- **Data:** add `has_bell: bool` to `TabLite`, populated from `TabInfo.has_bell_notification`
  in the existing `Event::TabUpdate` arm. (We do **not** track `is_flashing_bell` — it's a
  transient animation flag; the boolean is enough.)
- **Render:** when `has_bell` is set, append a yellow `⚑` after the tab name on line 1. The
  marker costs 2 columns (space + glyph); include that in the line-1 name-truncation budget so
  no line exceeds width.
- **State:** none. Zellij owns the flag and clears it when the tab is focused; we mirror it on
  every `TabUpdate`. Pure render + one captured field.

## B. Roll-up summary header

**What:** a single top line summarizing all tabs, so you don't scan every row to find what
needs you.

- **Logic (pure):** `fn summary(rows: &[TabRow]) -> Vec<(Status, usize)>` — count **tabs** by
  their dominant `agg.status`, **active buckets only** (exclude `Idle`). One tab = one count,
  by its aggregate severity (e.g. a quad-grid tab counts once).
- **Render:** one line at the very top, compact colored glyphs, non-zero buckets only, in
  severity order, e.g. `✗1 ◑1 ◐2 ●3`. Shown **only when ≥1 active tab exists**; otherwise no
  header line at all.
- **Cross-cutting (click mapping):** the header shifts every tab row down by its line count.
  `tab_position_at_line` must subtract the header height first; a click on the header line maps
  to no tab (no-op).

## C. Stuck / long-running cue

**What:** flag a `Running` agent whose elapsed time is suspiciously high (likely wedged or
silently waiting).

- **Threshold:** `const STUCK_SECS: u64 = 600` (10 minutes; tick ≈ 1s).
- **Render:** in `detail_tag`, for `Status::Running` where `now_tick - since_tick >= STUCK_SECS`,
  append an amber `⚠` after the elapsed (e.g. `12m ⚠`). Framing is "long-running," not a hard
  "stuck" assertion.
- **State:** none new — uses `since_tick`/`now_tick` already threaded into the renderer.

## D. Tab naming (real `rename_tab`)

**What:** give tabs meaningful names (agent repo, or the running program) instead of generic
`Tab #N`, applied to Zellij's real tab state so all surfaces benefit.

### Sources (push-only)
- **Agent tab:** `repo` from the `StateStore` (hook-provided; already sanitized).
- **Plain tab:** the focused (else first) pane's `PaneInfo.title` from the `PaneUpdate`
  manifest. **New capture:** `PaneUpdate` currently keeps only pane `id`; it must also retain
  `title` (and the already-read `is_focused`) per tab. No new subscription, no cwd/filesystem
  walk. *git-root/cwd-based naming is explicitly deferred to a follow-up.*

### Guards
- **Clobber guard** (Zellij exposes no "manually named" flag): only rename a tab whose current
  name is the default `Tab #N` pattern **or** equals the name we last auto-applied to that
  position. A manual `MOD+r` name (matching neither) is never overwritten.
- **Change/loop guard:** track `applied_names: HashMap<usize, String>` (position → last
  auto-applied name). Emit a rename only when the computed name differs from the tab's current
  name. This converges in one step (after renaming to `X`, the next `TabUpdate` reports `X`,
  which now matches → no further rename), so there is no feedback loop and no per-tick host
  spam.

### Shape
- **Pure:** `fn compute_renames(tabs, tab_panes, pane_titles, store, applied) -> Vec<(usize,
  String)>` returns the position→new-name diff. Fully unit-testable.
- **Glue:** the `wasm32` layer loops the diff, calls `rename_tab(pos as u32 + 1, name)`
  (fire-and-forget `ChangeApplicationState`, permission already held), and records each applied
  name into `applied_names`.
- **Default-name pattern:** match Zellij's default tab name, `Tab #<n>` (verify exact format
  against zellij-tile 0.44 during implementation; treat an unrecognized format as "manual / do
  not touch" — fail safe toward never clobbering).

## Data-model & wiring changes (summary)

| Item | Change |
|---|---|
| `TabLite` | + `has_bell: bool` |
| `State` | + `applied_names: HashMap<usize, String>`; `tab_panes` value grows from `Vec<u32>` to carry per-pane `title` + `is_focused` (e.g. `Vec<PaneLite { id, title, is_focused }>`) |
| `Event::TabUpdate` | capture `has_bell_notification` |
| `Event::PaneUpdate` | capture `title` + `is_focused` per terminal pane; run `compute_renames` → `rename_tab` for each diff entry; update `applied_names` |
| `Event::pipe` | after `store.apply`, agent-tab names may change → recompute renames |
| `render` | emit optional header line (B); bell suffix (A); long-running marker (C) |
| `tab_position_at_line` | subtract header height before per-row span walk (B) |
| host calls | + `rename_tab` (fire-and-forget); **no blocking queries** |

`row_lines` + a new `header_lines(rows)` remain the single source of truth for vertical layout,
consumed by both `render` and `tab_position_at_line`.

## Testing (host `cargo test`, pure)

New tests, in addition to the existing 38:
- **A bell:** `has_bell` renders `⚑`; absent → no marker; line still within width with marker.
- **B summary:** counts tabs by dominant status, idle excluded; empty/all-idle → no header
  line; non-zero buckets only, severity order.
- **B click offset:** with a header present, `tab_position_at_line` maps row clicks correctly
  (off-by-header-height); clicking the header line → `None`.
- **C stuck:** elapsed `< 600` → no `⚠`; `>= 600` on `Running` → `⚠`; non-running long elapsed →
  no `⚠`.
- **D naming:** agent tab → repo; plain tab → focused pane title (falls back to first pane);
  default `Tab #N` is renamed; manual name is **not** touched; name equal to last-applied → no
  re-emit; computed == current → empty diff.

## Out of scope (follow-ups)
- git-root/cwd-based plain-tab naming (via `CwdChanged`).
- Per-pane breakdown within a multi-agent tab; `MOD+a` collapse toggle (already parked in
  `design.md` §12).
- Configurable `STUCK_SECS` / header format (hardcoded constants in v1).
