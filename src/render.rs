//! Pure renderer: per-tab rows → ANSI string. No zellij-tile dependency.

use crate::model::TabAgg;
use crate::status::{Role, Status};
pub use crate::status::GlyphSet;

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";

pub struct RenderOpts {
    pub width: usize,
    pub height: usize,
    pub now_tick: u64,
    pub glyphs: GlyphSet,
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
    if s.chars().count() <= max {
        s.to_string()
    } else if max == 0 {
        String::new()
    } else {
        let kept: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", kept)
    }
}

/// Single source of truth for how many lines a tab row occupies.
pub fn row_lines(agg: &TabAgg) -> usize {
    match agg.status {
        Status::Idle | Status::Done => 1,
        Status::Running | Status::Error => {
            if agg.detail.is_some() { 2 } else { 1 }
        }
        Status::Pending => match &agg.detail {
            Some(d) if !d.msg.trim().is_empty() => 3,
            Some(_) => 2,
            None => 1,
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
        Status::Error => if width < 16 { "err".to_string() } else { "failed".to_string() },
    }
}

/// The rail's identity header is two lines (title + rule) whenever any rows
/// exist (always-on identity). Single source of truth for the header's
/// vertical span (consumed by click mapping in lib.rs). Only the truly-empty
/// case (no rows at all) is headerless.
pub fn header_lines(rows: &[TabRow]) -> usize {
    if rows.is_empty() {
        0
    } else {
        2
    }
}

pub fn render(rows: &[TabRow], opts: &RenderOpts) -> String {
    let mut out = String::new();
    if rows.is_empty() {
        return out;
    }
    let width = opts.width;
    let now_tick = opts.now_tick;
    let accent = Role::Accent.ansi();

    // Always emit the header block for non-empty rows (always-on rail identity).
    // Header line 1: " AGENTS" + right-aligned "·N" tab count.
    let title = " AGENTS";
    let count = format!("·{}", rows.len());
    let gap = width
        .saturating_sub(title.chars().count() + count.chars().count())
        .max(1);
    out.push_str(&format!(
        "{}{}{}{}{}\n",
        accent, title, " ".repeat(gap), count, RESET
    ));
    // Header line 2: rule across the full width.
    out.push_str(&format!("{}{}{}\n", accent, "═".repeat(width), RESET));

    for row in rows {
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
        let prefix_len = 1 + 1 + 1 + num.chars().count() + 1; // bar+glyph+sp+num+sp
        let bell_len = if row.has_bell { 2 } else { 0 };
        let slot_len = slot.chars().count();
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
        let used = prefix_len + name.chars().count() + bell_len + slot_len;
        let gap = width.saturating_sub(used).max(1);
        out.push_str(&format!(
            "{}{} {} {}{}{}{}\n",
            bar, glyph, num, name_styled, " ".repeat(gap), bell, slot_styled
        ));

        // detail lines, per state.
        if let Some(d) = &row.agg.detail {
            let muted = Role::Muted.ansi();
            match st {
                Status::Running => {
                    let spin = crate::status::msg_spin(now_tick as usize);
                    let avail = width.saturating_sub(3);
                    let full = {
                        let loc = if d.branch.is_empty() { d.repo.clone() } else { format!("{}/{}", d.repo, d.branch) };
                        format!("{} {} {}", loc, spin, d.msg)
                    };
                    let body = if full.chars().count() <= avail {
                        full
                    } else {
                        // drop branch
                        let no_branch = format!("{} {} {}", d.repo, spin, d.msg);
                        if no_branch.chars().count() <= avail {
                            no_branch
                        } else {
                            // drop message, keep repo + spinner
                            truncate(&format!("{} {}", d.repo, spin), avail)
                        }
                    };
                    out.push_str(&format!("   {}{}{}\n", Role::Muted.ansi(), truncate(&body, avail), RESET));
                }
                Status::Error => {
                    let loc = if d.branch.is_empty() { d.repo.clone() } else { format!("{}/{}", d.repo, d.branch) };
                    let body = if d.msg.trim().is_empty() { loc } else { format!("{} · {}", loc, d.msg) };
                    out.push_str(&format!("   {}{}{}\n", muted, truncate(&body, width.saturating_sub(3)), RESET));
                }
                Status::Pending => {
                    let loc = if d.branch.is_empty() { d.repo.clone() } else { d.branch.clone() };
                    out.push_str(&format!("   {}{} · {}needs you{}\n", muted, truncate(&loc, width.saturating_sub(15)), Role::Attention.ansi(), RESET));
                    if !d.msg.trim().is_empty() {
                        out.push_str(&format!("   {}\"{}\"{}\n", muted, truncate(&d.msg, width.saturating_sub(5)), RESET));
                    }
                }
                Status::Done | Status::Idle => {}
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Detail;

    fn agg(status: Status, done: usize, total: usize, detail: Option<Detail>) -> TabAgg {
        TabAgg { status, done, total, detail }
    }

    fn ro(width: usize, now_tick: u64) -> RenderOpts {
        RenderOpts { width, height: 100, now_tick, glyphs: GlyphSet::Plain }
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
        assert_eq!(header_lines(&rows), 2);
        let s = render(&rows, &ro(24, 0));
        let mut lines = s.lines();
        let title = lines.next().unwrap();
        let rule = lines.next().unwrap();
        assert!(title.contains("AGENTS"));
        assert!(title.contains("·1")); // one tab
        assert!(rule.contains('═'));
    }

    #[test]
    fn header_absent_for_empty_rows() {
        let rows: Vec<TabRow> = vec![];
        assert_eq!(header_lines(&rows), 0);
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
        let f0 = render(&[row(0)], &RenderOpts { width: 30, height: 100, now_tick: 0, glyphs: GlyphSet::Plain });
        let f1 = render(&[row(1)], &RenderOpts { width: 30, height: 100, now_tick: 1, glyphs: GlyphSet::Plain });
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

    /// Strip `\x1b[...m` SGR escape sequences and count remaining visible chars.
    fn visible_len(line: &str) -> usize {
        let mut count = 0usize;
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
                count += 1;
            }
        }
        count
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
}
