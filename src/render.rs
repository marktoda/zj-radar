//! Pure renderer: per-tab rows → ANSI string. No zellij-tile dependency.

use crate::config::Density;
use crate::model::TabAgg;
use crate::status::{Role, Status};
use crate::theme::DerivedColors;
pub use crate::status::GlyphSet;
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
    /// Theme-derived colors for card surfaces and readable detail text.
    /// Defaults to Catppuccin Mocha values before the first ModeUpdate.
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

/// Single source of truth for how many lines a tab row occupies.
pub fn row_lines(agg: &TabAgg) -> usize {
    let base = match agg.status {
        Status::Idle | Status::Done => 1,
        Status::Running | Status::Error => {
            if agg.detail.is_some() { 2 } else { 1 }
        }
        Status::Pending => match &agg.detail {
            Some(d) if !d.msg.trim().is_empty() => 3,
            Some(_) => 2,
            None => 1,
        },
    };
    // Add one extra line for the per-member roster when the tab has >1 agent,
    // the overall status is active (not Idle), and the roster is non-empty.
    // Must match the three-part guard in render() exactly.
    if agg.total > 1 && agg.status.is_active() && !agg.roster.is_empty() { base + 1 } else { base }
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
        Status::Error => if width < 16 { "err".to_string() } else { "failed".to_string() },
    }
}

