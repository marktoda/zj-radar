//! Pure renderer: per-tab rows → ANSI string. No zellij-tile dependency.

use crate::config::Density;
use crate::rollup::{Outcome, PaneDisplay, TabDisplay};
pub use crate::status::GlyphSet;
use crate::status::{Role, Status};
use crate::theme::DerivedColors;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";

/// A colored (optionally bold) text run that terminates its own SGR with RESET.
///
/// Every colored token in the rail is built through `Seg`, so "colored ⇒
/// RESET-terminated" is structural rather than reviewer-enforced —
/// `paint_card_line` re-arms the card background after each RESET, and a run that
/// forgot its RESET would bleed the foreground color across the rest of the band.
/// `Display` emits `{color}{BOLD?}{text}{RESET}`; a run groups *all* text that
/// shares one color (e.g. line 1's glyph+number+name) so the byte stream matches
/// the hand-built `format!`s it replaces.
struct Seg<'a> {
    color: &'a str,
    bold: bool,
    text: std::borrow::Cow<'a, str>,
}

impl<'a> Seg<'a> {
    fn new(color: &'a str, text: impl Into<std::borrow::Cow<'a, str>>) -> Self {
        Self { color, bold: false, text: text.into() }
    }

    fn bold(color: &'a str, text: impl Into<std::borrow::Cow<'a, str>>) -> Self {
        Self { color, bold: true, text: text.into() }
    }
}

impl std::fmt::Display for Seg<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.color)?;
        if self.bold {
            f.write_str(BOLD)?;
        }
        f.write_str(&self.text)?;
        f.write_str(RESET)
    }
}

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

/// Presentation for the roll-up's `Outcome` tag. The enum itself lives in
/// `rollup` (pure semantics); these methods encode the glyphs and the
/// width-driven roomy/tight forms, which are the renderer's concern.
impl Outcome {
    /// The roomy form: `✓` / `(exit N)` (or `✗` when the code is unknown).
    fn full(self) -> String {
        match self {
            Outcome::Ok => "✓".to_string(),
            Outcome::Failed(Some(code)) => format!("(exit {})", code),
            Outcome::Failed(None) => "✗".to_string(),
        }
    }

