//! Pure renderer: per-tab rows → ANSI string. No zellij-tile dependency.

use crate::config::Density;
use crate::model::{PaneEntry, TabAgg};
pub use crate::status::GlyphSet;
use crate::status::{Role, Status};
use crate::theme::DerivedColors;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";

#[derive(Clone)]
pub struct RenderOpts {
    pub width: usize,
    pub height: usize,
    pub now_tick: u64,
    pub glyphs: GlyphSet,
    /// Whether to render the " RADAR" identity header block.
    pub header: bool,
    /// Vertical density between tabs.
    pub density: Density,
    /// Terminal-derived colors for card surfaces and readable dim text. These
    /// are the only truecolor values (status hues are ANSI-16). Defaults to a
    /// neutral-dark fallback until the terminal reports its bg/fg via PaneInfo.
    pub theme: DerivedColors,
}

pub struct TabRow {
    pub number: u32,
    pub name: String,
    pub active: bool,
    pub has_bell: bool,
    pub agg: TabAgg,
}

/// "0:14" under a minute-ish, "2m", "1h3m".
pub fn format_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("0:{:02}", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if UnicodeWidthStr::width(s) <= max {
        s.to_string()
    } else if max == 0 {
        String::new()
    } else {
        // Reserve 1 column for '…' (which itself has display width 1).
        let budget = max.saturating_sub(1);
        let mut kept = String::new();
        let mut used = 0usize;
        for c in s.chars() {
            let w = UnicodeWidthChar::width(c).unwrap_or(0);
            if used + w > budget {
                break;
            }
            kept.push(c);
            used += w;
        }
        format!("{}…", kept)
    }
}

/// The ONE source of truth for inter- and intra-card spacing, by density.
///
/// Three knobs, all measured in terminal cells:
///   - `pad_x`: columns of internal LEFT padding inserted before a card's
///     content (so content isn't flush to the band edge).
///   - `pad_y`: rows of internal TOP padding — blank rows painted with THIS
///     card's own surface bg (internal breathing room). Currently 0 for all
///     densities; retained as a knob for future tuning.
///   - `gap`: rows of EXTERNAL separation after a card — blank rows painted
///     `rail_bg` in Cards (panel shows through), plain blank in Comfortable.
///     Sheds first under overflow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CardSpacing {
    pub pad_x: usize,
    pub pad_y: usize,
    pub gap: usize,
}

