//! Pure renderer: per-tab rows → ANSI string. No zellij-tile dependency.

use crate::config::Density;
use crate::rollup::{Outcome, PaneDisplay, TabDisplay};
pub use crate::status::GlyphSet;
use crate::status::{Role, Status};
use crate::theme::DerivedColors;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

mod layout;
// Layout planning (overflow folding, card spacing, multi-pane expansion) is
// implementation *behind* the rail seam — only `render.rs` drives it. Import it
// privately here rather than re-exporting crate-wide so the planning
// intermediates (`RowMeta`, `plan_layout`, …) can't leak into new callers.
use layout::*;

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
/// never mistaken for a working one. Points at the `Ctrl-y` keybind (baked into
/// the owned `config.kdl`) rather than "press y here": the borderless rail can't
/// host Zellij's prompt legibly (Zellij #4749), and on an attached session no
/// onboarding float was ever opened — `Ctrl-y` summons that legible float from
/// any session state.
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
    line(&mut out, muted, " press Ctrl-y to", w);
    line(&mut out, muted, " open the grant", w);
    line(&mut out, muted, " prompt.", w);
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

    // ── Multi-pane line-per-pane tree (new design) ────────────────────────────
    // A tab with >1 tracked pane renders as: header (line 1, above) + one line
    // per tracked pane (in position order), up to MAX_PANE_LINES, joined by tree
    // connectors (`├` for every child that has a sibling/`+N more` below it, `└`
    // for the last visible child); if more exist, a final `+N more` line is the
    // `└`. No collapse — the tree is purely a visual affordance for "these panes
    // belong to the tab above."
    if is_multi_pane(&row.display) {
        const MAX_PANE_LINES: usize = 6;
        let tracked_panes: Vec<&PaneDisplay> = row.display.panes.iter()
            .filter(|p| p.is_tracked())
            .collect();
        let total_tracked = tracked_panes.len();
        let show = total_tracked.min(MAX_PANE_LINES);
        let remaining = total_tracked - show;

        for (i, pane) in tracked_panes.iter().take(show).enumerate() {
            // The final pane line is the `└` only when no `+N more` line follows
            // it; otherwise that trailing line carries the elbow.
            let branch = if i + 1 == show && remaining == 0 {
                Branch::Elbow
            } else {
                Branch::Tee
            };
            let text = emit_pane_line(pane, opts, row.active, st, &dim_strong, &idle_color, branch);
            lines.push(Line {
                text,
                target: Some(RailTarget { tab_position: tab_target.tab_position, pane_id: Some(pane.pane_id()) }),
                bg: if row.active { LineBg::ActiveChild } else { LineBg::Card },
            });
        }

        if remaining > 0 {
            let more_text = format!("+{} more", remaining);
            // The prefix is a 3-col tree prefix (spine/space + connector + space),
            // so reserve those columns before clamping the text to avoid overflow.
            let clamped = truncate(&more_text, opts.width.saturating_sub(3));
            let text = format!(
                "{}{}\n",
                child_prefix(row.active, st, Branch::Elbow, &idle_color),
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

/// Emit one pane line in the line-per-pane / tree design:
/// Inactive: ` {connector} {glyph} {mark} {msg}` (space + `├`/`└` + space)
/// Active:   `▌{connector} {glyph} {mark} {msg}` (spine + `├`/`└` + space)
fn emit_pane_line(
    pane: &PaneDisplay,
    opts: &RenderOpts,
    tab_active: bool,
    tab_status: Status,
    dim_strong: &str,
    conn_color: &str,
    branch: Branch,
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
    // Prefix: 3 cols (spine/space + connector + space) + glyph + 1 space + mark + 1 space
    let prefix_vis = 3 + glyph_w + 1 + mark_w + 1;
    // Narrow-width fallback: the colored path always emits the full fixed prefix
    // (spine/connector/indent + glyph + mark + spaces) unconditionally, so at widths
    // below it the line would overflow. Below that floor, drop all color and emit a
    // single plain line clamped to `width` so nothing exceeds the band.
    if width < prefix_vis {
        let spine = if tab_active { "▌" } else { " " };
        let plain = format!("{spine}{} {glyph} {mark} {}", branch.glyph(), pane.msg());
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
        child_prefix(tab_active, tab_status, branch, conn_color),
        glyph_seg,
        mark_seg,
        activity,
    )
}

/// Which tree connector a multi-pane child line draws at column 1.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Branch {
    /// `├` — a child that has more siblings (or a `+N more` line) below it.
    Tee,
    /// `└` — the last *visible* child under the tab (the `+N more` line when
    /// present, otherwise the final pane line).
    Elbow,
}

impl Branch {
    fn glyph(self) -> &'static str {
        match self {
            Branch::Tee => "├",
            Branch::Elbow => "└",
        }
    }
}

/// The 3-column left prefix shared by multi-pane child / `+N more` lines:
///   col 0  — active-tab spine `▌` (status-hued: peach when waiting/error,
///            mauve accent otherwise) or a plain space when inactive;
///   col 1  — the tree connector (`├`/`└`), in the muted `conn_color`;
///   col 2  — a separating space before the glyph.
///
/// Holding the connector at a fixed column (1) whether or not the tab is active
/// keeps the glyph aligned at column 3 across all child lines, so the per-line
/// truncation budget is constant (`prefix_vis` in [`emit_pane_line`]).
fn child_prefix(active: bool, tab_status: Status, branch: Branch, conn_color: &str) -> String {
    let spine = if active {
        Seg::new(spine_role(tab_status).ansi(), "▌").to_string()
    } else {
        " ".to_string()
    };
    format!("{}{} ", spine, Seg::new(conn_color, branch.glyph()))
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
mod tests;
