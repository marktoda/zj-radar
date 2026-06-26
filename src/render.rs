//! Pure renderer: per-tab rows → ANSI string. No zellij-tile dependency.

use crate::model::TabAgg;
use crate::status::Status;

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";

pub struct TabRow {
    pub number: u32,
    pub name: String,
    pub active: bool,
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
        Status::Running => format_elapsed(elapsed),
        Status::Pending => "needs you".to_string(),
        Status::Error => "error".to_string(),
        Status::Idle => String::new(),
    }
}

pub fn render(rows: &[TabRow], width: usize, now_tick: u64) -> String {
    let mut out = String::new();
    for row in rows {
        let dot = format!("{}{}{}", row.agg.status.ansi(), row.agg.status.glyph(), RESET);
        let count = if row.agg.total > 1 {
            format!(" {}/{}", row.agg.done, row.agg.total)
        } else {
            String::new()
        };
        let name_budget = width.saturating_sub(4 + count.chars().count());
        let name = truncate(&row.name, name_budget);
        let name_styled = if row.active {
            format!("{}{}{}", BOLD, name, RESET)
        } else {
            name
        };
        // line 1: "<dot> <n> <name><count>"
        out.push_str(&format!("{} {} {}{}\n", dot, row.number, name_styled, count));

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

    #[test]
    fn format_elapsed_buckets() {
        assert_eq!(format_elapsed(14), "0:14");
        assert_eq!(format_elapsed(120), "2m");
        assert_eq!(format_elapsed(3780), "1h3m");
    }

    #[test]
    fn plain_tab_renders_name_only_no_second_line() {
        let rows = vec![TabRow {
            number: 4,
            name: "notes".into(),
            active: false,
            agg: agg(Status::Idle, 0, 0, None),
        }];
        let s = render(&rows, 24, 0);
        assert!(s.contains("notes"));
        assert_eq!(s.matches('\n').count(), 1); // single line
        assert!(s.contains(Status::Idle.glyph()));
    }

    #[test]
    fn agent_tab_has_two_lines_with_count_and_tag() {
        let detail = Detail {
            repo: "pinky".into(),
            branch: "fix/x".into(),
            msg: "m".into(),
            since_tick: 0,
            status: Status::Running,
        };
        let rows = vec![TabRow {
            number: 2,
            name: "pinky".into(),
            active: true,
            agg: agg(Status::Running, 2, 4, Some(detail)),
        }];
        let s = render(&rows, 24, 14);
        assert!(s.contains("2/4"));
        assert!(s.contains("pinky/fix/x"));
        assert!(s.contains("0:14"));
        assert_eq!(s.matches('\n').count(), 2); // two lines
    }

    #[test]
    fn narrow_width_truncates_with_ellipsis() {
        let rows = vec![TabRow {
            number: 1,
            name: "a-very-long-tab-name-indeed".into(),
            active: false,
            agg: agg(Status::Idle, 0, 0, None),
        }];
        let s = render(&rows, 12, 0);
        assert!(s.contains('…'));
    }
}
