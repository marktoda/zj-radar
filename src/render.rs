//! Pure renderer: per-tab rows → ANSI string. No zellij-tile dependency.

use crate::config::Density;
use crate::kind::Kind;
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

#[derive(Debug)]
pub struct TabRow {
    pub number: u32,
    pub name: String,
    pub active: bool,
    pub has_bell: bool,
    pub display: TabDisplay,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrimaryDetail {
    pub repo: String,
    pub branch: String,
    pub msg: String,
    pub since_tick: u64,
    pub status: Status,
    pub kind: Kind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneDisplay {
    Tracked {
        pane_id: u32,
        kind: Kind,
        status: Status,
        msg: String,
    },
    Untracked {
        pane_id: u32,
        title: String,
    },
}

impl PaneDisplay {
    pub(crate) fn tracked(pane_id: u32, kind: Kind, status: Status, msg: String) -> Self {
        Self::Tracked {
            pane_id,
            kind,
            status,
            msg,
        }
    }

    pub(crate) fn untracked(pane_id: u32, title: &str) -> Self {
        let title = if title.trim().is_empty() {
            "terminal".to_string()
        } else {
            title.to_string()
        };
        Self::Untracked { pane_id, title }
    }

    fn is_tracked(&self) -> bool {
        matches!(self, Self::Tracked { .. })
    }

    pub(crate) fn pane_id(&self) -> u32 {
        match self {
            Self::Tracked { pane_id, .. } | Self::Untracked { pane_id, .. } => *pane_id,
        }
    }

    fn status(&self) -> Option<Status> {
        match self {
            Self::Tracked { status, .. } => Some(*status),
            Self::Untracked { .. } => None,
        }
    }

    fn render_status(&self) -> Status {
        self.status().unwrap_or(Status::Idle)
    }

    fn kind(&self) -> Kind {
        match self {
            Self::Tracked { kind, .. } => *kind,
            Self::Untracked { .. } => Kind::Other,
        }
    }

    fn msg(&self) -> &str {
        match self {
            Self::Tracked { msg, .. } => msg,
            Self::Untracked { title, .. } => title,
        }
    }

}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabDisplay {
    pub status: Status,
    pub progress: ProgressCounts,
    pub detail: Option<PrimaryDetail>,
    pub panes: Vec<PaneDisplay>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProgressCounts {
    pub done: usize,
    pub total: usize,
    pub pending: usize,
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
struct CardSpacing {
    pad_x: usize,
    pad_y: usize,
    gap: usize,
}

/// Map a density to its spacing knobs. This is the single place to tune the
/// sidebar's vertical/horizontal rhythm.
fn card_spacing(d: Density) -> CardSpacing {
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

/// Surface class a line sits on (Cards density only); resolved to a concrete
/// bg escape during assembly using the owning row. `None` = never painted.
#[allow(dead_code)] // variants consumed by Tasks 2-3
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineBg {
    None,
    Rail,        // dark panel base (rail_bg): header, gaps, idle strip
    Card,        // this row's card surface (card_tint of the owning row)
    ActiveChild, // active multi-pane child line (surface_agent)
}

/// One physical rail line and the click target it resolves to. `text` always
/// ends in exactly one '\n'. The unit of rendering: ansi, targets, and
/// footprint all derive from a `Vec<Line>`, so they cannot drift.
#[derive(Clone, Debug)]
struct Line {
    text: String,
    target: Option<RailTarget>,
    #[allow(dead_code)]
    bg: LineBg,
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

    /// The single derive point: `ansi` and `targets` come from one `Vec<Line>`,
    /// so they are always in 1:1 correspondence. `text` is already final
    /// (painted during assembly); `bg` is ignored here. The trailing newline of
    /// the last line is popped to prevent vt100 scroll in the test harness.
    fn from_lines(lines: Vec<Line>) -> RenderedRail {
        let mut ansi = String::new();
        let mut targets = Vec::with_capacity(lines.len());
        for line in lines {
            ansi.push_str(&line.text);
            targets.push(line.target);
        }
        if ansi.ends_with('\n') {
            ansi.pop();
        }
        RenderedRail { ansi, targets }
    }

    fn from_ansi_without_targets(ansi: String) -> Self {
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
fn card_block_lines(display: &TabDisplay, active: bool, spacing: CardSpacing) -> usize {
    spacing.pad_y + row_lines(display, active) + spacing.gap
}

/// A tab is "multi-pane" for tree purposes when it has more than one tracked pane.
/// Single-pane tabs keep the chunk-1 line-2 behavior; multi-pane tabs use
/// the line-per-pane design.
fn is_multi_pane(display: &TabDisplay) -> bool {
    display.panes.iter().filter(|p| p.is_tracked()).count() > 1
}

/// Single source of truth for how many lines a tab row occupies.
///
/// Single-pane tabs (chunk-1 line-2 rule): idle → 1 line; any other active
/// state with a non-empty msg → 2 lines (line 1 = status, line 2 = mark +
/// activity); a detail without a msg → 1 line (line 2 suppressed).
///
/// Multi-pane tabs (line-per-pane design): 1 header line + one line per tracked
/// pane (up to MAX_PANE_LINES=6) + one `+N more` line if capped. The `active`
/// parameter is unused since all panes show regardless of focus.
fn row_lines(display: &TabDisplay, _active: bool) -> usize {
    let tracked_count = display.panes.iter().filter(|p| p.is_tracked()).count();
    if tracked_count > 1 {
        const MAX_PANE_LINES: usize = 6;
        let pane_lines = tracked_count.min(MAX_PANE_LINES);
        let more_line = if tracked_count > MAX_PANE_LINES { 1 } else { 0 };
        return 1 + pane_lines + more_line;
    }
    match display.status {
        Status::Idle => 1,
        Status::Done | Status::Running | Status::Error | Status::Pending => match &display.detail {
            Some(d) if !d.msg.trim().is_empty() => 2,
            _ => 1,
        },
    }
}

/// Right-aligned status slot text (no color). Always empty (removed for now).
fn right_slot(_display: &TabDisplay, _now_tick: u64, _width: usize) -> String {
    String::new()
}

/// The rail's identity header. Single source of truth for the header's vertical
/// span. Only the truly-empty case (no rows at all) is headerless; when `header`
/// is false the identity block is suppressed and rows start at line 0.
///
/// In Cards density the carded hero is just the " RADAR …" title (1 line) — the
/// `═` rule is dropped so cards begin immediately under the title. Compact and
/// Comfortable keep the two-line title+rule header. `render_rail()` uses the
/// same emitted header lines for ANSI and targets, so the count stays in lockstep.
fn header_lines(rows: &[TabRow], header: bool, density: Density) -> usize {
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
fn plan_overflow(rows: &[TabRow], body_budget: usize) -> (Vec<(usize, usize)>, usize) {
    let total: usize = rows.iter().map(|r| row_lines(&r.display, r.active)).sum();
    if total <= body_budget {
        // Everything fits at full fidelity.
        let plan = rows
            .iter()
            .enumerate()
            .map(|(i, r)| (i, row_lines(&r.display, r.active)))
            .collect();
        return (plan, 0);
    }

    // Step 1: fold idle rows; keep non-idle at full line counts.
    let non_idle_idx: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.display.status != Status::Idle)
        .map(|(i, _)| i)
        .collect();
    let folded_count = rows
        .iter()
        .filter(|r| r.display.status == Status::Idle)
        .count();

    // Each kept row starts at its full (uncompressed) line count.
    let mut planned: Vec<(usize, usize)> = non_idle_idx
        .iter()
        .map(|&i| (i, row_lines(&rows[i].display, rows[i].active)))
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
        if is_calm(rows[idx].display.status) {
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
            if !is_calm(rows[idx].display.status) {
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
fn plan_layout(
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
        .map(|r| card_block_lines(&r.display, r.active, base))
        .sum();
    if full_footprint <= body_budget {
        let plan = rows
            .iter()
            .enumerate()
            .map(|(i, r)| (i, row_lines(&r.display, r.active)))
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
pub fn onboarding(opts: &RenderOpts) -> RenderedRail {
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
        let role_code = st.role().ansi();
        out.push_str(&format!(
            " {}{}{} {}{}{}\n",
            role_code,
            st.glyph_for(g),
            RESET,
            muted,
            label,
            RESET
        ));
    }
    out.push('\n');
    out.push_str(&format!("{} click a row to jump{}\n", muted, RESET));
    RenderedRail::from_ansi_without_targets(out)
}

/// Emit one row's body into `out`, respecting `max_lines`.
///
/// Line 1 (gutter+glyph+num+name+slot) is ALWAYS emitted.
/// PrimaryDetail/roster lines are emitted in priority order while `lines_emitted < max_lines`:
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
    let st = row.display.status;
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
        String::new()
    };

    // Internal left padding: `pad_x` cells after the col-0 spine/space, before
    // the glyph. At extreme-narrow widths clamp pad_x then num so the prefix
    // never exceeds `width`.
    let pad_x = card_spacing(opts.density).pad_x;

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
    let slot_raw = right_slot(&row.display, now_tick, width);

    // bell marker just before the slot.
    let bell = if row.has_bell {
        format!("{}⚑{} ", hue(Role::Working), RESET)
    } else {
        String::new()
    };

    // left visible prefix is "X[pad]<glyph> <num> " — bar/glyph are 1 cell each;
    // `pad_len` is the Cards-only internal left pad (1 col, else 0).
    // Bare minimum: bar(1 if active, 0 if not) + glyph(1) + sp(1) + num. Clamp pad first, then num.
    let num_full = row.number.to_string();
    let bar_width = if row.active { 1 } else { 0 };
    let bare_min = bar_width + 1 + 1; // bar + glyph + sp (before num)
    let pad_len = pad_x.min(width.saturating_sub(bare_min + 1)); // keep 1 col for at least '1'
    let num_budget = width.saturating_sub(bare_min + pad_len);
    let num = truncate(&num_full, num_budget);
    let num_w = UnicodeWidthStr::width(num.as_str());
    // Trailing sp after num only if it fits.
    let has_trailing_sp = bare_min + pad_len + num_w < width;
    let pad = " ".repeat(pad_len);
    let prefix_len = bare_min + pad_len + num_w + if has_trailing_sp { 1 } else { 0 };
    let bell_len = if row.has_bell { 2 } else { 0 };
    // At extreme-narrow widths the slot may not fit at all; clamp slot_len to
    // what's available after the fixed prefix and bell so nothing overflows.
    let raw_slot_len = UnicodeWidthStr::width(slot_raw.as_str());
    let slot_len = raw_slot_len.min(width.saturating_sub(prefix_len + bell_len));
    let slot = if slot_len < raw_slot_len {
        truncate(&slot_raw, slot_len)
    } else {
        slot_raw
    };
    let slot_styled = if slot.is_empty() {
        String::new()
    } else if st == Status::Running {
        format!("{}{}{}", tc_fg(opts.theme.idle_text), slot, RESET)
    } else {
        format!("{}{}{}", hue(st.role()), slot, RESET)
    };
    // At extreme-narrow widths name_budget saturates to 0 → name = ""; no
    // .max(1) so we never force an extra `…` that would push past `width`.
    let name_budget = width.saturating_sub(prefix_len + bell_len + slot_len);
    let name = truncate(&row.name, name_budget);

    // gap can be 0 at extreme-narrow widths; saturating_sub prevents underflow.
    let used = prefix_len + UnicodeWidthStr::width(name.as_str()) + bell_len + slot_len;
    let gap = width.saturating_sub(used);
    let sp_after_num = if has_trailing_sp { " " } else { "" };
    out.push_str(&format!(
        "{}{}{}{}{} {}{}{}{}{}{}{}\n",
        bar,
        pad,
        label_color,
        label_bold,
        glyph_char,
        num,
        sp_after_num,
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

    // ── Multi-pane line-per-pane (new design) ─────────────────────────────────
    // A tab with >1 tracked pane renders as: header (line 1, above) + one line
    // per tracked pane (in position order), up to MAX_PANE_LINES; if more exist,
    // a final `+N more` line is emitted. No collapse, no tree chars.
    if is_multi_pane(&row.display) {
        const MAX_PANE_LINES: usize = 6;
        let tracked_panes: Vec<&PaneDisplay> = row.display.panes.iter()
            .filter(|p| p.is_tracked())
            .collect();
        let total_tracked = tracked_panes.len();
        let show = total_tracked.min(MAX_PANE_LINES);

        for pane in tracked_panes.iter().take(show) {
            if emitted >= max_lines {
                return;
            }
            emit_pane_line(out, pane, opts, row.active, &idle_color, &dim_strong);
            record_target(Some(RailTarget {
                tab_position: tab_target.tab_position,
                pane_id: Some(pane.pane_id()),
            }));
            emitted += 1;
        }

        let remaining = total_tracked - show;
        if remaining > 0 && emitted < max_lines {
            let more_text = format!("+{} more", remaining);
            let clamped = truncate(&more_text, opts.width);
            if row.active {
                let bar_role = match st {
                    Status::Pending | Status::Error => Role::Attention,
                    _ => Role::Accent,
                };
                out.push_str(&format!("{}▌{} {}{}{}\n",
                    hue(bar_role), RESET, idle_color, clamped, RESET));
            } else {
                out.push_str(&format!("  {}{}{}\n", idle_color, clamped, RESET));
            }
            record_target(Some(tab_target));
        }
        return;
    }

    // ── Single-pane line 2 (chunk 1) ───────────────────────────────────────
    // Line 2: `‹mark› ‹activity›` — source-agnostic for all active statuses.
    // Only emitted when the status is active, a detail with a non-empty msg exists,
    // and there is remaining budget. For Running, the braille spinner is appended.
    // For Pending (the question), activity is colored in attention (loud). Others dim_strong.
    if let Some(d) = &row.display.detail {
        if emitted < max_lines && !d.msg.trim().is_empty() {
            match st {
                Status::Idle => {}
                Status::Done | Status::Running | Status::Error | Status::Pending => {
                    // mark glyph in neutral idle_text color (vendor-neutral)
                    let mark = d.kind.mark();
                    let mark_width = UnicodeWidthChar::width(mark).unwrap_or(1);
                    // "  ‹mark› " prefix: 2-space indent + mark + space. The
                    // mark sits one column right of the line-1 glyph (which is at
                    // col 1 after the bar/spine column), matching the design.
                    let prefix_vis = 2 + mark_width + 1;
                    let avail = width.saturating_sub(prefix_vis);
                    // Build activity string (no braille spinner).
                    let activity = d.msg.clone();
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

/// Emit one pane line in the new line-per-pane design:
/// Inactive: `  {glyph} {mark} {msg}` (2-space indent)
/// Active:   `▌ {glyph} {mark} {msg}` (spine + space)
fn emit_pane_line(
    out: &mut String,
    pane: &PaneDisplay,
    opts: &RenderOpts,
    tab_active: bool,
    idle_color: &str,
    dim_strong: &str,
) {
    let width = opts.width;
    let mark = pane.kind().mark();
    let mark_w = UnicodeWidthChar::width(mark).unwrap_or(1);
    let status = pane.render_status();
    let glyph = if status == Status::Running {
        crate::status::working_spin(opts.now_tick as usize)
    } else {
        status.glyph_for(opts.glyphs)
    };
    let glyph_w = UnicodeWidthChar::width(glyph).unwrap_or(1);
    // Prefix: 2 cols (either "  " or "▌ ") + glyph + 1 space + mark + 1 space
    let prefix_vis = 2 + glyph_w + 1 + mark_w + 1;
    let avail = width.saturating_sub(prefix_vis);
    let activity_str = truncate(pane.msg(), avail);
    let role_ansi = |r: Role| -> &'static str { r.ansi() };
    let glyph_color = role_ansi(status.role()).to_string();
    let activity_color = if status == Status::Pending {
        role_ansi(Role::Attention).to_string()
    } else {
        dim_strong.to_string()
    };
    if tab_active {
        out.push_str(&format!(
            "{}▌{} {}{}{} {}{}{} {}{}{}\n",
            role_ansi(Role::Accent), RESET,
            glyph_color, glyph, RESET,
            idle_color, mark, RESET,
            activity_color, activity_str, RESET,
        ));
    } else {
        out.push_str(&format!(
            "  {}{}{} {}{}{} {}{}{}\n",
            glyph_color, glyph, RESET,
            idle_color, mark, RESET,
            activity_color, activity_str, RESET,
        ));
    }
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
fn tc_fg(c: (u8, u8, u8)) -> String {
    format!("\x1b[38;2;{};{};{}m", c.0, c.1, c.2)
}

/// The truecolor surface tint for a card, by class: the focused tab is
/// brightest, agent rows (active status) are mid, idle/plain panes are
/// the dimmest surface. Returns an owned ANSI escape string.
fn card_tint(row: &TabRow, theme: &DerivedColors) -> String {
    let rgb = if row.active {
        theme.surface_active
    } else if row.display.status.is_active() {
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
        format!("{}▲", rows.len())
    } else {
        format!("·{}", rows.len())
    };
    // Emit the identity header block only when configured on (and rows exist).
    // Header line 1: " RADAR" + right-aligned count.
    if opts.header {
        let title = " RADAR";
        let count_w = UnicodeWidthStr::width(count.as_str());
        let right_w = count_w.min(width);
        // At extreme-narrow widths the gap can be 0 (no `.max(1)`) so the
        // assembled visible content never exceeds `width`.
        let gap = width.saturating_sub(UnicodeWidthStr::width(title) + right_w);
        // Title in accent; total count muted (accent when overflowing, so the
        // ▲ marker stays loud).
        let count_color = if overflow { accent } else { Role::Muted.ansi() };
        // Clamp visible portions to width before assembling the ANSI line.
        let title_budget = width.saturating_sub(right_w + gap);
        let title_clamped = truncate(title, title_budget);
        let count_clamped = truncate(&count, right_w);
        let mut title_line = String::new();
        title_line.push_str(&format!(
            "{}{}{}{}{}{}{}",
            accent,
            title_clamped,
            RESET,
            " ".repeat(gap),
            count_color,
            count_clamped,
            RESET
        ));
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
            let active_child_bg = tc_bg(opts.theme.surface_agent);
            // pad_y rows: blank, painted with THIS card's own surface bg —
            // card-colored internal TOP padding (breathing room) that belongs
            // to this tab's click span.
            for _ in 0..spacing.pad_y {
                out.push_str(&paint_card_line("\n", width, &bg));
                targets.push(Some(row_target));
            }
            let (tab_buf, row_targets) = render_row_buffer(&rows[i], opts, max_lines);
            for (line_idx, line) in tab_buf.split_inclusive('\n').enumerate() {
                let line_bg = if rows[i].active && is_multi_pane(&rows[i].display) && line_idx > 0 {
                    &active_child_bg
                } else {
                    &bg
                };
                out.push_str(&paint_card_line(line, width, line_bg));
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
        let text = format!("+{} idle ▾", strip_folded);
        let clamped = truncate(&text, width);
        let strip_accent = Role::Accent.ansi();
        let strip_line = format!("{}{}{}\n", strip_accent, clamped, RESET);
        // In Cards the idle strip is part of the dark panel → paint it on rail_bg.
        if cards {
            out.push_str(&paint_card_line(&strip_line, width, &rail));
        } else {
            out.push_str(&strip_line);
        }
        targets.push(None);
    }
    // Strip trailing newline to prevent vt100 scroll in the test harness.
    if out.ends_with('\n') {
        out.pop();
    }
    RenderedRail { ansi: out, targets }
}

#[cfg(test)]
fn render(rows: &[TabRow], opts: &RenderOpts) -> String {
    render_rail(rows, opts).ansi
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kind::Kind;

    fn display(
        status: Status,
        done: usize,
        total: usize,
        detail: Option<PrimaryDetail>,
    ) -> TabDisplay {
        TabDisplay {
            status,
            progress: ProgressCounts {
                done,
                total,
                pending: if status == Status::Pending { 1 } else { 0 },
            },
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
    fn header_is_title_then_rule_two_lines() {
        let rows = vec![TabRow {
            number: 1,
            name: "a".into(),
            active: false,
            has_bell: false,
            display: display(Status::Running, 0, 0, None),
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

        let detail = PrimaryDetail {
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
                display: TabDisplay {
                    status: Status::Pending,
                    progress: ProgressCounts {
                        done: 0,
                        total: 2,
                        pending: 1,
                    },
                    detail: Some(detail),
                    panes: vec![
                        PaneDisplay::tracked(10, Kind::Claude, Status::Pending, "approve".into()),
                        PaneDisplay::tracked(11, Kind::Claude, Status::Running, "tests".into()),
                    ],
                },
            },
            TabRow {
                number: 2,
                name: "plain".into(),
                active: false,
                has_bell: false,
                display: display(Status::Idle, 0, 0, None),
            },
        ];

        let rail = render_rail(&rows, &ro(40, 0));
        assert_eq!(rail.line_count(), rail.ansi.lines().count());
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
                pane_id: Some(11),
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
            display: display(Status::Idle, 0, 0, None),
        }];
        let s = render(&rows, &ro(24, 0));
        assert!(s.contains("notes"));
        assert_eq!(s.lines().count(), 3); // always-on header (2) + tab row (1)
        assert!(s.contains(Status::Idle.glyph_for(GlyphSet::Plain)));
    }

    #[test]
    fn row_lines_by_state() {
        assert_eq!(row_lines(&display(Status::Idle, 0, 0, None), false), 1);

        let detail = |status, msg: &str| {
            Some(PrimaryDetail {
                repo: "r".into(),
                branch: "b".into(),
                msg: msg.into(),
                kind: Kind::Claude,
                since_tick: 0,
                status,
            })
        };
        assert_eq!(
            row_lines(
                &display(Status::Done, 1, 1, detail(Status::Done, "")),
                false
            ),
            1
        );
        assert_eq!(
            row_lines(
                &display(Status::Running, 1, 1, detail(Status::Running, "x")),
                false
            ),
            2
        );
        assert_eq!(
            row_lines(
                &display(Status::Error, 1, 1, detail(Status::Error, "x")),
                false
            ),
            2
        );
        // Pending: no msg → 1 line (line 2 suppressed); with msg → 2 lines (mark + activity).
        // Old 3-line case (branch · needs you + quoted msg) is gone.
        assert_eq!(
            row_lines(
                &display(Status::Pending, 1, 1, detail(Status::Pending, "")),
                false
            ),
            1
        );
        assert_eq!(
            row_lines(
                &display(Status::Pending, 1, 1, detail(Status::Pending, "go?")),
                false
            ),
            2
        );
        // Running with no msg: only 1 line
        assert_eq!(
            row_lines(
                &display(Status::Running, 1, 1, detail(Status::Running, "")),
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
                display: display(Status::Idle, 0, 0, None),
            },
            TabRow {
                number: 2,
                name: "b".into(),
                active: false,
                has_bell: false,
                display: display(Status::Idle, 0, 0, None),
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
        let detail = PrimaryDetail {
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
            display: display(Status::Pending, 0, 0, Some(detail)),
        }];
        let s = render(&rows, &ro(30, 5));
        let line1 = s.lines().nth(2).unwrap();
        assert!(line1.contains('▌'));
        // the bar uses the attention role when the active tab is also waiting
        assert!(line1.contains(Role::Attention.ansi()));
    }

    #[test]
    fn right_slot_per_state() {
        // Right slot is intentionally empty (removed per design). Verify no
        // elapsed/status text appears in the rendered output.
        let mk = |status, done, total| {
            let d = PrimaryDetail {
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
                display: display(status, done, total, Some(d)),
            }
        };
        assert!(!render(&[mk(Status::Done, 1, 1)], &ro(30, 0)).contains("done"));
        assert!(!render(&[mk(Status::Error, 0, 1)], &ro(30, 0)).contains("failed"));
        assert!(!render(&[mk(Status::Running, 0, 1)], &ro(30, 14)).contains("0:14"));
        let waiting = render(&[mk(Status::Pending, 0, 1)], &ro(30, 2));
        assert!(!waiting.contains('⏵'));
        assert!(!waiting.contains("0:02"));
    }

    #[test]
    fn working_slot_is_dim_not_role_colored() {
        // Right slot is now empty; verify no elapsed appears in output.
        let d = PrimaryDetail {
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
            display: display(Status::Running, 0, 1, Some(d)),
        }];
        let opts = ro(30, 14);
        let s = render(&rows, &opts);
        // No elapsed in the right slot.
        assert!(!s.contains("0:14"), "no elapsed in right slot: {:?}", s);
    }

    #[test]
    fn working_glyph_spins_with_tick() {
        let d = PrimaryDetail {
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
            display: display(Status::Running, 0, 1, Some(d.clone())),
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
            display: display(Status::Idle, 0, 0, None),
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
            display: display(Status::Idle, 0, 0, None),
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
        let detail = PrimaryDetail {
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
            display: display(Status::Running, 2, 4, Some(detail)),
        }];
        let s = render(&rows, &ro(width, 14));
        // header (2) + two tab lines emitted (Running+detail = 2 lines)
        assert_eq!(s.lines().count(), 4);
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
        let detail = PrimaryDetail {
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
            display: display(Status::Pending, 0, 1, Some(detail)),
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
        let detail = PrimaryDetail {
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
            display: display(Status::Running, 1, 1, Some(detail)),
        }];
        assert!(!render(&rows, &ro(30, 599)).contains('⚠'));
    }

    #[test]
    fn done_has_no_warning_glyph() {
        let detail = PrimaryDetail {
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
            display: display(Status::Done, 1, 1, Some(detail)),
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
            display: display(Status::Idle, 0, 0, None),
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
            display: display(Status::Idle, 0, 0, None),
        }];
        assert!(!render(&rows, &ro(24, 0)).contains('⚑'));
    }

    #[test]
    fn error_word_narrows_when_tight() {
        // Right slot is now empty; verify "failed"/"err" do not appear.
        let d = PrimaryDetail {
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
            display: display(Status::Error, 0, 1, Some(d)),
        }];
        // Right slot is empty; no "failed" or "err" in output.
        assert!(!render(&rows, &ro(30, 0)).contains("failed"));
        let narrow = render(&rows, &ro(14, 0));
        // "err" appears in the tab name "infra" - just verify no "failed".
        assert!(!narrow.contains("failed"));
    }

    #[test]
    fn working_detail_drops_branch_before_message_when_narrow() {
        let d = PrimaryDetail {
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
            display: display(Status::Running, 0, 1, Some(d)),
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
            display: display(Status::Idle, 0, 0, None),
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
        let d = PrimaryDetail {
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
            display: display(Status::Pending, 0, 1, Some(d)),
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
        // still present in Compact-density output. (PrimaryDetail text now uses theme-derived
        // truecolor foregrounds; card surfaces use truecolor backgrounds in Cards
        // density — but glyphs remain role-colored ANSI-16.)

        let mk_detail = |status: Status| PrimaryDetail {
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
                display: display(Status::Idle, 0, 0, None),
            },
            // running — two lines, with detail
            TabRow {
                number: 2,
                name: "run-tab".into(),
                active: true,
                has_bell: false,
                display: display(Status::Running, 1, 2, Some(mk_detail(Status::Running))),
            },
            // pending with msg — three lines
            TabRow {
                number: 3,
                name: "pend-tab".into(),
                active: false,
                has_bell: false,
                display: display(Status::Pending, 0, 1, Some(mk_detail(Status::Pending))),
            },
            // done — one line
            TabRow {
                number: 4,
                name: "done-tab".into(),
                active: false,
                has_bell: false,
                display: display(Status::Done, 1, 1, Some(mk_detail(Status::Done))),
            },
            // error — two lines
            TabRow {
                number: 5,
                name: "err-tab".into(),
                active: false,
                has_bell: false,
                display: display(Status::Error, 0, 1, Some(mk_detail(Status::Error))),
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
        // PrimaryDetail lines use truecolor foreground for readable dims.
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
        let detail_with_msg = PrimaryDetail {
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
            display: TabDisplay {
                status: Status::Pending,
                progress: ProgressCounts {
                    done: 0,
                    total: 3,
                    pending: 2,
                },
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
        assert_eq!(row_lines(&rows[0].display, false), 2);

        // Case 2: pending without msg → 1 line only, no line 2.
        let detail_no_msg = PrimaryDetail {
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
            display: TabDisplay {
                status: Status::Pending,
                progress: ProgressCounts {
                    done: 0,
                    total: 1,
                    pending: 1,
                },
                detail: Some(detail_no_msg),
                panes: vec![],
            },
        }];
        // row_lines = 1 (no msg → no line 2)
        assert_eq!(row_lines(&rows2[0].display, false), 1);

        // Width constraint: pending detail line must not exceed width
        let detail_long = PrimaryDetail {
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
            display: TabDisplay {
                status: Status::Pending,
                progress: ProgressCounts {
                    done: 0,
                    total: 5,
                    pending: 3,
                },
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
        let detail = PrimaryDetail {
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
            display: TabDisplay {
                status: Status::Pending,
                progress: ProgressCounts {
                    done: 0,
                    total: 1,
                    pending: 3,
                },
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
        assert!(s.ansi.contains("RADAR"));
        assert!(s.ansi.contains('◆')); // legend includes the waiting glyph (plain set)
        assert!(s.ansi.to_lowercase().contains("needs you"));
        assert!(s.ansi.to_lowercase().contains("click"));
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
        let detail = PrimaryDetail {
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
            display: display(Status::Pending, 0, 1, Some(detail)),
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
            display: display(Status::Running, 0, 0, None),
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

    /// Build a PaneDisplay for tree tests.
    fn pe(id: u32, kind: Kind, status: Status, msg: &str) -> PaneDisplay {
        PaneDisplay::tracked(id, kind, status, msg.into())
    }

    /// Build a multi-pane TabDisplay from per-pane entries. The header status is the
    /// most-urgent (highest-severity) member; done/total derive from the entries.
    fn display_multi(panes: Vec<PaneDisplay>) -> TabDisplay {
        let status = panes
            .iter()
            .filter_map(PaneDisplay::status)
            .max_by_key(|s| s.severity())
            .unwrap_or(Status::Idle);
        let total = panes.iter().filter(|p| p.is_tracked()).count();
        let done = panes
            .iter()
            .filter(|p| p.status() == Some(Status::Done))
            .count();
        let pending = panes
            .iter()
            .filter(|p| p.status() == Some(Status::Pending))
            .count();
        let detail = panes.iter().find_map(|p| match p {
            PaneDisplay::Tracked {
                kind,
                status: pane_status,
                msg,
                ..
            } if *pane_status == status => Some(PrimaryDetail {
                repo: "r".into(),
                branch: "b".into(),
                msg: msg.clone(),
                kind: *kind,
                since_tick: 0,
                status: *pane_status,
            }),
            _ => None,
        });
        TabDisplay {
            status,
            progress: ProgressCounts {
                done,
                total,
                pending,
            },
            detail,
            panes,
        }
    }

    #[test]
    fn row_lines_multi_pane_counts_header_children_collapse() {
        // New design: 4 tracked panes → 1 header + 4 pane lines = 5 (regardless of active).
        let a = display_multi(vec![
            pe(1, Kind::Claude, Status::Pending, "run migration?"),
            pe(2, Kind::Claude, Status::Running, "x"),
            pe(3, Kind::Claude, Status::Running, "y"),
            pe(4, Kind::Claude, Status::Running, "z"),
        ]);
        assert_eq!(row_lines(&a, false), 5, "header + 4 pane lines");
        assert_eq!(row_lines(&a, true), 5, "same regardless of active");
    }

    #[test]
    fn multi_pane_render_emits_header_child_and_collapse_lines() {
        // New design: 4 tracked panes → 1 header + 4 pane lines each shown individually.
        let a = display_multi(vec![
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
            display: a,
        };
        let s = render(&[row], &ro(30, 0));
        let body: Vec<&str> = s.lines().skip(2).collect(); // skip header
        // Header line shows the most-urgent pending glyph.
        assert!(
            body[0].contains('◆'),
            "header glyph is the most-urgent (pending): {:?}",
            body[0]
        );
        // First pane line: pending pane with mark ✳ and activity.
        assert!(
            body[1].contains('✳'),
            "first pane shows the kind mark: {:?}",
            body[1]
        );
        assert!(
            body[1].contains("run migration?"),
            "first pane shows activity: {:?}",
            body[1]
        );
        assert!(
            body[1].contains(Role::Attention.ansi()),
            "pending pane activity in attention: {:?}",
            body[1]
        );
        // No tree chars (new design uses no ├/└).
        assert!(!s.contains('├'), "no tree chars in new design: {:?}", s);
        assert!(!s.contains('└'), "no tree chars in new design: {:?}", s);
        // Exactly 5 body lines (header + 4 pane lines).
        assert_eq!(body.len(), 5, "header + 4 pane lines: {:?}", s);
    }

    #[test]
    fn multi_pane_inactive_fully_collapsed_uses_roster_count_copy() {
        // New design: 2 running panes → each gets its own line (no collapse).
        let a = display_multi(vec![
            pe(1, Kind::Claude, Status::Running, "x"),
            pe(2, Kind::Codex, Status::Running, "y"),
        ]);
        let row = TabRow {
            number: 1,
            name: "team".into(),
            active: false,
            has_bell: false,
            display: a,
        };
        let s = render(&[row], &ro(30, 0));
        let body: Vec<&str> = s.lines().skip(2).collect();

        // body[0] = header, body[1] = pane1(Claude/Running x), body[2] = pane2(Codex/Running y)
        // No collapse line — each pane is on its own line.
        assert_eq!(body.len(), 3, "header + 2 pane lines: {:?}", s);
        assert!(body[1].contains('◐'), "pane1 shows running glyph: {:?}", body[1]);
        assert!(body[2].contains('◐'), "pane2 shows running glyph: {:?}", body[2]);
        assert!(!s.contains("2 working"), "no collapse line: {:?}", s);
    }

    #[test]
    fn multi_pane_untracked_only_summary_names_panes() {
        // New design: 0 tracked panes → is_multi_pane = false → single-pane path.
        // Idle status → 1 line only (no detail line).
        let row = TabRow {
            number: 1,
            name: "shells".into(),
            active: false,
            has_bell: false,
            display: TabDisplay {
                status: Status::Idle,
                progress: ProgressCounts {
                    done: 0,
                    total: 0,
                    pending: 0,
                },
                detail: None,
                panes: vec![
                    PaneDisplay::untracked(1, "shell"),
                    PaneDisplay::untracked(2, "logs"),
                ],
            },
        };
        let s = render(&[row], &ro(30, 0));
        let body: Vec<&str> = s.lines().skip(2).collect();

        // With 0 tracked panes: single-pane path, Idle → 1 line (header only).
        assert_eq!(body.len(), 1, "only header line, no pane lines: {:?}", s);
        // No "2 panes" summary (untracked panes don't get their own lines).
        assert!(!s.contains("2 panes"), "no untracked summary: {:?}", s);
    }

    #[test]
    fn multi_pane_mixed_untracked_summary_names_panes() {
        let row = TabRow {
            number: 1,
            name: "mixed".into(),
            active: false,
            has_bell: false,
            display: TabDisplay {
                status: Status::Running,
                progress: ProgressCounts {
                    done: 0,
                    total: 1,
                    pending: 0,
                },
                detail: Some(PrimaryDetail {
                    repo: "r".into(),
                    branch: "b".into(),
                    msg: "tests".into(),
                    since_tick: 0,
                    status: Status::Running,
                    kind: Kind::Codex,
                }),
                panes: vec![
                    PaneDisplay::tracked(1, Kind::Codex, Status::Running, "tests".into()),
                    PaneDisplay::untracked(2, "shell"),
                ],
            },
        };
        let s = render(&[row], &ro(30, 0));
        let body: Vec<&str> = s.lines().skip(2).collect();

        // 1 tracked + 1 untracked → is_multi_pane = false → single-pane path.
        // Running with msg "tests" → 2 lines (header + detail).
        assert_eq!(body.len(), 2, "header + detail line: {:?}", s);
        // Line 2 is the detail line with the Codex mark ❉ and msg.
        assert!(body[1].contains('❉'), "detail shows Codex mark: {:?}", body[1]);
        assert!(body[1].contains("tests"), "detail shows msg: {:?}", body[1]);
        // No "2 panes" summary in the new design.
        assert!(!s.contains("2 panes"), "no summary: {:?}", s);
        assert!(!s.contains("2 working"), "no working count: {:?}", s);
    }

    #[test]
    fn multi_pane_active_expands_all_no_collapse() {
        // New design: 2 tracked panes → 1 header + 2 pane lines (active adds spine ▌).
        let a = display_multi(vec![
            pe(1, Kind::Claude, Status::Running, "a"),
            pe(2, Kind::Claude, Status::Done, "b"),
        ]);
        let row = TabRow {
            number: 1,
            name: "team".into(),
            active: true,
            has_bell: false,
            display: a,
        };
        let s = render(&[row], &ro(30, 0));
        let body: Vec<&str> = s.lines().skip(2).collect();
        // header + 2 pane lines, no collapse.
        assert_eq!(body.len(), 3, "active: header + 2 pane lines: {:?}", s);
        assert!(
            !s.contains("more working"),
            "no collapse line: {:?}",
            s
        );
        // No tree chars in new design.
        assert!(!s.contains('├'), "no tree chars: {:?}", s);
        assert!(!s.contains('└'), "no tree chars: {:?}", s);
        // Active pane lines have the spine ▌.
        assert!(body[1].contains('▌'), "active pane line has spine: {:?}", body[1]);
    }

    #[test]
    fn multi_pane_child_and_collapse_lines_never_exceed_width() {
        // The chunk-2 tree CHILD lines (expanded panes) and the collapse line
        // must be width-safe at narrow widths. (The header line-1 follows the
        // existing renderer's own width rules, covered by other line-1 tests.)
        // Use an inactive tab so a collapse line accompanies an expanded child.
        for &width in &[16usize, 20, 24, 30] {
            let a = display_multi(vec![
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
                display: a,
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
        let a = display_multi(vec![pe(1, Kind::Claude, Status::Pending, "approve?")]);
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
            display: a,
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
        let detail_running = |n: u8| PrimaryDetail {
            repo: format!("repo{}", n),
            branch: "main".into(),
            msg: "working".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Running,
        };
        let detail_pending = PrimaryDetail {
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
                display: display(Status::Running, 0, 1, Some(detail_running(1))),
            },
            TabRow {
                number: 2,
                name: "r2".into(),
                active: false,
                has_bell: false,
                display: display(Status::Running, 0, 1, Some(detail_running(2))),
            },
            TabRow {
                number: 3,
                name: "r3".into(),
                active: false,
                has_bell: false,
                display: display(Status::Running, 0, 1, Some(detail_running(3))),
            },
            TabRow {
                number: 4,
                name: "urgent".into(),
                active: false,
                has_bell: false,
                display: display(Status::Pending, 0, 1, Some(detail_pending)),
            },
        ];

        // Verify uncompressed sizes (new line-2 rule: pending+msg = 2, not 3).
        assert_eq!(row_lines(&rows[0].display, false), 2);
        assert_eq!(row_lines(&rows[1].display, false), 2);
        assert_eq!(row_lines(&rows[2].display, false), 2);
        assert_eq!(row_lines(&rows[3].display, false), 2); // pending + msg = 2 (mark + activity)

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
        let detail = PrimaryDetail {
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
                display: display(Status::Pending, 0, 1, Some(detail.clone())),
            },
            TabRow {
                number: 2,
                name: "run".into(),
                active: false,
                has_bell: false,
                display: display(Status::Running, 0, 1, Some(detail.clone())),
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
        // With trailing \n stripped: the last gap line's newline is removed, so
        // .lines() sees 7 lines (the trailing blank gap is consumed by the strip).
        let rows: Vec<TabRow> = (1..=3).map(idle_row).collect();
        let s = render(&rows, &ro_comfortable(24, 100));
        // body lines: each idle row = 1 content + 1 gap = 2 lines each. Total body = 6.
        // But the last gap's trailing \n is stripped, so .lines() gives 7 total.
        assert_eq!(
            s.lines().count(),
            7,
            "comfortable: expected 7 lines (last gap stripped), got {}:\n{:?}",
            s.lines().count(),
            s
        );
        // Check that there is a blank line between tabs (an empty line between non-empty lines).
        let body_lines: Vec<&str> = s.lines().skip(2).collect();
        assert_eq!(body_lines.len(), 5);
        // Odd-indexed body lines (0-based: 1, 3) should be blank gap lines.
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
        // body_lines[4] is the last tab content (no trailing gap).
        assert!(
            !body_lines[4].is_empty(),
            "body line 4 should be last tab content: {:?}",
            body_lines[4]
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
            s.lines().count(),
            5,
            "compact: expected 2 header + 3 content = 5 lines, got {}:\n{:?}",
            s.lines().count(),
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
        let detail = PrimaryDetail {
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
                display: display(Status::Running, 0, 1, Some(detail)),
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
        let detail = PrimaryDetail {
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
                display: display(Status::Idle, 0, 0, None),
            },
            TabRow {
                number: 2,
                name: "work".into(),
                active: true,
                has_bell: false,
                display: display(Status::Running, 0, 1, Some(detail)),
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
        let done = PrimaryDetail {
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
            display: display(Status::Done, 1, 1, Some(done)),
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
        let detail = PrimaryDetail {
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
            display: display(Status::Running, 0, 1, Some(detail)),
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
        let detail = PrimaryDetail {
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
                display: display(Status::Idle, 0, 0, None),
            },
            TabRow {
                number: 2,
                name: "work".into(),
                active: true,
                has_bell: false,
                display: display(Status::Running, 0, 1, Some(detail)),
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
        let detail = PrimaryDetail {
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
                display: display(Status::Idle, 0, 0, None),
            },
            TabRow {
                number: 2,
                name: "work".into(),
                active: true,
                has_bell: false,
                display: display(Status::Running, 0, 1, Some(detail)),
            },
        ];
        let s = render(&rows, &ro_cards(30, 100));
        let lines: Vec<String> = s.lines().map(strip_ansi_local).collect();
        // line 0 = header; line 1 = idle row; line 3 = active row (line 2 is its
        // detail row, line between is the idle gap). Find by name.
        let idle = lines.iter().find(|l| l.contains("idle")).unwrap();
        let active = lines.iter().find(|l| l.contains("work")).unwrap();
        // Idle: no leading space (inactive bar is empty), glyph at col 0.
        assert!(
            idle.starts_with("○"),
            "idle row must be '○…' (no leading space): {:?}",
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
        // Pane lines (new design) render as `  ‹status› ‹mark› ‹activity›` (inactive)
        // or `▌ ‹status› ‹mark› ‹activity›` (active). The status glyph comes FIRST,
        // then a space, then the identity mark — so the status icons line up and
        // the mark isn't cramped against the status glyph. No tree chars (├/└) in new design.
        let a = display_multi(vec![
            pe(1, Kind::Claude, Status::Running, "searching web"),
            pe(2, Kind::Claude, Status::Done, "done thing"),
        ]);
        let row = TabRow {
            number: 1,
            name: "t".into(),
            active: true,
            has_bell: false,
            display: a,
        };
        let s = render(&[row], &ro_cards(30, 100));
        // Find the running pane line (active → has spine ▌, contains ◐ and ✳).
        let pane_lines: Vec<String> = s
            .lines()
            .map(strip_ansi_local)
            .filter(|l| l.contains('◐') && l.contains('✳'))
            .collect();
        assert!(!pane_lines.is_empty(), "running pane line with mark not found");
        let child = &pane_lines[0];
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
        // No tree chars in new design.
        assert!(!s.contains('├'), "no tree chars in new design: {:?}", s);
    }

    #[test]
    fn comfortable_and_compact_emit_no_bg() {
        // Same tabs with Comfortable and Compact must contain NO card band.
        let detail = PrimaryDetail {
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
                display: display(Status::Idle, 0, 0, None),
            },
            TabRow {
                number: 2,
                name: "work".into(),
                active: true,
                has_bell: false,
                display: display(Status::Running, 0, 1, Some(detail)),
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
        let detail = PrimaryDetail {
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
            display: display(Status::Running, 2, 4, Some(detail)),
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
        let idle = display(Status::Idle, 0, 0, None);
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
            display: display(Status::Idle, 0, 0, None),
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
        let detail = PrimaryDetail {
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
                display: display(Status::Idle, 0, 0, None),
            },
            TabRow {
                number: 2,
                name: "agent".into(),
                active: false,
                has_bell: false,
                display: display(Status::Running, 0, 1, Some(detail.clone())),
            },
            TabRow {
                number: 3,
                name: "focus".into(),
                active: true,
                has_bell: false,
                display: display(Status::Running, 0, 1, Some(detail)),
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
    fn cards_active_multi_pane_children_use_subordinate_tint() {
        // Active multi-pane tabs should not paint every child row as selected:
        // the parent header owns the active tint; child rows step down to the
        // normal agent tint so the hierarchy remains legible.
        let row = TabRow {
            number: 1,
            name: "team".into(),
            active: true,
            has_bell: false,
            display: display_multi(vec![
                pe(1, Kind::Codex, Status::Running, "codex"),
                pe(2, Kind::Test, Status::Running, "cargo test"),
            ]),
        };
        let s = render(&[row], &ro_cards(30, 100));
        let lines: Vec<&str> = s.lines().collect();
        let active = "\x1b[48;2;56;59;71m";
        let agent = "\x1b[48;2;24;25;35m";
        // line 0 = header/rail, line 1 = tab parent, lines 2-3 = child panes.
        assert!(
            lines[1].contains(active),
            "parent row must carry active tint: {:?}",
            lines[1]
        );
        for line in &lines[2..=3] {
            assert!(
                line.contains(agent) && !line.contains(active),
                "child row must carry subordinate agent tint: {:?}",
                line
            );
        }
    }

    #[test]
    fn cards_3tint_layout_snapshot() {
        // Golden tint-map for the canonical sidebar.dc.html "cards" session:
        // active running agent, pending agent, done agent, then two idle panes.
        // Every tab is a card; cards are adjacent (no gap rows); tints encode the class.
        let running = PrimaryDetail {
            repo: "web".into(),
            branch: "".into(),
            msg: "building…".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Running,
        };
        let pending = PrimaryDetail {
            repo: "api".into(),
            branch: "fix".into(),
            msg: "".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Pending,
        };
        let done = PrimaryDetail {
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
                display: display(Status::Running, 0, 1, Some(running)),
            },
            TabRow {
                number: 2,
                name: "api".into(),
                active: false,
                has_bell: false,
                display: display(Status::Pending, 0, 1, Some(pending)),
            },
            TabRow {
                number: 3,
                name: "worker".into(),
                active: false,
                has_bell: false,
                display: display(Status::Done, 1, 1, Some(done)),
            },
            TabRow {
                number: 4,
                name: "Pane #1".into(),
                active: false,
                has_bell: false,
                display: display(Status::Idle, 0, 0, None),
            },
            TabRow {
                number: 5,
                name: "Pane #1".into(),
                active: false,
                has_bell: false,
                display: display(Status::Idle, 0, 0, None),
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
        let pending = PrimaryDetail {
            repo: "pinky".into(),
            branch: "fix".into(),
            msg: "".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Pending,
        };
        let err = PrimaryDetail {
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
                display: display(Status::Pending, 0, 1, Some(pending)),
            },
            TabRow {
                number: 2,
                name: "infra".into(),
                active: false,
                has_bell: false,
                display: display(Status::Error, 0, 1, Some(err)),
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
        // Header reads " RADAR" with tab count. The ·N! urgent marker has been
        // removed per design rule 7 (no right-slot for now).
        let pending = PrimaryDetail {
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
                display: display(Status::Pending, 0, 1, Some(pending)),
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
        // The ·N! urgent marker is removed per design.
        assert!(
            !header.contains("·1!"),
            "urgent marker must not appear: {:?}",
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

    // ── Color additivity guard ────────────────────────────────────────────────

    /// Strip `\x1b[...m` SGR escape sequences from a string.
    fn strip_sgr(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                if chars.peek() == Some(&'[') {
                    chars.next(); // consume '['
                    for ch in chars.by_ref() {
                        if ch == 'm' {
                            break;
                        }
                    }
                } else {
                    out.push(c);
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn color_is_purely_additive_over_a_fixed_layout() {
        let rows = vec![
            TabRow {
                number: 1,
                name: "agent".into(),
                active: true,
                has_bell: false,
                display: display(Status::Pending, 0, 1, None),
            },
            TabRow {
                number: 2,
                name: "idle".into(),
                active: false,
                has_bell: false,
                display: display(Status::Idle, 0, 0, None),
            },
        ];
        let out = render(&rows, &ro(30, 0));
        // Stripping all SGR leaves a clean character grid (no escape residue),
        // i.e. color sits *on top of* layout and never alters the cell content.
        let stripped = strip_sgr(&out);
        assert!(
            !stripped.contains('\x1b'),
            "no escape residue after strip: {stripped:?}"
        );
        assert!(stripped.contains("agent"));
        assert!(stripped.contains("idle"));
        // Visible width per line is unchanged by color (color adds zero columns).
        for line in stripped.lines() {
            assert!(visible_width(line) <= 30, "line exceeds width: {line:?}");
        }
    }

    // ── Stage 3b: snapshot / proptest / overflow / color-glyph-axis tests ──────

    /// Render raw output into the visible character grid (ANSI stripped via a real
    /// VT parser), one line per terminal row — the human-readable snapshot.
    fn grid(raw: &str, width: u16) -> String {
        let height = raw.lines().count().max(1) as u16;
        let mut parser = vt100::Parser::new(height, width, 0);
        let joined = raw.replace('\n', "\r\n");
        parser.process(joined.as_bytes());
        let screen = parser.screen();
        (0..height)
            .map(|r| {
                (0..width)
                    .map(|c| {
                        screen
                            .cell(r, c)
                            .map(|cell| cell.contents())
                            .unwrap_or_default()
                    })
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A representative multi-state session used by several snapshot tests.
    fn scenario_canonical() -> Vec<TabRow> {
        let running = PrimaryDetail {
            repo: "web".into(),
            branch: "".into(),
            msg: "building\u{2026}".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Running,
        };
        let pending = PrimaryDetail {
            repo: "api".into(),
            branch: "fix".into(),
            msg: "".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Pending,
        };
        let done = PrimaryDetail {
            repo: "worker".into(),
            branch: "".into(),
            msg: "".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Done,
        };
        vec![
            TabRow {
                number: 1,
                name: "web".into(),
                active: true,
                has_bell: false,
                display: display(Status::Running, 0, 1, Some(running)),
            },
            TabRow {
                number: 2,
                name: "api".into(),
                active: false,
                has_bell: false,
                display: display(Status::Pending, 0, 1, Some(pending)),
            },
            TabRow {
                number: 3,
                name: "worker".into(),
                active: false,
                has_bell: false,
                display: display(Status::Done, 1, 1, Some(done)),
            },
            TabRow {
                number: 4,
                name: "notes".into(),
                active: false,
                has_bell: false,
                display: display(Status::Idle, 0, 0, None),
            },
        ]
    }

    fn ro_full(
        width: usize,
        height: usize,
        density: crate::config::Density,
        glyphs: GlyphSet,
    ) -> RenderOpts {
        RenderOpts {
            width,
            height,
            now_tick: 0,
            glyphs,
            header: true,
            density,
            theme: crate::theme::DerivedColors::default(),
        }
    }

    // ── Snapshot tests ──

    #[test]
    fn snapshot_canonical_cards_grid() {
        let rows = scenario_canonical();
        let raw = render(
            &rows,
            &ro_full(30, 100, crate::config::Density::Cards, GlyphSet::Plain),
        );
        insta::assert_snapshot!("canonical_cards_grid", grid(&raw, 30));
    }

    #[test]
    fn snapshot_canonical_cards_raw() {
        let rows = scenario_canonical();
        let raw = render(
            &rows,
            &ro_full(30, 100, crate::config::Density::Cards, GlyphSet::Plain),
        );
        let shown = raw.replace('\x1b', "\\e");
        insta::assert_snapshot!("canonical_cards_raw", shown);
    }

    #[test]
    fn snapshot_canonical_tint_map() {
        let rows = scenario_canonical();
        let raw = render(
            &rows,
            &ro_full(30, 100, crate::config::Density::Cards, GlyphSet::Plain),
        );
        insta::assert_snapshot!("canonical_tint_map", tint_map(&raw));
    }

    // ── Overflow tests ──

    #[test]
    fn renders_at_extreme_small_width_without_panic_or_overflow() {
        let rows = scenario_canonical();
        let s = render(
            &rows,
            &ro_full(8, 100, crate::config::Density::Cards, GlyphSet::Plain),
        );
        for line in s.lines() {
            assert!(visible_width(line) <= 8, "line exceeds width 8: {:?}", line);
        }
    }

    #[test]
    fn renders_at_extreme_small_height_clamps_lines() {
        let rows = scenario_canonical();
        let s = render(
            &rows,
            &ro_full(30, 3, crate::config::Density::Cards, GlyphSet::Plain),
        );
        assert!(
            s.lines().count() <= 3,
            "exceeded height budget: {}",
            s.lines().count()
        );
    }

    #[test]
    fn renders_many_tabs_high_counts_at_narrow_width_no_overflow() {
        // 15 tabs: 5 idle (folded) + 10 pending (urgent marker " ·10!" = 5 cols).
        // At width=8, height=6 overflow mode: count+urgent combined can exceed width.
        // The renderer must clamp the assembled header line to width.
        let mut rows: Vec<TabRow> = (1u32..=5)
            .map(|n| TabRow {
                number: n,
                name: format!("t{}", n),
                active: false,
                has_bell: false,
                display: display(Status::Idle, 0, 0, None),
            })
            .collect();
        for n in 6u32..=15 {
            let d = PrimaryDetail {
                repo: "r".into(),
                branch: "b".into(),
                msg: "".into(),
                kind: Kind::Claude,
                since_tick: 0,
                status: Status::Pending,
            };
            rows.push(TabRow {
                number: n,
                name: format!("t{}", n),
                active: false,
                has_bell: false,
                display: display(Status::Pending, 0, 1, Some(d)),
            });
        }
        let s = render(
            &rows,
            &ro_full(8, 6, crate::config::Density::Cards, GlyphSet::Plain),
        );
        for line in s.lines() {
            assert!(
                visible_width(line) <= 8,
                "line exceeds width 8 at narrow render: {:?} (visible {})",
                line,
                visible_width(line)
            );
        }
    }

    // ── Color/glyph axis tests ──

    #[test]
    fn truecolor_mode_emits_24bit_sgr() {
        let rows = scenario_canonical();
        let s = render(
            &rows,
            &ro_full(30, 100, crate::config::Density::Cards, GlyphSet::Plain),
        );
        assert!(s.contains("\x1b[48;2;"), "expected 24-bit background SGR");
    }

    #[test]
    fn both_glyph_sets_keep_columns_within_width() {
        for glyphs in [GlyphSet::Plain, GlyphSet::Nerd] {
            let rows = scenario_canonical();
            let width = 30u16;
            let raw = render(
                &rows,
                &ro_full(width as usize, 100, crate::config::Density::Cards, glyphs),
            );
            let g = grid(&raw, width);
            for line in g.lines() {
                assert!(
                    visible_width(line) <= width as usize,
                    "glyphs={:?} line wider than {}: {:?}",
                    glyphs,
                    width,
                    line
                );
            }
        }
    }

    #[test]
    fn wide_and_combining_chars_do_not_break_alignment() {
        let detail = PrimaryDetail {
            repo: "caf\u{00e9}".into(),
            branch: "".into(),
            msg: "测试 \u{1f680} e\u{0301}".into(),
            kind: Kind::Claude,
            since_tick: 0,
            status: Status::Running,
        };
        let rows = vec![TabRow {
            number: 1,
            name: "测试caf\u{00e9}\u{1f680}".into(),
            active: true,
            has_bell: false,
            display: display(Status::Running, 0, 1, Some(detail)),
        }];
        let width = 24u16;
        let raw = render(
            &rows,
            &ro_full(
                width as usize,
                100,
                crate::config::Density::Cards,
                GlyphSet::Plain,
            ),
        );
        for line in grid(&raw, width).lines() {
            assert!(
                visible_width(line) <= width as usize,
                "alignment broke: {:?}",
                line
            );
        }
    }

    // ── Property-based tests ──

    use proptest::prelude::*;

    prop_compose! {
        fn arb_status()(n in 0u8..5) -> Status {
            match n {
                0 => Status::Idle,
                1 => Status::Done,
                2 => Status::Running,
                3 => Status::Pending,
                _ => Status::Error,
            }
        }
    }

    prop_compose! {
        fn arb_row()(
            status in arb_status(),
            name in "[a-zA-Z0-9_-]{0,20}",
            active in any::<bool>(),
            total in 0usize..4,
        ) -> TabRow {
            let detail = if total > 0 {
                Some(PrimaryDetail {
                    repo: "r".into(),
                    branch: "".into(),
                    msg: "m".into(),
                    kind: Kind::Claude,
                    since_tick: 0,
                    status,
                })
            } else {
                None
            };
            TabRow {
                number: 1,
                name,
                active,
                has_bell: false,
                display: display(status, 0, total, detail),
            }
        }
    }

    pub fn arb_rows() -> impl Strategy<Value = Vec<TabRow>> {
        proptest::collection::vec(arb_row(), 0..8).prop_map(|rows| {
            rows.into_iter()
                .enumerate()
                .map(|(i, mut r)| {
                    r.number = (i as u32) + 1;
                    r
                })
                .collect()
        })
    }

    proptest! {
        #[test]
        fn render_respects_width_height_and_never_panics(
            rows in arb_rows(),
            width in 4usize..120,
            height in 1usize..60,
        ) {
            let opts = ro_full(width, height, crate::config::Density::Cards, GlyphSet::Plain);
            let s = render(&rows, &opts);
            prop_assert!(
                s.lines().count() <= height,
                "lines {} > height {}",
                s.lines().count(),
                height
            );
            for line in s.lines() {
                prop_assert!(
                    visible_width(line) <= width,
                    "line width {} > {}: {:?}",
                    visible_width(line),
                    width,
                    line
                );
            }
        }
    }

    proptest! {
        /// Lockstep: the emitted ANSI and the click-target map stay in exact
        /// 1:1 line correspondence, at every width the rail can be drawn at.
        #[test]
        fn render_rail_lockstep_lines_match_targets(
            rows in arb_rows(),
            width in 8usize..=120,
            height in 1usize..=60,
        ) {
            let mut opts = ro(width, 0);
            opts.height = height;
            let rail = render_rail(&rows, &opts);
            // 1:1 correspondence between physical lines and target slots.
            prop_assert_eq!(rail.line_count(), rail.ansi.lines().count());
            // Every in-range line resolves without panic; out-of-range is None.
            for line in 0..rail.line_count() {
                let _ = rail.target_at_line(line as isize);
            }
            prop_assert_eq!(rail.target_at_line(-1), None);
            prop_assert_eq!(rail.target_at_line(rail.line_count() as isize), None);
        }
    }

    #[test]
    fn render_rail_empty_has_zero_lines_and_no_targets() {
        let opts = ro(24, 0);
        let rail = render_rail(&[], &opts);
        assert_eq!(rail.line_count(), 0);
        assert_eq!(rail.ansi, "");
        assert_eq!(rail.target_at_line(0), None);
    }

    #[test]
    fn onboarding_returns_rail_with_no_targets_but_matching_line_count() {
        let opts = ro(24, 0);
        let rail = onboarding(&opts);
        assert!(rail.line_count() > 0, "onboarding paints a panel");
        assert_eq!(rail.line_count(), rail.ansi.matches('\n').count());
        for line in 0..rail.line_count() {
            assert_eq!(
                rail.target_at_line(line as isize),
                None,
                "onboarding has no clickable rows"
            );
        }
    }

    // ── Layout-footprint facts (migrated from lib.rs click-mapping tests) ──

    #[test]
    fn multi_pane_collapsed_footprint_is_header_plus_expanded_plus_collapse() {
        // New design: 3 tracked panes → 1 header + 3 pane lines = 4 content lines.
        let a = display_multi(vec![
            pe(10, Kind::Claude, Status::Pending, "approve?"),
            pe(11, Kind::Claude, Status::Running, "building"),
            pe(12, Kind::Claude, Status::Running, "testing"),
        ]);
        assert_eq!(row_lines(&a, false), 4, "header + 3 pane lines");
    }

    #[test]
    fn single_running_pane_with_detail_is_two_content_lines() {
        // Single-pane Running tab with a non-empty detail msg → 2 content lines
        // (name row + detail row). Mirrors the row_lines assertion from
        // lib.rs::click_mapping_cards_pad_y_and_post_content_row.
        let a = display_multi(vec![pe(10, Kind::Claude, Status::Running, "msg")]);
        assert_eq!(row_lines(&a, false), 2, "tab 0 should be 2 content lines");
    }

    #[test]
    fn from_lines_derives_ansi_and_targets_in_lockstep() {
        let t = RailTarget { tab_position: 2, pane_id: None };
        let lines = vec![
            Line { text: "alpha\n".into(), target: Some(t), bg: LineBg::None },
            Line { text: "beta\n".into(),  target: None,    bg: LineBg::None },
            Line { text: "gamma\n".into(), target: Some(RailTarget { tab_position: 3, pane_id: Some(9) }), bg: LineBg::None },
        ];
        let rr = RenderedRail::from_lines(lines);
        // ansi: joined, trailing newline popped.
        assert_eq!(rr.ansi, "alpha\nbeta\ngamma");
        // targets: 1:1 with lines, never off-by-one.
        assert_eq!(rr.line_count(), 3);
        assert_eq!(rr.target_at_line(0), Some(t));
        assert_eq!(rr.target_at_line(1), None);
        assert_eq!(rr.target_at_line(2), Some(RailTarget { tab_position: 3, pane_id: Some(9) }));
        assert_eq!(rr.target_at_line(3), None);
        // Structural lockstep: every '\n'-terminated segment has a target slot.
        assert_eq!(rr.ansi.split('\n').count(), rr.line_count());
    }

    #[test]
    fn from_lines_empty_is_empty() {
        let rr = RenderedRail::from_lines(vec![]);
        assert_eq!(rr.ansi, "");
        assert_eq!(rr.line_count(), 0);
    }
}
