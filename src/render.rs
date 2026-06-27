//! Pure renderer: per-tab rows → ANSI string. No zellij-tile dependency.

use crate::model::TabAgg;
use crate::status::{Role, Status};
pub use crate::status::GlyphSet;

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const YELLOW: &str = "\x1b[33m";

pub struct RenderOpts {
    pub width: usize,
    pub height: usize,
    pub now_tick: u64,
    pub glyphs: GlyphSet,
}

/// A `running` agent whose elapsed time reaches this (seconds ≈ ticks) is
/// flagged as long-running / possibly stuck.
const STUCK_SECS: u64 = 600;

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

/// Second-line tag for a tab's aggregate state.
fn detail_tag(agg: &TabAgg, now_tick: u64) -> String {
    let Some(d) = &agg.detail else { return String::new() };
    let elapsed = now_tick.saturating_sub(d.since_tick);
    match d.status {
        Status::Done => format!("done {}", format_elapsed(elapsed)),
        Status::Running => {
            let e = format_elapsed(elapsed);
            if elapsed >= STUCK_SECS {
                format!("{} ⚠", e)
            } else {
                e
            }
        }
        Status::Pending => "needs you".to_string(),
        Status::Error => "error".to_string(),
        Status::Idle => String::new(),
    }
}

/// Single source of truth for how many lines a tab row occupies.
/// - plain tab (no detail): 1 line
/// - agent tab with empty msg: 2 lines
/// - agent tab with non-empty msg: 3 lines
pub fn row_lines(agg: &TabAgg) -> usize {
    match &agg.detail {
        None => 1,
        Some(d) if d.msg.trim().is_empty() => 2,
        Some(_) => 3,
    }
}

/// The rail's identity header is two lines (title + rule) whenever any rows
/// with active (non-idle) status exist. Single source of truth for the
/// header's vertical span (consumed by click mapping in lib.rs).
pub fn header_lines(rows: &[TabRow]) -> usize {
    if rows.iter().any(|r| r.agg.status.is_active()) {
        2
    } else {
        0
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

    // Only emit the header block when at least one row is active (non-idle).
    if rows.iter().any(|r| r.agg.status.is_active()) {
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
    }

    for row in rows {
        let dot = format!("{}{}{}", row.agg.status.ansi(), row.agg.status.glyph(), RESET);
        let count = if row.agg.total > 1 {
            format!(" {}/{}", row.agg.done, row.agg.total)
        } else {
            String::new()
        };
        // reserve 2 cols for " ⚑" when a bell is set
        let bell_budget = if row.has_bell { 2 } else { 0 };
        let name_budget = width.saturating_sub(4 + count.chars().count() + bell_budget);
        let name = truncate(&row.name, name_budget);
        let name_styled = if row.active {
            format!("{}{}{}", BOLD, name, RESET)
        } else {
            name
        };
        let bell = if row.has_bell {
            format!(" {}⚑{}", YELLOW, RESET)
        } else {
            String::new()
        };
        // line 1: "<dot> <n> <name><count><bell>"
        out.push_str(&format!("{} {} {}{}{}\n", dot, row.number, name_styled, count, bell));

        // line 2: "  repo/branch · tag"  (only when there is agent detail)
        if let Some(d) = &row.agg.detail {
            let loc = if d.branch.is_empty() {
                d.repo.clone()
            } else {
                format!("{}/{}", d.repo, d.branch)
            };
            let tag = detail_tag(&row.agg, now_tick);
            let second = truncate(&format!("{} · {}", loc, tag), width.saturating_sub(2));
            out.push_str(&format!("  {}\n", second));

            // line 3: `  "msg"` (only when msg is non-empty). The fixed
            // overhead is 4 cols (2 spaces + 2 quotes), so reserve 4.
            if !d.msg.trim().is_empty() {
                let msg_line = format!("  \"{}\"\n", truncate(&d.msg, width.saturating_sub(4)));
                out.push_str(&msg_line);
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
        assert_eq!(s.matches('\n').count(), 1); // no header (idle), just tab row
        assert!(s.contains(Status::Idle.glyph()));
    }

    #[test]
    fn agent_tab_has_three_lines_with_count_tag_and_msg() {
        let detail = Detail {
            repo: "pinky".into(),
            branch: "fix/x".into(),
            msg: "doing the thing".into(),
            since_tick: 0,
            status: Status::Running,
        };
        let rows = vec![TabRow {
            number: 2,
            name: "pinky".into(),
            active: true,
            has_bell: false,
            agg: agg(Status::Running, 2, 4, Some(detail)),
        }];
        let s = render(&rows, &ro(24, 14));
        assert!(s.contains("2/4"));
        assert!(s.contains("pinky/fix/x"));
        assert!(s.contains("0:14"));
        assert_eq!(s.matches('\n').count(), 5); // header (2) + three tab lines
        assert!(s.contains("\"doing the thing\""));
    }

    #[test]
    fn agent_tab_with_empty_msg_has_two_lines() {
        let detail = Detail {
            repo: "pinky".into(),
            branch: "fix/x".into(),
            msg: "   ".into(), // whitespace-only → empty
            since_tick: 0,
            status: Status::Running,
        };
        let rows = vec![TabRow {
            number: 2,
            name: "pinky".into(),
            active: true,
            has_bell: false,
            agg: agg(Status::Running, 2, 4, Some(detail)),
        }];
        let s = render(&rows, &ro(24, 14));
        assert_eq!(s.matches('\n').count(), 4); // header (2) + two tab lines, no quoted line
        assert!(!s.contains('"'));
    }

    #[test]
    fn row_lines_all_three_cases() {
        // None → 1
        let plain = agg(Status::Idle, 0, 0, None);
        assert_eq!(row_lines(&plain), 1);

        // detail with empty msg → 2
        let empty_msg = agg(
            Status::Running,
            1,
            1,
            Some(Detail {
                repo: "r".into(),
                branch: "b".into(),
                msg: "  ".into(),
                since_tick: 0,
                status: Status::Running,
            }),
        );
        assert_eq!(row_lines(&empty_msg), 2);

        // detail with non-empty msg → 3
        let with_msg = agg(
            Status::Running,
            1,
            1,
            Some(Detail {
                repo: "r".into(),
                branch: "b".into(),
                msg: "hello".into(),
                since_tick: 0,
                status: Status::Running,
            }),
        );
        assert_eq!(row_lines(&with_msg), 3);
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
        // header (2) + three tab lines emitted
        assert_eq!(s.matches('\n').count(), 5);
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
    fn running_under_threshold_has_no_warning() {
        let detail = Detail { repo: "r".into(), branch: "b".into(), msg: "".into(), since_tick: 0, status: Status::Running };
        let rows = vec![TabRow { number: 1, name: "t".into(), active: false, has_bell: false, agg: agg(Status::Running, 1, 1, Some(detail)) }];
        assert!(!render(&rows, &ro(30, 599)).contains('⚠'));
    }

    #[test]
    fn running_at_threshold_shows_warning() {
        let detail = Detail { repo: "r".into(), branch: "b".into(), msg: "".into(), since_tick: 0, status: Status::Running };
        let rows = vec![TabRow { number: 1, name: "t".into(), active: false, has_bell: false, agg: agg(Status::Running, 1, 1, Some(detail)) }];
        assert!(render(&rows, &ro(30, 600)).contains('⚠'));
    }

    #[test]
    fn done_with_long_elapsed_has_no_warning() {
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
}