/// The rail's identity header. Single source of truth for the header's vertical
/// span (consumed by click mapping in lib.rs). Only the truly-empty case (no
/// rows at all) is headerless; when `header` is false the identity block is
/// suppressed and rows start at line 0.
///
/// In Cards density the carded hero is just the " RADAR …" title (1 line) — the
/// `═` rule is dropped so cards begin immediately under the title. Compact and
/// Comfortable keep the two-line title+rule header. `render()` and the click
/// mapping both consult this function, so the count stays in lockstep.
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
    let total: usize = rows.iter().map(|r| row_lines(&r.agg)).sum();
    if total <= body_budget {
        // Everything fits at full fidelity.
        let plan = rows.iter().enumerate()
            .map(|(i, r)| (i, row_lines(&r.agg)))
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
        .map(|&i| (i, row_lines(&rows[i].agg)))
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
        if *lines <= 1 { continue; }
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
            if *lines <= 1 { continue; }
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

/// Single source of truth for the layout plan consumed by BOTH `render()` and
/// `tab_position_at_line()`. Returns:
///   - the per-row planned content-line counts (same as `plan_overflow`),
///   - the number of idle rows folded into the strip (`strip_folded`), and
///   - `gap_used`: 0 (no blank lines) or 1 (one blank line after each kept tab).
///
/// Gap rule:
///   - If `density == Compact`, `gap_used` is always 0.
///   - Otherwise, gaps are included only when ALL of them still fit within
///     `body_budget` (after accounting for content lines and the strip line).
///     If even one gap would overflow, `gap_used` falls back to 0 (flush
///     spacing), letting T3+ overflow compression handle the rest.
pub fn plan_layout(
    rows: &[TabRow],
    body_budget: usize,
    density: Density,
) -> (Vec<(usize, usize)>, usize, usize) {
    let (plan, strip_folded) = plan_overflow(rows, body_budget);
    let gap_used = if density == Density::Compact {
        0
    } else {
        let content_total: usize = plan.iter().map(|(_, l)| l).sum();
        let kept_count = plan.len();
        let strip_line = if strip_folded > 0 { 1 } else { 0 };
        // Include gaps only when content + one-gap-per-tab + strip all fit.
        if content_total + kept_count + strip_line <= body_budget {
            1
        } else {
            0
        }
    };
    (plan, strip_folded, gap_used)
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
            st.role().ansi(), st.glyph_for(g), RESET, muted, label, RESET
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
fn render_row(out: &mut String, row: &TabRow, opts: &RenderOpts, max_lines: usize) {
    let width = opts.width;
    let now_tick = opts.now_tick;
    let st = row.agg.status;
    let cards = opts.density == Density::Cards;

    // In Cards density the label/slot/spine use vivid theme-derived truecolor
    // hues (peach attention, red error, yellow working, green success, mauve
    // accent) so each row reads in the theme's own colors; other densities keep
    // the ANSI-16 role codes (Compact/Comfortable are intentionally unchanged).
    // The KEY outcome: a waiting "needs you" row renders PEACH, not red.
    let hue = |r: Role| -> String {
        if cards {
            let rgb = match r {
                Role::Attention => opts.theme.attention,
                Role::Error => opts.theme.error,
                Role::Working => opts.theme.working,
                Role::Success => opts.theme.success,
                Role::Accent => opts.theme.accent,
                Role::Muted => opts.theme.idle_text,
            };
            tc_fg(rgb)
        } else {
            r.ansi().to_string()
        }
    };

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

    // Internal left padding (Cards only): one pad space after the col-0
    // spine/space, before the glyph, so content isn't flush to the band edge.
    let pad = if cards { " " } else { "" };
    let pad_len = pad.len(); // ASCII space → 1 display col when present.

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
        bar, pad, label_color, label_bold, glyph_char, num, name, RESET, " ".repeat(gap), bell, slot_styled
    ));

    // Line 1 done. Emit detail/roster only within the remaining budget.
    if max_lines <= 1 {
        return;
    }
    let mut emitted = 1usize;

    // Theme-derived detail text colors: dim_strong for location/branch lines,
    // dim_weak for quoted message lines. Both are truecolor foreground escapes
    // derived from the bg/fg palette blend so they adapt to any Zellij theme.
    let dim_strong = tc_fg(opts.theme.dim_strong);
    let dim_weak = tc_fg(opts.theme.dim_weak);

    // Determine per-status detail line order.
    // For Pending: priority 1 = "branch · needs you"; priority 2 = "msg"; priority 3 = roster.
    // For Error: priority 1 = "loc · msg"; priority 2 = roster (no separate msg line).
    // For Running: priority 1 = "loc ⠋ msg"; priority 2 = roster.
    // For Done/Idle: no detail lines.
    if let Some(d) = &row.agg.detail {
        match st {
            Status::Running => {
                // Detail line (priority 1 after line 1).
                if emitted < max_lines {
                    let spin = crate::status::msg_spin(now_tick as usize);
                    let avail = width.saturating_sub(3);
                    let full = {
                        let loc = if d.branch.is_empty() { d.repo.clone() } else { format!("{}/{}", d.repo, d.branch) };
                        format!("{} {} {}", loc, spin, d.msg)
                    };
                    let body = if UnicodeWidthStr::width(full.as_str()) <= avail {
                        full
                    } else {
                        // drop branch
                        let no_branch = format!("{} {} {}", d.repo, spin, d.msg);
                        if UnicodeWidthStr::width(no_branch.as_str()) <= avail {
                            no_branch
                        } else {
                            // drop message, keep repo + spinner
                            truncate(&format!("{} {}", d.repo, spin), avail)
                        }
                    };
                    out.push_str(&format!("   {}{}{}\n", dim_strong, truncate(&body, avail), RESET));
                    emitted += 1;
                }
            }
            Status::Error => {
                // Detail line (priority 1 after line 1).
                if emitted < max_lines {
                    let loc = if d.branch.is_empty() { d.repo.clone() } else { format!("{}/{}", d.repo, d.branch) };
                    let body = if d.msg.trim().is_empty() { loc } else { format!("{} · {}", loc, d.msg) };
                    out.push_str(&format!("   {}{}{}\n", dim_strong, truncate(&body, width.saturating_sub(3)), RESET));
                    emitted += 1;
                }
            }
            Status::Pending => {
                // Priority 1: "branch · needs you" line.
                if emitted < max_lines {
                    let loc = if d.branch.is_empty() { d.repo.clone() } else { d.branch.clone() };
                    let needs_phrase = if row.agg.pending > 1 {
                        format!("{} needs you", row.agg.pending)
                    } else {
                        "needs you".to_string()
                    };
                    let phrase_len = UnicodeWidthStr::width(needs_phrase.as_str());
                    let loc_budget = width.saturating_sub(3 + 3 + phrase_len);
                    let loc_str = truncate(&loc, loc_budget);
                    let visible_content = format!("   {} · {}", loc_str, needs_phrase);
                    let visible_len = UnicodeWidthStr::width(visible_content.as_str());
                    if visible_len <= width {
                        out.push_str(&format!("   {}{} · {}{}{}\n", dim_strong, loc_str, hue(Role::Attention), needs_phrase, RESET));
                    } else {
                        let clamped = truncate(&visible_content, width);
                        out.push_str(&format!("{}{}{}\n", dim_strong, clamped, RESET));
                    }
                    emitted += 1;
                }
                // Priority 2: quoted msg line (only if non-empty).
                if emitted < max_lines && !d.msg.trim().is_empty() {
                    out.push_str(&format!("   {}\"{}\"{}\n", dim_weak, truncate(&d.msg, width.saturating_sub(5)), RESET));
                    emitted += 1;
                }
            }
            Status::Done | Status::Idle => {}
        }
    }

    // Roster line: one extra line for multi-agent active tabs showing each
    // member's status glyph colored by its role. This is the lowest-priority
    // line and is only emitted when we still have budget.
    if emitted < max_lines
        && row.agg.total > 1
        && row.agg.status.is_active()
        && !row.agg.roster.is_empty()
    {
        let indent = "   ";
        let indent_width = 3usize;
        // Build glyph tokens: "<color><glyph><reset>", measure visible width.
        let tokens: Vec<(String, usize)> = row.agg.roster.iter().map(|&rst| {
            let glyph = rst.glyph_for(opts.glyphs);
            let token = format!("{}{}{}", rst.role().ansi(), glyph, RESET);
            let vis = UnicodeWidthChar::width(glyph).unwrap_or(1);
            (token, vis)
        }).collect();
        // Build the visible line, dropping trailing glyphs that would overflow.
        // Layout: indent(3) + glyph(w) + " "(1) + glyph(w) + ...
        let max_vis = width.saturating_sub(indent_width);
        let mut parts: Vec<&str> = Vec::new();
        let mut vis_used = 0usize;
        for (idx, (tok, vis)) in tokens.iter().enumerate() {
            let needed = if idx == 0 { *vis } else { 1 + vis }; // space separator
            if vis_used + needed > max_vis {
                break;
            }
            parts.push(tok.as_str());
            vis_used += needed;
        }
        // Always emit the roster physical line when it is within budget, even
        // at very narrow widths where no glyphs fit (parts.is_empty()). This
        // keeps the emitted line count in lockstep with row_lines(), which
        // counts the roster unconditionally. Without this, render() emits one
        // fewer line than tab_position_at_line() budgets, causing all rows
        // below a narrow-width multi-agent active row to map one line too high.
        //
        // Do NOT run the ANSI-colored `joined` string through `truncate()`:
        // the glyph-fitting loop above already guarantees `vis_used <= max_vis
        // = width - indent_width`, so the content fits. We only need to
        // width-clamp the plain-text indent (at extreme narrow widths where
        // even the 3-space indent overflows).
        let safe_indent = &indent[..indent.len().min(width)];
        if parts.is_empty() {
            out.push_str(&format!("{}\n", safe_indent));
        } else {
            out.push_str(&format!("{}{}\n", safe_indent, parts.join(" ")));
        }
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

pub fn render(rows: &[TabRow], opts: &RenderOpts) -> String {
    let mut out = String::new();
    if rows.is_empty() {
        return out;
    }
    let width = opts.width;
    let accent = Role::Accent.ansi();
    let cards = opts.density == Density::Cards;
    // In Cards density the whole sidebar is a cohesive dark panel: the header,
    // gaps and idle strip all sit on `rail_bg`; only card content lines carry
    // their (subtle, ladder-derived) surface tint.
    let rail = tc_bg(opts.theme.rail_bg);

    let body_budget = opts.height.saturating_sub(header_lines(rows, opts.header, opts.density));
    let (plan, strip_folded, gap_used) = plan_layout(rows, body_budget, opts.density);
    // Overflow = any row is absent from the plan (those are idle-folded rows).
    let overflow = plan.len() < rows.len();
    // Right-aligned count: total tabs (·N, or "N ▲" when overflowing), plus a
    // "·P!" urgent marker in the attention role when any tab needs you.
    let count = if overflow {
        format!("{} ▲", rows.len())
    } else {
        format!("·{}", rows.len())
    };
    let pending = rows.iter().filter(|r| r.agg.status == Status::Pending).count();
    let urgent = if pending > 0 { format!(" ·{}!", pending) } else { String::new() };

    // Emit the identity header block only when configured on (and rows exist).
    // Header line 1: " RADAR" + right-aligned count (+ urgent marker).
    if opts.header {
        let title = " RADAR";
        let right_w = UnicodeWidthStr::width(count.as_str()) + UnicodeWidthStr::width(urgent.as_str());
        let gap = width
            .saturating_sub(UnicodeWidthStr::width(title) + right_w)
            .max(1);
        // Title in accent; total count muted (accent when overflowing, so the
        // ▲ marker stays loud); urgent marker in the attention role.
        let count_color = if overflow { accent } else { Role::Muted.ansi() };
        let mut title_line = String::new();
        title_line.push_str(&format!(
            "{}{}{}{}{}{}{}",
            accent, title, RESET, " ".repeat(gap), count_color, count, RESET
        ));
        if pending > 0 {
            title_line.push_str(&format!("{}{}{}", Role::Attention.ansi(), urgent, RESET));
        }
        title_line.push('\n');
        if cards {
            // Carded hero: just the " RADAR …" title on the dark panel — no
            // rule line (header_lines() returns 1 for Cards to match).
            out.push_str(&paint_card_line(&title_line, width, &rail));
        } else {
            out.push_str(&title_line);
            // Header line 2: rule across the full width.
            out.push_str(&format!("{}{}{}\n", accent, "═".repeat(width), RESET));
        }
    }

    for &(i, max_lines) in &plan {
        // Every tab is a card: render it into a temporary buffer, then paint
        // each content line with its class's surface tint (idle < agent <
        // active) — a subtle step up from the dark panel.
        if cards {
            let bg = card_tint(&rows[i], &opts.theme);
            let mut tab_buf = String::new();
            render_row(&mut tab_buf, &rows[i], opts, max_lines.max(1));
            for line in tab_buf.split_inclusive('\n') {
                out.push_str(&paint_card_line(line, width, &bg));
            }
        } else {
            render_row(&mut out, &rows[i], opts, max_lines.max(1));
        }
        // Emit blank gap line(s) after each tab's content block when density > Compact.
        // In Cards the gap is painted with the dark panel base (so the whole
        // column is one cohesive panel); otherwise it stays bare.
        for _ in 0..gap_used {
            if cards {
                out.push_str(&paint_card_line("\n", width, &rail));
            } else {
                out.push('\n');
            }
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
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Detail;

    fn agg(status: Status, done: usize, total: usize, detail: Option<Detail>) -> TabAgg {
        TabAgg { status, done, total, pending: if status == Status::Pending { 1 } else { 0 }, detail, roster: vec![] }
    }

    fn ro(width: usize, now_tick: u64) -> RenderOpts {
        RenderOpts { width, height: 100, now_tick, glyphs: GlyphSet::Plain, header: true, density: crate::config::Density::Compact, theme: crate::theme::DerivedColors::default() }
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
            number: 1, name: "a".into(), active: false, has_bell: false,
            agg: agg(Status::Running, 0, 0, None),
        }];
        assert_eq!(header_lines(&rows, true, crate::config::Density::Compact), 2);
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
        assert_eq!(header_lines(&rows, true, crate::config::Density::Compact), 0);
        assert!(render(&rows, &ro(24, 0)).is_empty());
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
        assert_eq!(row_lines(&agg(Status::Idle, 0, 0, None)), 1);

        let detail = |status, msg: &str| Some(Detail {
            repo: "r".into(), branch: "b".into(), msg: msg.into(),
            since_tick: 0, status,
        });
        assert_eq!(row_lines(&agg(Status::Done, 1, 1, detail(Status::Done, ""))), 1);
        assert_eq!(row_lines(&agg(Status::Running, 1, 1, detail(Status::Running, "x"))), 2);
        assert_eq!(row_lines(&agg(Status::Error, 1, 1, detail(Status::Error, "x"))), 2);
        assert_eq!(row_lines(&agg(Status::Pending, 1, 1, detail(Status::Pending, ""))), 2);
        assert_eq!(row_lines(&agg(Status::Pending, 1, 1, detail(Status::Pending, "go?"))), 3);
    }

    #[test]
    fn active_row_has_accent_bar_idle_does_not() {
        let rows = vec![
            TabRow { number: 1, name: "a".into(), active: true, has_bell: false,
                     agg: agg(Status::Idle, 0, 0, None) },
            TabRow { number: 2, name: "b".into(), active: false, has_bell: false,
                     agg: agg(Status::Idle, 0, 0, None) },
        ];
        let s = render(&rows, &ro(24, 0));
        let body: Vec<&str> = s.lines().skip(2).collect(); // skip 2-line header
        assert!(body[0].contains('▌'));         // active row → bar
        assert!(body[0].contains(Role::Accent.ansi())); // accent-colored bar
        assert!(!body[1].contains('▌'));        // idle non-active → no bar
    }

    #[test]
    fn active_and_waiting_row_bar_is_attention_not_accent() {
        let detail = Detail { repo: "p".into(), branch: "fix".into(), msg: "".into(),
                              since_tick: 0, status: Status::Pending };
        let rows = vec![TabRow {
            number: 3, name: "pinky".into(), active: true, has_bell: false,
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
            let d = Detail { repo: "r".into(), branch: "b".into(), msg: "".into(),
                             since_tick: 0, status };
            TabRow { number: 1, name: "n".into(), active: false, has_bell: false,
                     agg: agg(status, done, total, Some(d)) }
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
        let d = Detail { repo: "r".into(), branch: "b".into(), msg: "".into(),
                         since_tick: 0, status: Status::Running };
        let rows = vec![TabRow { number: 1, name: "n".into(), active: false,
                                 has_bell: false, agg: agg(Status::Running, 0, 1, Some(d)) }];
        let opts = ro(30, 14);
        let s = render(&rows, &opts);
        // elapsed is wrapped in the theme idle_text (dim) color…
        assert!(s.contains(&format!("{}0:14", tc_fg(opts.theme.idle_text))),
            "working elapsed should be dim idle_text: {:?}", s);
        // …NOT the working role color.
        assert!(!s.contains(&format!("{}0:14", Role::Working.ansi())),
            "working elapsed must not be work-yellow: {:?}", s);
    }

    #[test]
    fn working_glyph_spins_with_tick() {
        let d = Detail { repo: "r".into(), branch: "b".into(), msg: "".into(),
                         since_tick: 0, status: Status::Running };
        let row = |_t| TabRow { number: 1, name: "n".into(), active: false, has_bell: false,
                               agg: agg(Status::Running, 0, 1, Some(d.clone())) };
        let f0 = render(&[row(0)], &RenderOpts { width: 30, height: 100, now_tick: 0, glyphs: GlyphSet::Plain, header: true, density: crate::config::Density::Compact, theme: crate::theme::DerivedColors::default() });
        let f1 = render(&[row(1)], &RenderOpts { width: 30, height: 100, now_tick: 1, glyphs: GlyphSet::Plain, header: true, density: crate::config::Density::Compact, theme: crate::theme::DerivedColors::default() });
        assert!(f0.contains('◐'));
        assert!(f1.contains('◓'));
    }

    #[test]
    fn idle_row_is_single_line_with_no_right_slot_text() {
        let rows = vec![TabRow {
            number: 7, name: "logs".into(), active: false, has_bell: false,
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
            status: Status::Pending,
        };
        let rows = vec![TabRow {
            number: 3, name: "a-long-tab-name".into(), active: true, has_bell: false,
            agg: agg(Status::Pending, 0, 1, Some(detail)),
        }];
        for width in [16usize, 20, 24, 30] {
            let s = render(&rows, &ro(width, 5));
            for line in s.lines() {
                assert!(visible_len(line) <= width,
                    "pending line exceeds width {}: {:?} (visible {})", width, line, visible_len(line));
            }
        }
    }

    #[test]
    fn running_has_no_warning_glyph() {
        let detail = Detail { repo: "r".into(), branch: "b".into(), msg: "".into(), since_tick: 0, status: Status::Running };
        let rows = vec![TabRow { number: 1, name: "t".into(), active: false, has_bell: false, agg: agg(Status::Running, 1, 1, Some(detail)) }];
        assert!(!render(&rows, &ro(30, 599)).contains('⚠'));
    }

    #[test]
    fn done_has_no_warning_glyph() {
        let detail = Detail { repo: "r".into(), branch: "b".into(), msg: "".into(), since_tick: 0, status: Status::Done };
        let rows = vec![TabRow { number: 1, name: "t".into(), active: false, has_bell: false, agg: agg(Status::Done, 1, 1, Some(detail)) }];
        assert!(!render(&rows, &ro(30, 10_000)).contains('⚠'));
    }

    #[test]
    fn bell_renders_marker() {
        let rows = vec![TabRow { number: 1, name: "t".into(), active: false, has_bell: true, agg: agg(Status::Idle, 0, 0, None) }];
        assert!(render(&rows, &ro(24, 0)).contains('⚑'));
    }

    #[test]
    fn no_bell_no_marker() {
        let rows = vec![TabRow { number: 1, name: "t".into(), active: false, has_bell: false, agg: agg(Status::Idle, 0, 0, None) }];
        assert!(!render(&rows, &ro(24, 0)).contains('⚑'));
    }

    #[test]
    fn error_word_narrows_when_tight() {
        let d = Detail { repo: "infra".into(), branch: "".into(), msg: "".into(),
                         since_tick: 0, status: Status::Error };
        let rows = vec![TabRow { number: 5, name: "infra".into(), active: false,
                                 has_bell: false, agg: agg(Status::Error, 0, 1, Some(d)) }];
        // wide: "failed"; narrow: "err"
        assert!(render(&rows, &ro(30, 0)).contains("failed"));
        let narrow = render(&rows, &ro(14, 0));
        assert!(narrow.contains("err"));
        assert!(!narrow.contains("failed"));
    }

    #[test]
    fn working_detail_drops_branch_before_message_when_narrow() {
        let d = Detail { repo: "web".into(), branch: "main".into(),
                         msg: "running tests".into(), since_tick: 0, status: Status::Running };
        let rows = vec![TabRow { number: 1, name: "api".into(), active: false,
                                 has_bell: false, agg: agg(Status::Running, 0, 1, Some(d)) }];
        let narrow = render(&rows, &ro(16, 5));
        for line in narrow.lines() {
            assert!(visible_len(line) <= 16);
        }
        // branch path is the first thing to go: "web/main" should not survive at 16 cols
        assert!(!narrow.contains("web/main"));
    }

    fn idle_row(n: u32) -> TabRow {
        TabRow { number: n, name: format!("t{}", n), active: false, has_bell: false,
                 agg: agg(Status::Idle, 0, 0, None) }
    }

    #[test]
    fn overflow_folds_idle_into_strip_and_marks_header() {
        // 20 idle tabs, height only fits a few → fold.
        let rows: Vec<TabRow> = (1..=20).map(idle_row).collect();
        let s = render(&rows, &RenderOpts { width: 24, height: 6, now_tick: 0, glyphs: GlyphSet::Plain, header: true, density: crate::config::Density::Compact, theme: crate::theme::DerivedColors::default() });
        assert!(s.contains("idle"));   // "+N idle ▾" footer
        assert!(s.contains('▾'));
        assert!(s.lines().next().unwrap().contains('▲')); // header overflow marker
        // total emitted lines fit the height budget
        assert!(s.lines().count() <= 6);
    }

    #[test]
    fn overflow_keeps_non_idle_rows_visible() {
        let mut rows: Vec<TabRow> = (1..=18).map(idle_row).collect();
        // an urgent waiting tab at the very end (high position)
        let d = Detail { repo: "p".into(), branch: "x".into(), msg: "approve?".into(),
                         since_tick: 0, status: Status::Pending };
        rows.push(TabRow { number: 19, name: "pinky".into(), active: false,
                           has_bell: false, agg: agg(Status::Pending, 0, 1, Some(d)) });
        let s = render(&rows, &RenderOpts { width: 30, height: 8, now_tick: 2, glyphs: GlyphSet::Plain, header: true, density: crate::config::Density::Compact, theme: crate::theme::DerivedColors::default() });
        assert!(s.contains("pinky"));     // urgent row never folded
        assert!(s.contains("needs you")); // its detail survives
    }

    #[test]
    fn no_overflow_when_everything_fits() {
        let rows: Vec<TabRow> = (1..=3).map(idle_row).collect();
        let s = render(&rows, &RenderOpts { width: 24, height: 40, now_tick: 0, glyphs: GlyphSet::Plain, header: true, density: crate::config::Density::Compact, theme: crate::theme::DerivedColors::default() });
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
            status,
        };

        let rows = vec![
            // idle — one line, no detail
            TabRow { number: 1, name: "idle-tab".into(), active: false, has_bell: false,
                     agg: agg(Status::Idle, 0, 0, None) },
            // running — two lines, with detail
            TabRow { number: 2, name: "run-tab".into(), active: true, has_bell: false,
                     agg: agg(Status::Running, 1, 2, Some(mk_detail(Status::Running))) },
            // pending with msg — three lines
            TabRow { number: 3, name: "pend-tab".into(), active: false, has_bell: false,
                     agg: agg(Status::Pending, 0, 1, Some(mk_detail(Status::Pending))) },
            // done — one line
            TabRow { number: 4, name: "done-tab".into(), active: false, has_bell: false,
                     agg: agg(Status::Done, 1, 1, Some(mk_detail(Status::Done))) },
            // error — two lines
            TabRow { number: 5, name: "err-tab".into(), active: false, has_bell: false,
                     agg: agg(Status::Error, 0, 1, Some(mk_detail(Status::Error))) },
        ];

        let opts = RenderOpts { width: 30, height: 100, now_tick: 7, glyphs: GlyphSet::Plain, header: true, density: crate::config::Density::Compact, theme: crate::theme::DerivedColors::default() };
        let s = render(&rows, &opts);

        // Compact density must NOT have card background bands.
        assert!(!s.contains("48;2;"),
            "Compact must not emit truecolor bg bands");
        // Must NOT contain raw hex color literals
        assert!(!s.contains('#'),
            "'#' hex color literal found in render output");
        // Glyph/status indicators must use ANSI-16 role codes.
        // Accent role: header title bar + active-row bar (\x1b[35m)
        assert!(s.contains(Role::Accent.ansi()),
            "expected accent role ANSI code not found");
        // Attention role: pending row glyph and "needs you" label (\x1b[91m)
        assert!(s.contains(Role::Attention.ansi()),
            "expected attention role ANSI code not found");
        // Working role: running row glyph (\x1b[33m)
        assert!(s.contains(Role::Working.ansi()),
            "expected working role ANSI code not found");
        // Error role: error row glyph (\x1b[31m)
        assert!(s.contains(Role::Error.ansi()),
            "expected error role ANSI code not found");
        // Detail lines use truecolor foreground for readable dims.
        assert!(s.contains("38;2;"),
            "detail lines must use theme-derived truecolor foreground for readable dims");
    }

    #[test]
    fn multi_agent_pending_shows_count() {
        // Multi-pending: agg.pending == 2 → detail line shows "2 needs you"
        let detail = Detail {
            repo: "proj".into(),
            branch: "fix".into(),
            msg: "".into(),
            since_tick: 0,
            status: Status::Pending,
        };
        let rows = vec![TabRow {
            number: 1,
            name: "agents".into(),
            active: false,
            has_bell: false,
            agg: TabAgg { status: Status::Pending, done: 0, total: 3, pending: 2, detail: Some(detail), roster: vec![] },
        }];
        let s = render(&rows, &ro(30, 0));
        assert!(s.contains("2 needs you"), "expected '2 needs you' in output: {:?}", s);
        assert!(!s.contains("1 needs you"), "unexpected '1 needs you' in output: {:?}", s);

        // Single-pending: agg.pending == 1 → detail line shows "needs you" (no count)
        let detail2 = Detail {
            repo: "proj".into(),
            branch: "fix".into(),
            msg: "".into(),
            since_tick: 0,
            status: Status::Pending,
        };
        let rows2 = vec![TabRow {
            number: 2,
            name: "solo".into(),
            active: false,
            has_bell: false,
            agg: TabAgg { status: Status::Pending, done: 0, total: 1, pending: 1, detail: Some(detail2), roster: vec![] },
        }];
        let s2 = render(&rows2, &ro(30, 0));
        assert!(s2.contains("needs you"), "expected 'needs you' in single-pending output: {:?}", s2);
        // Must NOT have a leading digit before "needs you" for the single case
        assert!(!s2.contains("1 needs you"), "single-pending must not show count: {:?}", s2);

        // Width constraint: multi-pending detail must not exceed width
        let detail3 = Detail {
            repo: "averylongreponame".into(),
            branch: "feature/some-very-long-branch".into(),
            msg: "".into(),
            since_tick: 0,
            status: Status::Pending,
        };
        let rows3 = vec![TabRow {
            number: 3,
            name: "multi".into(),
            active: false,
            has_bell: false,
            agg: TabAgg { status: Status::Pending, done: 0, total: 5, pending: 3, detail: Some(detail3), roster: vec![] },
        }];
        // Width constraint check: use widths where the slot ("0/5 ⏵ 0:00" = 11 cols)
        // plus row chrome can actually fit on the first line (>=20).
        for width in [20usize, 24, 30] {
            let s3 = render(&rows3, &ro(width, 0));
            assert!(s3.contains("3 needs you"), "expected '3 needs you' at width {}", width);
            for line in s3.lines() {
                assert!(
                    visible_len(line) <= width,
                    "multi-pending detail line exceeds width {}: {:?} (visible {})",
                    width, line, visible_len(line)
                );
            }
        }
    }

    #[test]
    fn multi_pending_detail_never_exceeds_width() {
        // Regression: at sub-17 widths the "N needs you" phrase is longer than
        // the available space after indent (3) + sep (3), so loc_budget saturates
        // to 0 and the raw phrase still overflows. This test must pass after the
        // width-clamp fix.
        //
        // Use total=1 so the first-row right-slot is compact ("⏵ 0:00", 6 cols)
        // and the first row fits at all tested widths — the focus is the detail
        // line (line 2), where "3 needs you" (11 cols) + indent (3) + sep (3) = 17
        // overflows at width ≤ 16 without the clamp.
        let detail = Detail {
            repo: "averylongreponame".into(),
            branch: "feature/some-very-long-branch-name".into(),
            msg: "".into(),
            since_tick: 0,
            status: Status::Pending,
        };
        let rows = vec![TabRow {
            number: 1,
            name: "m".into(),
            active: false,
            has_bell: false,
            agg: TabAgg { status: Status::Pending, done: 0, total: 1, pending: 3, detail: Some(detail), roster: vec![] },
        }];
        for width in [14usize, 16, 17, 20, 24] {
            let s = render(&rows, &ro(width, 0));
            for line in s.lines() {
                assert!(
                    visible_len(line) <= width,
                    "multi-pending detail exceeds width {}: {:?} (visible {})",
                    width, line, visible_len(line)
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
            let s = render(&rows, &RenderOpts { width, height: 6, now_tick: 0, glyphs: GlyphSet::Plain, header: true, density: crate::config::Density::Compact, theme: crate::theme::DerivedColors::default() });
            // folding must have happened
            assert!(s.contains("idle ▾"), "expected idle strip at width {}", width);
            for line in s.lines() {
                assert!(visible_len(line) <= width,
                    "idle strip/line exceeds width {}: {:?} (visible {})", width, line, visible_len(line));
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
            number: 1, name: "a".into(), active: false, has_bell: false,
            agg: agg(Status::Running, 0, 0, None),
        }];
        assert_eq!(header_lines(&rows, false, crate::config::Density::Compact), 0);
        let opts = RenderOpts { width: 24, height: 100, now_tick: 0, glyphs: GlyphSet::Plain, header: false, density: crate::config::Density::Compact, theme: crate::theme::DerivedColors::default() };
        let s = render(&rows, &opts);
        // No identity header: rows start at line 0, so no "RADAR"/"═" line.
        assert!(!s.contains("RADAR"));
        assert!(!s.contains('═'));
        // The single tab row is still rendered.
        assert!(s.contains('a') || s.matches('\n').count() >= 1);
    }

    #[test]
    fn multi_agent_active_tab_shows_roster_line() {
        use crate::status::Status::*;
        // Build a multi-agent running tab with a 4-member roster.
        let detail = Detail {
            repo: "repo".into(), branch: "main".into(), msg: "working".into(),
            since_tick: 0, status: Running,
        };
        let mut a = agg(Running, 2, 4, Some(detail));
        a.roster = vec![Running, Done, Done, Pending];

        // row_lines should be base + 1 for multi-agent active tab.
        // Running with detail normally = 2 lines; +1 roster = 3.
        assert_eq!(row_lines(&a), 3, "multi-agent active tab should add roster line");

        let row = TabRow { number: 1, name: "agents".into(), active: false, has_bell: false, agg: a };
        let s = render(&[row], &ro(30, 0));
        // The pending glyph should appear (roster member)
        assert!(s.contains('◆'), "roster line must contain pending glyph: {:?}", s);
        // A working/half-circle glyph should appear (Running member)
        assert!(s.contains('◐') || s.contains('◓') || s.contains('◑') || s.contains('◒'),
            "roster line must contain a working glyph: {:?}", s);
        // The roster line is separate from line 1 (which has the tab name)
        let lines: Vec<&str> = s.lines().collect();
        // header(2) + line1(1) + detail(1) + roster(1) = 5 lines
        assert!(lines.len() >= 4, "expected at least 4 lines, got {}: {:?}", lines.len(), s);
    }

    #[test]
    fn roster_line_never_exceeds_width() {
        use crate::status::Status::*;
        for &width in &[16usize, 20, 24] {
            let detail = Detail {
                repo: "r".into(), branch: "b".into(), msg: "m".into(),
                since_tick: 0, status: Running,
            };
            // 10-member roster
            let mut a = agg(Running, 0, 10, Some(detail));
            a.roster = vec![Running, Done, Pending, Running, Done, Running, Pending, Done, Running, Error];
            let row = TabRow { number: 1, name: "big-team".into(), active: false, has_bell: false, agg: a };
            let s = render(&[row], &RenderOpts {
                width, height: 100, now_tick: 0, glyphs: GlyphSet::Plain, header: true, density: crate::config::Density::Compact, theme: crate::theme::DerivedColors::default(),
            });
            for line in s.lines() {
                assert!(visible_len(line) <= width,
                    "roster line exceeds width {} at width {}: {:?} (visible {})",
                    width, width, line, visible_len(line));
            }
        }
    }

    /// Regression: at width ≤ 3 the 3-col indent leaves no room for any glyph
    /// so `parts` is empty, but the roster line must still be physically emitted
    /// to keep the emitted line count in lockstep with `row_lines()`.
    #[test]
    fn roster_line_emitted_even_when_too_narrow_for_glyphs() {
        use crate::status::Status::*;
        let detail = crate::model::Detail {
            repo: "r".into(),
            branch: "b".into(),
            msg: "working".into(),
            since_tick: 0,
            status: Running,
        };
        // Multi-agent running tab with a roster; total=3, status=Running.
        // row_lines() = 2 (running+detail) + 1 (roster) = 3 lines.
        let mut a = agg(Running, 1, 3, Some(detail));
        a.roster = vec![Running, Done, Pending];
        let expected_lines = row_lines(&a);

        for &width in &[1usize, 2, 3, 4] {
            let row = TabRow {
                number: 1,
                name: "a".into(),
                active: false,
                has_bell: false,
                agg: a.clone(),
            };
            // Use header:false to isolate the row count to just this row's lines.
            let opts = RenderOpts {
                width,
                height: 100,
                now_tick: 0,
                glyphs: GlyphSet::Plain,
                header: false,
                density: crate::config::Density::Compact,
                theme: crate::theme::DerivedColors::default(),
            };
            let s = render(&[row], &opts);
            let emitted = s.matches('\n').count();
            assert_eq!(
                emitted, expected_lines,
                "at width={}: emitted {} lines but row_lines()={} (output: {:?})",
                width, emitted, expected_lines, s
            );
            // The roster line itself (the last line emitted) must not exceed `width`.
            // (Other lines may be wider at extreme narrow widths, but the roster
            // line is the one this test focuses on.)
            let lines: Vec<&str> = s.lines().collect();
            if let Some(last) = lines.last() {
                assert!(
                    visible_len(last) <= width,
                    "at width={}: roster line exceeds width: {:?} (visible {})",
                    width, last, visible_len(last)
                );
            }
        }
    }

    #[test]
    fn single_agent_pending_no_roster_line() {
        use crate::status::Status::*;
        let detail = Detail {
            repo: "r".into(), branch: "b".into(), msg: "approve?".into(),
            since_tick: 0, status: Pending,
        };
        // Single-agent: roster is empty, total == 1.
        let a = agg(Pending, 0, 1, Some(detail));
        // row_lines should be 3 (pending + msg) — NOT 4.
        let base = row_lines(&a);
        assert_eq!(base, 3, "single-agent pending+msg should be 3 lines");
        // Roster vec is empty so no extra line.
        assert!(a.roster.is_empty());
    }

    #[test]
    fn row_lines_no_roster_line_when_roster_empty() {
        use crate::status::Status::*;
        // Multi-agent active tab but roster is empty — row_lines must NOT add +1.
        // Running with detail = base 2 lines; empty roster => still 2, not 3.
        let detail = crate::model::Detail {
            repo: "r".into(), branch: "b".into(), msg: "working".into(),
            since_tick: 0, status: Running,
        };
        let a = agg(Running, 1, 3, Some(detail));
        assert!(a.roster.is_empty(), "agg() helper always creates empty roster");
        assert_eq!(
            row_lines(&a), 2,
            "multi-agent active tab with empty roster must not add roster line"
        );
    }

    /// Calm rows (Running/Done) are compressed to 1 line before urgent rows
    /// (Pending) lose their detail lines.
    #[test]
    fn overflow_compresses_calm_before_urgent() {
        // 3 Running rows (each 2 lines) + 1 Pending-with-msg (3 lines) = 9 lines.
        // header = 2. body_budget = height - 2.
        // We pick height = 7 → body_budget = 5.
        // Compression: Running rows compressed to 1 line each (3 lines saved);
        // total becomes 3×1 + 3 = 6, still > 5 → one more Running line → 3×1+3 = 6?
        // Actually: 3×1 + 3 = 6 > 5, so one more calm row: but all calm are already at 1.
        // Wait: calm rows each go from 2 to 1 (saving 1 line each). After all 3:
        //   3×1 + 3 = 6 > 5. Then compress Pending: 3→2 (drop msg). 3×1 + 2 = 5 ≤ 5. Done.
        // So Pending still has 2 lines (the "branch · needs you" line survives).
        // Pending loses ONLY the msg line; its detail line stays.
        let detail_running = |n: u8| Detail {
            repo: format!("repo{}", n), branch: "main".into(), msg: "working".into(),
            since_tick: 0, status: Status::Running,
        };
        let detail_pending = Detail {
            repo: "urgent-proj".into(), branch: "fix/thing".into(),
            msg: "please review".into(),
            since_tick: 0, status: Status::Pending,
        };

        let rows = vec![
            TabRow { number: 1, name: "r1".into(), active: false, has_bell: false,
                     agg: agg(Status::Running, 0, 1, Some(detail_running(1))) },
            TabRow { number: 2, name: "r2".into(), active: false, has_bell: false,
                     agg: agg(Status::Running, 0, 1, Some(detail_running(2))) },
            TabRow { number: 3, name: "r3".into(), active: false, has_bell: false,
                     agg: agg(Status::Running, 0, 1, Some(detail_running(3))) },
            TabRow { number: 4, name: "urgent".into(), active: false, has_bell: false,
                     agg: agg(Status::Pending, 0, 1, Some(detail_pending)) },
        ];

        // Verify uncompressed sizes.
        assert_eq!(row_lines(&rows[0].agg), 2);
        assert_eq!(row_lines(&rows[1].agg), 2);
        assert_eq!(row_lines(&rows[2].agg), 2);
        assert_eq!(row_lines(&rows[3].agg), 3); // pending + msg

        // body_budget = 5 (height 7, header 2)
        let body_budget = 5usize;
        let (plan, strip_folded) = plan_overflow(&rows, body_budget);
        assert_eq!(strip_folded, 0, "no idle rows to strip");
        assert_eq!(plan.len(), 4, "all 4 rows kept");

        // Running rows should be at 1 line each (calm, compressed first).
        assert_eq!(plan[0].1, 1, "Running row 0 compressed to 1 line");
        assert_eq!(plan[1].1, 1, "Running row 1 compressed to 1 line");
        assert_eq!(plan[2].1, 1, "Running row 2 compressed to 1 line");

        // Pending row: 3 lines initially, should still keep its detail (≥ 2 lines).
        // After calm compression: 3 + 3 = 6 > 5; one step of urgent compression:
        // pending → 2 → total = 3 + 2 = 5 ≤ 5. Done.
        assert!(plan[3].1 >= 2,
            "Pending row should retain detail (≥2 lines), got {}", plan[3].1);

        // Total body lines must be ≤ budget.
        let total_body: usize = plan.iter().map(|(_, l)| l).sum();
        assert!(total_body <= body_budget,
            "total body lines {} exceeds budget {}", total_body, body_budget);

        // Render and verify: Running rows have no detail line; urgent keeps "needs you".
        let opts = RenderOpts { width: 30, height: 7, now_tick: 0, glyphs: GlyphSet::Plain, header: true, density: crate::config::Density::Compact, theme: crate::theme::DerivedColors::default() };
        let s = render(&rows, &opts);
        assert!(s.contains("needs you"), "urgent row detail must survive");
        // Total line count ≤ height
        assert!(s.lines().count() <= 7,
            "rendered lines {} exceed height 7", s.lines().count());
    }

    /// When height is extremely small, every kept row is compressed to exactly
    /// 1 line; no panic; total output lines ≤ budget.
    #[test]
    fn overflow_all_one_line_when_extreme() {
        let detail = Detail {
            repo: "r".into(), branch: "b".into(), msg: "msg".into(),
            since_tick: 0, status: Status::Pending,
        };
        let rows = vec![
            TabRow { number: 1, name: "pending".into(), active: false, has_bell: false,
                     agg: agg(Status::Pending, 0, 1, Some(detail.clone())) },
            TabRow { number: 2, name: "run".into(), active: false, has_bell: false,
                     agg: agg(Status::Running, 0, 1, Some(detail.clone())) },
        ];
        // height = 3 → body_budget = 1 (header=2). Each non-idle row at min 1 line.
        let opts = RenderOpts { width: 24, height: 3, now_tick: 0, glyphs: GlyphSet::Plain, header: true, density: crate::config::Density::Compact, theme: crate::theme::DerivedColors::default() };
        let s = render(&rows, &opts);
        let line_count = s.lines().count();
        assert!(line_count <= 3,
            "rendered {} lines but height is 3", line_count);
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
        assert_eq!(s.matches('\n').count(), 8,
            "comfortable: expected 2 header + 3×(1 content + 1 gap) = 8 newlines, got {}:\n{:?}", s.matches('\n').count(), s);
        // Check that there is a blank line between tabs (an empty line between non-empty lines).
        let body_lines: Vec<&str> = s.lines().skip(2).collect();
        assert_eq!(body_lines.len(), 6);
        // Odd-indexed body lines (0-based: 1, 3, 5) should be blank gap lines.
        assert!(body_lines[1].is_empty(), "body line 1 should be blank gap: {:?}", body_lines[1]);
        assert!(body_lines[3].is_empty(), "body line 3 should be blank gap: {:?}", body_lines[3]);
        assert!(body_lines[5].is_empty(), "body line 5 should be blank gap: {:?}", body_lines[5]);
    }

    #[test]
    fn compact_has_no_gaps() {
        // 3 idle tabs, compact → no gap lines at all.
        // Total lines = 2 header + 3 content = 5 \n chars.
        let rows: Vec<TabRow> = (1..=3).map(idle_row).collect();
        let opts = RenderOpts {
            width: 24, height: 100, now_tick: 0, glyphs: GlyphSet::Plain,
            header: true, density: crate::config::Density::Compact, theme: crate::theme::DerivedColors::default(),
        };
        let s = render(&rows, &opts);
        assert_eq!(s.matches('\n').count(), 5,
            "compact: expected 2 header + 3 content = 5 newlines, got {}:\n{:?}", s.matches('\n').count(), s);
        // No empty lines in the body.
        for line in s.lines().skip(2) {
            assert!(!line.is_empty(), "compact should have no blank lines, found one: {:?}", line);
        }
    }

    #[test]
    fn cards_content_lines_differ_from_comfortable() {
        // Cards paints a background band on AGENT rows — so a session with at
        // least one agent differs from Comfortable. (An all-idle session would
        // now render identically, since idle rows are bare in the hybrid.)
        use crate::model::Detail;
        let detail = Detail {
            repo: "r".into(), branch: "b".into(), msg: "working".into(),
            since_tick: 0, status: Status::Running,
        };
        let rows = vec![
            TabRow { number: 1, name: "work".into(), active: false, has_bell: false,
                     agg: agg(Status::Running, 0, 1, Some(detail)) },
            idle_row(2),
        ];
        let comfortable = render(&rows, &RenderOpts {
            width: 24, height: 100, now_tick: 0, glyphs: GlyphSet::Plain,
            header: true, density: crate::config::Density::Comfortable, theme: crate::theme::DerivedColors::default(),
        });
        let cards = render(&rows, &RenderOpts {
            width: 24, height: 100, now_tick: 0, glyphs: GlyphSet::Plain,
            header: true, density: crate::config::Density::Cards, theme: crate::theme::DerivedColors::default(),
        });
        assert_ne!(comfortable, cards, "Cards should differ from Comfortable (has bg bands)");
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
        let (plan, strip, gap_used) = plan_layout(&rows, height - 2, crate::config::Density::Comfortable);
        // All 3 idle rows fit at 1 line each (total=3 ≤ body_budget=4).
        assert_eq!(plan.len(), 3, "all 3 rows should be kept");
        assert_eq!(strip, 0, "no rows folded into strip");
        // 3 content + 3 gaps = 6 > 4 (body_budget) → gaps dropped.
        assert_eq!(gap_used, 0, "gaps should be dropped when they don't fit");
        // Render and verify: no blank lines in output.
        let s = render(&rows, &RenderOpts {
            width: 24, height, now_tick: 0, glyphs: GlyphSet::Plain,
            header: true, density: crate::config::Density::Comfortable, theme: crate::theme::DerivedColors::default(),
        });
        let line_count = s.lines().count();
        assert!(line_count <= height,
            "rendered {} lines but height is {}", line_count, height);
        // When gaps are dropped, no blank body lines.
        for line in s.lines().skip(2) {
            assert!(!line.is_empty(), "gaps dropped — no blank body lines expected: {:?}", line);
        }
    }

    #[test]
    fn plan_layout_compact_always_zero_gap() {
        let rows: Vec<TabRow> = (1..=5).map(idle_row).collect();
        // Even with very large budget, compact never adds gaps.
        let (_, _, gap_used) = plan_layout(&rows, 100, crate::config::Density::Compact);
        assert_eq!(gap_used, 0, "Compact density must never produce gaps");
    }

    #[test]
    fn plan_layout_comfortable_gap_when_space_available() {
        // 2 idle rows, body_budget=10: 2 content + 2 gaps = 4 ≤ 10 → gap_used = 1.
        let rows: Vec<TabRow> = (1..=2).map(idle_row).collect();
        let (_, _, gap_used) = plan_layout(&rows, 10, crate::config::Density::Comfortable);
        assert_eq!(gap_used, 1, "Comfortable with room should use gaps");
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
            repo: "repo".into(), branch: "main".into(), msg: "working".into(),
            since_tick: 0, status: Status::Running,
        };
        let rows = vec![
            TabRow { number: 1, name: "idle".into(), active: false, has_bell: false,
                     agg: agg(Status::Idle, 0, 0, None) },
            TabRow { number: 2, name: "work".into(), active: true, has_bell: false,
                     agg: agg(Status::Running, 0, 1, Some(detail)) },
        ];
        let s = render(&rows, &ro_cards(30, 100));
        let lines: Vec<&str> = s.lines().collect();

        // Cards is now ONE cohesive dark panel: EVERY emitted line is painted.
        // The 1-line header and the gap carry the dark panel base (rail_bg =
        // 21,21,32); content lines carry their own (subtle) surface tint.
        const RAIL: &str = "\x1b[48;2;21;21;32m";

        // line 0 = header title (no rule in Cards) → painted with rail_bg.
        assert!(lines[0].contains(RAIL),
            "header title line must carry the rail panel band: {:?}", lines[0]);
        assert!(lines[0].contains("RADAR"),
            "header title must read RADAR: {:?}", lines[0]);

        // line 1 = idle tab content → a card surface (NOT the rail base).
        assert!(lines[1].contains("\x1b[48;2;") && !lines[1].contains(RAIL),
            "idle content line must carry a card surface band, not rail: {:?}", lines[1]);

        // line 2 = the gap → painted with rail_bg (the panel base).
        assert!(lines[2].contains(RAIL),
            "gap line must carry the rail panel band: {:?}", lines[2]);

        // line 3 = working tab line 1, line 4 = working detail → card surface.
        assert!(lines[3].contains("\x1b[48;2;") && !lines[3].contains(RAIL),
            "working tab line 1 must carry a card surface band: {:?}", lines[3]);
        assert!(lines[4].contains("\x1b[48;2;") && !lines[4].contains(RAIL),
            "working tab detail line must carry a card surface band: {:?}", lines[4]);

        // Every painted line must end with bg reset (\x1b[49m).
        for (i, line) in lines.iter().enumerate() {
            assert!(line.contains("\x1b[49m"),
                "panel line {} must contain bg reset: {:?}", i, line);
        }
    }

    #[test]
    fn cards_band_fills_full_width() {
        // Short-name agent (done) tab at width 24, Cards: the painted band fills
        // the full width.
        use crate::model::Detail;
        let done = Detail {
            repo: "r".into(), branch: "".into(), msg: "".into(),
            since_tick: 0, status: Status::Done,
        };
        let rows = vec![
            TabRow { number: 1, name: "x".into(), active: false, has_bell: false,
                     agg: agg(Status::Done, 1, 1, Some(done)) },
        ];
        let width = 24usize;
        let s = render(&rows, &ro_cards(width, 100));
        // Skip 2 header lines; first body line is the painted content line.
        let body: Vec<&str> = s.lines().skip(2).collect();
        let content_line = body[0];
        assert!(content_line.contains("\x1b[48;2;"),
            "content line must have truecolor card bg: {:?}", content_line);
        // Visible width must equal exactly `width`.
        let vw = visible_len(content_line);
        assert_eq!(vw, width,
            "painted content line visible width must equal {} (full band), got {}: {:?}",
            width, vw, content_line);
    }

    #[test]
    fn cards_rearm_bg_after_resets() {
        // Active working tab (line has multiple role-colored tokens with \x1b[0m
        // resets) under Cards: the active truecolor tint must re-arm after every reset,
        // so \x1b[0m\x1b[48;2;... (reset immediately followed by the truecolor band) appears.
        use crate::model::Detail;
        let detail = Detail {
            repo: "pinky".into(), branch: "fix/x".into(), msg: "some work".into(),
            since_tick: 0, status: Status::Running,
        };
        let rows = vec![TabRow {
            number: 1, name: "agent".into(), active: true, has_bell: false,
            agg: agg(Status::Running, 0, 1, Some(detail)),
        }];
        let s = render(&rows, &ro_cards(30, 100));
        // The default theme (Mocha) active surface from the dark-panel ladder is (48,48,66).
        assert!(s.contains("\x1b[0m\x1b[48;2;48;48;66m"),
            "reset immediately followed by truecolor bg re-arm must appear in Cards output: {:?}", s);
    }

    #[test]
    fn cards_use_truecolor_not_256color() {
        // Card surfaces use theme-derived truecolor (48;2;r;g;b) — not fixed 256-color
        // indices, not truecolor foreground (38;2;), and not raw hex literals.
        use crate::model::Detail;
        let detail = Detail {
            repo: "r".into(), branch: "b".into(), msg: "work".into(),
            since_tick: 0, status: Status::Running,
        };
        let rows = vec![
            TabRow { number: 1, name: "idle".into(), active: false, has_bell: false,
                     agg: agg(Status::Idle, 0, 0, None) },
            TabRow { number: 2, name: "work".into(), active: true, has_bell: false,
                     agg: agg(Status::Running, 0, 1, Some(detail)) },
        ];
        let s = render(&rows, &ro_cards(30, 100));
        // Card surfaces must emit truecolor backgrounds.
        assert!(s.contains("\x1b[48;2;"), "cards must use a truecolor surface (48;2;): {:?}", s);
        // Must NOT use legacy 256-color indices for the surface band.
        assert!(!s.contains("\x1b[48;5;"), "cards must not use 256-color surface (48;5;): {:?}", s);
        // Must NOT contain raw hex color literals.
        assert!(!s.contains('#'), "cards must not emit raw hex: {:?}", s);
        // Note: 38;2; truecolor foreground IS expected (detail lines use theme-derived dim foreground).
    }

    #[test]
    fn comfortable_and_compact_emit_no_bg() {
        // Same tabs with Comfortable and Compact must contain NO card band.
        use crate::model::Detail;
        let detail = Detail {
            repo: "r".into(), branch: "b".into(), msg: "working".into(),
            since_tick: 0, status: Status::Running,
        };
        let rows = vec![
            TabRow { number: 1, name: "idle".into(), active: false, has_bell: false,
                     agg: agg(Status::Idle, 0, 0, None) },
            TabRow { number: 2, name: "work".into(), active: true, has_bell: false,
                     agg: agg(Status::Running, 0, 1, Some(detail)) },
        ];
        for density in [crate::config::Density::Comfortable, crate::config::Density::Compact] {
            let s = render(&rows, &RenderOpts {
                width: 30, height: 100, now_tick: 0, glyphs: GlyphSet::Plain,
                header: true, density, theme: crate::theme::DerivedColors::default(),
            });
            assert!(!s.contains("\x1b[48;2;"),
                "density {:?} must NOT emit a truecolor card band: {:?}", density, s);
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

    // ── 3-tint cards: every tab is a card, idle dim / agent mid / active bright ──

    /// Classify each rendered line for a tint-map snapshot by which truecolor
    /// surface band it carries (using Mocha defaults from the dark-panel ladder):
    ///   "active" = surface_active (48,48,66)  — brighter than bg, gently pops
    ///   "agent"  = surface_agent  (26,26,39)  — mid step up from rail
    ///   "idle"   = surface_idle   (22,22,34)  — barely above the panel
    ///   "rail"   = rail_bg        (21,21,32)  — the dark panel base (gaps/header)
    ///   "bare"   = no band at all.
    /// Note: in Cards every emitted line is painted, so blank gaps now carry the
    /// rail band rather than being empty — they classify as "rail", not "gap".
    fn tint_map(s: &str) -> String {
        s.lines()
            .map(|line| {
                if line.contains("\x1b[48;2;48;48;66m") {
                    "active"
                } else if line.contains("\x1b[48;2;26;26;39m") {
                    "agent"
                } else if line.contains("\x1b[48;2;22;22;34m") {
                    "idle"
                } else if line.contains("\x1b[48;2;21;21;32m") {
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
            repo: "repo".into(), branch: "main".into(), msg: "working".into(),
            since_tick: 0, status: Status::Running,
        };
        let rows = vec![
            TabRow { number: 1, name: "idle".into(), active: false, has_bell: false,
                     agg: agg(Status::Idle, 0, 0, None) },
            TabRow { number: 2, name: "agent".into(), active: false, has_bell: false,
                     agg: agg(Status::Running, 0, 1, Some(detail.clone())) },
            TabRow { number: 3, name: "focus".into(), active: true, has_bell: false,
                     agg: agg(Status::Running, 0, 1, Some(detail)) },
        ];
        let s = render(&rows, &ro_cards(30, 100));
        let lines: Vec<&str> = s.lines().collect();
        // Cards header is now 1 line (no rule), so content starts at line 1.
        // line 1 = idle content → barely-above-panel idle surface (22,22,34)
        assert!(lines[1].contains("\x1b[48;2;22;22;34m"),
            "idle row must carry the dim truecolor card tint (22;22;34): {:?}", lines[1]);
        // gap at line 2 (rail), agent line 1 at line 3 → mid surface (26,26,39)
        assert!(lines[3].contains("\x1b[48;2;26;26;39m"),
            "agent row must carry the mid truecolor card tint (26;26;39): {:?}", lines[3]);
        // gap at line 5 (rail), focused agent line 1 at line 6 → active surface (48,48,66)
        assert!(lines[6].contains("\x1b[48;2;48;48;66m"),
            "focused row must carry the active truecolor card tint (48;48;66): {:?}", lines[6]);
    }

    #[test]
    fn cards_3tint_layout_snapshot() {
        // Golden tint-map for the canonical sidebar.dc.html "cards" session:
        // active running agent, pending agent, done agent, then two idle panes.
        // Every tab is a card; one gap row after each; tints encode the class.
        use crate::model::Detail;
        let running = Detail { repo: "web".into(), branch: "".into(),
            msg: "building…".into(), since_tick: 0, status: Status::Running };
        let pending = Detail { repo: "api".into(), branch: "fix".into(),
            msg: "".into(), since_tick: 0, status: Status::Pending };
        let done = Detail { repo: "worker".into(), branch: "".into(),
            msg: "".into(), since_tick: 0, status: Status::Done };
        let rows = vec![
            TabRow { number: 1, name: "Claude".into(), active: true, has_bell: false,
                     agg: agg(Status::Running, 0, 1, Some(running)) },
            TabRow { number: 2, name: "api".into(), active: false, has_bell: false,
                     agg: agg(Status::Pending, 0, 1, Some(pending)) },
            TabRow { number: 3, name: "worker".into(), active: false, has_bell: false,
                     agg: agg(Status::Done, 1, 1, Some(done)) },
            TabRow { number: 4, name: "Pane #1".into(), active: false, has_bell: false,
                     agg: agg(Status::Idle, 0, 0, None) },
            TabRow { number: 5, name: "Pane #1".into(), active: false, has_bell: false,
                     agg: agg(Status::Idle, 0, 0, None) },
        ];
        let s = render(&rows, &ro_cards(24, 100));
        // Cards is now a cohesive dark panel: the 1-line header (no rule) and
        // every gap are painted with rail_bg; card content carries its surface.
        let expected = "\
rail\n\
active\n\
active\n\
rail\n\
agent\n\
agent\n\
rail\n\
agent\n\
rail\n\
idle\n\
rail\n\
idle\n\
rail";
        assert_eq!(tint_map(&s), expected,
            "3-tint card map drifted from the design:\n{:?}", s);
    }

    #[test]
    fn cards_waiting_is_peach_not_red() {
        // THE KEY OUTCOME: in Cards density a waiting "needs you" row renders in
        // the theme's peach attention hue, clearly different from a red error row.
        use crate::model::Detail;
        let pending = Detail { repo: "pinky".into(), branch: "fix".into(),
            msg: "".into(), since_tick: 0, status: Status::Pending };
        let err = Detail { repo: "infra".into(), branch: "".into(),
            msg: "boom".into(), since_tick: 0, status: Status::Error };
        let rows = vec![
            TabRow { number: 1, name: "pinky".into(), active: false, has_bell: false,
                     agg: agg(Status::Pending, 0, 1, Some(pending)) },
            TabRow { number: 2, name: "infra".into(), active: false, has_bell: false,
                     agg: agg(Status::Error, 0, 1, Some(err)) },
        ];
        let theme = crate::theme::DerivedColors::default();
        let s = render(&rows, &ro_cards(30, 100));
        // Examine the body only (skip the 1-line header, whose urgent "·N!"
        // marker keeps the ANSI attention code — header styling is out of scope).
        let body: String = s.lines().skip(1).collect::<Vec<_>>().join("\n");
        let peach = tc_fg(theme.attention); // Mocha peach (250,179,135)
        let red = tc_fg(theme.error);       // Mocha red   (243,139,168)
        // Waiting label + "needs you" use the peach attention hue…
        assert!(body.contains(&peach),
            "waiting row must render in the peach attention hue: {:?}", body);
        // …and the body never uses the ANSI bright-red that previously read as "error".
        assert!(!body.contains("\x1b[91m"),
            "Cards body must not use ANSI bright-red for attention: {:?}", body);
        // The error row uses the distinct red hue.
        assert!(body.contains(&red),
            "error row must render in the red error hue: {:?}", body);
        assert_ne!(peach, red, "peach attention must differ from red error");
    }

    #[test]
    fn header_shows_radar_and_urgent_count() {
        // Header reads " RADAR" and, when any tab is pending, appends a "·N!"
        // urgent marker in the attention role.
        use crate::model::Detail;
        let pending = Detail { repo: "p".into(), branch: "x".into(),
            msg: "approve?".into(), since_tick: 0, status: Status::Pending };
        let rows = vec![
            TabRow { number: 1, name: "pinky".into(), active: false, has_bell: false,
                     agg: agg(Status::Pending, 0, 1, Some(pending)) },
            idle_row(2),
            idle_row(3),
        ];
        let s = render(&rows, &ro(30, 0));
        let header = s.lines().next().unwrap();
        assert!(header.contains("RADAR"), "header must read RADAR: {:?}", header);
        assert!(!header.contains("AGENTS"), "header must not say AGENTS: {:?}", header);
        assert!(header.contains("·3"), "header must show total count ·3: {:?}", header);
        assert!(header.contains("·1!"), "header must show urgent count ·1!: {:?}", header);
        assert!(header.contains(Role::Attention.ansi()),
            "urgent marker must use the attention role: {:?}", header);
    }

    #[test]
    fn header_no_urgent_marker_when_nothing_pending() {
        let rows: Vec<TabRow> = (1..=3).map(idle_row).collect();
        let s = render(&rows, &ro(30, 0));
        let header = s.lines().next().unwrap();
        assert!(header.contains("·3"), "header shows total: {:?}", header);
        assert!(!header.contains('!'), "no urgent marker when nothing pending: {:?}", header);
    }
}