    /// The irreducible 1-column form, shown when width is too tight for `full`.
    fn minimal(self) -> &'static str {
        match self {
            Outcome::Ok => "✓",
            Outcome::Failed(_) => "✗",
        }
    }

    /// The hue the tag reads in: success (green) / error (red).
    fn role(self) -> Role {
        match self {
            Outcome::Ok => Role::Success,
            Outcome::Failed(_) => Role::Error,
        }
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineBg {
    None,
    Rail,        // dark panel base (rail_bg): header, gaps, idle strip
    Card,        // this row's card surface (card_tint of the owning row)
    ActiveChild, // active multi-pane child line (surface_agent)
}

impl LineBg {
    /// The one home for the surface-class → bg-escape map. `render_rail` resolves
    /// every painted line through here, so the `ActiveChild` vs `Card` split (the
    /// drift `cards_active_more_line_*` guards) lives in a single place. `row` is
    /// the card's owning row (only `Card` consults it); `rail` is the precomputed
    /// panel-base escape. `None` means the line is never painted.
    fn escape(self, row: &TabRow, theme: &DerivedColors, rail: &str) -> Option<String> {
        match self {
            LineBg::None => Option::None,
            LineBg::Rail => Some(rail.to_string()),
            LineBg::Card => Some(card_tint(row, theme)),
            LineBg::ActiveChild => Some(tc_bg(theme.surface_agent)),
        }
    }
}

/// One physical rail line and the click target it resolves to. `text` always
/// ends in exactly one '\n'. The unit of rendering: ansi, targets, and
/// footprint all derive from a `Vec<Line>`, so they cannot drift.
#[derive(Clone, Debug)]
struct Line {
    text: String,
    target: Option<RailTarget>,
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

/// Carries the planner's view of a row: status (for compression priority)
/// and the pre-rendered line count sourced directly from the block that will be emitted.
struct RowMeta {
    status: Status,
    full_lines: usize,
}

/// Single source of truth for a card's full vertical footprint (top→bottom:
/// `pad_y` internal-pad rows + the card's uncompressed content rows + `gap`
/// external-separation rows). `render_rail()` budgets in terms of this so the
/// emitted ANSI lines and line targets stay exact.
fn card_block_lines(full_lines: usize, spacing: CardSpacing) -> usize {
    spacing.pad_y + full_lines + spacing.gap
}

/// Color of the active-tab spine (col-0 `▌`), by the tab's dominant status:
/// accent (mauve) normally, attention (peach) when the tab is waiting/error so
/// the focus cue carries the alarm. Single source for all three spine sites
/// (line-1 header, `+N more`, active pane lines).
fn spine_role(status: Status) -> Role {
    match status {
        Status::Pending | Status::Error => Role::Attention,
        _ => Role::Accent,
    }
}

/// A tab is "multi-pane" for tree purposes when it has more than one tracked pane.
/// Single-pane tabs keep the chunk-1 line-2 behavior; multi-pane tabs use
/// the line-per-pane design.
fn is_multi_pane(display: &TabDisplay) -> bool {
    display.panes.iter().filter(|p| p.is_tracked()).count() > 1
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
/// `full_lines` (from `RowMeta`) is the *uncompressed* line count; this function
/// produces the *planned* per-row line count actually rendered.
fn plan_overflow(rows: &[RowMeta], body_budget: usize) -> (Vec<(usize, usize)>, usize) {
    let total: usize = rows.iter().map(|r| r.full_lines).sum();
    if total <= body_budget {
        // Everything fits at full fidelity.
        let plan = rows
            .iter()
            .enumerate()
            .map(|(i, r)| (i, r.full_lines))
            .collect();
        return (plan, 0);
    }

    // Step 1: fold idle rows; keep non-idle at full line counts.
    let non_idle_idx: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.status != Status::Idle)
        .map(|(i, _)| i)
        .collect();
    let folded_count = rows
        .iter()
        .filter(|r| r.status == Status::Idle)
        .count();

    // Each kept row starts at its full (uncompressed) line count.
    let mut planned: Vec<(usize, usize)> = non_idle_idx
        .iter()
        .map(|&i| (i, rows[i].full_lines))
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
        if is_calm(rows[idx].status) {
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
            if !is_calm(rows[idx].status) {
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
    rows: &[RowMeta],
    body_budget: usize,
    density: Density,
) -> (Vec<(usize, usize)>, usize, CardSpacing) {
    let base = card_spacing(density);

    // Fast path: if every row's FULL block (pad_y + content + gap) fits, render
    // everything at full fidelity with full spacing. `card_block_lines` is the
    // single footprint source shared with the budgeting below.
    let full_footprint: usize = rows
        .iter()
        .map(|r| card_block_lines(r.full_lines, base))
        .sum();
    if full_footprint <= body_budget {
        let plan = rows
            .iter()
            .enumerate()
            .map(|(i, r)| (i, r.full_lines))
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

/// The onboarding legend: every `Status` with its plain-English gloss, in a
/// deliberate *display* order (loudest first), distinct from the `Status::ALL`
/// severity order. Each variant must appear exactly once — pinned by
/// `onboarding_legend_covers_every_status` so a new `statuses!` row can't be
/// silently dropped from the onboarding screen.
const ONBOARDING_LEGEND: [(Status, &str); Status::ALL.len()] = [
    (Status::Pending, "needs you"),
    (Status::Running, "working"),
    (Status::Done, "done"),
    (Status::Error, "error"),
    (Status::Idle, "idle"),
];

/// The rail's resting "hello / how it works" face — shown on cold start or
/// before permission is granted. Not a permission interceptor.
pub fn onboarding(opts: &RenderOpts) -> RenderedRail {
    // Every line is clamped to `opts.width`, so the panel honors the same width
    // discipline as the rail (`onboarding_never_exceeds_width`). Color is purely
    // additive: the truncation acts on the visible text, never the SGR codes.
    fn line(out: &mut String, role: &str, text: &str, w: usize) {
        out.push_str(&format!("{}\n", Seg::new(role, truncate(text, w))));
    }

    let w = opts.width;
    let mut out = String::new();
    let accent = Role::Accent.ansi();
    let muted = Role::Muted.ansi();
    let g = opts.glyphs;
    line(&mut out, accent, " RADAR", w);
    line(&mut out, accent, &"═".repeat(w), w);
    line(&mut out, muted, " watching your tabs for", w);
    line(&mut out, muted, " AI agent activity.", w);
    out.push('\n');
    for (st, label) in ONBOARDING_LEGEND {
        let role_code = st.role().ansi();
        let glyph = st.glyph_for(g);
        // " {glyph} {label}" — the marker+spaces are a fixed 3-col prefix. Below
        // that the label has no room, so clamp the marker alone.
        if w < 3 {
            line(&mut out, role_code, &format!(" {glyph}"), w);
        } else {
            out.push_str(&format!(
                " {} {}\n",
                Seg::new(role_code, glyph.to_string()),
                Seg::new(muted, truncate(label, w - 3)),
            ));
        }
    }
    out.push('\n');
    line(&mut out, muted, " click a row to jump", w);
    RenderedRail::from_ansi_without_targets(out)
}

/// Rail face shown when permission has NOT been granted. Distinct from
/// `onboarding` (which is the granted-but-idle face) so a blocked install is
/// never mistaken for a working one.
pub fn needs_permission(opts: &RenderOpts) -> RenderedRail {
    fn line(out: &mut String, role: &str, text: &str, w: usize) {
        out.push_str(&format!("{}\n", Seg::new(role, truncate(text, w))));
    }
    let w = opts.width;
    let mut out = String::new();
    let accent = Role::Accent.ansi();
    let needs = Role::Attention.ansi(); // Attention (bright orange/red) for the warning line
    let muted = Role::Muted.ansi();
    line(&mut out, accent, " RADAR", w);
    line(&mut out, accent, &"═".repeat(w), w);
    line(&mut out, needs, " ⚠ needs permission", w);
    out.push('\n');
    line(&mut out, muted, " focus this pane", w);
    line(&mut out, muted, " and press y to", w);
    line(&mut out, muted, " enable agent status.", w);
    RenderedRail::from_ansi_without_targets(out)
}

/// Emit one row's body into `out`, respecting `max_lines`.
///
/// Line 1 (gutter+glyph+num+name+slot) is ALWAYS emitted.
/// PrimaryDetail/roster lines are emitted in priority order. Returns the full
/// untruncated set of lines; caller applies `.take(max_lines)` for overflow.
fn render_row(row: &TabRow, opts: &RenderOpts) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
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
        Seg::new(&hue(spine_role(st)), "▌").to_string()
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
    // Bold encodes *activity*: every non-idle row's glyph+number+name is bold so
    // state reads at a glance. Idle stays light/recessed. (Focus is a separate
    // cue — the accent spine + brighter card — so the two stay independent.)
    let label_bold = st != Status::Idle;

    // bell marker just before the (removed) slot.
    let bell = if row.has_bell {
        format!("{} ", Seg::new(&hue(Role::Working), "⚑"))
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
    // At extreme-narrow widths name_budget saturates to 0 → name = ""; no
    // .max(1) so we never force an extra `…` that would push past `width`.
    let name_budget = width.saturating_sub(prefix_len + bell_len);
    let name = truncate(&row.name, name_budget);

    // gap can be 0 at extreme-narrow widths; saturating_sub prevents underflow.
    let used = prefix_len + UnicodeWidthStr::width(name.as_str()) + bell_len;
    let gap = width.saturating_sub(used);
    let sp_after_num = if has_trailing_sp { " " } else { "" };
    let label_text = format!("{glyph_char} {num}{sp_after_num}{name}");
    let label = Seg {
        color: &label_color,
        bold: label_bold,
        text: label_text.into(),
    };
    lines.push(Line {
        text: format!("{}{}{}{}{}\n", bar, pad, label, " ".repeat(gap), bell),
        target: Some(tab_target),
        bg: LineBg::Card,
    });

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
            let text = emit_pane_line(pane, opts, row.active, st, &dim_strong);
            lines.push(Line {
                text,
                target: Some(RailTarget { tab_position: tab_target.tab_position, pane_id: Some(pane.pane_id()) }),
                bg: if row.active { LineBg::ActiveChild } else { LineBg::Card },
            });
        }

        let remaining = total_tracked - show;
        if remaining > 0 {
            let more_text = format!("+{} more", remaining);
            // Both branches prepend a 2-col prefix (spine+space or 2 spaces), so
            // reserve those columns before clamping the text to avoid overflow.
            let clamped = truncate(&more_text, opts.width.saturating_sub(2));
            let text = format!(
                "{}{}\n",
                child_prefix(row.active, st),
                Seg::new(&idle_color, clamped),
            );
            lines.push(Line {
                text,
                target: Some(tab_target),
                bg: if row.active { LineBg::ActiveChild } else { LineBg::Card },
            });
        }
        return lines;
    }

    // ── Single-pane line 2 (chunk 1) ───────────────────────────────────────
    // Line 2: `‹mark› ‹activity›` — source-agnostic for all active statuses.
    // Emitted when a detail exists with a non-empty msg OR a finished-command
    // outcome tag to show. For Pending (the question), the command is colored in
    // attention (loud); others dim. The outcome tag carries its own role hue.
    if let Some(d) = &row.display.detail {
        if !d.msg.trim().is_empty() || d.outcome.is_some() {
            match st {
                Status::Idle => {}
                Status::Done | Status::Running | Status::Error | Status::Pending => {
                    // Identity mark: vendor-neutral but bold + the stronger dim
                    // (dim_strong, not the faint idle_text) so it reads as a
                    // deliberate mark, not a footnote. Glyph-set aware.
                    let mark = d.kind.mark(opts.glyphs);
                    let mark_width = UnicodeWidthChar::width(mark).unwrap_or(1);
                    // "  ‹mark› " prefix: 2-space indent + mark + space. The
                    // mark sits one column right of the line-1 glyph (which is at
                    // col 1 after the bar/spine column), matching the design.
                    let prefix_vis = 2 + mark_width + 1;
                    let avail = width.saturating_sub(prefix_vis);
                    let cmd_color = if st == Status::Pending {
                        hue(Role::Attention)
                    } else {
                        dim_strong.clone()
                    };
                    let activity = compose_activity(&d.msg, d.outcome, avail, &cmd_color);
                    lines.push(Line {
                        text: format!(
                            "  {} {}\n",
                            Seg::bold(&dim_strong, mark.to_string()),
                            activity
                        ),
                        target: Some(tab_target),
                        bg: LineBg::Card,
                    });
                }
            }
        }
    }
    lines
}

/// Compose the styled activity segment for a detail/pane line: the command text
/// (in `cmd_color`) plus, when the pane has finished, its outcome tag in the
/// outcome's role hue. The tag is reserved FIRST so it always survives — the
/// command absorbs any truncation (degrading to `…`, then vanishing), while the
/// outcome shrinks only from its full form (`(exit 1)`) to the irreducible
/// 1-column glyph (`✓`/`✗`). The returned string fits within `avail` columns and
/// carries its own color escapes (each segment RESET-terminated).
fn compose_activity(cmd: &str, outcome: Option<Outcome>, avail: usize, cmd_color: &str) -> String {
    let Some(oc) = outcome else {
        return Seg::new(cmd_color, truncate(cmd, avail)).to_string();
    };
    let role = oc.role().ansi();
    let cmd = cmd.trim();
    // Outcome with no command (e.g. an exit with no recorded command string):
    // show the largest form that fits.
    if cmd.is_empty() {
        let full = oc.full();
        let tag = if UnicodeWidthStr::width(full.as_str()) <= avail {
            full
        } else {
            truncate(oc.minimal(), avail)
        };
        return Seg::new(role, tag).to_string();
    }
    // Command + tag: prefer the full tag, falling back to the minimal tag, as
    // long as ≥1 command column remains (tag width + 1 separating space + ≥1
    // command col). The `+ 2` reserves the space and that one command column.
    let full = oc.full();
    let min = oc.minimal();
    for tag in [full.as_str(), min] {
        let tag_w = UnicodeWidthStr::width(tag);
        if tag_w + 2 <= avail {
            return format!(
                "{} {}",
                Seg::new(cmd_color, truncate(cmd, avail - tag_w - 1)),
                Seg::new(role, tag),
            );
        }
    }
    // Too tight for any command: show the outcome glyph alone (clip if even that
    // overflows the extreme-narrow width).
    if UnicodeWidthStr::width(min) <= avail {
        Seg::new(role, min).to_string()
    } else {
        Seg::new(role, truncate(min, avail)).to_string()
    }
}

/// Emit one pane line in the new line-per-pane design:
/// Inactive: `  {glyph} {mark} {msg}` (2-space indent)
/// Active:   `▌ {glyph} {mark} {msg}` (spine + space)
fn emit_pane_line(
    pane: &PaneDisplay,
    opts: &RenderOpts,
    tab_active: bool,
    tab_status: Status,
    dim_strong: &str,
) -> String {
    let width = opts.width;
    let mark = pane.kind().mark(opts.glyphs);
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
    // Narrow-width fallback: the colored path always emits the full fixed prefix
    // (spine/indent + glyph + mark + spaces) unconditionally, so at widths below
    // it the line would overflow. Below that floor, drop all color and emit a
    // single plain line clamped to `width` so nothing exceeds the band.
    if width < prefix_vis {
        let indent = if tab_active { "▌ " } else { "  " };
        let plain = format!("{indent}{glyph} {mark} {}", pane.msg());
        return format!("{}\n", truncate(&plain, width));
    }
    let avail = width.saturating_sub(prefix_vis);
    let role_ansi = |r: Role| -> &'static str { r.ansi() };
    // Glyph bold on non-idle (matches line 1); mark bold + the stronger dim
    // (vendor-neutral, heavier than the faint idle_text).
    let glyph_color = role_ansi(status.role());
    let cmd_color = if status == Status::Pending {
        role_ansi(Role::Attention).to_string()
    } else {
        dim_strong.to_string()
    };
    let activity = compose_activity(pane.msg(), pane.outcome(), avail, &cmd_color);
    // The glyph carries the status color (bold on non-idle, matching line 1); the
    // mark is the vendor-neutral stronger dim, always bold.
    let glyph_seg = Seg {
        color: glyph_color,
        bold: status != Status::Idle,
        text: glyph.to_string().into(),
    };
    let mark_seg = Seg::bold(dim_strong, mark.to_string());
    format!(
        "{}{} {} {}\n",
        child_prefix(tab_active, tab_status),
        glyph_seg,
        mark_seg,
        activity,
    )
}

/// The 2-column left prefix shared by child / detail lines: an accent spine
/// followed by a space when the tab is active, two plain spaces otherwise. The
/// spine hue tracks the tab's status (peach when waiting/error, mauve accent
/// otherwise), matching the line-1 spine in [`render_row`].
fn child_prefix(active: bool, tab_status: Status) -> String {
    if active {
        format!("{} ", Seg::new(spine_role(tab_status).ansi(), "▌"))
    } else {
        "  ".to_string()
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

/// Produce the identity header lines as raw (unpainted) `Line` values.
/// Returns 0 lines if `!opts.header` or rows is empty; 1 line (title only) in
/// Cards density; 2 lines (title + `═` rule) in all other densities.
/// `overflow` is computed by the caller and passed in.
fn render_header(rows: &[TabRow], opts: &RenderOpts, overflow: bool) -> Vec<Line> {
    if !opts.header || rows.is_empty() {
        return vec![];
    }
    let width = opts.width;
    let accent = Role::Accent.ansi();
    let count = if overflow {
        format!("{}▲", rows.len())
    } else {
        format!("·{}", rows.len())
    };
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
        "{}{}{}\n",
        Seg::new(accent, title_clamped),
        " ".repeat(gap),
        Seg::new(count_color, count_clamped),
    ));

    let mut lines = vec![Line {
        text: title_line,
        target: None,
        bg: LineBg::Rail,
    }];
    // Header line 2: rule across the full width — only in non-Cards densities.
    if opts.density != Density::Cards {
        lines.push(Line {
            text: format!("{}\n", Seg::new(accent, "═".repeat(width))),
            target: None,
            bg: LineBg::Rail,
        });
    }
    lines
}

/// Produce the idle-strip line as a raw (unpainted) `Line` value.
/// Returns 0 lines if `strip_folded == 0`; else 1 line tagged `LineBg::Rail`.
fn render_strip(strip_folded: usize, opts: &RenderOpts) -> Vec<Line> {
    if strip_folded == 0 {
        return vec![];
    }
    vec![Line {
        text: format!(
            "{}\n",
            Seg::new(
                Role::Accent.ansi(),
                truncate(&format!("+{} idle ▾", strip_folded), opts.width),
            ),
        ),
        target: None,
        bg: LineBg::Rail,
    }]
}

pub fn render_rail(rows: &[TabRow], opts: &RenderOpts) -> RenderedRail {
    if rows.is_empty() {
        return RenderedRail::from_lines(vec![]);
    }
    let width = opts.width;
    let cards = opts.density == Density::Cards;
    let rail = tc_bg(opts.theme.rail_bg);

    let blocks: Vec<Vec<Line>> = rows.iter().map(|r| render_row(r, opts)).collect();
    let metas: Vec<RowMeta> = rows.iter().zip(&blocks)
        .map(|(r, b)| RowMeta { status: r.display.status, full_lines: b.len() })
        .collect();
    let body_budget = opts
        .height
        .saturating_sub(header_lines(rows, opts.header, opts.density));
    let (plan, strip_folded, spacing) = plan_layout(&metas, body_budget, opts.density);
    let overflow = plan.len() < rows.len();

    let mut flat: Vec<Line> = Vec::new();

    // Header.
    for mut line in render_header(rows, opts, overflow) {
        if cards { line.text = paint_card_line(&line.text, width, &rail); }
        flat.push(line);
    }

    // Body: one card block per kept row.
    for &(i, budget) in &plan {
        let row = &rows[i];
        let row_target = target_for_row(row);

        // Resolve a raw line's surface through the one `LineBg::escape` map and
        // paint it (Cards only). The emitted line is final, so it carries
        // `LineBg::None`; its footprint is exactly the lines pushed here.
        let finalize = |bg: LineBg, text: String, target: Option<RailTarget>| -> Line {
            let text = match bg.escape(row, &opts.theme, &rail) {
                Some(esc) if cards => paint_card_line(&text, width, &esc),
                _ => text,
            };
            Line { text, target, bg: LineBg::None }
        };

        // pad_y internal top padding — belongs to this card's click span.
        for _ in 0..spacing.pad_y {
            flat.push(finalize(LineBg::Card, "\n".to_string(), Some(row_target)));
        }

        // content (truncated to the planned budget == today's compression).
        for line in blocks[i].iter().take(budget) {
            flat.push(finalize(line.bg, line.text.clone(), line.target));
        }

        // gap external separation (dark panel base in Cards).
        for _ in 0..spacing.gap {
            flat.push(finalize(LineBg::Rail, "\n".to_string(), None));
        }
    }

    // Idle strip.
    for mut line in render_strip(strip_folded, opts) {
        if cards { line.text = paint_card_line(&line.text, width, &rail); }
        flat.push(line);
    }

    RenderedRail::from_lines(flat)
}

#[cfg(test)]
fn render(rows: &[TabRow], opts: &RenderOpts) -> String {
    render_rail(rows, opts).ansi
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kind::Kind;
    use crate::rollup::{PrimaryDetail, ProgressCounts};

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

    // ── Surface-tint oracle ──────────────────────────────────────────────────
    //
    // The Cards renderer paints every row with one of four truecolor background
    // "surface" bands. Tests classify rows by *role* — never by literal hex — and
    // the escape strings are derived from the same `DerivedColors` the renderer
    // uses (`DerivedColors::default()`, which all Cards tests render against). So
    // a theme-color change touches exactly one place (`theme.rs`); the role names
    // here and in snapshots stay stable, and the oracle can never silently drift
    // from the renderer.

    /// One of the four background surface bands, in brightness order.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Surface {
        /// `surface_active` — the focused row; the only band brighter than bg.
        Active,
        /// `surface_agent` — a running (non-focused) agent row.
        Agent,
        /// `surface_idle` — an idle/plain row, barely above the panel.
        Idle,
        /// `rail_bg` — the dark panel base (header + gap rows).
        Rail,
        /// No truecolor background band at all.
        Bare,
    }

    /// The `\e[48;2;r;g;bm` background SGR a given color is emitted as.
    fn bg_sgr(rgb: (u8, u8, u8)) -> String {
        format!("\x1b[48;2;{};{};{}m", rgb.0, rgb.1, rgb.2)
    }

    /// Classify a rendered line by which band of `theme` paints it. Checked
    /// brightest-first so the dominant band wins when a line is mixed.
    fn surface_of_theme(line: &str, theme: &crate::theme::DerivedColors) -> Surface {
        if line.contains(&bg_sgr(theme.surface_active)) {
            Surface::Active
        } else if line.contains(&bg_sgr(theme.surface_agent)) {
            Surface::Agent
        } else if line.contains(&bg_sgr(theme.surface_idle)) {
            Surface::Idle
        } else if line.contains(&bg_sgr(theme.rail_bg)) {
            Surface::Rail
        } else {
            Surface::Bare
        }
    }

    /// Classify against the default (neutral-dark fallback) theme — the one all
    /// Cards tests render against unless they pass an explicit theme.
    fn surface_of(line: &str) -> Surface {
        surface_of_theme(line, &crate::theme::DerivedColors::default())
    }

    /// True iff the line carries any truecolor background band (any surface).
    fn is_painted(line: &str) -> bool {
        line.contains("\x1b[48;2;")
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
            outcome: None,
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
                        PaneDisplay::tracked(10, Kind::Claude, Status::Pending, "approve".into(), None),
                        PaneDisplay::tracked(11, Kind::Claude, Status::Running, "tests".into(), None),
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
    fn render_row_lines_by_state() {
        let opts = ro(40, 0);
        let mk_row = |d: TabDisplay, active: bool| TabRow {
            number: 1,
            name: "t".into(),
            active,
            has_bell: false,
            display: d,
        };
        let rl = |d: TabDisplay, active: bool| render_row(&mk_row(d, active), &opts).len();

        assert_eq!(rl(display(Status::Idle, 0, 0, None), false), 1);

        let detail = |status, msg: &str| {
            Some(PrimaryDetail {
                repo: "r".into(),
                branch: "b".into(),
                msg: msg.into(),
                kind: Kind::Claude,
                since_tick: 0,
                outcome: None,
                status,
            })
        };
        assert_eq!(
            rl(display(Status::Done, 1, 1, detail(Status::Done, "")), false),
            1
        );
        assert_eq!(
            rl(display(Status::Running, 1, 1, detail(Status::Running, "x")), false),
            2
        );
        assert_eq!(
            rl(display(Status::Error, 1, 1, detail(Status::Error, "x")), false),
            2
        );
        // Pending: no msg → 1 line (line 2 suppressed); with msg → 2 lines (mark + activity).
        // Old 3-line case (branch · needs you + quoted msg) is gone.
        assert_eq!(
            rl(display(Status::Pending, 1, 1, detail(Status::Pending, "")), false),
            1
        );
        assert_eq!(
            rl(display(Status::Pending, 1, 1, detail(Status::Pending, "go?")), false),
            2
        );
        // Running with no msg: only 1 line
        assert_eq!(
            rl(display(Status::Running, 1, 1, detail(Status::Running, "")), false),
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
            outcome: None,
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
                outcome: None,
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
            outcome: None,
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
            outcome: None,
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
        assert!(f0.contains('⠋'));
        assert!(f1.contains('⠙'));
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
            outcome: None,
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
            outcome: None,
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
            outcome: None,
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
            outcome: None,
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
            outcome: None,
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
            outcome: None,
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
            outcome: None,
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
            outcome: None,
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
            !is_painted(&s),
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
            outcome: None,
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
        // full_lines = 2 (mark+activity line present)
        assert_eq!(render_row(&rows[0], &ro(30, 0)).len(), 2);

        // Case 2: pending without msg → 1 line only, no line 2.
        let detail_no_msg = PrimaryDetail {
            repo: "proj".into(),
            branch: "fix".into(),
            msg: "".into(),
            since_tick: 0,
            outcome: None,
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
        // full_lines = 1 (no msg → no line 2)
        assert_eq!(render_row(&rows2[0], &ro(30, 0)).len(), 1);

        // Width constraint: pending detail line must not exceed width
        let detail_long = PrimaryDetail {
            repo: "averylongreponame".into(),
            branch: "feature/some-very-long-branch".into(),
            msg: "a very long question that should be truncated appropriately here".into(),
            since_tick: 0,
            outcome: None,
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
            outcome: None,
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
    fn onboarding_legend_covers_every_status() {
        // The const's length is already pinned to `Status::ALL.len()` at compile
        // time; this guards the complementary property — every variant appears
        // exactly once (no duplicate covering for a missing one). Adding a
        // `statuses!` row therefore forces a matching legend entry.
        for &want in Status::ALL {
            let hits = ONBOARDING_LEGEND.iter().filter(|(st, _)| *st == want).count();
            assert_eq!(hits, 1, "{want:?} must appear exactly once in the legend");
        }
    }

    #[test]
    fn onboarding_never_exceeds_width() {
        // The onboarding panel must honor the same width discipline as the rail
        // (which is proptested down to tiny widths). Includes degenerate widths
        // so the legend's fixed " glyph " prefix can't overflow either.
        for width in [1usize, 2, 3, 4, 8, 12, 20, 28] {
            let s = onboarding(&ro(width, 0));
            for line in s.ansi.lines() {
                assert!(
                    visible_len(line) <= width,
                    "onboarding line exceeds width {width}: {line:?} (visible {})",
                    visible_len(line)
                );
            }
        }
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
            outcome: None,
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
        PaneDisplay::tracked(id, kind, status, msg.into(), None)
    }

    /// Build a PaneDisplay carrying an end-result outcome, for tag tests.
    fn pe_outcome(id: u32, kind: Kind, status: Status, msg: &str, outcome: Outcome) -> PaneDisplay {
        PaneDisplay::tracked(id, kind, status, msg.into(), Some(outcome))
    }

    // ── End-result outcome tag rendering ──

    #[test]
    fn child_prefix_spine_when_active_two_spaces_when_not() {
        // Inactive: exactly two plain columns, no escape, no spine.
        assert_eq!(child_prefix(false, Status::Running), "  ");
        assert_eq!(child_prefix(false, Status::Error), "  ");
        // Active: an accent spine + trailing space, hue tracking the tab status
        // (mauve accent normally, peach attention when waiting/error) — the same
        // spine_role coupling as the line-1 bar.
        let running = child_prefix(true, Status::Running);
        assert!(running.starts_with(Role::Accent.ansi()), "accent spine: {running:?}");
        assert!(running.contains('▌') && running.ends_with(' '), "spine + space: {running:?}");
        assert!(child_prefix(true, Status::Error).starts_with(Role::Attention.ansi()));
        assert!(child_prefix(true, Status::Pending).starts_with(Role::Attention.ansi()));
    }

    #[test]
    fn compose_activity_reserves_outcome_against_truncation() {
        let cmd_color = "\x1b[2m"; // stand-in; we assert on visible text + role
        // Wide: command and full tag both intact.
        let wide = compose_activity("cargo build", Some(Outcome::Failed(Some(1))), 30, cmd_color);
        assert!(wide.contains("cargo build"), "command shown: {:?}", wide);
        assert!(wide.contains("(exit 1)"), "full tag shown: {:?}", wide);
        assert!(wide.contains(Role::Error.ansi()), "tag is red: {:?}", wide);

        // Narrow: command is squeezed but the outcome survives in full.
        let narrow = compose_activity(
            "cargo build integration suite",
            Some(Outcome::Failed(Some(1))),
            14,
            cmd_color,
        );
        assert!(narrow.contains("(exit 1)"), "tag must survive truncation: {:?}", narrow);
        assert!(
            narrow.contains('…') && !narrow.contains("integration"),
            "command is the part that truncates: {:?}",
            narrow
        );

        // Extreme: only the irreducible glyph fits; command is dropped entirely.
        let tiny = compose_activity("cargo build", Some(Outcome::Failed(Some(1))), 2, cmd_color);
        assert!(tiny.contains('✗'), "minimal glyph survives: {:?}", tiny);
        assert!(!tiny.contains("cargo"), "command dropped at extreme width: {:?}", tiny);

        // Width-safety across the whole range (incl. a wide exit code).
        for avail in 1..=30 {
            let s = compose_activity(
                "cargo build integration",
                Some(Outcome::Failed(Some(137))),
                avail,
                cmd_color,
            );
            assert!(
                visible_len(&s) <= avail,
                "compose_activity exceeds avail={}: {:?} (visible {})",
                avail,
                s,
                visible_len(&s)
            );
        }
    }

    #[test]
    fn finished_command_line2_shows_role_colored_tag() {
        let mk = |status, outcome, msg: &str| {
            let d = PrimaryDetail {
                repo: "r".into(),
                branch: "".into(),
                msg: msg.into(),
                since_tick: 0,
                status,
                kind: Kind::Build,
                outcome,
            };
            TabRow {
                number: 1,
                name: "web".into(),
                active: false,
                has_bell: false,
                display: display(status, 1, 1, Some(d)),
            }
        };
        let done = render(&[mk(Status::Done, Some(Outcome::Ok), "cargo build")], &ro(30, 0));
        let dline = done.lines().find(|l| l.contains("cargo build")).unwrap();
        assert!(dline.contains('✓') && dline.contains(Role::Success.ansi()), "done tag green ✓: {:?}", dline);

        let err = render(
            &[mk(Status::Error, Some(Outcome::Failed(Some(2))), "cargo build")],
            &ro(30, 0),
        );
        let eline = err.lines().find(|l| l.contains("cargo build")).unwrap();
        assert!(
            eline.contains("(exit 2)") && eline.contains(Role::Error.ansi()),
            "error tag red (exit 2): {:?}",
            eline
        );
    }

    #[test]
    fn multi_pane_finished_command_shows_outcome_tag() {
        let a = display_multi(vec![
            pe(1, Kind::Build, Status::Running, "cargo build"),
            pe_outcome(2, Kind::Test, Status::Done, "cargo test", Outcome::Ok),
        ]);
        let row = TabRow {
            number: 1,
            name: "ci".into(),
            active: false,
            has_bell: false,
            display: a,
        };
        let s = render(&[row], &ro(30, 0));
        let line = s.lines().find(|l| l.contains("cargo test")).unwrap();
        assert!(line.contains('✓') && line.contains(Role::Success.ansi()), "pane tag green ✓: {:?}", line);
    }

    #[test]
    fn nerd_set_renders_robot_mark_for_claude() {
        let d = PrimaryDetail {
            repo: "r".into(),
            branch: "b".into(),
            msg: "thinking".into(),
            since_tick: 0,
            status: Status::Running,
            kind: Kind::Claude,
            outcome: None,
        };
        let rows = vec![TabRow {
            number: 1,
            name: "agent".into(),
            active: false,
            has_bell: false,
            display: display(Status::Running, 0, 1, Some(d)),
        }];
        let opts = RenderOpts {
            glyphs: GlyphSet::Nerd,
            ..ro(30, 0)
        };
        let s = render(&rows, &opts);
        assert!(s.contains('\u{f06a9}'), "Nerd Claude mark (robot): {:?}", s);
        assert!(!s.contains('✳'), "plain mark must not appear in Nerd set: {:?}", s);
    }

    /// Build a multi-pane TabDisplay from per-pane entries. The header status is the
    /// most-urgent (highest-severity) member; done/total derive from the entries.
    fn display_multi(panes: Vec<PaneDisplay>) -> TabDisplay {
        let status = panes
            .iter()
            .filter_map(PaneDisplay::status)
            .max()
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
                outcome: None,
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
    fn multi_pane_render_row_counts_header_and_children() {
        // New design: 4 tracked panes → 1 header + 4 pane lines = 5 (regardless of active).
        let opts = ro(40, 0);
        let a = display_multi(vec![
            pe(1, Kind::Claude, Status::Pending, "run migration?"),
            pe(2, Kind::Claude, Status::Running, "x"),
            pe(3, Kind::Claude, Status::Running, "y"),
            pe(4, Kind::Claude, Status::Running, "z"),
        ]);
        let row_inactive = TabRow { number: 1, name: "t".into(), active: false, has_bell: false, display: a.clone() };
        let row_active = TabRow { number: 1, name: "t".into(), active: true, has_bell: false, display: a };
        assert_eq!(render_row(&row_inactive, &opts).len(), 5, "header + 4 pane lines");
        assert_eq!(render_row(&row_active, &opts).len(), 5, "same regardless of active");
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
        assert!(body[1].contains('⠋'), "pane1 shows running glyph: {:?}", body[1]);
        assert!(body[2].contains('⠋'), "pane2 shows running glyph: {:?}", body[2]);
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
                    outcome: None,
                    status: Status::Running,
                    kind: Kind::Codex,
                }),
                panes: vec![
                    PaneDisplay::tracked(1, Kind::Codex, Status::Running, "tests".into(), None),
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
        let opts = ro(30, 0);
        let row_check = TabRow { number: 1, name: "solo".into(), active: false, has_bell: false, display: a.clone() };
        assert_eq!(
            render_row(&row_check, &opts).len(),
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
            outcome: None,
            status: Status::Running,
        };
        let detail_pending = PrimaryDetail {
            repo: "urgent-proj".into(),
            branch: "fix/thing".into(),
            msg: "please review".into(),
            kind: Kind::Claude,
            since_tick: 0,
            outcome: None,
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
        let opts_check = ro(30, 0);
        assert_eq!(render_row(&rows[0], &opts_check).len(), 2);
        assert_eq!(render_row(&rows[1], &opts_check).len(), 2);
        assert_eq!(render_row(&rows[2], &opts_check).len(), 2);
        assert_eq!(render_row(&rows[3], &opts_check).len(), 2); // pending + msg = 2 (mark + activity)

        // body_budget = 5 (height 7, header 2)
        let body_budget = 5usize;
        let metas: Vec<RowMeta> = rows.iter().map(|r| RowMeta {
            status: r.display.status,
            full_lines: render_row(r, &opts_check).len(),
        }).collect();
        let (plan, strip_folded) = plan_overflow(&metas, body_budget);
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
            outcome: None,
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
        let opts_check = ro(24, 0);
        let metas: Vec<RowMeta> = rows.iter().map(|r| RowMeta {
            status: r.display.status,
            full_lines: render_row(r, &opts_check).len(),
        }).collect();
        let (plan, _) = plan_overflow(&metas, 1);
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
            outcome: None,
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
        let opts_check = ro(24, 0);
        let metas: Vec<RowMeta> = rows.iter().map(|r| RowMeta {
            status: r.display.status,
            full_lines: render_row(r, &opts_check).len(),
        }).collect();
        let (plan, strip, spacing) =
            plan_layout(&metas, height - 2, crate::config::Density::Comfortable);
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
        let opts_check = ro(24, 0);
        let metas: Vec<RowMeta> = rows.iter().map(|r| RowMeta {
            status: r.display.status,
            full_lines: render_row(r, &opts_check).len(),
        }).collect();
        // Even with very large budget, compact never adds gaps.
        let (_, _, spacing) = plan_layout(&metas, 100, crate::config::Density::Compact);
        assert_eq!(spacing.gap, 0, "Compact density must never produce gaps");
    }

    #[test]
    fn plan_layout_comfortable_gap_when_space_available() {
        // 2 idle rows, body_budget=10: 2 content + 2 gaps = 4 ≤ 10 → gap_used = 1.
        let rows: Vec<TabRow> = (1..=2).map(idle_row).collect();
        let opts_check = ro(24, 0);
        let metas: Vec<RowMeta> = rows.iter().map(|r| RowMeta {
            status: r.display.status,
            full_lines: render_row(r, &opts_check).len(),
        }).collect();
        let (_, _, spacing) = plan_layout(&metas, 10, crate::config::Density::Comfortable);
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
            outcome: None,
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
        // line 0 = header title (no rule in Cards) → painted with rail_bg.
        assert_eq!(
            surface_of(lines[0]),
            Surface::Rail,
            "header title line must carry the rail panel band: {:?}",
            lines[0]
        );
        assert!(
            lines[0].contains("RADAR"),
            "header title must read RADAR: {:?}",
            lines[0]
        );

        // A card surface is any painted band that is NOT the rail base.
        let is_card_surface = |line: &str| {
            matches!(
                surface_of(line),
                Surface::Idle | Surface::Agent | Surface::Active
            )
        };

        // line 1 = idle tab content → a card surface (NOT the rail base).
        assert!(
            is_card_surface(lines[1]),
            "idle content line must carry a card surface band, not rail: {:?}",
            lines[1]
        );

        // line 2 = idle card gap → painted with rail_bg (panel shows through).
        assert_eq!(
            surface_of(lines[2]),
            Surface::Rail,
            "idle card gap row must carry the rail panel band: {:?}",
            lines[2]
        );

        // line 3 = working tab line 1, line 4 = working detail.
        assert!(
            is_card_surface(lines[3]),
            "working tab line 1 must carry a card surface band: {:?}",
            lines[3]
        );
        assert!(
            is_card_surface(lines[4]),
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
            outcome: None,
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
            is_painted(content_line),
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
            outcome: None,
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
        // After a role-reset (\x1b[0m) the active surface band must be re-armed
        // immediately, so the focused card stays painted across token boundaries.
        let active = bg_sgr(crate::theme::DerivedColors::default().surface_active);
        let rearm = format!("\x1b[0m{active}");
        assert!(
            s.contains(&rearm),
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
            outcome: None,
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
            is_painted(&s),
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
            outcome: None,
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
            active.starts_with("▌⠋"),
            "active row must be '▌⠋…' (spine+glyph, no pad): {:?}",
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
        // Find the running pane line (active → has spine ▌, contains ⠋ and ✳).
        let pane_lines: Vec<String> = s
            .lines()
            .map(strip_ansi_local)
            .filter(|l| l.contains('⠋') && l.contains('✳'))
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
            outcome: None,
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
                !is_painted(&s),
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
            outcome: None,
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
        // The single footprint source: pad_y + full_lines + gap.
        let opts = ro(40, 0);
        let idle_row_val = TabRow { number: 1, name: "t".into(), active: false, has_bell: false, display: display(Status::Idle, 0, 0, None) };
        let full_lines = render_row(&idle_row_val, &opts).len();
        assert_eq!(full_lines, 1);
        // Cards: 0 pad_y + 1 content + 1 gap = 2.
        assert_eq!(
            card_block_lines(full_lines, card_spacing(crate::config::Density::Cards)),
            2
        );
        // Comfortable: 0 pad_y + 1 content + 1 gap = 2.
        assert_eq!(
            card_block_lines(full_lines, card_spacing(crate::config::Density::Comfortable)),
            2
        );
        // Compact: 0 + 1 + 0 = 1.
        assert_eq!(
            card_block_lines(full_lines, card_spacing(crate::config::Density::Compact)),
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
        assert_eq!(
            surface_of(content_row),
            Surface::Idle,
            "content row must carry the card's own idle surface tint, not rail: {:?}",
            content_row
        );
        assert!(
            content_row.contains("idle"),
            "content row must contain the tab name: {:?}",
            content_row
        );
        // The trailing gap row (line 2) carries the rail panel base.
        let gap_row = lines[2];
        assert_eq!(
            surface_of(gap_row),
            Surface::Rail,
            "gap row must carry the rail panel base, not the card surface tint: {:?}",
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

    /// Classify each rendered line for a tint-map snapshot by which surface band
    /// it carries, via the shared `surface_of` oracle (see "Surface-tint oracle"
    /// near the top of this module). The labels are stable across theme changes.
    /// Note: in Cards every emitted line is painted, so blank gaps carry the rail
    /// band rather than being empty — they classify as "rail", not "gap".
    fn tint_map(s: &str) -> String {
        tint_map_for(s, &crate::theme::DerivedColors::default())
    }

    /// Theme-parameterized tint map, so a light-theme render can be classified
    /// against the colors it was actually painted with.
    fn tint_map_for(s: &str, theme: &crate::theme::DerivedColors) -> String {
        s.lines()
            .map(|line| match surface_of_theme(line, theme) {
                Surface::Active => "active",
                Surface::Agent => "agent",
                Surface::Idle => "idle",
                Surface::Rail => "rail",
                Surface::Bare => {
                    if line.is_empty() {
                        "gap"
                    } else {
                        "bare"
                    }
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
            outcome: None,
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
        // Cards header is 1 line (no rule); each card emits content then a
        // trailing gap row (rail_bg — the panel shows through). Classifying every
        // row by surface band pins the full hierarchy in one assertion: idle <
        // agent < active, with the panel base (rail) under the header and gaps.
        let expected = [
            "rail",   // line 0  header
            "idle",   // line 1  idle content
            "rail",   // line 2  idle gap
            "agent",  // line 3  agent content line 1
            "agent",  // line 4  agent detail
            "rail",   // line 5  agent gap
            "active", // line 6  focus content line 1
            "active", // line 7  focus detail
            "rail",   // line 8  focus gap
        ]
        .join("\n");
        assert_eq!(
            tint_map(&s),
            expected,
            "per-row surface bands must encode idle<agent<active hierarchy;\nrender:\n{s:?}"
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
        // line 0 = header/rail, line 1 = tab parent, lines 2-3 = child panes.
        assert_eq!(
            surface_of(lines[1]),
            Surface::Active,
            "parent row must carry active tint: {:?}",
            lines[1]
        );
        for line in &lines[2..=3] {
            assert_eq!(
                surface_of(line),
                Surface::Agent,
                "child row must carry subordinate agent tint, not the active card tint: {:?}",
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
            outcome: None,
            status: Status::Running,
        };
        let pending = PrimaryDetail {
            repo: "api".into(),
            branch: "fix".into(),
            msg: "".into(),
            kind: Kind::Claude,
            since_tick: 0,
            outcome: None,
            status: Status::Pending,
        };
        let done = PrimaryDetail {
            repo: "worker".into(),
            branch: "".into(),
            msg: "".into(),
            kind: Kind::Claude,
            since_tick: 0,
            outcome: None,
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
        // SHAPE + CODE, not by hue. (Bold no longer distinguishes them: bold now
        // encodes *activity*, so every non-idle row — including error — is bold.)
        //   waiting → `\x1b[91m` (bright red) + ◆ glyph
        //   error   → `\x1b[31m` (red)        + ✗ glyph
        let pending = PrimaryDetail {
            repo: "pinky".into(),
            branch: "fix".into(),
            msg: "".into(),
            kind: Kind::Claude,
            since_tick: 0,
            outcome: None,
            status: Status::Pending,
        };
        let err = PrimaryDetail {
            repo: "infra".into(),
            branch: "".into(),
            msg: "boom".into(),
            kind: Kind::Claude,
            since_tick: 0,
            outcome: None,
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
        // Error is also bold now (activity cue); shape + code still distinguish it
        // from the waiting row.
        assert!(
            error_line.contains(BOLD),
            "error row is bold (non-idle activity cue): {:?}",
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
            outcome: None,
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
        // +1 row of headroom so a trailing newline (Cards/Comfortable emit a
        // trailing gap row) cannot scroll the " RADAR" title off the top. The
        // extra blank row is removed by the trailing-blank trim below.
        let height = (raw.lines().count().max(1) + 1) as u16;
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
            outcome: None,
            status: Status::Running,
        };
        let pending = PrimaryDetail {
            repo: "api".into(),
            branch: "fix".into(),
            msg: "".into(),
            kind: Kind::Claude,
            since_tick: 0,
            outcome: None,
            status: Status::Pending,
        };
        let done = PrimaryDetail {
            repo: "worker".into(),
            branch: "".into(),
            msg: "".into(),
            kind: Kind::Claude,
            since_tick: 0,
            outcome: None,
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

    // NOTE: the byte-exact raw-ANSI snapshot was retired. Its information is
    // covered at higher signal by two siblings — `canonical_cards_grid` pins the
    // visible text (vt100-rendered) and `canonical_tint_map` pins the per-row
    // surface bands semantically — while foreground role colors are asserted by
    // `render_glyph_role_colors_are_present` and the `*_role` tests. A raw escape
    // dump churned on every spacing/color tweak and carried a self-rewriting
    // `assertion_line:` header, so it cost diff noise without adding coverage.

    #[test]
    fn snapshot_canonical_tint_map() {
        let rows = scenario_canonical();
        let raw = render(
            &rows,
            &ro_full(30, 100, crate::config::Density::Cards, GlyphSet::Plain),
        );
        insta::assert_snapshot!("canonical_tint_map", tint_map(&raw));
    }

    // ── Widened snapshot matrix ──
    //
    // The proptests prove these states never panic and never exceed width; the
    // snapshots below pin *what they actually look like* as reviewable goldens.
    // Each scenario picks the oracle that carries the most signal for what it
    // exercises: `grid` for visible text/layout, `tint_map` for surface bands.

    /// Narrow width: long names/messages clamp (the branch path is dropped
    /// first, then names/messages ellipsize) while the painted band still fills
    /// the full column. Uses deliberately long content so the clamp is visible.
    #[test]
    fn snapshot_cards_narrow_width_grid() {
        let detail = PrimaryDetail {
            repo: "payments-service".into(),
            branch: "feature/long-branch".into(),
            msg: "refactoring the auth middleware".into(),
            kind: Kind::Claude,
            since_tick: 0,
            outcome: None,
            status: Status::Running,
        };
        let rows = vec![TabRow {
            number: 1,
            name: "payments-service".into(),
            active: true,
            has_bell: false,
            display: display(Status::Running, 0, 1, Some(detail)),
        }];
        let raw = render(
            &rows,
            &ro_full(16, 100, crate::config::Density::Cards, GlyphSet::Plain),
        );
        insta::assert_snapshot!("cards_narrow_width_grid", grid(&raw, 16));
    }

    /// Nerd glyph set: the visible status/identity marks swap to nerd-font
    /// glyphs. `grid` captures the substituted glyphs in the rendered text.
    #[test]
    fn snapshot_cards_nerd_glyphs_grid() {
        let rows = scenario_canonical();
        let raw = render(
            &rows,
            &ro_full(30, 100, crate::config::Density::Cards, GlyphSet::Nerd),
        );
        insta::assert_snapshot!("cards_nerd_glyphs_grid", grid(&raw, 30));
    }

    /// Height-constrained overflow: many idle tabs fold into a `+N idle ▾` strip
    /// and the header gains the `▲` overflow marker, while an urgent (pending)
    /// row is never folded. `grid` pins which rows survive and the strip copy.
    #[test]
    fn snapshot_overflow_fold_grid() {
        let mut rows: Vec<TabRow> = (1..=14).map(idle_row).collect();
        let urgent = PrimaryDetail {
            repo: "pinky".into(),
            branch: "fix".into(),
            msg: "approve?".into(),
            kind: Kind::Claude,
            since_tick: 0,
            outcome: None,
            status: Status::Pending,
        };
        rows.push(TabRow {
            number: 15,
            name: "pinky".into(),
            active: false,
            has_bell: false,
            display: display(Status::Pending, 0, 1, Some(urgent)),
        });
        let raw = render(
            &rows,
            &ro_full(30, 8, crate::config::Density::Compact, GlyphSet::Plain),
        );
        insta::assert_snapshot!("overflow_fold_grid", grid(&raw, 30));
    }

    /// Multi-pane tab tree (active, three panes with distinct statuses):
    /// `grid` pins the parent header + one child row per pane (running/pending/
    /// done glyphs), and `tint_map` pins the hierarchy — the active parent over
    /// subordinate (agent) child rows. (The `+N more` overflow path is covered by
    /// `cards_active_more_line_uses_active_child_surface_not_card_tint`.)
    #[test]
    fn snapshot_cards_multi_pane() {
        let panes: Vec<PaneDisplay> = vec![
            pe(1, Kind::Claude, Status::Running, "building"),
            pe(2, Kind::Codex, Status::Pending, "approve?"),
            pe(3, Kind::Test, Status::Done, "cargo test"),
        ];
        let row = TabRow {
            number: 1,
            name: "team".into(),
            active: true,
            has_bell: false,
            display: display_multi(panes),
        };
        let raw = render(&[row], &ro_full(30, 100, crate::config::Density::Cards, GlyphSet::Plain));
        insta::assert_snapshot!("cards_multi_pane_grid", grid(&raw, 30));
        insta::assert_snapshot!("cards_multi_pane_tint", tint_map(&raw));
    }

    /// Light terminal theme: the visible text is theme-independent (so `grid`
    /// matches the dark canonical layout), but the surface ladder derives
    /// different colors. The tint map — computed against the *light* theme it was
    /// painted with — must still order idle < agent < active just like dark.
    #[test]
    fn snapshot_cards_light_theme() {
        let light = crate::theme::DerivedColors::from_bg_fg((250, 250, 250), (40, 40, 40));
        let rows = scenario_canonical();
        let opts = RenderOpts {
            theme: light.clone(),
            ..ro_full(30, 100, crate::config::Density::Cards, GlyphSet::Plain)
        };
        let raw = render(&rows, &opts);
        insta::assert_snapshot!("cards_light_theme_grid", grid(&raw, 30));
        insta::assert_snapshot!("cards_light_theme_tint", tint_map_for(&raw, &light));
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
                outcome: None,
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
        assert!(is_painted(&s), "expected 24-bit background SGR");
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
            outcome: None,
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
        // Draw from the `statuses!` table so a new variant is exercised by every
        // property test using `arb_status` automatically — no ladder to update.
        fn arb_status()(s in proptest::sample::select(Status::ALL.to_vec())) -> Status {
            s
        }
    }

    prop_compose! {
        fn arb_kind()(n in 0u8..9) -> Kind {
            match n {
                0 => Kind::Claude,
                1 => Kind::Codex,
                2 => Kind::Gemini,
                3 => Kind::Command,
                4 => Kind::Other,
                5 => Kind::Test,
                6 => Kind::Build,
                7 => Kind::Deploy,
                _ => Kind::Server,
            }
        }
    }

    prop_compose! {
        /// An arbitrary pane: ~15% untracked, else a tracked pane with an
        /// arbitrary Kind/Status and a short / long / CJK message so truncation,
        /// wide-glyph width, and the narrow-width plain fallback all get hit.
        fn arb_pane()(
            id in 1u32..100,
            kind in arb_kind(),
            status in arb_status(),
            untracked in 0u8..100,
            msg_pick in 0u8..3,
        ) -> PaneDisplay {
            if untracked < 15 {
                PaneDisplay::untracked(id, "term")
            } else {
                let msg = match msg_pick {
                    0 => "ok",
                    1 => "running a fairly long migration command across the cluster now",
                    _ => "日本語のメッセージ表示テスト中です", // CJK wide glyphs
                };
                PaneDisplay::tracked(id, kind, status, msg.to_string(), None)
            }
        }
    }

    prop_compose! {
        fn arb_row()(
            status in arb_status(),
            name in "[a-zA-Z0-9_-]{0,20}",
            active in any::<bool>(),
            total in 0usize..4,
            panes in proptest::collection::vec(arb_pane(), 0..=8),
        ) -> TabRow {
            // With >1 tracked pane, render as a multi-pane row (line-per-pane +
            // `+N more`); otherwise keep the single-/zero-pane chunk-1 shape so
            // existing invariants stay exercised in their original form.
            let tracked = panes.iter().filter(|p| p.is_tracked()).count();
            let display = if tracked > 1 {
                display_multi(panes)
            } else {
                let detail = if total > 0 {
                    Some(PrimaryDetail {
                        repo: "r".into(),
                        branch: "".into(),
                        msg: "m".into(),
                        kind: Kind::Claude,
                        since_tick: 0,
                        outcome: None,
                        status,
                    })
                } else {
                    None
                };
                display(status, 0, total, detail)
            };
            TabRow {
                number: 1,
                name,
                active,
                has_bell: false,
                display,
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
        /// Multi-pane + narrow-width fuzz: with rows carrying arbitrary panes
        /// (tracked/untracked, every Kind/Status, short/long/CJK messages) the
        /// rail must (1) never panic and (2) keep every ANSI-stripped emitted
        /// line within `width` — including the extreme-narrow band where the
        /// per-pane prefix and `+N more` line take the plain clamped fallback.
        #[test]
        fn render_multi_pane_never_overflows_width(
            rows in arb_rows(),
            width in 4usize..=120,
            height in 1usize..=60,
        ) {
            for (density, glyphs) in [
                (Density::Compact, GlyphSet::Plain),
                (Density::Comfortable, GlyphSet::Plain),
                (Density::Cards, GlyphSet::Nerd),
            ] {
                let opts = ro_full(width, height, density, glyphs);
                let s = render(&rows, &opts); // must not panic
                for line in s.lines() {
                    prop_assert!(
                        visible_width(line) <= width,
                        "line width {} > {} (density {:?}, glyphs {:?}): {:?}",
                        visible_width(line),
                        width,
                        density,
                        glyphs,
                        line
                    );
                }
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

    prop_compose! {
        /// Like arb_row but also produces multi-pane rows with >6 panes so the
        /// `+N more` overflow path and every density mode are exercised.
        fn arb_tab_row()(
            status in arb_status(),
            name in "[a-zA-Z0-9_-]{0,20}",
            active in any::<bool>(),
            n_panes in 0usize..9,
        ) -> TabRow {
            let display = if n_panes == 0 {
                display(status, 0, 0, None)
            } else {
                let panes: Vec<PaneDisplay> = (1u32..=(n_panes as u32))
                    .map(|id| pe(id, Kind::Claude, status, "m"))
                    .collect();
                display_multi(panes)
            };
            TabRow { number: 1, name, active, has_bell: false, display }
        }
    }

    proptest! {
        /// Structural lockstep holds across all densities and input shapes:
        /// `ansi` line count == `line_count()` (== targets.len()) always.
        #[test]
        fn lockstep_holds_for_arbitrary_rails(
            rows in prop::collection::vec(arb_tab_row(), 0..8),
            width in 8usize..40,
            height in 1usize..30,
            density in prop_oneof![
                Just(Density::Compact),
                Just(Density::Comfortable),
                Just(Density::Cards),
            ],
        ) {
            let opts = RenderOpts { width, height, density, ..ro(width, 0) };
            let rr = render_rail(&rows, &opts);
            let ansi_lines = if rr.ansi.is_empty() { 0 } else { rr.ansi.split('\n').count() };
            prop_assert_eq!(ansi_lines, rr.line_count());
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
        let opts = ro(40, 0);
        let a = display_multi(vec![
            pe(10, Kind::Claude, Status::Pending, "approve?"),
            pe(11, Kind::Claude, Status::Running, "building"),
            pe(12, Kind::Claude, Status::Running, "testing"),
        ]);
        let row = TabRow { number: 1, name: "t".into(), active: false, has_bell: false, display: a };
        assert_eq!(render_row(&row, &opts).len(), 4, "header + 3 pane lines");
    }

    #[test]
    fn single_running_pane_with_detail_is_two_content_lines() {
        // Single-pane Running tab with a non-empty detail msg → 2 content lines
        // (name row + detail row). Mirrors the row_lines assertion from
        // lib.rs::click_mapping_cards_pad_y_and_post_content_row.
        let opts = ro(40, 0);
        let a = display_multi(vec![pe(10, Kind::Claude, Status::Running, "msg")]);
        let row = TabRow { number: 1, name: "t".into(), active: false, has_bell: false, display: a };
        assert_eq!(render_row(&row, &opts).len(), 2, "tab 0 should be 2 content lines");
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

    /// Regression guard: the `+N more` summary line in an active multi-pane tab
    /// (Cards density, >6 tracked panes) must carry the active-child surface
    /// (`surface_agent` = `\x1b[48;2;24;25;35m`), NOT the card-tint (`surface_active`).
    ///
    /// Before the fix, `render_row` tagged this line `LineBg::Card`, which resolved
    /// to `surface_active` (56,59,71) — a byte change vs the pre-refactor behaviour.
    /// After the fix, it is tagged `LineBg::ActiveChild`, matching all other active
    /// child pane lines.
    #[test]
    fn cards_active_more_line_uses_active_child_surface_not_card_tint() {
        // Build an active multi-pane tab with 8 tracked panes (>6 → emits +2 more).
        let panes: Vec<PaneDisplay> = (1u32..=8)
            .map(|id| pe(id, Kind::Claude, Status::Running, "working"))
            .collect();
        let row = TabRow {
            number: 1,
            name: "team".into(),
            active: true,
            has_bell: false,
            display: display_multi(panes),
        };
        let s = render(&[row], &ro_cards(30, 100));

        // All pane child lines in an active multi-pane tab use the subordinate
        // agent surface — including the "+N more" line, NOT the brighter card
        // (active) header tint.
        // Find the "+2 more" line.
        let more_line = s.lines().find(|l| l.contains("more"))
            .expect("'+N more' summary line must be emitted when >6 panes");

        // The +more line must carry the active-child (agent) surface, not the card tint.
        assert_eq!(
            surface_of(more_line),
            Surface::Agent,
            "+more line must carry the active-child (agent) surface, not the card-header tint: {:?}",
            more_line
        );

        // Confirm a regular pane child line also carries the agent surface,
        // proving the +more line is consistent with its siblings.
        let child_lines: Vec<&str> = s.lines()
            .filter(|l| surface_of(l) == Surface::Agent && !l.contains("more"))
            .collect();
        assert!(
            !child_lines.is_empty(),
            "there must be at least one visible pane child line carrying the agent surface: {:?}", s
        );
    }

    #[test]
    fn line_bg_escape_is_the_one_home_for_the_surface_map() {
        let theme = DerivedColors::default();
        let rail = tc_bg(theme.rail_bg);
        let active_row = TabRow {
            number: 1,
            name: "a".into(),
            active: true,
            has_bell: false,
            display: display(Status::Running, 0, 1, None),
        };

        // Each class resolves to exactly the surface the old inline logic used —
        // asserted against the existing helpers, not hard-coded RGB.
        assert_eq!(LineBg::None.escape(&active_row, &theme, &rail), None);
        assert_eq!(LineBg::Rail.escape(&active_row, &theme, &rail), Some(rail.clone()));
        assert_eq!(
            LineBg::Card.escape(&active_row, &theme, &rail),
            Some(card_tint(&active_row, &theme)),
        );
        assert_eq!(
            LineBg::ActiveChild.escape(&active_row, &theme, &rail),
            Some(tc_bg(theme.surface_agent)),
        );
        // The drift the `cards_active_more_line_*` regression guards: on an active
        // row a child line (ActiveChild → surface_agent) must NOT resolve to the
        // card tint (surface_active). One resolver makes that structural.
        assert_ne!(
            LineBg::ActiveChild.escape(&active_row, &theme, &rail),
            LineBg::Card.escape(&active_row, &theme, &rail),
        );
    }

    #[test]
    fn seg_is_always_reset_terminated() {
        // color + text + RESET; bold inserts BOLD after the color.
        assert_eq!(Seg::new("\x1b[31m", "hi").to_string(), "\x1b[31mhi\x1b[0m");
        assert_eq!(
            Seg::bold("\x1b[31m", "hi").to_string(),
            "\x1b[31m\x1b[1mhi\x1b[0m"
        );
        // The structural guarantee `paint_card_line`'s bg re-arm depends on: a
        // colored run can never escape un-RESET, whatever the color or content.
        assert!(Seg::new("\x1b[35m", "▌").to_string().ends_with("\x1b[0m"));
        assert!(Seg::bold("\x1b[38;2;1;2;3m", "x y z").to_string().ends_with("\x1b[0m"));
    }

    #[test]
    fn needs_permission_face_is_distinct_and_actionable() {
        let opts = ro(24, 0); // existing test helper: RenderOpts at width 24
        let onboard = onboarding(&opts).ansi;
        let needs = needs_permission(&opts).ansi;
        assert_ne!(needs, onboard, "permission face must differ from idle onboarding");
        // The searched substrings ("press y", "permission") contain no characters that
        // appear in SGR escape sequences (`\x1b`, `[`, digits, `;`, `m`), so a plain
        // `contains` on the raw ANSI string is valid without stripping SGR first.
        let plain: String = needs.chars().collect();
        assert!(plain.contains("press y"), "must tell the user to press y:\n{needs}");
        assert!(plain.to_lowercase().contains("permission"), "must mention permission");
    }
}
