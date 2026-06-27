//! Pure renderer: per-tab rows → ANSI string. No zellij-tile dependency.

use crate::config::Density;
use crate::model::TabAgg;
use crate::status::{Role, Status};
pub use crate::status::GlyphSet;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";

#[derive(Clone, Copy)]
pub struct RenderOpts {
    pub width: usize,
    pub height: usize,
    pub now_tick: u64,
    pub glyphs: GlyphSet,
    /// Whether to render the " RADAR" identity header block.
    pub header: bool,
    /// Vertical density between tabs.
    pub density: Density,
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

/// The rail's identity header is two lines (title + rule) whenever any rows
/// exist (always-on identity). Single source of truth for the header's
/// vertical span (consumed by click mapping in lib.rs). Only the truly-empty
/// case (no rows at all) is headerless. When `header` is false the identity
/// block is suppressed and rows start at line 0.
pub fn header_lines(rows: &[TabRow], header: bool) -> usize {
    if rows.is_empty() || !header {
        0
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
    let role = st.role().ansi();

    // col 0: active bar — accent normally, attention when active+urgent.
    let bar = if row.active {
        let bar_role = match st {
            Status::Pending | Status::Error => Role::Attention,
            _ => Role::Accent,
        };
        format!("{}▌{}", bar_role.ansi(), RESET)
    } else {
        " ".to_string()
    };

    // col 1: status glyph (working spins).
    let glyph_char = if st == Status::Running {
        crate::status::working_spin(now_tick as usize)
    } else {
        st.glyph_for(opts.glyphs)
    };
    let glyph = format!("{}{}{}", role, glyph_char, RESET);

    // right slot (reserved width even when empty).
    let slot = right_slot(&row.agg, now_tick, width);
    let slot_styled = if slot.is_empty() {
        String::new()
    } else {
        format!("{}{}{}", role, slot, RESET)
    };

    // bell marker just before the slot.
    let bell = if row.has_bell {
        format!("{}⚑{} ", Role::Working.ansi(), RESET)
    } else {
        String::new()
    };

    // left visible prefix is "X<glyph> <num> " — bar/glyph are 1 cell each.
    let num = row.number.to_string();
    let prefix_len = 1 + 1 + 1 + UnicodeWidthStr::width(num.as_str()) + 1; // bar+glyph+sp+num+sp
    let bell_len = if row.has_bell { 2 } else { 0 };
    let slot_len = UnicodeWidthStr::width(slot.as_str());
    let name_budget = width
        .saturating_sub(prefix_len + bell_len + slot_len + 1) // +1 min gap
        .max(1);
    let name = truncate(&row.name, name_budget);
    let name_styled = if row.active {
        format!("{}{}{}", BOLD, name, RESET)
    } else {
        name.clone()
    };

    // pad so the slot sits flush right.
    let used = prefix_len + UnicodeWidthStr::width(name.as_str()) + bell_len + slot_len;
    let gap = width.saturating_sub(used).max(1);
    out.push_str(&format!(
        "{}{} {} {}{}{}{}\n",
        bar, glyph, num, name_styled, " ".repeat(gap), bell, slot_styled
    ));

    // Line 1 done. Emit detail/roster only within the remaining budget.
    if max_lines <= 1 {
        return;
    }
    let mut emitted = 1usize;

    let muted = Role::Muted.ansi();

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
                    out.push_str(&format!("   {}{}{}\n", Role::Muted.ansi(), truncate(&body, avail), RESET));
                    emitted += 1;
                }
            }
            Status::Error => {
                // Detail line (priority 1 after line 1).
                if emitted < max_lines {
                    let loc = if d.branch.is_empty() { d.repo.clone() } else { format!("{}/{}", d.repo, d.branch) };
                    let body = if d.msg.trim().is_empty() { loc } else { format!("{} · {}", loc, d.msg) };
                    out.push_str(&format!("   {}{}{}\n", muted, truncate(&body, width.saturating_sub(3)), RESET));
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
                        out.push_str(&format!("   {}{} · {}{}{}\n", muted, loc_str, Role::Attention.ansi(), needs_phrase, RESET));
                    } else {
                        let clamped = truncate(&visible_content, width);
                        out.push_str(&format!("{}{}{}\n", muted, clamped, RESET));
                    }
                    emitted += 1;
                }
                // Priority 2: quoted msg line (only if non-empty).
                if emitted < max_lines && !d.msg.trim().is_empty() {
                    out.push_str(&format!("   {}\"{}\"{}\n", muted, truncate(&d.msg, width.saturating_sub(5)), RESET));
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

/// Paint a single content line with an ANSI-16 bright-black background band
/// (theme-portable `\x1b[100m`).
///
/// Steps:
/// 1. Replace every `RESET` (`\x1b[0m`) in the line with `RESET + "\x1b[100m"`
///    so that colored tokens re-arm the background after they reset.
/// 2. Strip the trailing newline (if present), measure, pad to `width`, restore.
/// 3. Wrap: `"\x1b[100m" + transformed_line + pad + "\x1b[49m\x1b[0m"`.
///
/// The returned string ends with `\n`.
fn paint_card_line(line: &str, width: usize) -> String {
    const CARD_BG: &str = "\x1b[100m";
    const BG_RESET: &str = "\x1b[49m";

    // Strip trailing newline; we'll add it back at the end.
    let bare = line.strip_suffix('\n').unwrap_or(line);

    // Re-arm bg after every reset token inside the line.
    let rearmed = bare.replace(RESET, &format!("{}{}", RESET, CARD_BG));

    // Measure visible width of the re-armed content.
    let vis = visible_width(&rearmed);

    // Pad to fill the band up to `width`.
    let pad = if vis < width {
        " ".repeat(width - vis)
    } else {
        String::new()
    };

    format!("{}{}{}{}{}\n", CARD_BG, rearmed, pad, BG_RESET, RESET)
}

pub fn render(rows: &[TabRow], opts: &RenderOpts) -> String {
    let mut out = String::new();
    if rows.is_empty() {
        return out;
    }
    let width = opts.width;
    let accent = Role::Accent.ansi();

    let body_budget = opts.height.saturating_sub(header_lines(rows, opts.header));
    let (plan, strip_folded, gap_used) = plan_layout(rows, body_budget, opts.density);
    // Overflow = any row is absent from the plan (those are idle-folded rows).
    let overflow = plan.len() < rows.len();
    let count = if overflow {
        format!("{} ▲", rows.len())
    } else {
        format!("·{}", rows.len())
    };

    // Emit the identity header block only when configured on (and rows exist).
    // Header line 1: " RADAR" + right-aligned count.
    if opts.header {
        let title = " RADAR";
        let gap = width
            .saturating_sub(UnicodeWidthStr::width(title) + UnicodeWidthStr::width(count.as_str()))
            .max(1);
        out.push_str(&format!(
            "{}{}{}{}{}\n",
            accent, title, " ".repeat(gap), count, RESET
        ));
        // Header line 2: rule across the full width.
        out.push_str(&format!("{}{}{}\n", accent, "═".repeat(width), RESET));
    }

    let use_card_bg = opts.density == Density::Cards;
    for &(i, max_lines) in &plan {
        if use_card_bg {
            // Render the tab into a temporary buffer, then paint each content
            // line with the bright-black background band.
            let mut tab_buf = String::new();
            render_row(&mut tab_buf, &rows[i], opts, max_lines.max(1));
            for line in tab_buf.split_inclusive('\n') {
                out.push_str(&paint_card_line(line, width));
            }
        } else {
            render_row(&mut out, &rows[i], opts, max_lines.max(1));
        }
        // Emit blank gap line(s) after each tab's content block when density > Compact.
        // Gap lines stay rail-background (NOT painted).
        for _ in 0..gap_used {
            out.push('\n');
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
        out.push_str(&format!("{}{}{}\n", Role::Accent.ansi(), clamped, RESET));
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
        RenderOpts { width, height: 100, now_tick, glyphs: GlyphSet::Plain, header: true, density: crate::config::Density::Compact }
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
        assert_eq!(header_lines(&rows, true), 2);
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
        assert_eq!(header_lines(&rows, true), 0);
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
    fn working_glyph_spins_with_tick() {
        let d = Detail { repo: "r".into(), branch: "b".into(), msg: "".into(),
                         since_tick: 0, status: Status::Running };
        let row = |_t| TabRow { number: 1, name: "n".into(), active: false, has_bell: false,
                               agg: agg(Status::Running, 0, 1, Some(d.clone())) };
        let f0 = render(&[row(0)], &RenderOpts { width: 30, height: 100, now_tick: 0, glyphs: GlyphSet::Plain, header: true, density: crate::config::Density::Compact });
        let f1 = render(&[row(1)], &RenderOpts { width: 30, height: 100, now_tick: 1, glyphs: GlyphSet::Plain, header: true, density: crate::config::Density::Compact });
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
        let s = render(&rows, &RenderOpts { width: 24, height: 6, now_tick: 0, glyphs: GlyphSet::Plain, header: true, density: crate::config::Density::Compact });
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
        let s = render(&rows, &RenderOpts { width: 30, height: 8, now_tick: 2, glyphs: GlyphSet::Plain, header: true, density: crate::config::Density::Compact });
        assert!(s.contains("pinky"));     // urgent row never folded
        assert!(s.contains("needs you")); // its detail survives
    }

    #[test]
    fn no_overflow_when_everything_fits() {
        let rows: Vec<TabRow> = (1..=3).map(idle_row).collect();
        let s = render(&rows, &RenderOpts { width: 24, height: 40, now_tick: 0, glyphs: GlyphSet::Plain, header: true, density: crate::config::Density::Compact });
        assert!(!s.contains("idle ▾"));
        assert!(!s.lines().next().unwrap().contains('▲'));
    }

    #[test]
    fn render_emits_only_role_ansi_colors() {
        // Spec §14.10: renderer must use only ANSI-16 palette role codes —
        // no truecolor (38;2;/48;2;) and no raw '#' hex literals.
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

        let opts = RenderOpts { width: 30, height: 100, now_tick: 7, glyphs: GlyphSet::Plain, header: true, density: crate::config::Density::Compact };
        let s = render(&rows, &opts);

        // Must NOT contain truecolor sequences
        assert!(!s.contains("38;2;"),
            "truecolor foreground sequence found in render output");
        assert!(!s.contains("48;2;"),
            "truecolor background sequence found in render output");
        // Must NOT contain raw hex color literals
        assert!(!s.contains('#'),
            "'#' hex color literal found in render output");
        // Must contain the accent role code (used for header + bars)
        assert!(s.contains(Role::Accent.ansi()),  // "\x1b[35m"
            "expected accent role ANSI code not found");
        // Must contain the attention role code (pending row)
        assert!(s.contains(Role::Attention.ansi()),  // "\x1b[91m"
            "expected attention role ANSI code not found");
        // Must contain the working role code (running row)
        assert!(s.contains(Role::Working.ansi()),  // "\x1b[33m"
            "expected working role ANSI code not found");
        // Must contain the error role code
        assert!(s.contains(Role::Error.ansi()),  // "\x1b[31m"
            "expected error role ANSI code not found");
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
            let s = render(&rows, &RenderOpts { width, height: 6, now_tick: 0, glyphs: GlyphSet::Plain, header: true, density: crate::config::Density::Compact });
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
        assert_eq!(header_lines(&rows, false), 0);
        let opts = RenderOpts { width: 24, height: 100, now_tick: 0, glyphs: GlyphSet::Plain, header: false, density: crate::config::Density::Compact };
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
                width, height: 100, now_tick: 0, glyphs: GlyphSet::Plain, header: true, density: crate::config::Density::Compact,
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
        let opts = RenderOpts { width: 30, height: 7, now_tick: 0, glyphs: GlyphSet::Plain, header: true, density: crate::config::Density::Compact };
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
        let opts = RenderOpts { width: 24, height: 3, now_tick: 0, glyphs: GlyphSet::Plain, header: true, density: crate::config::Density::Compact };
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
            header: true, density: crate::config::Density::Compact,
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
        // Cards now adds a background band on content lines — output differs from Comfortable.
        let rows: Vec<TabRow> = (1..=3).map(idle_row).collect();
        let comfortable = render(&rows, &RenderOpts {
            width: 24, height: 100, now_tick: 0, glyphs: GlyphSet::Plain,
            header: true, density: crate::config::Density::Comfortable,
        });
        let cards = render(&rows, &RenderOpts {
            width: 24, height: 100, now_tick: 0, glyphs: GlyphSet::Plain,
            header: true, density: crate::config::Density::Cards,
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
            header: true, density: crate::config::Density::Comfortable,
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
        }
    }

    #[test]
    fn cards_paint_content_lines_with_bg() {
        // Render an idle tab and an active working tab at normal width with Cards.
        // Content lines must contain \x1b[100m; gap lines and header must NOT.
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

        // lines[0] and lines[1] are the header (RADAR + rule) — must NOT have bg
        assert!(!lines[0].contains("\x1b[100m"),
            "header title line must NOT have card bg: {:?}", lines[0]);
        assert!(!lines[1].contains("\x1b[100m"),
            "header rule line must NOT have card bg: {:?}", lines[1]);

        // lines[2] is the idle tab content line — MUST have bg
        assert!(lines[2].contains("\x1b[100m"),
            "idle content line must have card bg: {:?}", lines[2]);

        // lines[3] is a blank gap line — must NOT have bg
        assert!(!lines[3].contains("\x1b[100m"),
            "gap line must NOT have card bg: {:?}", lines[3]);

        // lines[4] is the working tab line 1 — MUST have bg
        assert!(lines[4].contains("\x1b[100m"),
            "working tab line 1 must have card bg: {:?}", lines[4]);

        // lines[5] is the working tab detail line — MUST have bg
        assert!(lines[5].contains("\x1b[100m"),
            "working tab detail line must have card bg: {:?}", lines[5]);

        // Each content line must end with bg reset (\x1b[49m) before \x1b[0m
        assert!(lines[2].contains("\x1b[49m"),
            "content line must contain bg reset: {:?}", lines[2]);
        assert!(lines[4].contains("\x1b[49m"),
            "content line must contain bg reset: {:?}", lines[4]);
        assert!(lines[5].contains("\x1b[49m"),
            "detail line must contain bg reset: {:?}", lines[5]);
    }

    #[test]
    fn cards_band_fills_full_width() {
        // Short-name idle tab at width 24, Cards: visible width of painted line == 24.
        let rows = vec![
            TabRow { number: 1, name: "x".into(), active: false, has_bell: false,
                     agg: agg(Status::Idle, 0, 0, None) },
        ];
        let width = 24usize;
        let s = render(&rows, &ro_cards(width, 100));
        // Skip 2 header lines; first body line is the painted content line.
        let body: Vec<&str> = s.lines().skip(2).collect();
        let content_line = body[0];
        assert!(content_line.contains("\x1b[100m"),
            "content line must have card bg: {:?}", content_line);
        // Visible width must equal exactly `width`.
        let vw = visible_len(content_line);
        assert_eq!(vw, width,
            "painted content line visible width must equal {} (full band), got {}: {:?}",
            width, vw, content_line);
    }

    #[test]
    fn cards_rearm_bg_after_resets() {
        // Active working tab (line has multiple role-colored tokens with \x1b[0m resets)
        // under Cards: assert \x1b[0m\x1b[100m appears (reset immediately followed by bg re-arm).
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
        // The re-arm sequence is RESET followed immediately by CARD_BG.
        assert!(s.contains("\x1b[0m\x1b[100m"),
            "reset immediately followed by bg re-arm must appear in Cards output: {:?}", s);
    }

    #[test]
    fn comfortable_and_compact_emit_no_bg() {
        // Same tabs with Comfortable and Compact must contain NO \x1b[100m.
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
                header: true, density,
            });
            assert!(!s.contains("\x1b[100m"),
                "density {:?} must NOT emit card bg \x1b[100m: {:?}", density, s);
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

}