/// Map a density to its spacing knobs. This is the single place to tune the
/// sidebar's vertical/horizontal rhythm.
pub fn card_spacing(d: Density) -> CardSpacing {
    match d {
        Density::Compact => CardSpacing {
            pad_x: 0,
            pad_y: 0,
            gap: 0,
        },
        Density::Comfortable => CardSpacing {
            pad_x: 0,
            pad_y: 0,
            gap: 1,
        },
        Density::Cards => CardSpacing {
            pad_x: 0,
            pad_y: 0,
            gap: 1,
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RailTarget {
    pub tab_position: usize,
    pub pane_id: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenderedRail {
    pub ansi: String,
    targets: Vec<Option<RailTarget>>,
}

impl RenderedRail {
    #[cfg_attr(all(target_arch = "wasm32", not(test)), allow(dead_code))]
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_ansi_without_targets(ansi: String) -> Self {
        let targets = ansi.lines().map(|_| None).collect();
        RenderedRail { ansi, targets }
    }

    pub fn target_at_line(&self, line: isize) -> Option<RailTarget> {
        if line < 0 {
            return None;
        }
        self.targets.get(line as usize).copied().flatten()
    }

    #[cfg_attr(all(target_arch = "wasm32", not(test)), allow(dead_code))]
    pub fn line_count(&self) -> usize {
        self.targets.len()
    }
}

/// Single source of truth for a card's full vertical footprint (top→bottom:
/// `pad_y` internal-pad rows + the card's uncompressed content rows + `gap`
/// external-separation rows). `render_rail()` budgets in terms of this so the
/// emitted ANSI lines and line targets stay exact.
pub fn card_block_lines(agg: &TabAgg, active: bool, spacing: CardSpacing) -> usize {
    spacing.pad_y + row_lines(agg, active) + spacing.gap
}

/// A pane is "multi-pane" for tree purposes when it has more than one ever-active
/// pane. Single-pane tabs keep the chunk-1 line-2 behavior; multi-pane tabs use
/// the adaptive tree (`pane_tree_plan`).
pub fn is_multi_pane(agg: &TabAgg) -> bool {
    agg.panes.len() > 1
}

/// The expand/collapse split for a multi-pane tab's adaptive tree. The SINGLE
/// SOURCE OF TRUTH consulted by `row_lines`, `render_rail`, and target mapping,
/// so line counts and click targets stay in lockstep.
///
/// Rules:
///   - Panes that NEED YOU (Pending or Error) ALWAYS expand.
///   - If the tab is the ACTIVE (focused) tab, ALL its panes expand.
///   - The remaining calm panes (Running/Done/Idle) collapse into a single
///     count line. `collapsed_verb` is "working" if any collapsed pane is
///     Running, else "done".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneTreePlan {
    pub expanded: Vec<PaneEntry>,
    pub collapsed_count: usize,
    /// The dominant calm verb for the collapse line ("working" or "done").
    pub collapsed_verb: &'static str,
}

/// Compute the expand/collapse split for a multi-pane tab. Callers should only
/// invoke this when `is_multi_pane(agg)` is true; for a single-pane tab it would
/// return all panes collapsed (which is not how single-pane tabs render).
pub fn pane_tree_plan(agg: &TabAgg, active: bool) -> PaneTreePlan {
    let needs_you = |s: Status| matches!(s, Status::Pending | Status::Error);
    let mut expanded: Vec<PaneEntry> = Vec::new();
    let mut collapsed: Vec<&PaneEntry> = Vec::new();
    for p in &agg.panes {
        if active || needs_you(p.status) {
            expanded.push(p.clone());
        } else {
            collapsed.push(p);
        }
    }
    let any_running = collapsed.iter().any(|p| p.status == Status::Running);
    let collapsed_verb = if any_running { "working" } else { "done" };
    PaneTreePlan {
        expanded,
        collapsed_count: collapsed.len(),
        collapsed_verb,
    }
}

/// Single source of truth for how many lines a tab row occupies.
///
/// Single-pane tabs (chunk-1 line-2 rule): idle/done → 1 line; any other active
/// state with a non-empty msg → 2 lines (line 1 = status, line 2 = mark +
/// activity); a detail without a msg → 1 line (line 2 suppressed).
///
/// Multi-pane tabs (chunk-2 adaptive tree): 1 header line + one child line per
/// expanded pane + one collapse line when any calm pane is collapsed. The split
/// is computed by `pane_tree_plan(agg, active)` so rendered lines and their
/// click targets stay in lockstep.
pub fn row_lines(agg: &TabAgg, active: bool) -> usize {
    if is_multi_pane(agg) {
        let plan = pane_tree_plan(agg, active);
        let collapse_line = if plan.collapsed_count > 0 { 1 } else { 0 };
        return 1 + plan.expanded.len() + collapse_line;
    }
    match agg.status {
        Status::Idle | Status::Done => 1,
        Status::Running | Status::Error | Status::Pending => match &agg.detail {
            Some(d) if !d.msg.trim().is_empty() => 2,
            _ => 1,
        },
    }
}

/// Right-aligned status slot text (no color). Empty for idle.
fn right_slot(agg: &TabAgg, now_tick: u64, width: usize) -> String {
    let elapsed = agg
        .detail
        .as_ref()
        .map(|d| format_elapsed(now_tick.saturating_sub(d.since_tick)))
        .unwrap_or_default();
    let count = if agg.total > 1 {
        format!("{}/{} ", agg.done, agg.total)
    } else {
        String::new()
    };
    match agg.status {
        Status::Idle => String::new(),
        Status::Running => format!("{}{}", count, elapsed),
        Status::Pending => format!("{}⏵ {}", count, elapsed),
        Status::Done => "done".to_string(),
        Status::Error => {
            if width < 16 {
                "err".to_string()
            } else {
                "failed".to_string()
            }
        }
    }
}

/// The rail's identity header. Single source of truth for the header's vertical
/// span. Only the truly-empty case (no rows at all) is headerless; when `header`
/// is false the identity block is suppressed and rows start at line 0.
///
/// In Cards density the carded hero is just the " RADAR …" title (1 line) — the
/// `═` rule is dropped so cards begin immediately under the title. Compact and
/// Comfortable keep the two-line title+rule header. `render_rail()` uses the
/// same emitted header lines for ANSI and targets, so the count stays in lockstep.
pub fn header_lines(rows: &[TabRow], header: bool, density: Density) -> usize {
    if rows.is_empty() || !header {
        0
    } else if density == Density::Cards {
        1
    } else {
        2
    }
}

/// Returns whether a row's status is "calm" (can be compressed first).
fn is_calm(status: Status) -> bool {
    matches!(status, Status::Done | Status::Running)
}

/// Decide which rows render and at how many lines each, given the vertical
/// budget. Returns `(Vec<(row_idx, rendered_lines)>, strip_folded_count)`.
///
/// Compression order when `sum(full lines of kept rows) > body_budget`:
///   1. Fold idle rows into a strip (existing behaviour).
///   2. Drop the strip line itself (set `strip_folded_count = 0`) if even the
///      non-idle rows exceed the budget.
///   3. Compress calm non-idle rows (Done, Running) to 1 line each —
///      lowest-position first, one at a time until it fits.
///   4. Compress urgent rows (Pending, Error) toward 1 line — drop msg line
///      first, then drop the branch/needs-you detail line — lowest-position
///      first, one step at a time.
///   5. If still over: include rows from the top as long as they fit; drop the
///      rest (never panics, never exceeds budget).
///
/// `row_lines` remains the *uncompressed* line count; this function produces
/// the *planned* per-row line count actually rendered.
pub fn plan_overflow(rows: &[TabRow], body_budget: usize) -> (Vec<(usize, usize)>, usize) {
    let total: usize = rows.iter().map(|r| row_lines(&r.agg, r.active)).sum();
    if total <= body_budget {
        // Everything fits at full fidelity.
        let plan = rows
            .iter()
            .enumerate()
            .map(|(i, r)| (i, row_lines(&r.agg, r.active)))
            .collect();
        return (plan, 0);
    }

    // Step 1: fold idle rows; keep non-idle at full line counts.
    let non_idle_idx: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.agg.status != Status::Idle)
        .map(|(i, _)| i)
        .collect();
    let folded_count = rows.iter().filter(|r| r.agg.status == Status::Idle).count();

    // Each kept row starts at its full (uncompressed) line count.
    let mut planned: Vec<(usize, usize)> = non_idle_idx
        .iter()
        .map(|&i| (i, row_lines(&rows[i].agg, rows[i].active)))
        .collect();

    // Step 2: check if the strip line itself (1 extra line) would overflow.
    let strip_line: usize = if folded_count > 0 { 1 } else { 0 };
    let mut used: usize = planned.iter().map(|(_, l)| l).sum();
    // `strip_folded` is what we actually show in the strip (0 = dropped).
    let strip_folded = if used + strip_line <= body_budget {
        folded_count // strip fits
    } else {
        0 // drop the strip; don't count its line
    };
    // `used` now reflects whether the strip is shown.
    let strip_used = if strip_folded > 0 { 1 } else { 0 };

    if used + strip_used <= body_budget {
        return (planned, strip_folded);
    }

    // Step 3: compress calm rows (Done/Running) to 1 line, lowest-idx first.
    for entry in planned.iter_mut() {
        let (idx, ref mut lines) = *entry;
        if *lines <= 1 {
            continue;
        }
        if is_calm(rows[idx].agg.status) {
            used -= *lines - 1; // account for the lines we're dropping
            *lines = 1;
            if used + strip_used <= body_budget {
                return (planned, strip_folded);
            }
        }
    }

    // Step 4: compress urgent rows (Pending/Error) toward 1 line.
    // Each urgent row: full -> drop msg -> drop branch/needs-you -> 1 line.
    // We decrement one line at a time per row (lowest-idx first), repeating
    // until the row reaches 1 or the budget is satisfied.
    let mut changed = true;
    while changed {
        changed = false;
        if used + strip_used <= body_budget {
            return (planned, strip_folded);
        }
        for entry in planned.iter_mut() {
            let (idx, ref mut lines) = *entry;
            if *lines <= 1 {
                continue;
            }
            if !is_calm(rows[idx].agg.status) {
                used -= 1;
                *lines -= 1;
                changed = true;
                if used + strip_used <= body_budget {
                    return (planned, strip_folded);
                }
                // Restart from lowest-idx for next step.
                break;
            }
        }
    }

    // Step 5: everything is at 1 line; if still over, drop bottom rows that
    // don't fit. (Extreme case: even 1-line rows exceed the budget.)
    let mut trimmed: Vec<(usize, usize)> = Vec::new();
    let mut remaining = body_budget;
    for (idx, lines) in &planned {
        if *lines <= remaining {
            trimmed.push((*idx, *lines));
            remaining -= lines;
        } else if remaining > 0 {
            trimmed.push((*idx, remaining));
            remaining = 0;
        } else {
            break;
        }
    }
    (trimmed, strip_folded)
}

/// Single source of truth for the layout plan consumed by `render_rail()`.
/// Returns:
///   - the per-row planned content-line counts (same as `plan_overflow`),
///   - the number of idle rows folded into the strip (`strip_folded`), and
///   - the EFFECTIVE `CardSpacing` actually applied (luxury rows may be shed
///     under overflow): `pad_x` is unchanged; `pad_y` and `gap` are each 1 or 0.
///
/// Luxury-shedding rule (mirrors "gaps are dropped first"): the per-card block
/// is `pad_y + content + gap`. Under overflow we shed the cheapest separation
/// first — drop `gap`, then drop `pad_y` — before letting `plan_overflow`
/// compress the content itself. We pick the richest spacing whose total block
/// footprint (plus the strip line) still fits the budget.
pub fn plan_layout(
    rows: &[TabRow],
    body_budget: usize,
    density: Density,
) -> (Vec<(usize, usize)>, usize, CardSpacing) {
    let base = card_spacing(density);

    // Fast path: if every row's FULL block (pad_y + content + gap) fits, render
    // everything at full fidelity with full spacing. `card_block_lines` is the
    // single footprint source shared with the budgeting below.
    let full_footprint: usize = rows
        .iter()
        .map(|r| card_block_lines(&r.agg, r.active, base))
        .sum();
    if full_footprint <= body_budget {
        let plan = rows
            .iter()
            .enumerate()
            .map(|(i, r)| (i, row_lines(&r.agg, r.active)))
            .collect();
        return (plan, 0, base);
    }

    // Candidate spacings, richest → leanest: full, then drop gap, then drop pad_y.
    // pad_x never sheds (it's a fixed horizontal inset, not a vertical row).
    let candidates = [
        base,
        CardSpacing { gap: 0, ..base },
        CardSpacing {
            gap: 0,
            pad_y: 0,
            ..base
        },
    ];

    for spacing in candidates {
        // Budget the content against the space left after this spacing's luxury
        // rows. Each kept row costs `pad_y + gap` luxury rows on top of content.
        let (plan, strip_folded) = plan_overflow(rows, body_budget);
        let content_total: usize = plan.iter().map(|(_, l)| l).sum();
        let kept = plan.len();
        let strip_line = if strip_folded > 0 { 1 } else { 0 };
        let luxury = kept * (spacing.pad_y + spacing.gap);
        if content_total + luxury + strip_line <= body_budget {
            return (plan, strip_folded, spacing);
        }
    }

    // Even the leanest spacing (no pad_y, no gap) overflows: let plan_overflow
    // compress content against the raw budget and apply no luxury rows.
    let lean = CardSpacing {
        gap: 0,
        pad_y: 0,
        ..base
    };
    let (plan, strip_folded) = plan_overflow(rows, body_budget);
    (plan, strip_folded, lean)
}

/// The rail's resting "hello / how it works" face — shown on cold start or
/// before permission is granted. Not a permission interceptor.
pub fn onboarding(opts: &RenderOpts) -> String {
    let mut out = String::new();
    let accent = Role::Accent.ansi();
    let muted = Role::Muted.ansi();
    let g = opts.glyphs;
    out.push_str(&format!("{} RADAR{}\n", accent, RESET));
    out.push_str(&format!("{}{}{}\n", accent, "═".repeat(opts.width), RESET));
    out.push_str(&format!("{} watching your tabs for{}\n", muted, RESET));
    out.push_str(&format!("{} AI agent activity.{}\n", muted, RESET));
    out.push('\n');
    let legend = [
        (Status::Pending, "needs you"),
        (Status::Running, "working"),
        (Status::Done, "done"),
        (Status::Error, "error"),
        (Status::Idle, "idle"),
    ];
    for (st, label) in legend {
        out.push_str(&format!(
            " {}{}{} {}{}{}\n",
            st.role().ansi(),
            st.glyph_for(g),
            RESET,
            muted,
            label,
            RESET
        ));
    }
    out.push('\n');
    out.push_str(&format!("{} click a row to jump{}\n", muted, RESET));
    out
}

/// Emit one row's body into `out`, respecting `max_lines`.
///
/// Line 1 (gutter+glyph+num+name+slot) is ALWAYS emitted.
/// Detail/roster lines are emitted in priority order while `lines_emitted < max_lines`:
///
/// - For urgent rows (Pending/Error): detail line comes before msg line;
///   roster is least-priority and is dropped first.
/// - For calm rows (Done/Running): there is at most 1 detail line + 1 roster;
///   roster is dropped before the detail line.
///
/// Caller guarantees `max_lines >= 1`.
fn render_row<F>(
    out: &mut String,
    row: &TabRow,
    opts: &RenderOpts,
    max_lines: usize,
    mut record_target: F,
) where
    F: FnMut(Option<RailTarget>),
{
    let width = opts.width;
    let now_tick = opts.now_tick;
    let st = row.agg.status;
    let tab_target = target_for_row(row);

    // Status HUES are always ANSI-16 role codes so the terminal renders them in
    // its OWN theme (any theme, zero config): attention `\x1b[91m` (waiting),
    // error `\x1b[31m`, working `\x1b[33m`, success `\x1b[32m`, accent `\x1b[35m`
    // (spine). ANSI has no orange/peach, so waiting (also bright-red-family)
    // stays distinct from the error row via shape + bold (◆ + bold vs ✗), not
    // hue. Only the dark panel surfaces + dim greys are truecolor (terminal-bg/fg
    // derived), so those match the terminal's theme too.
    let hue = |r: Role| -> String { r.ansi().to_string() };

    // col 0: active spine — accent (mauve) normally, attention (peach) when the
    // active row is also waiting/error.
    let bar = if row.active {
        let bar_role = match st {
            Status::Pending | Status::Error => Role::Attention,
            _ => Role::Accent,
        };
        format!("{}▌{}", hue(bar_role), RESET)
    } else {
        " ".to_string()
    };

    // Internal left padding: `pad_x` cells after the col-0 spine/space, before
    // the glyph. Currently 0 for all densities — the col-0 bar/spine column
    // already provides the design's 1-col card inset, so an extra pad column
    // would double the left margin (and push content off the right at narrow
    // widths). Retained as a knob.
    let pad_len = card_spacing(opts.density).pad_x;
    let pad = " ".repeat(pad_len);

    // col 1: status glyph (working spins).
    let glyph_char = if st == Status::Running {
        crate::status::working_spin(now_tick as usize)
    } else {
        st.glyph_for(opts.glyphs)
    };
    // The whole label (glyph + number + name) shares the status color so each
    // row reads as its state at a glance — design: "4 web" is green, "5 infra"
    // red, "3 pinky" peach+bold. Idle recedes to the dim idle_text; waiting is
    // also bold (it's the alarm). Active is shown by the spine + brighter card,
    // NOT bold, so the two cues stay independent.
    let label_color = if st == Status::Idle {
        tc_fg(opts.theme.idle_text)
    } else {
        hue(st.role())
    };
    let label_bold = if st == Status::Pending { BOLD } else { "" };

    // right slot (reserved width even when empty). Waiting/done/error color
    // their slot with the status role (it carries meaning); the *working*
    // elapsed is ambient info, not an alarm, so it's dimmed (design uses `id`).
    let slot = right_slot(&row.agg, now_tick, width);
    let slot_styled = if slot.is_empty() {
        String::new()
    } else if st == Status::Running {
        format!("{}{}{}", tc_fg(opts.theme.idle_text), slot, RESET)
    } else {
        format!("{}{}{}", hue(st.role()), slot, RESET)
    };

    // bell marker just before the slot.
    let bell = if row.has_bell {
        format!("{}⚑{} ", Role::Working.ansi(), RESET)
    } else {
        String::new()
    };

    // left visible prefix is "X[pad]<glyph> <num> " — bar/glyph are 1 cell each;
    // `pad_len` is the Cards-only internal left pad (1 col, else 0).
    let num = row.number.to_string();
    let prefix_len = 1 + pad_len + 1 + 1 + UnicodeWidthStr::width(num.as_str()) + 1; // bar+pad+glyph+sp+num+sp
    let bell_len = if row.has_bell { 2 } else { 0 };
    let slot_len = UnicodeWidthStr::width(slot.as_str());
    let name_budget = width
        .saturating_sub(prefix_len + bell_len + slot_len + 1) // +1 min gap
        .max(1);
    let name = truncate(&row.name, name_budget);

    // pad so the slot sits flush right.
    let used = prefix_len + UnicodeWidthStr::width(name.as_str()) + bell_len + slot_len;
    let gap = width.saturating_sub(used).max(1);
    out.push_str(&format!(
        "{}{}{}{}{} {} {}{}{}{}{}\n",
        bar,
        pad,
        label_color,
        label_bold,
        glyph_char,
        num,
        name,
        RESET,
        " ".repeat(gap),
        bell,
        slot_styled
    ));
    record_target(Some(tab_target));

    // Line 1 done. Emit child/detail lines only within the remaining budget.
    if max_lines <= 1 {
        return;
    }
    let mut emitted = 1usize;

    // Theme-derived detail text colors: dim_strong for activity text on non-pending rows,
    // idle_text for the muted tree chars and identity mark glyph (neutral/vendor color).
    // Both are truecolor foreground escapes derived from the bg/fg palette blend.
    let dim_strong = tc_fg(opts.theme.dim_strong);
    let idle_color = tc_fg(opts.theme.idle_text);

    // ── Multi-pane adaptive tree (chunk 2) ─────────────────────────────────
    // A tab with >1 reporting pane renders as: header (line 1, above) + one
    // child line per EXPANDED pane (needs-you always; all panes when active) +
    // one collapse line for the calm remainder. The expand/collapse split comes
    // from `pane_tree_plan`, the SINGLE source of truth shared with `row_lines`
    // and this function's target recorder, so emitted lines stay in lockstep.
    if is_multi_pane(&row.agg) {
        let plan = pane_tree_plan(&row.agg, row.active);
        let has_collapse = plan.collapsed_count > 0;
        let total_children = plan.expanded.len() + if has_collapse { 1 } else { 0 };

        for (idx, pane) in plan.expanded.iter().enumerate() {
            if emitted >= max_lines {
                return;
            }
            // The last child line (the collapse line, or the last expanded pane
            // when nothing collapses) gets the `└ ` corner; the rest get `├ `.
            let is_last = !has_collapse && idx + 1 == total_children;
            let tree = if is_last { "└ " } else { "├ " };
            emit_child_line(out, tree, pane, opts, &idle_color, &dim_strong);
            record_target(Some(RailTarget {
                tab_position: tab_target.tab_position,
                pane_id: Some(pane.pane_id),
            }));
            emitted += 1;
        }

        if has_collapse && emitted < max_lines {
            // Collapse line: `  └ N more working` (or "done").
            let text = format!("└ {} more {}", plan.collapsed_count, plan.collapsed_verb);
            let clamped = truncate(&text, width.saturating_sub(2));
            out.push_str(&format!("  {}{}{}\n", idle_color, clamped, RESET));
            record_target(Some(tab_target));
        }
        return;
    }

    // ── Single-pane line 2 (chunk 1) ───────────────────────────────────────
    // Line 2: `‹mark› ‹activity›` — source-agnostic for all active statuses.
    // Only emitted when the status is active, a detail with a non-empty msg exists,
    // and there is remaining budget. For Running, the braille spinner is appended.
    // For Pending (the question), activity is colored in attention (loud). Others dim_strong.
    if let Some(d) = &row.agg.detail {
        if emitted < max_lines && !d.msg.trim().is_empty() {
            match st {
                Status::Idle | Status::Done => {}
                Status::Running | Status::Error | Status::Pending => {
                    // mark glyph in neutral idle_text color (vendor-neutral)
                    let mark = d.kind.mark();
                    let mark_width = UnicodeWidthChar::width(mark).unwrap_or(1);
                    // "  ‹mark› " prefix: 2-space indent + mark + space. The
                    // mark sits one column right of the line-1 glyph (which is at
                    // col 1 after the bar/spine column), matching the design.
                    let prefix_vis = 2 + mark_width + 1;
                    let avail = width.saturating_sub(prefix_vis);
                    // Build activity string: for Running append braille spinner
                    let activity = if st == Status::Running {
                        let spin = crate::status::msg_spin(now_tick as usize);
                        format!("{} {}", d.msg, spin)
                    } else {
                        d.msg.clone()
                    };
                    let activity_str = truncate(&activity, avail);
                    let activity_color = if st == Status::Pending {
                        hue(Role::Attention)
                    } else {
                        dim_strong.clone()
                    };
                    out.push_str(&format!(
                        "  {}{}{} {}{}{}\n",
                        idle_color, mark, RESET, activity_color, activity_str, RESET
                    ));
                    record_target(Some(tab_target));
                }
            }
        }
    }
}

/// Emit one expanded-pane child line of the adaptive tree:
/// `  ‹tree›‹status-glyph› ‹mark› ‹activity›` — 2-space indent, then the tree char
/// (muted/idle_text), the pane's status glyph in its status role color, a space,
/// the pane's `kind.mark()` in the neutral vendor color (idle_text), a space,
/// then the activity (`msg`), width-truncated (attention color when Pending,
/// else dim).
///
/// The STATUS glyph comes first (right after the tree connector) so it sits in a
/// fixed column that a variable-width identity mark can't shift — keeping the
/// status icons in a clean vertical line down the tree. The space between the
/// status glyph and the identity mark keeps the two glyphs from cramping (they
/// were previously rendered flush, e.g. `✳◐`, which read as one illegible blob).
fn emit_child_line(
    out: &mut String,
    tree: &str,
    pane: &PaneEntry,
    opts: &RenderOpts,
    idle_color: &str,
    dim_strong: &str,
) {
    let width = opts.width;
    let mark = pane.kind.mark();
    let mark_w = UnicodeWidthChar::width(mark).unwrap_or(1);
    // Status glyph: working spins; others use the static glyph.
    let glyph = if pane.status == Status::Running {
        crate::status::working_spin(opts.now_tick as usize)
    } else {
        pane.status.glyph_for(opts.glyphs)
    };
    let glyph_w = UnicodeWidthChar::width(glyph).unwrap_or(1);
    // Visible prefix: 2 indent + tree(2) + glyph + 1 space + mark + 1 space.
    let tree_w = UnicodeWidthStr::width(tree);
    let prefix_vis = 2 + tree_w + glyph_w + 1 + mark_w + 1;
    let avail = width.saturating_sub(prefix_vis);
    let activity_str = truncate(&pane.msg, avail);
    let activity_color = if pane.status == Status::Pending {
        Role::Attention.ansi().to_string()
    } else {
        dim_strong.to_string()
    };
    out.push_str(&format!(
        "  {}{}{}{}{}{} {}{}{} {}{}{}\n",
        idle_color,
        tree,
        RESET, // tree char (muted)
        pane.status.role().ansi(),
        glyph,
        RESET, // status glyph in role color (aligned column)
        idle_color,
        mark,
        RESET, // identity mark (neutral vendor color)
        activity_color,
        activity_str,
        RESET, // activity
    ));
}

/// Measure visible (display) width of a string that may contain ANSI SGR escapes.
fn visible_width(s: &str) -> usize {
    let mut width = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                for inner in chars.by_ref() {
                    if inner == 'm' {
                        break;
                    }
                }
            }
        } else {
            width += UnicodeWidthChar::width(c).unwrap_or(0);
        }
    }
    width
}

/// Emit an ANSI truecolor background escape for a given (r, g, b) triple.
fn tc_bg(c: (u8, u8, u8)) -> String {
    format!("\x1b[48;2;{};{};{}m", c.0, c.1, c.2)
}

/// Emit an ANSI truecolor foreground escape for a given (r, g, b) triple.
#[allow(dead_code)]
fn tc_fg(c: (u8, u8, u8)) -> String {
    format!("\x1b[38;2;{};{};{}m", c.0, c.1, c.2)
}

/// The truecolor surface tint for a card, by class: the focused tab is
/// brightest, agent rows (active status) are mid, idle/plain panes are
/// the dimmest surface. Returns an owned ANSI escape string.
fn card_tint(row: &TabRow, theme: &DerivedColors) -> String {
    let rgb = if row.active {
        theme.surface_active
    } else if row.agg.status.is_active() {
        theme.surface_agent
    } else {
        theme.surface_idle
    };
    tc_bg(rgb)
}

/// Paint a single content line with a 256-color surface background band (`bg`).
///
/// Steps:
/// 1. Replace every `RESET` (`\x1b[0m`) in the line with `RESET + bg` so that
///    colored tokens re-arm the background after they reset.
/// 2. Strip the trailing newline (if present), measure, pad to `width`, restore.
/// 3. Wrap: `bg + transformed_line + pad + "\x1b[49m\x1b[0m"`.
///
/// The returned string ends with `\n`.
fn paint_card_line(line: &str, width: usize, bg: &str) -> String {
    const BG_RESET: &str = "\x1b[49m";

    // Strip trailing newline; we'll add it back at the end.
    let bare = line.strip_suffix('\n').unwrap_or(line);

    // Re-arm bg after every reset token inside the line.
    let rearmed = bare.replace(RESET, &format!("{}{}", RESET, bg));

    // Measure visible width of the re-armed content.
    let vis = visible_width(&rearmed);

    // Pad to fill the band up to `width`.
    let pad = if vis < width {
        " ".repeat(width - vis)
    } else {
        String::new()
    };

    format!("{}{}{}{}{}\n", bg, rearmed, pad, BG_RESET, RESET)
}

fn target_for_row(row: &TabRow) -> RailTarget {
    RailTarget {
        tab_position: row.number.saturating_sub(1) as usize,
        pane_id: None,
    }
}

fn render_row_buffer(
    row: &TabRow,
    opts: &RenderOpts,
    max_lines: usize,
) -> (String, Vec<Option<RailTarget>>) {
    let mut tab_buf = String::new();
    let mut row_targets = Vec::new();
    render_row(&mut tab_buf, row, opts, max_lines.max(1), |target| {
        row_targets.push(target);
    });
    debug_assert_eq!(row_targets.len(), tab_buf.matches('\n').count());
    (tab_buf, row_targets)
}

pub fn render_rail(rows: &[TabRow], opts: &RenderOpts) -> RenderedRail {
    let mut out = String::new();
    let mut targets: Vec<Option<RailTarget>> = Vec::new();
    if rows.is_empty() {
        return RenderedRail { ansi: out, targets };
    }
    let width = opts.width;
    let accent = Role::Accent.ansi();
    let cards = opts.density == Density::Cards;
    // In Cards density the whole sidebar is a cohesive dark panel: the header,
    // gaps and idle strip all sit on `rail_bg`; only card content lines carry
    // their (subtle, ladder-derived) surface tint.
    let rail = tc_bg(opts.theme.rail_bg);

    let body_budget = opts
        .height
        .saturating_sub(header_lines(rows, opts.header, opts.density));
    let (plan, strip_folded, spacing) = plan_layout(rows, body_budget, opts.density);
    // Overflow = any row is absent from the plan (those are idle-folded rows).
    let overflow = plan.len() < rows.len();
    // Right-aligned count: total tabs (·N, or "N ▲" when overflowing), plus a
    // "·P!" urgent marker in the attention role when any tab needs you.
    let count = if overflow {
        format!("{} ▲", rows.len())
    } else {
        format!("·{}", rows.len())
    };
    let pending = rows
        .iter()
        .filter(|r| r.agg.status == Status::Pending)
        .count();
    let urgent = if pending > 0 {
        format!(" ·{}!", pending)
    } else {
        String::new()
    };

    // Emit the identity header block only when configured on (and rows exist).
    // Header line 1: " RADAR" + right-aligned count (+ urgent marker).
    if opts.header {
        let title = " RADAR";
        let right_w =
            UnicodeWidthStr::width(count.as_str()) + UnicodeWidthStr::width(urgent.as_str());
        let gap = width
            .saturating_sub(UnicodeWidthStr::width(title) + right_w)
            .max(1);
        // Title in accent; total count muted (accent when overflowing, so the
        // ▲ marker stays loud); urgent marker in the attention role.
        let count_color = if overflow { accent } else { Role::Muted.ansi() };
        let mut title_line = String::new();
        title_line.push_str(&format!(
            "{}{}{}{}{}{}{}",
            accent,
            title,
            RESET,
            " ".repeat(gap),
            count_color,
            count,
            RESET
        ));
        if pending > 0 {
            title_line.push_str(&format!("{}{}{}", Role::Attention.ansi(), urgent, RESET));
        }
        title_line.push('\n');
        if cards {
            // Carded hero: just the " RADAR …" title on the dark panel — no
            // rule line (header_lines() returns 1 for Cards to match).
            out.push_str(&paint_card_line(&title_line, width, &rail));
            targets.push(None);
        } else {
            out.push_str(&title_line);
            targets.push(None);
            // Header line 2: rule across the full width.
            out.push_str(&format!("{}{}{}\n", accent, "═".repeat(width), RESET));
            targets.push(None);
        }
    }

    for &(i, max_lines) in &plan {
        let row_target = target_for_row(&rows[i]);
        // Every tab is a card: render it into a temporary buffer, then paint
        // each content line with its class's surface tint (idle < agent <
        // active) — a subtle step up from the dark panel.
        if cards {
            let bg = card_tint(&rows[i], &opts.theme);
            // pad_y rows: blank, painted with THIS card's own surface bg —
            // card-colored internal TOP padding (breathing room) that belongs
            // to this tab's click span.
            for _ in 0..spacing.pad_y {
                out.push_str(&paint_card_line("\n", width, &bg));
                targets.push(Some(row_target));
            }
            let (tab_buf, row_targets) = render_row_buffer(&rows[i], opts, max_lines);
            for (line_idx, line) in tab_buf.split_inclusive('\n').enumerate() {
                out.push_str(&paint_card_line(line, width, &bg));
                targets.push(
                    row_targets
                        .get(line_idx)
                        .copied()
                        .unwrap_or(Some(row_target)),
                );
            }
        } else {
            // Non-card densities: pad_y is 0, so emit content directly.
            for _ in 0..spacing.pad_y {
                out.push('\n');
                targets.push(Some(row_target));
            }
            let (tab_buf, row_targets) = render_row_buffer(&rows[i], opts, max_lines);
            for (line_idx, line) in tab_buf.split_inclusive('\n').enumerate() {
                out.push_str(line);
                targets.push(
                    row_targets
                        .get(line_idx)
                        .copied()
                        .unwrap_or(Some(row_target)),
                );
            }
        }
        // Emit blank gap line(s) after each tab's content block (external
        // separation). In Cards the gap is painted with the dark panel base (so
        // the whole column is one cohesive panel); otherwise it stays bare.
        for _ in 0..spacing.gap {
            if cards {
                out.push_str(&paint_card_line("\n", width, &rail));
            } else {
                out.push('\n');
            }
            targets.push(None);
        }
    }

    if strip_folded > 0 {
        // Width-safe idle strip: keep the "── +N idle ▾" suffix, fill the
        // space before it with as many "○ " dot pairs as fit.
        let suffix = format!("── +{} idle ▾", strip_folded);
        let dot_budget = width.saturating_sub(UnicodeWidthStr::width(suffix.as_str()) + 1); // +1 gap
        let mut dots = String::new();
        while UnicodeWidthStr::width(dots.as_str()) + 2 <= dot_budget {
            dots.push_str("○ ");
        }
        let plain = format!("{}{}", dots, suffix);
        // Guard the extreme-narrow case where even the suffix overflows.
        let clamped = truncate(&plain, width);
        let strip_line = format!("{}{}{}\n", Role::Accent.ansi(), clamped, RESET);
        // In Cards the idle strip is part of the dark panel → paint it on rail_bg.
        if cards {
            out.push_str(&paint_card_line(&strip_line, width, &rail));
        } else {
            out.push_str(&strip_line);
        }
        targets.push(None);
    }
    RenderedRail { ansi: out, targets }
}

#[cfg_attr(all(target_arch = "wasm32", not(test)), allow(dead_code))]
pub fn render(rows: &[TabRow], opts: &RenderOpts) -> String {
    render_rail(rows, opts).ansi
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kind::Kind;
    use crate::model::Detail;

    fn agg(status: Status, done: usize, total: usize, detail: Option<Detail>) -> TabAgg {
        TabAgg {
            status,
            done,
            total,
            pending: if status == Status::Pending { 1 } else { 0 },
            detail,
            panes: vec![],
        }
    }

    fn ro(width: usize, now_tick: u64) -> RenderOpts {
        RenderOpts {
            width,
            height: 100,
            now_tick,
            glyphs: GlyphSet::Plain,
            header: true,
            density: crate::config::Density::Compact,
            theme: crate::theme::DerivedColors::default(),
        }
    }

    #[test]
    fn format_elapsed_buckets() {
        assert_eq!(format_elapsed(14), "0:14");
        assert_eq!(format_elapsed(120), "2m");
        assert_eq!(format_elapsed(3780), "1h3m");
    }

    #[test]
    fn header_is_title_then_rule_two_lines() {
        let rows = vec![TabRow {
            number: 1,
            name: "a".into(),
            active: false,
            has_bell: false,
            agg: agg(Status::Running, 0, 0, None),
        }];
        assert_eq!(
            header_lines(&rows, true, crate::config::Density::Compact),
            2
        );
        let s = render(&rows, &ro(24, 0));
        let mut lines = s.lines();
        let title = lines.next().unwrap();
        let rule = lines.next().unwrap();
        assert!(title.contains("RADAR"));
        assert!(title.contains("·1")); // one tab
        assert!(rule.contains('═'));
    }

    #[test]
    fn header_absent_for_empty_rows() {
        let rows: Vec<TabRow> = vec![];
        assert_eq!(
            header_lines(&rows, true, crate::config::Density::Compact),
            0
        );
        assert!(render(&rows, &ro(24, 0)).is_empty());
    }

    #[test]
    fn rendered_rail_tracks_targets_for_each_emitted_line() {
        assert_eq!(RenderedRail::empty().line_count(), 0);
        let untargeted = RenderedRail::from_ansi_without_targets("a\nb\n".to_string());
        assert_eq!(untargeted.line_count(), 2);
        assert_eq!(untargeted.target_at_line(0), None);

        let detail = Detail {
            repo: "repo".into(),
            branch: "main".into(),
            msg: "approve".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Pending,
        };
        let rows = vec![
            TabRow {
                number: 1,
                name: "team".into(),
                active: false,
                has_bell: false,
                agg: TabAgg {
                    status: Status::Pending,
                    done: 0,
                    total: 2,
                    pending: 1,
                    detail: Some(detail),
                    panes: vec![
                        PaneEntry {
                            pane_id: 10,
                            kind: Kind::Claude,
                            status: Status::Pending,
                            msg: "approve".into(),
                        },
                        PaneEntry {
                            pane_id: 11,
                            kind: Kind::Claude,
                            status: Status::Running,
                            msg: "tests".into(),
                        },
                    ],
                },
            },
            TabRow {
                number: 2,
                name: "plain".into(),
                active: false,
                has_bell: false,
                agg: agg(Status::Idle, 0, 0, None),
            },
        ];

        let rail = render_rail(&rows, &ro(40, 0));
        assert_eq!(rail.line_count(), rail.ansi.matches('\n').count());
        assert_eq!(rail.target_at_line(-1), None);
        assert_eq!(rail.target_at_line(0), None);
        assert_eq!(rail.target_at_line(1), None);
        assert_eq!(
            rail.target_at_line(2),
            Some(RailTarget {
                tab_position: 0,
                pane_id: None,
            })
        );
        assert_eq!(
            rail.target_at_line(3),
            Some(RailTarget {
                tab_position: 0,
                pane_id: Some(10),
            })
        );
        assert_eq!(
            rail.target_at_line(4),
            Some(RailTarget {
                tab_position: 0,
                pane_id: None,
            })
        );
        assert_eq!(
            rail.target_at_line(5),
            Some(RailTarget {
                tab_position: 1,
                pane_id: None,
            })
        );
    }

    #[test]
    fn plain_tab_renders_name_only_no_second_line() {
        let rows = vec![TabRow {
            number: 4,
            name: "notes".into(),
            active: false,
            has_bell: false,
            agg: agg(Status::Idle, 0, 0, None),
        }];
        let s = render(&rows, &ro(24, 0));
        assert!(s.contains("notes"));
        assert_eq!(s.matches('\n').count(), 3); // always-on header (2) + tab row (1)
        assert!(s.contains(Status::Idle.glyph_for(GlyphSet::Plain)));
    }

    #[test]
    fn row_lines_by_state() {
        assert_eq!(row_lines(&agg(Status::Idle, 0, 0, None), false), 1);

        let detail = |status, msg: &str| {
            Some(Detail {
                repo: "r".into(),
                branch: "b".into(),
                msg: msg.into(),
                kind: Kind::Claude,
                since_tick: 0,
                status,
            })
        };
        assert_eq!(
            row_lines(&agg(Status::Done, 1, 1, detail(Status::Done, "")), false),
            1
        );
        assert_eq!(
            row_lines(
                &agg(Status::Running, 1, 1, detail(Status::Running, "x")),
                false
            ),
            2
        );
        assert_eq!(
            row_lines(&agg(Status::Error, 1, 1, detail(Status::Error, "x")), false),
            2
        );
        // Pending: no msg → 1 line (line 2 suppressed); with msg → 2 lines (mark + activity).
        // Old 3-line case (branch · needs you + quoted msg) is gone.
        assert_eq!(
            row_lines(
                &agg(Status::Pending, 1, 1, detail(Status::Pending, "")),
                false
            ),
            1
        );
        assert_eq!(
            row_lines(
                &agg(Status::Pending, 1, 1, detail(Status::Pending, "go?")),
                false
            ),
            2
        );
        // Running with no msg: only 1 line
        assert_eq!(
            row_lines(
                &agg(Status::Running, 1, 1, detail(Status::Running, "")),
                false
            ),
            1
        );
    }

    #[test]
    fn active_row_has_accent_bar_idle_does_not() {
        let rows = vec![
            TabRow {
                number: 1,
                name: "a".into(),
                active: true,
                has_bell: false,
                agg: agg(Status::Idle, 0, 0, None),
            },
            TabRow {
                number: 2,
                name: "b".into(),
                active: false,
                has_bell: false,
                agg: agg(Status::Idle, 0, 0, None),
            },
        ];
        let s = render(&rows, &ro(24, 0));
        let body: Vec<&str> = s.lines().skip(2).collect(); // skip 2-line header
        assert!(body[0].contains('▌')); // active row → bar
        assert!(body[0].contains(Role::Accent.ansi())); // accent-colored bar
        assert!(!body[1].contains('▌')); // idle non-active → no bar
    }

    #[test]
    fn active_and_waiting_row_bar_is_attention_not_accent() {
        let detail = Detail {
            repo: "p".into(),
            branch: "fix".into(),
            msg: "".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Pending,
        };
        let rows = vec![TabRow {
            number: 3,
            name: "pinky".into(),
            active: true,
            has_bell: false,
            agg: agg(Status::Pending, 0, 0, Some(detail)),
        }];
        let s = render(&rows, &ro(30, 5));
        let line1 = s.lines().nth(2).unwrap();
        assert!(line1.contains('▌'));
        // the bar uses the attention role when the active tab is also waiting
        assert!(line1.contains(Role::Attention.ansi()));
    }

    #[test]
    fn right_slot_per_state() {
        let mk = |status, done, total| {
            let d = Detail {
                repo: "r".into(),
                branch: "b".into(),
                msg: "".into(),
                kind: Kind::Claude,
                since_tick: 0,
                status,
            };
            TabRow {
                number: 1,
                name: "n".into(),
                active: false,
                has_bell: false,
                agg: agg(status, done, total, Some(d)),
            }
        };
        assert!(render(&[mk(Status::Done, 1, 1)], &ro(30, 0)).contains("done"));
        assert!(render(&[mk(Status::Error, 0, 1)], &ro(30, 0)).contains("failed"));
        assert!(render(&[mk(Status::Running, 0, 1)], &ro(30, 14)).contains("0:14"));
        let waiting = render(&[mk(Status::Pending, 0, 1)], &ro(30, 2));
        assert!(waiting.contains('⏵'));
        assert!(waiting.contains("0:02"));
        let multi = render(&[mk(Status::Pending, 2, 4)], &ro(30, 18));
        assert!(multi.contains("2/4"));
    }

    #[test]
    fn working_slot_is_dim_not_role_colored() {
        // Design: the working elapsed is ambient `id`-dim, not loud work-yellow.
        let d = Detail {
            repo: "r".into(),
            branch: "b".into(),
            msg: "".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Running,
        };
        let rows = vec![TabRow {
            number: 1,
            name: "n".into(),
            active: false,
            has_bell: false,
            agg: agg(Status::Running, 0, 1, Some(d)),
        }];
        let opts = ro(30, 14);
        let s = render(&rows, &opts);
        // elapsed is wrapped in the theme idle_text (dim) color…
        assert!(
            s.contains(&format!("{}0:14", tc_fg(opts.theme.idle_text))),
            "working elapsed should be dim idle_text: {:?}",
            s
        );
        // …NOT the working role color.
        assert!(
            !s.contains(&format!("{}0:14", Role::Working.ansi())),
            "working elapsed must not be work-yellow: {:?}",
            s
        );
    }

    #[test]
    fn working_glyph_spins_with_tick() {
        let d = Detail {
            repo: "r".into(),
            branch: "b".into(),
            msg: "".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Running,
        };
        let row = |_t| TabRow {
            number: 1,
            name: "n".into(),
            active: false,
            has_bell: false,
            agg: agg(Status::Running, 0, 1, Some(d.clone())),
        };
        let f0 = render(
            &[row(0)],
            &RenderOpts {
                width: 30,
                height: 100,
                now_tick: 0,
                glyphs: GlyphSet::Plain,
                header: true,
                density: crate::config::Density::Compact,
                theme: crate::theme::DerivedColors::default(),
            },
        );
        let f1 = render(
            &[row(1)],
            &RenderOpts {
                width: 30,
                height: 100,
                now_tick: 1,
                glyphs: GlyphSet::Plain,
                header: true,
                density: crate::config::Density::Compact,
                theme: crate::theme::DerivedColors::default(),
            },
        );
        assert!(f0.contains('◐'));
        assert!(f1.contains('◓'));
    }

    #[test]
    fn idle_row_is_single_line_with_no_right_slot_text() {
        let rows = vec![TabRow {
            number: 7,
            name: "logs".into(),
            active: false,
            has_bell: false,
            agg: agg(Status::Idle, 0, 0, None),
        }];
        let s = render(&rows, &ro(24, 0));
        assert_eq!(s.lines().skip(2).count(), 1); // exactly one body line
        assert!(s.contains('○'));
        assert!(s.contains("logs"));
    }

    #[test]
    fn narrow_width_truncates_with_ellipsis() {
        let rows = vec![TabRow {
            number: 1,
            name: "a-very-long-tab-name-indeed".into(),
            active: false,
            has_bell: false,
            agg: agg(Status::Idle, 0, 0, None),
        }];
        let s = render(&rows, &ro(12, 0));
        assert!(s.contains('…'));
    }

    /// Strip `\x1b[...m` SGR escape sequences and sum display widths of remaining chars.
    fn visible_len(line: &str) -> usize {
        let mut width = 0usize;
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // consume "[...m"
                if chars.peek() == Some(&'[') {
                    for inner in chars.by_ref() {
                        if inner == 'm' {
                            break;
                        }
                    }
                }
            } else {
                width += UnicodeWidthChar::width(c).unwrap_or(0);
            }
        }
        width
    }

    #[test]
    fn no_emitted_line_exceeds_width() {
        let width = 20;
        let detail = Detail {
            repo: "pinky".into(),
            branch: "fix/x".into(),
            msg: "abcdefghijklmnopqrstuvwxyz".into(), // longer than width
            since_tick: 0,
            kind: Kind::Claude,
            status: Status::Running,
        };
        let rows = vec![TabRow {
            number: 2,
            name: "a-very-long-tab-name-indeed".into(),
            active: true, // exercises BOLD escapes too
            has_bell: false,
            agg: agg(Status::Running, 2, 4, Some(detail)),
        }];
        let s = render(&rows, &ro(width, 14));
        // header (2) + two tab lines emitted (Running+detail = 2 lines)
        assert_eq!(s.matches('\n').count(), 4);
        // every visible (ANSI-stripped) line fits within the sidebar width
        for line in s.lines() {
            assert!(
                visible_len(line) <= width,
                "line exceeds width {}: {:?} (visible {})",
                width,
                line,
                visible_len(line)
            );
        }
    }

    #[test]
    fn pending_detail_lines_never_exceed_width() {
        let detail = Detail {
            repo: "averylongreponame".into(),
            branch: "feature/some-long-branch".into(),
            msg: "should we proceed with this long question".into(),
            since_tick: 0,
            kind: Kind::Claude,
            status: Status::Pending,
        };
        let rows = vec![TabRow {
            number: 3,
            name: "a-long-tab-name".into(),
            active: true,
            has_bell: false,
            agg: agg(Status::Pending, 0, 1, Some(detail)),
        }];
        for width in [16usize, 20, 24, 30] {
            let s = render(&rows, &ro(width, 5));
            for line in s.lines() {
                assert!(
                    visible_len(line) <= width,
                    "pending line exceeds width {}: {:?} (visible {})",
                    width,
                    line,
                    visible_len(line)
                );
            }
        }
    }

    #[test]
    fn running_has_no_warning_glyph() {
        let detail = Detail {
            repo: "r".into(),
            branch: "b".into(),
            msg: "".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Running,
        };
        let rows = vec![TabRow {
            number: 1,
            name: "t".into(),
            active: false,
            has_bell: false,
            agg: agg(Status::Running, 1, 1, Some(detail)),
        }];
        assert!(!render(&rows, &ro(30, 599)).contains('⚠'));
    }

    #[test]
    fn done_has_no_warning_glyph() {
        let detail = Detail {
            repo: "r".into(),
            branch: "b".into(),
            msg: "".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Done,
        };
        let rows = vec![TabRow {
            number: 1,
            name: "t".into(),
            active: false,
            has_bell: false,
            agg: agg(Status::Done, 1, 1, Some(detail)),
        }];
        assert!(!render(&rows, &ro(30, 10_000)).contains('⚠'));
    }

    #[test]
    fn bell_renders_marker() {
        let rows = vec![TabRow {
            number: 1,
            name: "t".into(),
            active: false,
            has_bell: true,
            agg: agg(Status::Idle, 0, 0, None),
        }];
        assert!(render(&rows, &ro(24, 0)).contains('⚑'));
    }

    #[test]
    fn no_bell_no_marker() {
        let rows = vec![TabRow {
            number: 1,
            name: "t".into(),
            active: false,
            has_bell: false,
            agg: agg(Status::Idle, 0, 0, None),
        }];
        assert!(!render(&rows, &ro(24, 0)).contains('⚑'));
    }

    #[test]
    fn error_word_narrows_when_tight() {
        let d = Detail {
            repo: "infra".into(),
            branch: "".into(),
            msg: "".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Error,
        };
        let rows = vec![TabRow {
            number: 5,
            name: "infra".into(),
            active: false,
            has_bell: false,
            agg: agg(Status::Error, 0, 1, Some(d)),
        }];
        // wide: "failed"; narrow: "err"
        assert!(render(&rows, &ro(30, 0)).contains("failed"));
        let narrow = render(&rows, &ro(14, 0));
        assert!(narrow.contains("err"));
        assert!(!narrow.contains("failed"));
    }

    #[test]
    fn working_detail_drops_branch_before_message_when_narrow() {
        let d = Detail {
            repo: "web".into(),
            branch: "main".into(),
            msg: "running tests".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Running,
        };
        let rows = vec![TabRow {
            number: 1,
            name: "api".into(),
            active: false,
            has_bell: false,
            agg: agg(Status::Running, 0, 1, Some(d)),
        }];
        let narrow = render(&rows, &ro(16, 5));
        for line in narrow.lines() {
            assert!(visible_len(line) <= 16);
        }
        // branch path is the first thing to go: "web/main" should not survive at 16 cols
        assert!(!narrow.contains("web/main"));
    }

    fn idle_row(n: u32) -> TabRow {
        TabRow {
            number: n,
            name: format!("t{}", n),
            active: false,
            has_bell: false,
            agg: agg(Status::Idle, 0, 0, None),
        }
    }

    #[test]
    fn overflow_folds_idle_into_strip_and_marks_header() {
        // 20 idle tabs, height only fits a few → fold.
        let rows: Vec<TabRow> = (1..=20).map(idle_row).collect();
        let s = render(
            &rows,
            &RenderOpts {
                width: 24,
                height: 6,
                now_tick: 0,
                glyphs: GlyphSet::Plain,
                header: true,
                density: crate::config::Density::Compact,
                theme: crate::theme::DerivedColors::default(),
            },
        );
        assert!(s.contains("idle")); // "+N idle ▾" footer
        assert!(s.contains('▾'));
        assert!(s.lines().next().unwrap().contains('▲')); // header overflow marker
                                                          // total emitted lines fit the height budget
        assert!(s.lines().count() <= 6);
    }

    #[test]
    fn overflow_keeps_non_idle_rows_visible() {
        let mut rows: Vec<TabRow> = (1..=18).map(idle_row).collect();
        // an urgent waiting tab at the very end (high position)
        let d = Detail {
            repo: "p".into(),
            branch: "x".into(),
            msg: "approve?".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Pending,
        };
        rows.push(TabRow {
            number: 19,
            name: "pinky".into(),
            active: false,
            has_bell: false,
            agg: agg(Status::Pending, 0, 1, Some(d)),
        });
        let s = render(
            &rows,
            &RenderOpts {
                width: 30,
                height: 8,
                now_tick: 2,
                glyphs: GlyphSet::Plain,
                header: true,
                density: crate::config::Density::Compact,
                theme: crate::theme::DerivedColors::default(),
            },
        );
        assert!(s.contains("pinky")); // urgent row never folded
        assert!(s.contains("approve?")); // activity (msg) survives on line 2
        assert!(s.contains('✳')); // Claude identity mark on line 2
    }

    #[test]
    fn no_overflow_when_everything_fits() {
        let rows: Vec<TabRow> = (1..=3).map(idle_row).collect();
        let s = render(
            &rows,
            &RenderOpts {
                width: 24,
                height: 40,
                now_tick: 0,
                glyphs: GlyphSet::Plain,
                header: true,
                density: crate::config::Density::Compact,
                theme: crate::theme::DerivedColors::default(),
            },
        );
        assert!(!s.contains("idle ▾"));
        assert!(!s.lines().next().unwrap().contains('▲'));
    }

    #[test]
    fn render_glyph_role_colors_are_present() {
        // Verify that ANSI-16 palette role codes for glyphs/status indicators are
        // still present in Compact-density output. (Detail text now uses theme-derived
        // truecolor foregrounds; card surfaces use truecolor backgrounds in Cards
        // density — but glyphs remain role-colored ANSI-16.)
        use crate::model::Detail;

        let mk_detail = |status: Status| Detail {
            repo: "pinky".into(),
            branch: "fix/x".into(),
            msg: "some message".into(),
            since_tick: 0,
            kind: Kind::Claude,
            status,
        };

        let rows = vec![
            // idle — one line, no detail
            TabRow {
                number: 1,
                name: "idle-tab".into(),
                active: false,
                has_bell: false,
                agg: agg(Status::Idle, 0, 0, None),
            },
            // running — two lines, with detail
            TabRow {
                number: 2,
                name: "run-tab".into(),
                active: true,
                has_bell: false,
                agg: agg(Status::Running, 1, 2, Some(mk_detail(Status::Running))),
            },
            // pending with msg — three lines
            TabRow {
                number: 3,
                name: "pend-tab".into(),
                active: false,
                has_bell: false,
                agg: agg(Status::Pending, 0, 1, Some(mk_detail(Status::Pending))),
            },
            // done — one line
            TabRow {
                number: 4,
                name: "done-tab".into(),
                active: false,
                has_bell: false,
                agg: agg(Status::Done, 1, 1, Some(mk_detail(Status::Done))),
            },
            // error — two lines
            TabRow {
                number: 5,
                name: "err-tab".into(),
                active: false,
                has_bell: false,
                agg: agg(Status::Error, 0, 1, Some(mk_detail(Status::Error))),
            },
        ];

        let opts = RenderOpts {
            width: 30,
            height: 100,
            now_tick: 7,
            glyphs: GlyphSet::Plain,
            header: true,
            density: crate::config::Density::Compact,
            theme: crate::theme::DerivedColors::default(),
        };
        let s = render(&rows, &opts);

        // Compact density must NOT have card background bands.
        assert!(
            !s.contains("48;2;"),
            "Compact must not emit truecolor bg bands"
        );
        // Must NOT contain raw hex color literals
        assert!(
            !s.contains('#'),
            "'#' hex color literal found in render output"
        );
        // Glyph/status indicators must use ANSI-16 role codes.
        // Accent role: header title bar + active-row bar (\x1b[35m)
        assert!(
            s.contains(Role::Accent.ansi()),
            "expected accent role ANSI code not found"
        );
        // Attention role: pending row glyph and "needs you" label (\x1b[91m)
        assert!(
            s.contains(Role::Attention.ansi()),
            "expected attention role ANSI code not found"
        );
        // Working role: running row glyph (\x1b[33m)
        assert!(
            s.contains(Role::Working.ansi()),
            "expected working role ANSI code not found"
        );
        // Error role: error row glyph (\x1b[31m)
        assert!(
            s.contains(Role::Error.ansi()),
            "expected error role ANSI code not found"
        );
        // Detail lines use truecolor foreground for readable dims.
        assert!(
            s.contains("38;2;"),
            "detail lines must use theme-derived truecolor foreground for readable dims"
        );
    }

    #[test]
    fn pending_line2_shows_mark_and_activity() {
        // New design: line 2 = ‹mark› ‹activity› in attention color, NOT "needs you".
        // With msg → 2 lines; without msg → 1 line (line 2 suppressed).

        // Case 1: pending with msg — 2 lines, mark + activity in attention color.
        let detail_with_msg = Detail {
            repo: "proj".into(),
            branch: "fix".into(),
            msg: "approve the push?".into(),
            since_tick: 0,
            kind: Kind::Claude,
            status: Status::Pending,
        };
        let rows = vec![TabRow {
            number: 1,
            name: "agents".into(),
            active: false,
            has_bell: false,
            agg: TabAgg {
                status: Status::Pending,
                done: 0,
                total: 3,
                pending: 2,
                detail: Some(detail_with_msg),
                panes: vec![],
            },
        }];
        let s = render(&rows, &ro(30, 0));
        // line 2 must show the mark and the activity text
        assert!(
            s.contains('✳'),
            "pending line 2 must contain the Claude mark: {:?}",
            s
        );
        assert!(
            s.contains("approve the push?"),
            "pending line 2 must show the activity msg: {:?}",
            s
        );
        // Activity is in the attention role (the question is loud)
        assert!(
            s.contains(Role::Attention.ansi()),
            "pending activity must use attention color: {:?}",
            s
        );
        // No old "needs you" text
        assert!(
            !s.contains("needs you"),
            "old 'needs you' text must not appear: {:?}",
            s
        );
        // row_lines = 2 (mark+activity line present)
        assert_eq!(row_lines(&rows[0].agg, false), 2);

        // Case 2: pending without msg → 1 line only, no line 2.
        let detail_no_msg = Detail {
            repo: "proj".into(),
            branch: "fix".into(),
            msg: "".into(),
            since_tick: 0,
            kind: Kind::Claude,
            status: Status::Pending,
        };
        let rows2 = [TabRow {
            number: 2,
            name: "solo".into(),
            active: false,
            has_bell: false,
            agg: TabAgg {
                status: Status::Pending,
                done: 0,
                total: 1,
                pending: 1,
                detail: Some(detail_no_msg),
                panes: vec![],
            },
        }];
        // row_lines = 1 (no msg → no line 2)
        assert_eq!(row_lines(&rows2[0].agg, false), 1);

        // Width constraint: pending detail line must not exceed width
        let detail_long = Detail {
            repo: "averylongreponame".into(),
            branch: "feature/some-very-long-branch".into(),
            msg: "a very long question that should be truncated appropriately here".into(),
            since_tick: 0,
            kind: Kind::Claude,
            status: Status::Pending,
        };
        let rows3 = vec![TabRow {
            number: 3,
            name: "multi".into(),
            active: false,
            has_bell: false,
            agg: TabAgg {
                status: Status::Pending,
                done: 0,
                total: 5,
                pending: 3,
                detail: Some(detail_long),
                panes: vec![],
            },
        }];
        for width in [20usize, 24, 30] {
            let s3 = render(&rows3, &ro(width, 0));
            assert!(
                s3.contains('✳'),
                "mark must appear at width {}: {:?}",
                width,
                s3
            );
            for line in s3.lines() {
                assert!(
                    visible_len(line) <= width,
                    "pending detail line exceeds width {}: {:?} (visible {})",
                    width,
                    line,
                    visible_len(line)
                );
            }
        }
    }

    #[test]
    fn multi_pending_detail_never_exceeds_width() {
        // Width constraint for pending rows at narrow widths.
        // With msg:"" → no line 2, so only line 1 (the status line) is rendered.
        // This tests the width-safety of the first line at narrow widths.
        // (Kept as a regression guard; previously tested "N needs you" overflow.)
        let detail = Detail {
            repo: "averylongreponame".into(),
            branch: "feature/some-very-long-branch-name".into(),
            msg: "".into(),
            since_tick: 0,
            kind: Kind::Claude,
            status: Status::Pending,
        };
        let rows = vec![TabRow {
            number: 1,
            name: "m".into(),
            active: false,
            has_bell: false,
            agg: TabAgg {
                status: Status::Pending,
                done: 0,
                total: 1,
                pending: 3,
                detail: Some(detail),
                panes: vec![],
            },
        }];
        for width in [14usize, 16, 17, 20, 24] {
            let s = render(&rows, &ro(width, 0));
            for line in s.lines() {
                assert!(
                    visible_len(line) <= width,
                    "multi-pending detail exceeds width {}: {:?} (visible {})",
                    width,
                    line,
                    visible_len(line)
                );
            }
        }
    }

    #[test]
    fn onboarding_shows_legend_and_click_hint() {
        let s = onboarding(&ro(28, 0));
        assert!(s.contains("RADAR"));
        assert!(s.contains('◆')); // legend includes the waiting glyph (plain set)
        assert!(s.to_lowercase().contains("needs you"));
        assert!(s.to_lowercase().contains("click"));
    }

    #[test]
    fn idle_strip_never_exceeds_width() {
        let rows: Vec<TabRow> = (1..=30).map(idle_row).collect();
        for width in [18usize, 24, 30] {
            let s = render(
                &rows,
                &RenderOpts {
                    width,
                    height: 6,
                    now_tick: 0,
                    glyphs: GlyphSet::Plain,
                    header: true,
                    density: crate::config::Density::Compact,
                    theme: crate::theme::DerivedColors::default(),
                },
            );
            // folding must have happened
            assert!(
                s.contains("idle ▾"),
                "expected idle strip at width {}",
                width
            );
            for line in s.lines() {
                assert!(
                    visible_len(line) <= width,
                    "idle strip/line exceeds width {}: {:?} (visible {})",
                    width,
                    line,
                    visible_len(line)
                );
            }
        }
    }

    #[test]
    fn cjk_and_emoji_names_never_exceed_width() {
        // CJK: each char is 2 display columns. "作業中デプロイ" = 7 chars = 14 cols.
        // Emoji in msg: 🚀 = 2 cols.
        let detail = Detail {
            repo: "proj".into(),
            branch: "main".into(),
            msg: "🚀 deploying now".into(),
            since_tick: 0,
            kind: Kind::Claude,
            status: Status::Pending,
        };
        let rows = vec![TabRow {
            number: 1,
            name: "作業中デプロイ".into(),
            active: true,
            has_bell: false,
            agg: agg(Status::Pending, 0, 1, Some(detail)),
        }];
        for width in [16usize, 20, 24, 30] {
            let s = render(&rows, &ro(width, 5));
            for line in s.lines() {
                assert!(
                    visible_len(line) <= width,
                    "CJK/emoji line exceeds width {}: {:?} (visible {})",
                    width,
                    line,
                    visible_len(line)
                );
            }
        }
    }

    #[test]
    fn header_false_emits_no_header_lines() {
        let rows = vec![TabRow {
            number: 1,
            name: "a".into(),
            active: false,
            has_bell: false,
            agg: agg(Status::Running, 0, 0, None),
        }];
        assert_eq!(
            header_lines(&rows, false, crate::config::Density::Compact),
            0
        );
        let opts = RenderOpts {
            width: 24,
            height: 100,
            now_tick: 0,
            glyphs: GlyphSet::Plain,
            header: false,
            density: crate::config::Density::Compact,
            theme: crate::theme::DerivedColors::default(),
        };
        let s = render(&rows, &opts);
        // No identity header: rows start at line 0, so no "RADAR"/"═" line.
        assert!(!s.contains("RADAR"));
        assert!(!s.contains('═'));
        // The single tab row is still rendered.
        assert!(s.contains('a') || s.matches('\n').count() >= 1);
    }

    // ── Multi-pane adaptive tree (chunk 2) ──

    use crate::model::PaneEntry;

    /// Build a PaneEntry for tree tests.
    fn pe(id: u32, kind: Kind, status: Status, msg: &str) -> PaneEntry {
        PaneEntry {
            pane_id: id,
            kind,
            status,
            msg: msg.into(),
        }
    }

    /// Build a multi-pane TabAgg from per-pane entries. The header status is the
    /// most-urgent (highest-severity) member; done/total derive from the entries.
    fn agg_multi(panes: Vec<PaneEntry>) -> TabAgg {
        let status = panes
            .iter()
            .map(|p| p.status)
            .max_by_key(|s| s.severity())
            .unwrap_or(Status::Idle);
        let total = panes.len();
        let done = panes.iter().filter(|p| p.status == Status::Done).count();
        let pending = panes.iter().filter(|p| p.status == Status::Pending).count();
        let detail = panes.iter().find(|p| p.status == status).map(|p| Detail {
            repo: "r".into(),
            branch: "b".into(),
            msg: p.msg.clone(),
            kind: p.kind,
            since_tick: 0,
            status: p.status,
        });
        TabAgg {
            status,
            done,
            total,
            pending,
            detail,
            panes,
        }
    }

    #[test]
    fn pane_tree_plan_needs_you_always_expands() {
        // Inactive tab: only Pending/Error panes expand; calm panes collapse.
        let a = agg_multi(vec![
            pe(1, Kind::Claude, Status::Pending, "approve?"),
            pe(2, Kind::Claude, Status::Running, "building"),
            pe(3, Kind::Claude, Status::Running, "testing"),
            pe(4, Kind::Claude, Status::Error, "boom"),
        ]);
        let plan = pane_tree_plan(&a, false);
        // Pending + Error expand; two Running collapse.
        assert_eq!(plan.expanded.len(), 2);
        assert_eq!(plan.expanded[0].pane_id, 1);
        assert_eq!(plan.expanded[1].pane_id, 4);
        assert_eq!(plan.collapsed_count, 2);
        assert_eq!(plan.collapsed_verb, "working"); // any running → "working"
    }

    #[test]
    fn pane_tree_plan_active_expands_all() {
        let a = agg_multi(vec![
            pe(1, Kind::Claude, Status::Running, "a"),
            pe(2, Kind::Claude, Status::Done, "b"),
            pe(3, Kind::Claude, Status::Idle, "c"),
        ]);
        let plan = pane_tree_plan(&a, true);
        assert_eq!(plan.expanded.len(), 3, "active tab expands ALL panes");
        assert_eq!(plan.collapsed_count, 0);
    }

    #[test]
    fn pane_tree_plan_calm_collapse_verb_done_when_no_running() {
        // No needs-you panes, inactive: all collapse. No running → verb "done".
        let a = agg_multi(vec![
            pe(1, Kind::Claude, Status::Done, "a"),
            pe(2, Kind::Claude, Status::Done, "b"),
            pe(3, Kind::Claude, Status::Idle, "c"),
        ]);
        let plan = pane_tree_plan(&a, false);
        assert_eq!(plan.expanded.len(), 0);
        assert_eq!(plan.collapsed_count, 3);
        assert_eq!(plan.collapsed_verb, "done");
    }

    #[test]
    fn row_lines_multi_pane_counts_header_children_collapse() {
        // 1 pending (expands) + 3 running (collapse) → 1 header + 1 child + 1 collapse = 3.
        let a = agg_multi(vec![
            pe(1, Kind::Claude, Status::Pending, "run migration?"),
            pe(2, Kind::Claude, Status::Running, "x"),
            pe(3, Kind::Claude, Status::Running, "y"),
            pe(4, Kind::Claude, Status::Running, "z"),
        ]);
        assert_eq!(row_lines(&a, false), 3, "header + 1 expanded + collapse");
        // Active → all 4 expand: 1 header + 4 children, no collapse = 5.
        assert_eq!(
            row_lines(&a, true),
            5,
            "active expands all, no collapse line"
        );
    }

    #[test]
    fn multi_pane_render_emits_header_child_and_collapse_lines() {
        // Design example: pending child expanded + collapse line for the calm rest.
        let a = agg_multi(vec![
            pe(1, Kind::Claude, Status::Pending, "run migration?"),
            pe(2, Kind::Claude, Status::Running, "x"),
            pe(3, Kind::Claude, Status::Running, "y"),
            pe(4, Kind::Claude, Status::Running, "z"),
        ]);
        let row = TabRow {
            number: 7,
            name: "monorepo".into(),
            active: false,
            has_bell: false,
            agg: a,
        };
        let s = render(&[row], &ro(30, 0));
        let body: Vec<&str> = s.lines().skip(2).collect(); // skip header
                                                           // Header line shows the done/total count (0/4) and the most-urgent pending glyph.
        assert!(
            body[0].contains("0/4"),
            "header must show done/total: {:?}",
            body[0]
        );
        assert!(
            body[0].contains('◆'),
            "header glyph is the most-urgent (pending): {:?}",
            body[0]
        );
        // Expanded pending child: tree char `└ ` (it is last expanded, collapse line follows
        // so it uses ├ ), mark ✳, pending glyph ◆, activity.
        assert!(
            body[1].contains('├') || body[1].contains('└'),
            "child has a tree char: {:?}",
            body[1]
        );
        assert!(
            body[1].contains('✳'),
            "child shows the kind mark: {:?}",
            body[1]
        );
        assert!(
            body[1].contains("run migration?"),
            "child shows activity: {:?}",
            body[1]
        );
        assert!(
            body[1].contains(Role::Attention.ansi()),
            "pending child activity in attention: {:?}",
            body[1]
        );
        // Collapse line: `└ 3 more working`.
        assert!(
            body[2].contains("└"),
            "collapse line uses corner char: {:?}",
            body[2]
        );
        assert!(
            body[2].contains("3 more working"),
            "collapse line counts calm panes: {:?}",
            body[2]
        );
        // Exactly 3 body lines (header + 1 child + collapse).
        assert_eq!(body.len(), 3, "header + 1 child + collapse: {:?}", s);
    }

    #[test]
    fn multi_pane_active_expands_all_no_collapse() {
        let a = agg_multi(vec![
            pe(1, Kind::Claude, Status::Running, "a"),
            pe(2, Kind::Claude, Status::Done, "b"),
        ]);
        let row = TabRow {
            number: 1,
            name: "team".into(),
            active: true,
            has_bell: false,
            agg: a,
        };
        let s = render(&[row], &ro(30, 0));
        let body: Vec<&str> = s.lines().skip(2).collect();
        // header + 2 children, no collapse line.
        assert_eq!(body.len(), 3, "active: header + 2 children: {:?}", s);
        assert!(
            !s.contains("more working"),
            "no collapse line when all expand: {:?}",
            s
        );
        // First child ├, last child └.
        assert!(body[1].contains('├'), "first child uses ├: {:?}", body[1]);
        assert!(body[2].contains('└'), "last child uses └: {:?}", body[2]);
    }

    #[test]
    fn multi_pane_child_and_collapse_lines_never_exceed_width() {
        // The chunk-2 tree CHILD lines (expanded panes) and the collapse line
        // must be width-safe at narrow widths. (The header line-1 follows the
        // existing renderer's own width rules, covered by other line-1 tests.)
        // Use an inactive tab so a collapse line accompanies an expanded child.
        for &width in &[16usize, 20, 24, 30] {
            let a = agg_multi(vec![
                pe(
                    1,
                    Kind::Claude,
                    Status::Pending,
                    "a very long question that must be truncated to fit",
                ),
                pe(2, Kind::Claude, Status::Running, "x"),
                pe(3, Kind::Claude, Status::Running, "y"),
            ]);
            let row = TabRow {
                number: 1,
                name: "tab".into(),
                active: false,
                has_bell: false,
                agg: a,
            };
            let s = render(&[row], &ro(width, 5));
            let body: Vec<&str> = s.lines().skip(2).collect(); // skip 2-line header
                                                               // body = [card header line, expanded child, collapse line]; check the
                                                               // tree's own child (idx 1) and collapse (idx 2) lines.
            for line in &body[1..] {
                assert!(
                    visible_len(line) <= width,
                    "tree child/collapse line exceeds width {}: {:?} (visible {})",
                    width,
                    line,
                    visible_len(line)
                );
            }
        }
    }

    #[test]
    fn single_pane_tab_unchanged_no_tree() {
        // A single ever-active pane is NOT multi-pane → keeps chunk-1 line 2.
        let a = agg_multi(vec![pe(1, Kind::Claude, Status::Pending, "approve?")]);
        assert!(!is_multi_pane(&a), "one pane is not multi-pane");
        assert_eq!(
            row_lines(&a, false),
            2,
            "single-pane pending+msg = 2 lines (chunk-1)"
        );
        let row = TabRow {
            number: 1,
            name: "solo".into(),
            active: false,
            has_bell: false,
            agg: a,
        };
        let s = render(&[row], &ro(30, 0));
        // No tree chars, no collapse line.
        assert!(
            !s.contains("more working") && !s.contains("more done"),
            "no collapse line: {:?}",
            s
        );
        assert!(!s.contains('├'), "no tree branch char: {:?}", s);
        // Chunk-1 line 2 present: mark + activity.
        assert!(
            s.contains('✳') && s.contains("approve?"),
            "chunk-1 line 2 present: {:?}",
            s
        );
    }

    /// Calm rows (Running/Done) are compressed to 1 line before urgent rows
    /// (Pending) lose their detail lines.
    #[test]
    fn overflow_compresses_calm_before_urgent() {
        // 3 Running rows (each 2 lines) + 1 Pending-with-msg (now 2 lines) = 8 lines.
        // header = 2. body_budget = height - 2.
        // We pick height = 7 → body_budget = 5.
        // Compression: Running rows compressed to 1 line each (3 lines saved);
        //   3×1 + 2 = 5 ≤ 5. Done. Pending keeps its activity line (2 lines total).
        let detail_running = |n: u8| Detail {
            repo: format!("repo{}", n),
            branch: "main".into(),
            msg: "working".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Running,
        };
        let detail_pending = Detail {
            repo: "urgent-proj".into(),
            branch: "fix/thing".into(),
            msg: "please review".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Pending,
        };

        let rows = vec![
            TabRow {
                number: 1,
                name: "r1".into(),
                active: false,
                has_bell: false,
                agg: agg(Status::Running, 0, 1, Some(detail_running(1))),
            },
            TabRow {
                number: 2,
                name: "r2".into(),
                active: false,
                has_bell: false,
                agg: agg(Status::Running, 0, 1, Some(detail_running(2))),
            },
            TabRow {
                number: 3,
                name: "r3".into(),
                active: false,
                has_bell: false,
                agg: agg(Status::Running, 0, 1, Some(detail_running(3))),
            },
            TabRow {
                number: 4,
                name: "urgent".into(),
                active: false,
                has_bell: false,
                agg: agg(Status::Pending, 0, 1, Some(detail_pending)),
            },
        ];

        // Verify uncompressed sizes (new line-2 rule: pending+msg = 2, not 3).
        assert_eq!(row_lines(&rows[0].agg, false), 2);
        assert_eq!(row_lines(&rows[1].agg, false), 2);
        assert_eq!(row_lines(&rows[2].agg, false), 2);
        assert_eq!(row_lines(&rows[3].agg, false), 2); // pending + msg = 2 (mark + activity)

        // body_budget = 5 (height 7, header 2)
        let body_budget = 5usize;
        let (plan, strip_folded) = plan_overflow(&rows, body_budget);
        assert_eq!(strip_folded, 0, "no idle rows to strip");
        assert_eq!(plan.len(), 4, "all 4 rows kept");

        // Running rows should be at 1 line each (calm, compressed first).
        assert_eq!(plan[0].1, 1, "Running row 0 compressed to 1 line");
        assert_eq!(plan[1].1, 1, "Running row 1 compressed to 1 line");
        assert_eq!(plan[2].1, 1, "Running row 2 compressed to 1 line");

        // Pending row: 2 lines (mark + activity), fits without further compression.
        // After calm compression: 3×1 + 2 = 5 ≤ 5. Pending keeps its activity line.
        assert_eq!(plan[3].1, 2, "Pending row keeps activity line (2 lines)");

        // Total body lines must be ≤ budget.
        let total_body: usize = plan.iter().map(|(_, l)| l).sum();
        assert!(
            total_body <= body_budget,
            "total body lines {} exceeds budget {}",
            total_body,
            body_budget
        );

        // Render and verify: Running rows have no detail line; urgent shows activity.
        let opts = RenderOpts {
            width: 30,
            height: 7,
            now_tick: 0,
            glyphs: GlyphSet::Plain,
            header: true,
            density: crate::config::Density::Compact,
            theme: crate::theme::DerivedColors::default(),
        };
        let s = render(&rows, &opts);
        assert!(
            s.contains("please review"),
            "urgent row activity must survive"
        );
        assert!(
            s.contains('✳'),
            "urgent row must show the Claude identity mark"
        );
        // Total line count ≤ height
        assert!(
            s.lines().count() <= 7,
            "rendered lines {} exceed height 7",
            s.lines().count()
        );
    }

    /// When height is extremely small, every kept row is compressed to exactly
    /// 1 line; no panic; total output lines ≤ budget.
    #[test]
    fn overflow_all_one_line_when_extreme() {
        let detail = Detail {
            repo: "r".into(),
            branch: "b".into(),
            msg: "msg".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Pending,
        };
        let rows = vec![
            TabRow {
                number: 1,
                name: "pending".into(),
                active: false,
                has_bell: false,
                agg: agg(Status::Pending, 0, 1, Some(detail.clone())),
            },
            TabRow {
                number: 2,
                name: "run".into(),
                active: false,
                has_bell: false,
                agg: agg(Status::Running, 0, 1, Some(detail.clone())),
            },
        ];
        // height = 3 → body_budget = 1 (header=2). Each non-idle row at min 1 line.
        let opts = RenderOpts {
            width: 24,
            height: 3,
            now_tick: 0,
            glyphs: GlyphSet::Plain,
            header: true,
            density: crate::config::Density::Compact,
            theme: crate::theme::DerivedColors::default(),
        };
        let s = render(&rows, &opts);
        let line_count = s.lines().count();
        assert!(
            line_count <= 3,
            "rendered {} lines but height is 3",
            line_count
        );
        // No panic means the test passes. Also verify each body row is ≥ 1 line.
        let (plan, _) = plan_overflow(&rows, 1);
        for (_, lines) in &plan {
            assert!(*lines >= 1, "every planned row must have at least 1 line");
        }
    }

    // ── Density tests ──

    /// Helper: comfortable-density RenderOpts at the given width/height.
    fn ro_comfortable(width: usize, height: usize) -> RenderOpts {
        RenderOpts {
            width,
            height,
            now_tick: 0,
            glyphs: GlyphSet::Plain,
            header: true,
            density: crate::config::Density::Comfortable,
            theme: crate::theme::DerivedColors::default(),
        }
    }

    #[test]
    fn comfortable_inserts_blank_line_between_tabs() {
        // 3 idle tabs, large height → comfortable density inserts a gap after each tab.
        // body = header(2) + 3 content lines + 3 gap lines = 8 total lines.
        let rows: Vec<TabRow> = (1..=3).map(idle_row).collect();
        let s = render(&rows, &ro_comfortable(24, 100));
        // body lines: each idle row = 1 content + 1 gap = 2 lines each. Total body = 6.
        // Plus 2 header = 8 \n chars.
        assert_eq!(
            s.matches('\n').count(),
            8,
            "comfortable: expected 2 header + 3×(1 content + 1 gap) = 8 newlines, got {}:\n{:?}",
            s.matches('\n').count(),
            s
        );
        // Check that there is a blank line between tabs (an empty line between non-empty lines).
        let body_lines: Vec<&str> = s.lines().skip(2).collect();
        assert_eq!(body_lines.len(), 6);
        // Odd-indexed body lines (0-based: 1, 3, 5) should be blank gap lines.
        assert!(
            body_lines[1].is_empty(),
            "body line 1 should be blank gap: {:?}",
            body_lines[1]
        );
        assert!(
            body_lines[3].is_empty(),
            "body line 3 should be blank gap: {:?}",
            body_lines[3]
        );
        assert!(
            body_lines[5].is_empty(),
            "body line 5 should be blank gap: {:?}",
            body_lines[5]
        );
    }

    #[test]
    fn compact_has_no_gaps() {
        // 3 idle tabs, compact → no gap lines at all.
        // Total lines = 2 header + 3 content = 5 \n chars.
        let rows: Vec<TabRow> = (1..=3).map(idle_row).collect();
        let opts = RenderOpts {
            width: 24,
            height: 100,
            now_tick: 0,
            glyphs: GlyphSet::Plain,
            header: true,
            density: crate::config::Density::Compact,
            theme: crate::theme::DerivedColors::default(),
        };
        let s = render(&rows, &opts);
        assert_eq!(
            s.matches('\n').count(),
            5,
            "compact: expected 2 header + 3 content = 5 newlines, got {}:\n{:?}",
            s.matches('\n').count(),
            s
        );
        // No empty lines in the body.
        for line in s.lines().skip(2) {
            assert!(
                !line.is_empty(),
                "compact should have no blank lines, found one: {:?}",
                line
            );
        }
    }

    #[test]
    fn cards_content_lines_differ_from_comfortable() {
        // Cards paints a background band on AGENT rows — so a session with at
        // least one agent differs from Comfortable. (An all-idle session would
        // now render identically, since idle rows are bare in the hybrid.)
        use crate::model::Detail;
        let detail = Detail {
            repo: "r".into(),
            branch: "b".into(),
            msg: "working".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Running,
        };
        let rows = vec![
            TabRow {
                number: 1,
                name: "work".into(),
                active: false,
                has_bell: false,
                agg: agg(Status::Running, 0, 1, Some(detail)),
            },
            idle_row(2),
        ];
        let comfortable = render(
            &rows,
            &RenderOpts {
                width: 24,
                height: 100,
                now_tick: 0,
                glyphs: GlyphSet::Plain,
                header: true,
                density: crate::config::Density::Comfortable,
                theme: crate::theme::DerivedColors::default(),
            },
        );
        let cards = render(
            &rows,
            &RenderOpts {
                width: 24,
                height: 100,
                now_tick: 0,
                glyphs: GlyphSet::Plain,
                header: true,
                density: crate::config::Density::Cards,
                theme: crate::theme::DerivedColors::default(),
            },
        );
        assert_ne!(
            comfortable, cards,
            "Cards should differ from Comfortable (has bg bands)"
        );
    }

    #[test]
    fn gaps_dropped_under_overflow() {
        // 4 idle tabs, height = 8 (header=2, body_budget=6).
        // Comfortable: 4 content + 4 gaps = 8. That's exactly 8 — fits.
        // So use height = 7 (body_budget = 5) → 4 content + 4 gaps = 8 > 5 → gaps dropped.
        // At that point overflow compresses: 4 idle all folded (non-idle = 0), strip = 1 line.
        // But let's use non-idle rows to ensure content lines > budget with gaps.
        // 4 Running rows (2 content lines each) = 8 content lines. body_budget = 6.
        // Without gaps: 8 > 6, so compression occurs. plan_overflow will compress them.
        // With comfortable gaps: content total from plan * 2 would be even more.
        // Simpler: use 3 idle rows, body_budget = 4 (height=6).
        // Without gaps: 3 content fits in 4. With gaps: 3+3=6 > 4. → gap_used = 0.
        let rows: Vec<TabRow> = (1..=3).map(idle_row).collect();
        let height = 6; // body_budget = 4
        let (plan, strip, spacing) =
            plan_layout(&rows, height - 2, crate::config::Density::Comfortable);
        let gap_used = spacing.gap;
        // All 3 idle rows fit at 1 line each (total=3 ≤ body_budget=4).
        assert_eq!(plan.len(), 3, "all 3 rows should be kept");
        assert_eq!(strip, 0, "no rows folded into strip");
        // 3 content + 3 gaps = 6 > 4 (body_budget) → gaps dropped.
        assert_eq!(gap_used, 0, "gaps should be dropped when they don't fit");
        // Render and verify: no blank lines in output.
        let s = render(
            &rows,
            &RenderOpts {
                width: 24,
                height,
                now_tick: 0,
                glyphs: GlyphSet::Plain,
                header: true,
                density: crate::config::Density::Comfortable,
                theme: crate::theme::DerivedColors::default(),
            },
        );
        let line_count = s.lines().count();
        assert!(
            line_count <= height,
            "rendered {} lines but height is {}",
            line_count,
            height
        );
        // When gaps are dropped, no blank body lines.
        for line in s.lines().skip(2) {
            assert!(
                !line.is_empty(),
                "gaps dropped — no blank body lines expected: {:?}",
                line
            );
        }
    }

    #[test]
    fn plan_layout_compact_always_zero_gap() {
        let rows: Vec<TabRow> = (1..=5).map(idle_row).collect();
        // Even with very large budget, compact never adds gaps.
        let (_, _, spacing) = plan_layout(&rows, 100, crate::config::Density::Compact);
        assert_eq!(spacing.gap, 0, "Compact density must never produce gaps");
    }

    #[test]
    fn plan_layout_comfortable_gap_when_space_available() {
        // 2 idle rows, body_budget=10: 2 content + 2 gaps = 4 ≤ 10 → gap_used = 1.
        let rows: Vec<TabRow> = (1..=2).map(idle_row).collect();
        let (_, _, spacing) = plan_layout(&rows, 10, crate::config::Density::Comfortable);
        assert_eq!(spacing.gap, 1, "Comfortable with room should use gaps");
    }

    // ── Cards density background band tests ──

    fn ro_cards(width: usize, height: usize) -> RenderOpts {
        RenderOpts {
            width,
            height,
            now_tick: 0,
            glyphs: GlyphSet::Plain,
            header: true,
            density: crate::config::Density::Cards,
            theme: crate::theme::DerivedColors::default(),
        }
    }

    #[test]
    fn cards_paint_content_lines_with_bg() {
        // Render an idle tab and an active working tab at normal width with Cards.
        // Every content line carries a truecolor band; gap lines and header must NOT.
        use crate::model::Detail;
        let detail = Detail {
            repo: "repo".into(),
            branch: "main".into(),
            msg: "working".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Running,
        };
        let rows = vec![
            TabRow {
                number: 1,
                name: "idle".into(),
                active: false,
                has_bell: false,
                agg: agg(Status::Idle, 0, 0, None),
            },
            TabRow {
                number: 2,
                name: "work".into(),
                active: true,
                has_bell: false,
                agg: agg(Status::Running, 0, 1, Some(detail)),
            },
        ];
        let s = render(&rows, &ro_cards(30, 100));
        let lines: Vec<&str> = s.lines().collect();

        // Cards is now ONE cohesive dark panel: EVERY emitted line is painted.
        // The 1-line header and the gap carry the dark panel base (rail_bg, the
        // neutral-dark fallback's = 18,19,27); content lines carry their own
        // (subtle) surface tint.
        const RAIL: &str = "\x1b[48;2;18;19;27m";

        // line 0 = header title (no rule in Cards) → painted with rail_bg.
        assert!(
            lines[0].contains(RAIL),
            "header title line must carry the rail panel band: {:?}",
            lines[0]
        );
        assert!(
            lines[0].contains("RADAR"),
            "header title must read RADAR: {:?}",
            lines[0]
        );

        // line 1 = idle tab content → a card surface (NOT the rail base).
        assert!(
            lines[1].contains("\x1b[48;2;") && !lines[1].contains(RAIL),
            "idle content line must carry a card surface band, not rail: {:?}",
            lines[1]
        );

        // line 2 = idle card gap → painted with rail_bg (panel shows through).
        assert!(
            lines[2].contains(RAIL),
            "idle card gap row must carry the rail panel band: {:?}",
            lines[2]
        );

        // line 3 = working tab line 1, line 4 = working detail.
        assert!(
            lines[3].contains("\x1b[48;2;") && !lines[3].contains(RAIL),
            "working tab line 1 must carry a card surface band: {:?}",
            lines[3]
        );
        assert!(
            lines[4].contains("\x1b[48;2;") && !lines[4].contains(RAIL),
            "working tab detail line must carry a card surface band: {:?}",
            lines[4]
        );

        // Every painted line must end with bg reset (\x1b[49m).
        for (i, line) in lines.iter().enumerate() {
            assert!(
                line.contains("\x1b[49m"),
                "panel line {} must contain bg reset: {:?}",
                i,
                line
            );
        }
    }

    #[test]
    fn cards_band_fills_full_width() {
        // Short-name agent (done) tab at width 24, Cards: the painted band fills
        // the full width.
        use crate::model::Detail;
        let done = Detail {
            repo: "r".into(),
            branch: "".into(),
            msg: "".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Done,
        };
        let rows = vec![TabRow {
            number: 1,
            name: "x".into(),
            active: false,
            has_bell: false,
            agg: agg(Status::Done, 1, 1, Some(done)),
        }];
        let width = 24usize;
        let s = render(&rows, &ro_cards(width, 100));
        // Cards has a 1-line header (no rule); first body line is the painted content line.
        let body: Vec<&str> = s.lines().skip(1).collect();
        let content_line = body[0];
        assert!(
            content_line.contains("\x1b[48;2;"),
            "content line must have truecolor card bg: {:?}",
            content_line
        );
        // Visible width must equal exactly `width`.
        let vw = visible_len(content_line);
        assert_eq!(
            vw, width,
            "painted content line visible width must equal {} (full band), got {}: {:?}",
            width, vw, content_line
        );
    }

    #[test]
    fn cards_rearm_bg_after_resets() {
        // Active working tab (line has multiple role-colored tokens with \x1b[0m
        // resets) under Cards: the active truecolor tint must re-arm after every reset,
        // so \x1b[0m\x1b[48;2;... (reset immediately followed by the truecolor band) appears.
        use crate::model::Detail;
        let detail = Detail {
            repo: "pinky".into(),
            branch: "fix/x".into(),
            msg: "some work".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Running,
        };
        let rows = vec![TabRow {
            number: 1,
            name: "agent".into(),
            active: true,
            has_bell: false,
            agg: agg(Status::Running, 0, 1, Some(detail)),
        }];
        let s = render(&rows, &ro_cards(30, 100));
        // The neutral-dark fallback active surface from the dark-panel ladder is (56,59,71).
        assert!(
            s.contains("\x1b[0m\x1b[48;2;56;59;71m"),
            "reset immediately followed by truecolor bg re-arm must appear in Cards output: {:?}",
            s
        );
    }

    #[test]
    fn cards_use_truecolor_not_256color() {
        // Card surfaces use theme-derived truecolor (48;2;r;g;b) — not fixed 256-color
        // indices, not truecolor foreground (38;2;), and not raw hex literals.
        use crate::model::Detail;
        let detail = Detail {
            repo: "r".into(),
            branch: "b".into(),
            msg: "work".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Running,
        };
        let rows = vec![
            TabRow {
                number: 1,
                name: "idle".into(),
                active: false,
                has_bell: false,
                agg: agg(Status::Idle, 0, 0, None),
            },
            TabRow {
                number: 2,
                name: "work".into(),
                active: true,
                has_bell: false,
                agg: agg(Status::Running, 0, 1, Some(detail)),
            },
        ];
        let s = render(&rows, &ro_cards(30, 100));
        // Card surfaces must emit truecolor backgrounds.
        assert!(
            s.contains("\x1b[48;2;"),
            "cards must use a truecolor surface (48;2;): {:?}",
            s
        );
        // Must NOT use legacy 256-color indices for the surface band.
        assert!(
            !s.contains("\x1b[48;5;"),
            "cards must not use 256-color surface (48;5;): {:?}",
            s
        );
        // Must NOT contain raw hex color literals.
        assert!(!s.contains('#'), "cards must not emit raw hex: {:?}", s);
        // Note: 38;2; truecolor foreground IS expected (detail lines use theme-derived dim foreground).
    }

    /// Design alignment: the left chrome is exactly ONE column (the bar/spine),
    /// which provides the card's 1-col inset. Content (the status glyph) sits at
    /// column 1 for both active and idle rows — the active spine does NOT push
    /// content right. Guards against re-introducing a second pad column.
    fn strip_ansi_local(line: &str) -> String {
        let mut out = String::new();
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                if chars.peek() == Some(&'[') {
                    for i in chars.by_ref() {
                        if i == 'm' {
                            break;
                        }
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn cards_left_chrome_is_single_column() {
        use crate::model::Detail;
        let detail = Detail {
            repo: "r".into(),
            branch: "b".into(),
            msg: "x".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Running,
        };
        let rows = vec![
            TabRow {
                number: 1,
                name: "idle".into(),
                active: false,
                has_bell: false,
                agg: agg(Status::Idle, 0, 0, None),
            },
            TabRow {
                number: 2,
                name: "work".into(),
                active: true,
                has_bell: false,
                agg: agg(Status::Running, 0, 1, Some(detail)),
            },
        ];
        let s = render(&rows, &ro_cards(30, 100));
        let lines: Vec<String> = s.lines().map(strip_ansi_local).collect();
        // line 0 = header; line 1 = idle row; line 3 = active row (line 2 is its
        // detail row, line between is the idle gap). Find by name.
        let idle = lines.iter().find(|l| l.contains("idle")).unwrap();
        let active = lines.iter().find(|l| l.contains("work")).unwrap();
        // Idle: one leading space (the blank bar column), then the glyph at col 1.
        assert!(
            idle.starts_with(" ○"),
            "idle row must be ' ○…' (1-col inset): {:?}",
            idle
        );
        // Active: the spine in col 0 immediately followed by the glyph at col 1 —
        // no second pad column.
        assert!(
            active.starts_with("▌◐"),
            "active row must be '▌◐…' (spine+glyph, no pad): {:?}",
            active
        );
    }

    #[test]
    fn child_line_status_glyph_precedes_spaced_mark() {
        // Child lines render as `‹tree›‹status› ‹mark› ‹activity›`: the status
        // glyph comes FIRST (fixed/aligned column), then a space, then the
        // identity mark, then a space — so the status icons line up down the
        // tree and the mark isn't cramped against the status glyph.
        let a = agg_multi(vec![
            pe(1, Kind::Claude, Status::Running, "searching web"),
            pe(2, Kind::Claude, Status::Done, "done thing"),
        ]);
        let row = TabRow {
            number: 1,
            name: "t".into(),
            active: true,
            has_bell: false,
            agg: a,
        };
        let s = render(&[row], &ro_cards(30, 100));
        let child = s
            .lines()
            .map(strip_ansi_local)
            .find(|l| l.contains('├'))
            .expect("child line");
        let mark_idx = child.find('✳').expect("identity mark present");
        // A space immediately precedes and follows the mark.
        assert!(
            child[..mark_idx].ends_with(' '),
            "mark must be preceded by a space: {:?}",
            child
        );
        assert_eq!(
            child[mark_idx..].chars().nth(1),
            Some(' '),
            "mark must be followed by a space: {:?}",
            child
        );
        // The status glyph (working spinner at tick 0) comes before the mark.
        let spin = crate::status::working_spin(0);
        assert!(
            child.find(spin).is_some_and(|i| i < mark_idx),
            "status glyph must precede the identity mark: {:?}",
            child
        );
    }

    #[test]
    fn comfortable_and_compact_emit_no_bg() {
        // Same tabs with Comfortable and Compact must contain NO card band.
        use crate::model::Detail;
        let detail = Detail {
            repo: "r".into(),
            branch: "b".into(),
            msg: "working".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Running,
        };
        let rows = vec![
            TabRow {
                number: 1,
                name: "idle".into(),
                active: false,
                has_bell: false,
                agg: agg(Status::Idle, 0, 0, None),
            },
            TabRow {
                number: 2,
                name: "work".into(),
                active: true,
                has_bell: false,
                agg: agg(Status::Running, 0, 1, Some(detail)),
            },
        ];
        for density in [
            crate::config::Density::Comfortable,
            crate::config::Density::Compact,
        ] {
            let s = render(
                &rows,
                &RenderOpts {
                    width: 30,
                    height: 100,
                    now_tick: 0,
                    glyphs: GlyphSet::Plain,
                    header: true,
                    density,
                    theme: crate::theme::DerivedColors::default(),
                },
            );
            assert!(
                !s.contains("\x1b[48;2;"),
                "density {:?} must NOT emit a truecolor card band: {:?}",
                density,
                s
            );
        }
    }

    #[test]
    fn no_emitted_line_exceeds_width_cards() {
        // The no_emitted_line_exceeds_width invariant holds for Cards density too.
        let width = 20usize;
        use crate::model::Detail;
        let detail = Detail {
            repo: "pinky".into(),
            branch: "fix/x".into(),
            msg: "abcdefghijklmnopqrstuvwxyz".into(),
            since_tick: 0,
            kind: Kind::Claude,
            status: Status::Running,
        };
        let rows = vec![TabRow {
            number: 2,
            name: "a-very-long-tab-name-indeed".into(),
            active: true,
            has_bell: false,
            agg: agg(Status::Running, 2, 4, Some(detail)),
        }];
        let s = render(&rows, &ro_cards(width, 100));
        for line in s.lines() {
            assert!(
                visible_len(line) <= width,
                "Cards: line exceeds width {}: {:?} (visible {})",
                width,
                line,
                visible_len(line)
            );
        }
    }

    #[test]
    fn card_spacing_per_density() {
        // The ONE source of truth for spacing knobs, by density.
        use crate::config::Density::*;
        assert_eq!(
            card_spacing(Compact),
            CardSpacing {
                pad_x: 0,
                pad_y: 0,
                gap: 0
            }
        );
        assert_eq!(
            card_spacing(Comfortable),
            CardSpacing {
                pad_x: 0,
                pad_y: 0,
                gap: 1
            }
        );
        assert_eq!(
            card_spacing(Cards),
            CardSpacing {
                pad_x: 0,
                pad_y: 0,
                gap: 1
            }
        );
    }

    #[test]
    fn card_block_lines_is_pad_y_plus_content_plus_gap() {
        // The single footprint source: pad_y + row_lines + gap.
        let idle = agg(Status::Idle, 0, 0, None);
        assert_eq!(row_lines(&idle, false), 1);
        // Cards: 0 pad_y + 1 content + 1 gap = 2.
        assert_eq!(
            card_block_lines(&idle, false, card_spacing(crate::config::Density::Cards)),
            2
        );
        // Comfortable: 0 pad_y + 1 content + 1 gap = 2.
        assert_eq!(
            card_block_lines(
                &idle,
                false,
                card_spacing(crate::config::Density::Comfortable)
            ),
            2
        );
        // Compact: 0 + 1 + 0 = 1.
        assert_eq!(
            card_block_lines(&idle, false, card_spacing(crate::config::Density::Compact)),
            1
        );
    }

    #[test]
    fn cards_have_rail_gap_row() {
        // A cards-rendered tab emits a TRAILING gap row painted with the dark
        // rail panel base (rail_bg), and that row is blank once ANSI is stripped.
        let rows = vec![TabRow {
            number: 1,
            name: "idle".into(),
            active: false,
            has_bell: false,
            agg: agg(Status::Idle, 0, 0, None),
        }];
        let s = render(&rows, &ro_cards(24, 100));
        let lines: Vec<&str> = s.lines().collect();
        // line 0 = header (rail). line 1 = idle card content. line 2 = trailing gap row.
        let content_row = lines[1];
        // Content row carries the idle card surface tint (NOT rail_bg).
        assert!(
            content_row.contains("\x1b[48;2;20;21;30m"),
            "content row must carry the card's own idle surface tint: {:?}",
            content_row
        );
        assert!(
            !content_row.contains("\x1b[48;2;18;19;27m"),
            "content row must NOT be the rail panel base: {:?}",
            content_row
        );
        assert!(
            content_row.contains("idle"),
            "content row must contain the tab name: {:?}",
            content_row
        );
        // The trailing gap row (line 2) carries the rail panel base.
        let gap_row = lines[2];
        assert!(
            gap_row.contains("\x1b[48;2;18;19;27m"),
            "gap row must carry the rail panel base: {:?}",
            gap_row
        );
        assert!(
            !gap_row.contains("\x1b[48;2;20;21;30m"),
            "gap row must NOT be the card surface tint: {:?}",
            gap_row
        );
        // ANSI-stripped, the gap row is blank (only spaces / no glyphs or text).
        fn strip_ansi(line: &str) -> String {
            let mut out = String::new();
            let mut chars = line.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\x1b' {
                    if chars.peek() == Some(&'[') {
                        for inner in chars.by_ref() {
                            if inner == 'm' {
                                break;
                            }
                        }
                    }
                } else {
                    out.push(c);
                }
            }
            out
        }
        assert!(
            strip_ansi(gap_row).trim().is_empty(),
            "gap row must be visually blank (no content): {:?}",
            gap_row
        );
    }

    // ── 3-tint cards: every tab is a card, idle dim / agent mid / active bright ──

    /// Classify each rendered line for a tint-map snapshot by which truecolor
    /// surface band it carries (using the neutral-dark fallback's dark-panel ladder):
    ///   "active" = surface_active (56,59,71)  — brighter than bg, gently pops
    ///   "agent"  = surface_agent  (24,25,35)  — mid step up from rail
    ///   "idle"   = surface_idle   (20,21,30)  — barely above the panel
    ///   "rail"   = rail_bg        (18,19,27)  — the dark panel base (gaps/header)
    ///   "bare"   = no band at all.
    /// Note: in Cards every emitted line is painted, so blank gaps now carry the
    /// rail band rather than being empty — they classify as "rail", not "gap".
    fn tint_map(s: &str) -> String {
        s.lines()
            .map(|line| {
                if line.contains("\x1b[48;2;56;59;71m") {
                    "active"
                } else if line.contains("\x1b[48;2;24;25;35m") {
                    "agent"
                } else if line.contains("\x1b[48;2;20;21;30m") {
                    "idle"
                } else if line.contains("\x1b[48;2;18;19;27m") {
                    "rail"
                } else if line.is_empty() {
                    "gap"
                } else {
                    "bare"
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn cards_tint_per_row_class() {
        // Every tab is a card; the surface tint encodes its class:
        //   focused (active) row → brightest (240),
        //   agent row (active status, not focused) → mid (238),
        //   idle/plain row → dimmest (236).
        use crate::model::Detail;
        let detail = Detail {
            repo: "repo".into(),
            branch: "main".into(),
            msg: "working".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Running,
        };
        let rows = vec![
            TabRow {
                number: 1,
                name: "idle".into(),
                active: false,
                has_bell: false,
                agg: agg(Status::Idle, 0, 0, None),
            },
            TabRow {
                number: 2,
                name: "agent".into(),
                active: false,
                has_bell: false,
                agg: agg(Status::Running, 0, 1, Some(detail.clone())),
            },
            TabRow {
                number: 3,
                name: "focus".into(),
                active: true,
                has_bell: false,
                agg: agg(Status::Running, 0, 1, Some(detail)),
            },
        ];
        let s = render(&rows, &ro_cards(30, 100));
        let lines: Vec<&str> = s.lines().collect();
        // Cards header is 1 line (no rule); each card emits content then a
        // trailing gap row (rail_bg — the panel shows through). Layout:
        //   line 0  header(rail)
        //   line 1  idle content             → idle (20,21,30)
        //   line 2  idle gap                 → rail (18,19,27)
        //   line 3  agent content line 1     → agent (24,25,35)
        //   line 4  agent detail             → agent (24,25,35)
        //   line 5  agent gap                → rail (18,19,27)
        //   line 6  focus content line 1     → active (56,59,71)
        //   line 7  focus detail             → active (56,59,71)
        //   line 8  focus gap                → rail (18,19,27)
        let rail = "\x1b[48;2;18;19;27m";
        assert!(
            lines[1].contains("\x1b[48;2;20;21;30m"),
            "idle content must carry the dim card tint (20;21;30): {:?}",
            lines[1]
        );
        assert!(
            lines[2].contains(rail) && !lines[2].contains("\x1b[48;2;20;21;30m"),
            "idle gap row must carry the rail panel base: {:?}",
            lines[2]
        );
        assert!(
            lines[3].contains("\x1b[48;2;24;25;35m"),
            "agent content must carry the mid card tint (24;25;35): {:?}",
            lines[3]
        );
        assert!(
            lines[4].contains("\x1b[48;2;24;25;35m"),
            "agent detail must carry the mid card tint (24;25;35): {:?}",
            lines[4]
        );
        assert!(
            lines[5].contains(rail) && !lines[5].contains("\x1b[48;2;24;25;35m"),
            "agent gap row must carry the rail panel base: {:?}",
            lines[5]
        );
        assert!(
            lines[6].contains("\x1b[48;2;56;59;71m"),
            "focused content must carry the active card tint (56;59;71): {:?}",
            lines[6]
        );
        assert!(
            lines[7].contains("\x1b[48;2;56;59;71m"),
            "focused detail must carry the active card tint (56;59;71): {:?}",
            lines[7]
        );
    }

    #[test]
    fn cards_3tint_layout_snapshot() {
        // Golden tint-map for the canonical sidebar.dc.html "cards" session:
        // active running agent, pending agent, done agent, then two idle panes.
        // Every tab is a card; cards are adjacent (no gap rows); tints encode the class.
        use crate::model::Detail;
        let running = Detail {
            repo: "web".into(),
            branch: "".into(),
            msg: "building…".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Running,
        };
        let pending = Detail {
            repo: "api".into(),
            branch: "fix".into(),
            msg: "".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Pending,
        };
        let done = Detail {
            repo: "worker".into(),
            branch: "".into(),
            msg: "".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Done,
        };
        let rows = vec![
            TabRow {
                number: 1,
                name: "Claude".into(),
                active: true,
                has_bell: false,
                agg: agg(Status::Running, 0, 1, Some(running)),
            },
            TabRow {
                number: 2,
                name: "api".into(),
                active: false,
                has_bell: false,
                agg: agg(Status::Pending, 0, 1, Some(pending)),
            },
            TabRow {
                number: 3,
                name: "worker".into(),
                active: false,
                has_bell: false,
                agg: agg(Status::Done, 1, 1, Some(done)),
            },
            TabRow {
                number: 4,
                name: "Pane #1".into(),
                active: false,
                has_bell: false,
                agg: agg(Status::Idle, 0, 0, None),
            },
            TabRow {
                number: 5,
                name: "Pane #1".into(),
                active: false,
                has_bell: false,
                agg: agg(Status::Idle, 0, 0, None),
            },
        ];
        let s = render(&rows, &ro_cards(24, 100));
        // Cards is now a cohesive dark panel: the 1-line header (no rule) is
        // painted with rail_bg; each card emits its content lines then a
        // trailing gap row (rail_bg — the panel shows through between cards).
        // Per card:
        //   Claude (active, 2 content) → 2 content + 1 gap = "active","active","rail"
        //   api    (agent, 1 content)  → pending+empty-msg → 1 content + 1 gap = "agent","rail"
        //   worker (agent, 1 content)  → 1 content + 1 gap = "agent","rail"
        //   Pane#1 (idle, 1 content)   → 1 content + 1 gap = "idle","rail"
        //   Pane#2 (idle, 1 content)   → 1 content + 1 gap = "idle","rail"
        let expected = "\
rail\n\
active\n\
active\n\
rail\n\
agent\n\
rail\n\
agent\n\
rail\n\
idle\n\
rail\n\
idle\n\
rail";
        assert_eq!(
            tint_map(&s),
            expected,
            "3-tint card map drifted from the design:\n{:?}",
            s
        );
    }

    #[test]
    fn cards_waiting_distinguishable_from_error_by_shape_and_code() {
        // THE KEY OUTCOME: status hues are ANSI-16 so the terminal renders them
        // in its own theme. ANSI has no orange/peach, so a waiting "needs you"
        // row and a red error row are both red-FAMILY — they're kept distinct by
        // SHAPE + BOLD + CODE, not by hue:
        //   waiting → `\x1b[91m` (bright red) + ◆ glyph + bold
        //   error   → `\x1b[31m` (red)        + ✗ glyph (not bold)
        use crate::model::Detail;
        let pending = Detail {
            repo: "pinky".into(),
            branch: "fix".into(),
            msg: "".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Pending,
        };
        let err = Detail {
            repo: "infra".into(),
            branch: "".into(),
            msg: "boom".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Error,
        };
        let rows = vec![
            TabRow {
                number: 1,
                name: "pinky".into(),
                active: false,
                has_bell: false,
                agg: agg(Status::Pending, 0, 1, Some(pending)),
            },
            TabRow {
                number: 2,
                name: "infra".into(),
                active: false,
                has_bell: false,
                agg: agg(Status::Error, 0, 1, Some(err)),
            },
        ];
        let s = render(&rows, &ro_cards(30, 100));
        let lines: Vec<&str> = s.lines().collect();
        // Cards header is 1 line; line 1 = waiting row, then its detail, then the
        // error row. Find the waiting first-line and the error first-line.
        let waiting_line = lines
            .iter()
            .find(|l| l.contains("pinky"))
            .expect("waiting row");
        let error_line = lines
            .iter()
            .find(|l| l.contains("infra"))
            .expect("error row");
        // Waiting: bright-red ANSI + ◆ glyph + bold.
        assert!(
            waiting_line.contains(Role::Attention.ansi()),
            "waiting row must use the ANSI-16 attention code (\\x1b[91m): {:?}",
            waiting_line
        );
        assert!(
            waiting_line.contains('◆'),
            "waiting row must use the ◆ glyph: {:?}",
            waiting_line
        );
        assert!(
            waiting_line.contains(BOLD),
            "waiting row must be bold: {:?}",
            waiting_line
        );
        // Error: red ANSI + ✗ glyph, NOT bold.
        assert!(
            error_line.contains(Role::Error.ansi()),
            "error row must use the ANSI-16 error code (\\x1b[31m): {:?}",
            error_line
        );
        assert!(
            error_line.contains('✗'),
            "error row must use the ✗ glyph: {:?}",
            error_line
        );
        assert!(
            !error_line.contains(BOLD),
            "error row must not be bold: {:?}",
            error_line
        );
        // The two ANSI codes are distinct.
        assert_ne!(
            Role::Attention.ansi(),
            Role::Error.ansi(),
            "attention (bright-red) and error (red) ANSI codes must differ"
        );
    }

    #[test]
    fn header_shows_radar_and_urgent_count() {
        // Header reads " RADAR" and, when any tab is pending, appends a "·N!"
        // urgent marker in the attention role.
        use crate::model::Detail;
        let pending = Detail {
            repo: "p".into(),
            branch: "x".into(),
            msg: "approve?".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Pending,
        };
        let rows = vec![
            TabRow {
                number: 1,
                name: "pinky".into(),
                active: false,
                has_bell: false,
                agg: agg(Status::Pending, 0, 1, Some(pending)),
            },
            idle_row(2),
            idle_row(3),
        ];
        let s = render(&rows, &ro(30, 0));
        let header = s.lines().next().unwrap();
        assert!(
            header.contains("RADAR"),
            "header must read RADAR: {:?}",
            header
        );
        assert!(
            !header.contains("AGENTS"),
            "header must not say AGENTS: {:?}",
            header
        );
        assert!(
            header.contains("·3"),
            "header must show total count ·3: {:?}",
            header
        );
        assert!(
            header.contains("·1!"),
            "header must show urgent count ·1!: {:?}",
            header
        );
        assert!(
            header.contains(Role::Attention.ansi()),
            "urgent marker must use the attention role: {:?}",
            header
        );
    }

    #[test]
    fn header_no_urgent_marker_when_nothing_pending() {
        let rows: Vec<TabRow> = (1..=3).map(idle_row).collect();
        let s = render(&rows, &ro(30, 0));
        let header = s.lines().next().unwrap();
        assert!(header.contains("·3"), "header shows total: {:?}", header);
        assert!(
            !header.contains('!'),
            "no urgent marker when nothing pending: {:?}",
            header
        );
    }
}
