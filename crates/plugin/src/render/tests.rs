use super::*;
use crate::kind::Kind;
use crate::rollup::{PrimaryDetail, ProgressCounts};

#[test]
fn truncate_does_not_strand_a_zwj_before_the_ellipsis() {
    // A ZWJ family emoji cut mid-cluster must not leave a dangling U+200D that
    // fuses with the appended '…'. The result ends in a clean ellipsis.
    let out = truncate("👨\u{200d}👩\u{200d}👧 done", 5);
    assert!(out.ends_with('…'), "ends in a clean ellipsis: {out:?}");
    let before_ellipsis = out.strip_suffix('…').unwrap();
    assert!(
        !before_ellipsis.ends_with('\u{200d}'),
        "no joiner strands right before the ellipsis: {out:?}"
    );
    // Plain ASCII still truncates by display width with a reserved ellipsis col.
    assert_eq!(truncate("abcdef", 3), "ab…");
    // Fits untouched.
    assert_eq!(truncate("abc", 3), "abc");
}

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
        now_epoch_s: 0,
        ledger: Vec::new(),
    }
}

/// `opts` with its height replaced by `rows`' exact natural content height
/// (leftover 0 ⇒ no bottom region — see `body_line_count`). `height: 100` was
/// always just an "enough, no overflow" sentinel pre-Task-13; tests that
/// assert exact line counts/content unrelated to the pinned footer use this
/// so a generous sentinel height doesn't pull the footer into their
/// expectations.
fn tight(rows: &[TabRow], opts: RenderOpts) -> RenderOpts {
    let height = body_line_count(rows, &opts);
    RenderOpts { height, ..opts }
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
    let rows = vec![TabRow { flash: false,
        number: 1,
        name: "a".into(),
        active: false,
        has_bell: false,
        display: display(Status::Running, 0, 0, None),
    }];
    assert_eq!(
        header_lines(&rows, true, crate::config::Density::Compact, true),
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
        header_lines(&rows, true, crate::config::Density::Compact, false),
        0
    );
    assert!(render(&rows, &ro(24, 0)).is_empty());
}

#[test]
fn header_present_for_empty_rows_with_ledger_history() {
    // Zero tracked tabs but a non-empty ledger is still "has_content" — the
    // header renders with an honest `·0` tab count (spec §9's floor).
    let rows: Vec<TabRow> = vec![];
    assert_eq!(
        header_lines(&rows, true, crate::config::Density::Compact, true),
        2
    );
}

#[test]
fn rendered_rail_tracks_targets_for_each_emitted_line() {
    assert_eq!(RenderedRail::empty().line_count(), 0);
    let untargeted = RenderedRail::from_ansi_without_targets("a\nb\n", 100);
    assert_eq!(untargeted.line_count(), 2);
    assert_eq!(untargeted.target_at_line(0), None);
    assert!(
        !untargeted.ansi.ends_with('\n'),
        "trailing newline popped, like from_lines"
    );
    // Clamp: only `height` lines survive.
    let clamped = RenderedRail::from_ansi_without_targets("a\nb\nc\n", 2);
    assert_eq!(clamped.line_count(), 2);
    assert_eq!(clamped.ansi, "a\nb");

    let detail = PrimaryDetail {
        repo: "repo".into(),
        branch: "main".into(),
        msg: "approve".into(),
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Pending,
    };
    let rows = vec![
        TabRow { flash: false,
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
                    PaneDisplay::tracked(10, Kind::Claude, Status::Pending, "approve".into(), String::new(), None),
                    PaneDisplay::tracked(11, Kind::Claude, Status::Running, "tests".into(), String::new(), None),
                ],
            },
        },
        TabRow { flash: false,
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
    let rows = vec![TabRow { flash: false,
        number: 4,
        name: "notes".into(),
        active: false,
        has_bell: false,
        display: display(Status::Idle, 0, 0, None),
    }];
    let s = render(&rows, &tight(&rows, ro(24, 0)));
    assert!(s.contains("notes"));
    assert_eq!(s.lines().count(), 3); // always-on header (2) + tab row (1)
    assert!(s.contains(Status::Idle.glyph_for(GlyphSet::Plain)));
}

#[test]
fn render_row_lines_by_state() {
    let opts = ro(40, 0);
    let mk_row = |d: TabDisplay, active: bool| TabRow { flash: false,
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
            task: String::new(),
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
        TabRow { flash: false,
            number: 1,
            name: "a".into(),
            active: true,
            has_bell: false,
            display: display(Status::Idle, 0, 0, None),
        },
        TabRow { flash: false,
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
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Pending,
    };
    let rows = vec![TabRow { flash: false,
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
            task: String::new(),
            kind: Kind::Claude,
            since_tick: 0,
            outcome: None,
            status,
        };
        TabRow { flash: false,
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
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Running,
    };
    let rows = vec![TabRow { flash: false,
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
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Running,
    };
    let row = |_t| TabRow { flash: false,
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
            now_epoch_s: 0,
            ledger: Vec::new(),
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
            now_epoch_s: 0,
            ledger: Vec::new(),
        },
    );
    assert!(f0.contains('⠋'));
    assert!(f1.contains('⠙'));
}

#[test]
fn idle_row_is_single_line_with_no_right_slot_text() {
    let rows = vec![TabRow { flash: false,
        number: 7,
        name: "logs".into(),
        active: false,
        has_bell: false,
        display: display(Status::Idle, 0, 0, None),
    }];
    let s = render(&rows, &tight(&rows, ro(24, 0)));
    assert_eq!(s.lines().skip(2).count(), 1); // exactly one body line
    assert!(s.contains('○'));
    assert!(s.contains("logs"));
}

#[test]
fn narrow_width_truncates_with_ellipsis() {
    let rows = vec![TabRow { flash: false,
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
        task: String::new(),
        since_tick: 0,
        outcome: None,
        kind: Kind::Claude,
        status: Status::Running,
    };
    let rows = vec![TabRow { flash: false,
        number: 2,
        name: "a-very-long-tab-name-indeed".into(),
        active: true, // exercises BOLD escapes too
        has_bell: false,
        display: display(Status::Running, 2, 4, Some(detail)),
    }];
    let s = render(&rows, &tight(&rows, ro(width, 14)));
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
        task: String::new(),
        since_tick: 0,
        outcome: None,
        kind: Kind::Claude,
        status: Status::Pending,
    };
    let rows = vec![TabRow { flash: false,
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
fn bell_row_never_exceeds_width_even_when_extremely_narrow() {
    // Regression: the bell marker (⚑ + space, 2 cols) is appended after the
    // prefix clamp, so at widths where the prefix already fills the column the
    // bell used to spill 2 cells past the edge — breaking the width invariant
    // and the card-padding math. Every width down to 1 must still fit.
    for active in [true, false] {
        let rows = vec![TabRow { flash: false,
            number: 7,
            name: "infra".into(),
            active,
            has_bell: true,
            display: display(Status::Running, 0, 1, None),
        }];
        // From width 4 up: the bell bug reproduced here (prefix fits but the
        // 2-col bell spilled past the edge). Widths 1–3 are a separate,
        // pre-existing degenerate case (the spine+glyph alone can't fit a
        // 1–2-col column) and are unreachable in any real layout.
        for width in 4usize..=16 {
            let s = render(&rows, &ro(width, 3));
            for line in s.lines() {
                assert!(
                    visible_len(line) <= width,
                    "bell row exceeds width {} (active={}): {:?} (visible {})",
                    width,
                    active,
                    line,
                    visible_len(line)
                );
            }
        }
    }
}

#[test]
fn running_has_no_warning_glyph() {
    let detail = PrimaryDetail {
        repo: "r".into(),
        branch: "b".into(),
        msg: "".into(),
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Running,
    };
    let rows = vec![TabRow { flash: false,
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
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Done,
    };
    let rows = vec![TabRow { flash: false,
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
    let rows = vec![TabRow { flash: false,
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
    let rows = vec![TabRow { flash: false,
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
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Error,
    };
    let rows = vec![TabRow { flash: false,
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
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Running,
    };
    let rows = vec![TabRow { flash: false,
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
    TabRow { flash: false,
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
            now_epoch_s: 0,
            ledger: Vec::new(),
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
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Pending,
    };
    rows.push(TabRow { flash: false,
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
            now_epoch_s: 0,
            ledger: Vec::new(),
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
            now_epoch_s: 0,
            ledger: Vec::new(),
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
        task: String::new(),
        since_tick: 0,
        outcome: None,
        kind: Kind::Claude,
        status,
    };

    let rows = vec![
        // idle — one line, no detail
        TabRow { flash: false,
            number: 1,
            name: "idle-tab".into(),
            active: false,
            has_bell: false,
            display: display(Status::Idle, 0, 0, None),
        },
        // running — two lines, with detail
        TabRow { flash: false,
            number: 2,
            name: "run-tab".into(),
            active: true,
            has_bell: false,
            display: display(Status::Running, 1, 2, Some(mk_detail(Status::Running))),
        },
        // pending with msg — three lines
        TabRow { flash: false,
            number: 3,
            name: "pend-tab".into(),
            active: false,
            has_bell: false,
            display: display(Status::Pending, 0, 1, Some(mk_detail(Status::Pending))),
        },
        // done — one line
        TabRow { flash: false,
            number: 4,
            name: "done-tab".into(),
            active: false,
            has_bell: false,
            display: display(Status::Done, 1, 1, Some(mk_detail(Status::Done))),
        },
        // error — two lines
        TabRow { flash: false,
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
        now_epoch_s: 0,
        ledger: Vec::new(),
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
        task: String::new(),
        since_tick: 0,
        outcome: None,
        kind: Kind::Claude,
        status: Status::Pending,
    };
    let rows = vec![TabRow { flash: false,
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
        task: String::new(),
        since_tick: 0,
        outcome: None,
        kind: Kind::Claude,
        status: Status::Pending,
    };
    let rows2 = [TabRow { flash: false,
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
        task: String::new(),
        since_tick: 0,
        outcome: None,
        kind: Kind::Claude,
        status: Status::Pending,
    };
    let rows3 = vec![TabRow { flash: false,
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
        task: String::new(),
        since_tick: 0,
        outcome: None,
        kind: Kind::Claude,
        status: Status::Pending,
    };
    let rows = vec![TabRow { flash: false,
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
fn zero_state_is_a_scanning_one_liner() {
    // Task 14: the onboarding face is the minimal scanning line, not the old
    // status-glyph legend — rail-reference.md rule 8 ("not a marketing
    // screen") now holds end-to-end.
    let s = onboarding(&ro(28, 0));
    assert!(s.ansi.contains("RADAR"));
    assert!(s.ansi.to_lowercase().contains("scanning… no agents yet"));
    // No legend glyph lines and no click hint — the panel is click-inert.
    assert!(!s.ansi.contains('◆'));
    assert!(!s.ansi.to_lowercase().contains("needs you"));
    assert!(!s.ansi.to_lowercase().contains("click"));
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
fn panel_faces_never_exceed_height() {
    // Regression: both first-run faces used to ignore `opts.height` and keep a
    // trailing newline, so a short rail pane scrolled the " RADAR" header (and
    // the "needs permission" warning) off the top — on exactly the screens a
    // brand-new user sees. Clamp discipline must match the rail's.
    for height in [0usize, 1, 2, 3, 5, 7, 12, 100] {
        for (name, face) in [
            ("onboarding", onboarding(&RenderOpts { height, ..ro(24, 0) })),
            ("needs_permission", needs_permission(&RenderOpts { height, ..ro(24, 0) })),
        ] {
            assert!(
                face.line_count() <= height,
                "{name} at height {height} emits {} lines",
                face.line_count()
            );
            // vt100 scrolls when the newline COUNT reaches the pane height
            // (an LF on the bottom row scrolls; content after it doesn't
            // matter). A clamped face must therefore stay strictly under.
            if height > 0 {
                let newlines = face.ansi.matches('\n').count();
                assert!(
                    newlines < height,
                    "{name} at height {height} emits {newlines} newlines (vt100 scroll)"
                );
            }
            if height >= 1 {
                let first = face.ansi.lines().next().unwrap_or("");
                assert!(
                    first.contains("RADAR"),
                    "{name} at height {height} lost its header: {first:?}"
                );
            }
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
                now_epoch_s: 0,
                ledger: Vec::new(),
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
        task: String::new(),
        since_tick: 0,
        outcome: None,
        kind: Kind::Claude,
        status: Status::Pending,
    };
    let rows = vec![TabRow { flash: false,
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
    let rows = vec![TabRow { flash: false,
        number: 1,
        name: "a".into(),
        active: false,
        has_bell: false,
        display: display(Status::Running, 0, 0, None),
    }];
    assert_eq!(
        header_lines(&rows, false, crate::config::Density::Compact, true),
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
        now_epoch_s: 0,
        ledger: Vec::new(),
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
    PaneDisplay::tracked(id, kind, status, msg.into(), String::new(), None)
}

/// Build a PaneDisplay carrying an end-result outcome, for tag tests.
fn pe_outcome(id: u32, kind: Kind, status: Status, msg: &str, outcome: Outcome) -> PaneDisplay {
    PaneDisplay::tracked(id, kind, status, msg.into(), String::new(), Some(outcome))
}

// ── End-result outcome tag rendering ──

#[test]
fn child_prefix_is_tree_connector_with_optional_spine() {
    let conn = Role::Muted.ansi(); // stand-in connector color
    // Inactive: a leading space (no spine), then the connector, then a
    // trailing space before the glyph — col 1 always holds the connector.
    let tee = child_prefix(false, Status::Running, Branch::Tee, conn);
    assert!(tee.starts_with(' '), "inactive col 0 is a plain space: {tee:?}");
    assert!(!tee.contains('▌'), "inactive has no spine: {tee:?}");
    assert!(tee.contains('├') && tee.ends_with(' '), "tee connector + space: {tee:?}");
    let elbow = child_prefix(false, Status::Running, Branch::Elbow, conn);
    assert!(elbow.contains('└'), "elbow connector: {elbow:?}");
    // Active: an accent spine at col 0, hue tracking the tab status (mauve
    // accent normally, peach attention when waiting/error) — the same
    // spine_role coupling as the line-1 bar — then the connector + space.
    let running = child_prefix(true, Status::Running, Branch::Tee, conn);
    assert!(running.starts_with(Role::Accent.ansi()), "accent spine: {running:?}");
    assert!(running.contains('▌') && running.contains('├') && running.ends_with(' '),
        "spine + connector + space: {running:?}");
    assert!(child_prefix(true, Status::Error, Branch::Tee, conn).starts_with(Role::Attention.ansi()));
    assert!(child_prefix(true, Status::Pending, Branch::Elbow, conn).starts_with(Role::Attention.ansi()));
}

#[test]
fn compose_activity_reserves_outcome_against_truncation() {
    let cmd_color = "\x1b[2m"; // stand-in; we assert on visible text + role
    // Wide: command and full tag both intact.
    let wide = compose_activity("cargo build", Some(Outcome::Failed(Some(1))), 30, cmd_color);
    assert!(wide.contains("cargo build"), "command shown: {:?}", wide);
    assert!(wide.contains("exit 1"), "full tag shown: {:?}", wide);
    assert!(wide.contains(Role::Error.ansi()), "tag is red: {:?}", wide);

    // Narrow: command is squeezed but the outcome survives in full.
    let narrow = compose_activity(
        "cargo build integration suite",
        Some(Outcome::Failed(Some(1))),
        14,
        cmd_color,
    );
    assert!(narrow.contains("exit 1"), "tag must survive truncation: {:?}", narrow);
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
fn done_command_line_has_no_trailing_tag_or_stray_sgr() {
    let s = compose_activity("cargo build", Some(Outcome::Ok), 30, "\x1b[90m");
    let plain = strip_sgr(&s);
    assert_eq!(plain, "cargo build", "no ✓ and no trailing space: {plain:?}");
    assert!(!s.contains("\x1b[32m\x1b[0m"), "no empty green SGR pair: {s:?}");
}

#[test]
fn error_tag_is_exit_n_without_duplicate_cross() {
    let s = strip_sgr(&compose_activity("cargo build", Some(Outcome::Failed(Some(1))), 30, "\x1b[90m"));
    assert_eq!(s, "cargo build exit 1");
    let unknown = strip_sgr(&compose_activity("make", Some(Outcome::Failed(None)), 30, "\x1b[90m"));
    assert_eq!(unknown, "make ✗");
}

#[test]
fn finished_command_line2_shows_role_colored_tag() {
    let mk = |status, outcome, msg: &str| {
        let d = PrimaryDetail {
            repo: "r".into(),
            branch: "".into(),
            msg: msg.into(),
            task: String::new(),
            since_tick: 0,
            status,
            kind: Kind::Build,
            outcome,
        };
        TabRow { flash: false,
            number: 1,
            name: "web".into(),
            active: false,
            has_bell: false,
            display: display(status, 1, 1, Some(d)),
        }
    };
    let done = render(&[mk(Status::Done, Some(Outcome::Ok), "cargo build")], &ro(30, 0));
    let dline = done.lines().find(|l| l.contains("cargo build")).unwrap();
    assert!(
        !dline.contains('✓') && strip_sgr(dline).trim() == "⚙ cargo build",
        "done line carries no tag — the line-1 glyph is the one done signal: {:?}",
        dline
    );

    let err = render(
        &[mk(Status::Error, Some(Outcome::Failed(Some(2))), "cargo build")],
        &ro(30, 0),
    );
    let eline = err.lines().find(|l| l.contains("cargo build")).unwrap();
    assert!(
        eline.contains("exit 2") && eline.contains(Role::Error.ansi()),
        "error tag red exit 2: {:?}",
        eline
    );
}

#[test]
fn multi_pane_finished_command_shows_outcome_tag() {
    let a = display_multi(vec![
        pe(1, Kind::Build, Status::Running, "cargo build"),
        pe_outcome(2, Kind::Test, Status::Done, "cargo test", Outcome::Ok),
    ]);
    let row = TabRow { flash: false,
        number: 1,
        name: "ci".into(),
        active: false,
        has_bell: false,
        display: a,
    };
    let s = render(&[row], &ro(30, 0));
    let line = s.lines().find(|l| l.contains("cargo test")).unwrap();
    // No outcome tag for Ok — the pane's own status glyph (still green, from
    // Role::Success) is the one done signal; the line ends at the command.
    assert!(
        !line.contains('✓') && strip_sgr(line).trim_end().ends_with("cargo test"),
        "no outcome tag on a done pane line: {:?}",
        line
    );
    assert!(line.contains(Role::Success.ansi()), "pane glyph still green: {:?}", line);
}

#[test]
fn nerd_set_renders_robot_mark_for_claude() {
    let d = PrimaryDetail {
        repo: "r".into(),
        branch: "b".into(),
        msg: "thinking".into(),
        task: String::new(),
        since_tick: 0,
        status: Status::Running,
        kind: Kind::Claude,
        outcome: None,
    };
    let rows = vec![TabRow { flash: false,
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
            task: String::new(),
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
    let row_inactive = TabRow { flash: false, number: 1, name: "t".into(), active: false, has_bell: false, display: a.clone() };
    let row_active = TabRow { flash: false, number: 1, name: "t".into(), active: true, has_bell: false, display: a };
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
    let row = TabRow { flash: false,
        number: 7,
        name: "monorepo".into(),
        active: false,
        has_bell: false,
        display: a,
    };
    let rows = [row];
    let s = render(&rows, &tight(&rows, ro(30, 0)));
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
    // Tree connectors join the panes to the tab: `├` for the non-final
    // children, `└` for the last one.
    assert!(s.contains('├'), "non-final children use a tee: {:?}", s);
    assert!(s.contains('└'), "the last child uses an elbow: {:?}", s);
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
    let row = TabRow { flash: false,
        number: 1,
        name: "team".into(),
        active: false,
        has_bell: false,
        display: a,
    };
    let rows = [row];
    let s = render(&rows, &tight(&rows, ro(30, 0)));
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
    let row = TabRow { flash: false,
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
    let rows = [row];
    let s = render(&rows, &tight(&rows, ro(30, 0)));
    let body: Vec<&str> = s.lines().skip(2).collect();

    // With 0 tracked panes: single-pane path, Idle → 1 line (header only).
    assert_eq!(body.len(), 1, "only header line, no pane lines: {:?}", s);
    // No "2 panes" summary (untracked panes don't get their own lines).
    assert!(!s.contains("2 panes"), "no untracked summary: {:?}", s);
}

#[test]
fn multi_pane_mixed_untracked_summary_names_panes() {
    let row = TabRow { flash: false,
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
                task: String::new(),
                since_tick: 0,
                outcome: None,
                status: Status::Running,
                kind: Kind::Codex,
            }),
            panes: vec![
                PaneDisplay::tracked(1, Kind::Codex, Status::Running, "tests".into(), String::new(), None),
                PaneDisplay::untracked(2, "shell"),
            ],
        },
    };
    let rows = [row];
    let s = render(&rows, &tight(&rows, ro(30, 0)));
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
    let row = TabRow { flash: false,
        number: 1,
        name: "team".into(),
        active: true,
        has_bell: false,
        display: a,
    };
    let rows = [row];
    let s = render(&rows, &tight(&rows, ro(30, 0)));
    let body: Vec<&str> = s.lines().skip(2).collect();
    // header + 2 pane lines, no collapse.
    assert_eq!(body.len(), 3, "active: header + 2 pane lines: {:?}", s);
    assert!(
        !s.contains("more working"),
        "no collapse line: {:?}",
        s
    );
    // Tree connectors present: first child is a tee, last child an elbow.
    assert!(body[1].contains('├'), "first child uses a tee: {:?}", body[1]);
    assert!(body[2].contains('└'), "last child uses an elbow: {:?}", body[2]);
    // Active pane lines have the spine ▌ (at col 0, before the connector).
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
        let row = TabRow { flash: false,
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
    let row_check = TabRow { flash: false, number: 1, name: "solo".into(), active: false, has_bell: false, display: a.clone() };
    assert_eq!(
        render_row(&row_check, &opts).len(),
        2,
        "single-pane pending+msg = 2 lines (chunk-1)"
    );
    let row = TabRow { flash: false,
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
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Running,
    };
    let detail_pending = PrimaryDetail {
        repo: "urgent-proj".into(),
        branch: "fix/thing".into(),
        msg: "please review".into(),
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Pending,
    };

    let rows = vec![
        TabRow { flash: false,
            number: 1,
            name: "r1".into(),
            active: false,
            has_bell: false,
            display: display(Status::Running, 0, 1, Some(detail_running(1))),
        },
        TabRow { flash: false,
            number: 2,
            name: "r2".into(),
            active: false,
            has_bell: false,
            display: display(Status::Running, 0, 1, Some(detail_running(2))),
        },
        TabRow { flash: false,
            number: 3,
            name: "r3".into(),
            active: false,
            has_bell: false,
            display: display(Status::Running, 0, 1, Some(detail_running(3))),
        },
        TabRow { flash: false,
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
        now_epoch_s: 0,
        ledger: Vec::new(),
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
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Pending,
    };
    let rows = vec![
        TabRow { flash: false,
            number: 1,
            name: "pending".into(),
            active: false,
            has_bell: false,
            display: display(Status::Pending, 0, 1, Some(detail.clone())),
        },
        TabRow { flash: false,
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
        now_epoch_s: 0,
        ledger: Vec::new(),
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
        now_epoch_s: 0,
        ledger: Vec::new(),
    }
}

#[test]
fn comfortable_inserts_blank_line_between_tabs() {
    // 3 idle tabs, large height → comfortable density inserts a gap after each tab.
    // body = header(2) + 3 content lines + 3 gap lines = 8 total lines.
    // With trailing \n stripped: the last gap line's newline is removed, so
    // .lines() sees 7 lines (the trailing blank gap is consumed by the strip).
    let rows: Vec<TabRow> = (1..=3).map(idle_row).collect();
    let s = render(&rows, &tight(&rows, ro_comfortable(24, 100)));
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
        now_epoch_s: 0,
        ledger: Vec::new(),
    };
    let s = render(&rows, &tight(&rows, opts));
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
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Running,
    };
    let rows = vec![
        TabRow { flash: false,
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
            now_epoch_s: 0,
            ledger: Vec::new(),
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
            now_epoch_s: 0,
            ledger: Vec::new(),
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
            now_epoch_s: 0,
            ledger: Vec::new(),
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
        now_epoch_s: 0,
        ledger: Vec::new(),
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
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Running,
    };
    let rows = vec![
        TabRow { flash: false,
            number: 1,
            name: "idle".into(),
            active: false,
            has_bell: false,
            display: display(Status::Idle, 0, 0, None),
        },
        TabRow { flash: false,
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
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Done,
    };
    let rows = vec![TabRow { flash: false,
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
fn active_card_bg_spans_full_width_on_every_line() {
    // Repro for a live-build report: the focused card's background highlight
    // appeared to stop short of the rail's right edge. `cards_band_fills_full_width`
    // above only checks the *raw* line's visible width, which is guaranteed by
    // `paint_card_line`'s own padding math and so can't catch a bug that only
    // shows up once a real terminal parses the escapes — e.g. the bell slot
    // landing after the width-filling pad, or a glyph (`⚑`, `▌`) whose real
    // column count disagrees with `visible_width`'s count. This test goes one
    // level deeper: feed the ANSI through a real vt100 grid and assert the
    // background color of *every* cell, column by column, on every line of an
    // active 2-line card — with and without the bell — at several widths.
    let theme = crate::theme::DerivedColors::default();
    let expected = vt100::Color::Rgb(
        theme.surface_active.0,
        theme.surface_active.1,
        theme.surface_active.2,
    );

    for width in [20usize, 24, 30] {
        for has_bell in [false, true] {
            let detail = PrimaryDetail {
                repo: "repo".into(),
                branch: "main".into(),
                msg: "working".into(),
                task: String::new(),
                kind: Kind::Claude,
                since_tick: 0,
                outcome: None,
                status: Status::Running,
            };
            let rows = vec![TabRow { flash: false,
                number: 1,
                name: "focus".into(),
                active: true,
                has_bell,
                display: display(Status::Running, 0, 1, Some(detail)),
            }];
            let raw = render(&rows, &ro_cards(width, 100));
            let lines: Vec<&str> = raw.lines().collect();
            let active_rows: Vec<usize> = lines
                .iter()
                .enumerate()
                .filter(|(_, l)| surface_of(l) == Surface::Active)
                .map(|(i, _)| i)
                .collect();
            assert_eq!(
                active_rows.len(),
                2,
                "expected a 2-line active card (label + detail) at width {width} bell {has_bell}: {:?}",
                lines
            );

            // +1 row of headroom, matching the `grid()` helper's convention, so a
            // trailing gap row can't scroll content off the top of the parsed screen.
            let height = (lines.len().max(1) + 1) as u16;
            let mut parser = vt100::Parser::new(height, width as u16, 0);
            parser.process(raw.replace('\n', "\r\n").as_bytes());
            let screen = parser.screen();
            for &row in &active_rows {
                for col in 0..width as u16 {
                    let cell = screen.cell(row as u16, col).unwrap_or_else(|| {
                        panic!("missing vt100 cell at row {row} col {col} (width {width} bell {has_bell})")
                    });
                    assert_eq!(
                        cell.bgcolor(),
                        expected,
                        "active card bg gap at width {width} bell {has_bell}, row {row} col {col}\nrow text: {:?}",
                        lines[row]
                    );
                }
            }
        }
    }
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
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Running,
    };
    let rows = vec![TabRow { flash: false,
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
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Running,
    };
    let rows = vec![
        TabRow { flash: false,
            number: 1,
            name: "idle".into(),
            active: false,
            has_bell: false,
            display: display(Status::Idle, 0, 0, None),
        },
        TabRow { flash: false,
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
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Running,
    };
    let rows = vec![
        TabRow { flash: false,
            number: 1,
            name: "idle".into(),
            active: false,
            has_bell: false,
            display: display(Status::Idle, 0, 0, None),
        },
        TabRow { flash: false,
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
    // Idle: col 0 is the reserved (blank) spine column, glyph at col 1 — same
    // fixed columns as an active row, just without the `▌`.
    assert!(
        idle.starts_with(" ○"),
        "idle row must be ' ○…' (reserved spine column, blank): {:?}",
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
    // Pane lines render as ` ‹conn› ‹status› ‹mark› ‹activity›` (inactive) or
    // `▌‹conn› ‹status› ‹mark› ‹activity›` (active), where ‹conn› is the tree
    // connector (├/└). The status glyph comes FIRST after the connector, then a
    // space, then the identity mark — so the status icons line up and the mark
    // isn't cramped against the status glyph.
    let a = display_multi(vec![
        pe(1, Kind::Claude, Status::Running, "searching web"),
        pe(2, Kind::Claude, Status::Done, "done thing"),
    ]);
    let row = TabRow { flash: false,
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
    // Tree connectors join the children to the tab (tee on the non-final
    // child, elbow on the last).
    assert!(s.contains('├'), "tee connector present: {:?}", s);
    assert!(s.contains('└'), "elbow connector present: {:?}", s);
}

#[test]
fn comfortable_and_compact_emit_no_bg() {
    // Same tabs with Comfortable and Compact must contain NO card band.
    let detail = PrimaryDetail {
        repo: "r".into(),
        branch: "b".into(),
        msg: "working".into(),
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Running,
    };
    let rows = vec![
        TabRow { flash: false,
            number: 1,
            name: "idle".into(),
            active: false,
            has_bell: false,
            display: display(Status::Idle, 0, 0, None),
        },
        TabRow { flash: false,
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
                now_epoch_s: 0,
                ledger: Vec::new(),
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
        task: String::new(),
        since_tick: 0,
        outcome: None,
        kind: Kind::Claude,
        status: Status::Running,
    };
    let rows = vec![TabRow { flash: false,
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
    let idle_row_val = TabRow { flash: false, number: 1, name: "t".into(), active: false, has_bell: false, display: display(Status::Idle, 0, 0, None) };
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
    let rows = vec![TabRow { flash: false,
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
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Running,
    };
    let rows = vec![
        TabRow { flash: false,
            number: 1,
            name: "idle".into(),
            active: false,
            has_bell: false,
            display: display(Status::Idle, 0, 0, None),
        },
        TabRow { flash: false,
            number: 2,
            name: "agent".into(),
            active: false,
            has_bell: false,
            display: display(Status::Running, 0, 1, Some(detail.clone())),
        },
        TabRow { flash: false,
            number: 3,
            name: "focus".into(),
            active: true,
            has_bell: false,
            display: display(Status::Running, 0, 1, Some(detail)),
        },
    ];
    let s = render(&rows, &tight(&rows, ro_cards(30, 100)));
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
    let row = TabRow { flash: false,
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
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Running,
    };
    let pending = PrimaryDetail {
        repo: "api".into(),
        branch: "fix".into(),
        msg: "".into(),
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Pending,
    };
    let done = PrimaryDetail {
        repo: "worker".into(),
        branch: "".into(),
        msg: "".into(),
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Done,
    };
    let rows = vec![
        TabRow { flash: false,
            number: 1,
            name: "Claude".into(),
            active: true,
            has_bell: false,
            display: display(Status::Running, 0, 1, Some(running)),
        },
        TabRow { flash: false,
            number: 2,
            name: "api".into(),
            active: false,
            has_bell: false,
            display: display(Status::Pending, 0, 1, Some(pending)),
        },
        TabRow { flash: false,
            number: 3,
            name: "worker".into(),
            active: false,
            has_bell: false,
            display: display(Status::Done, 1, 1, Some(done)),
        },
        TabRow { flash: false,
            number: 4,
            name: "Pane #1".into(),
            active: false,
            has_bell: false,
            display: display(Status::Idle, 0, 0, None),
        },
        TabRow { flash: false,
            number: 5,
            name: "Pane #1".into(),
            active: false,
            has_bell: false,
            display: display(Status::Idle, 0, 0, None),
        },
    ];
    let s = render(&rows, &tight(&rows, ro_cards(24, 100)));
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
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Pending,
    };
    let err = PrimaryDetail {
        repo: "infra".into(),
        branch: "".into(),
        msg: "boom".into(),
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Error,
    };
    let rows = vec![
        TabRow { flash: false,
            number: 1,
            name: "pinky".into(),
            active: false,
            has_bell: false,
            display: display(Status::Pending, 0, 1, Some(pending)),
        },
        TabRow { flash: false,
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
    // Header reads " RADAR" with tab count. The old fused `·N!` urgent
    // marker (design rule 7) stays gone — Task 16's needs-you badge is a
    // separate, space-joined `{n}!` token (see `header_badge_*` below), not
    // a revival of the old format.
    let pending = PrimaryDetail {
        repo: "p".into(),
        branch: "x".into(),
        msg: "approve?".into(),
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Pending,
    };
    let rows = vec![
        TabRow { flash: false,
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
    // The old fused ·N! urgent marker is removed per design; the space-joined
    // needs-you badge (Task 16) reads "·3 1!" here, not "·1!".
    assert!(
        !header.contains("·1!"),
        "old fused urgent marker must not appear: {:?}",
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

#[test]
fn header_badge_counts_pending_and_error_tabs_only() {
    // Done, Pending, Error, Running → census ·4, badge counts only the two
    // needs-you rows (Pending + Error) — Done and Running don't count.
    let rows = vec![
        TabRow { flash: false,
            number: 1,
            name: "a".into(),
            active: false,
            has_bell: false,
            display: display(Status::Done, 1, 1, None),
        },
        TabRow { flash: false,
            number: 2,
            name: "b".into(),
            active: false,
            has_bell: false,
            display: display(Status::Pending, 0, 1, None),
        },
        TabRow { flash: false,
            number: 3,
            name: "c".into(),
            active: false,
            has_bell: false,
            display: display(Status::Error, 0, 1, None),
        },
        TabRow { flash: false,
            number: 4,
            name: "d".into(),
            active: false,
            has_bell: false,
            display: display(Status::Running, 0, 1, None),
        },
    ];
    let s = render(&rows, &ro(30, 0));
    let header = strip_sgr(s.lines().next().unwrap());
    assert!(
        header.contains("·4 2!"),
        "right slot reads census + badge: {:?}",
        header
    );
}

#[test]
fn header_badge_absent_at_zero() {
    // All Running/Idle rows → no needs-you row at all, so no badge — just
    // the plain census, same as before Task 16.
    let rows = vec![
        TabRow { flash: false,
            number: 1,
            name: "a".into(),
            active: false,
            has_bell: false,
            display: display(Status::Running, 0, 1, None),
        },
        idle_row(2),
    ];
    let s = render(&rows, &ro(30, 0));
    let header = strip_sgr(s.lines().next().unwrap());
    assert!(header.contains("·2"), "census still shows: {:?}", header);
    assert!(!header.contains('!'), "no badge at zero: {:?}", header);
}

#[test]
fn header_badge_survives_narrow_width_over_census() {
    // Width just fitting " RADAR" (6) + "2!" (2) = 8: the census (·2, width
    // 2) can't join the badge in that budget, so it's dropped entirely and
    // the bare badge stands alone — no leftover "·2 2!" truncation debris.
    let rows = vec![
        TabRow { flash: false,
            number: 1,
            name: "a".into(),
            active: false,
            has_bell: false,
            display: display(Status::Pending, 0, 1, None),
        },
        TabRow { flash: false,
            number: 2,
            name: "b".into(),
            active: false,
            has_bell: false,
            display: display(Status::Error, 0, 1, None),
        },
    ];
    let s = render(&rows, &tight(&rows, ro(8, 0)));
    let header = strip_sgr(s.lines().next().unwrap());
    assert!(
        header.contains("2!"),
        "badge survives the narrow width: {:?}",
        header
    );
    assert!(
        !header.contains('·'),
        "census is dropped to make room for the badge: {:?}",
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
        TabRow { flash: false,
            number: 1,
            name: "agent".into(),
            active: true,
            has_bell: false,
            display: display(Status::Pending, 0, 1, None),
        },
        TabRow { flash: false,
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

proptest! {
    /// Color additivity as a *property*, not a single layout: across arbitrary
    /// rows, widths, densities and glyph sets, every escape the renderer emits is
    /// a well-formed SGR (`\x1b[…m`), so stripping SGR leaves a grid with no
    /// escape residue at all and the same number of rows. This is the structural
    /// form of CONTEXT.md's "stripping SGR yields the exact same visible character
    /// grid" — it catches any non-SGR escape (a cursor move, an OSC) that some row
    /// shape might smuggle in, which the fixed-layout guard above cannot.
    #[test]
    fn color_additivity_leaves_no_escape_residue(
        rows in arb_rows(),
        width in 4usize..=120,
        height in 1usize..=60,
    ) {
        for (density, glyphs) in [
            (Density::Compact, GlyphSet::Plain),
            (Density::Comfortable, GlyphSet::Nerd),
            (Density::Cards, GlyphSet::Plain),
        ] {
            let out = render(&rows, &ro_full(width, height, density, glyphs));
            let stripped = strip_sgr(&out);
            prop_assert!(
                !stripped.contains('\x1b'),
                "non-SGR escape residue after strip (density {:?}, glyphs {:?}): {:?}",
                density, glyphs, stripped
            );
            // Stripping color removes no rows: the visible grid keeps its height.
            prop_assert_eq!(out.lines().count(), stripped.lines().count());
        }
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
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Running,
    };
    let pending = PrimaryDetail {
        repo: "api".into(),
        branch: "fix".into(),
        msg: "".into(),
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Pending,
    };
    let done = PrimaryDetail {
        repo: "worker".into(),
        branch: "".into(),
        msg: "".into(),
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Done,
    };
    vec![
        TabRow { flash: false,
            number: 1,
            name: "web".into(),
            active: true,
            has_bell: false,
            display: display(Status::Running, 0, 1, Some(running)),
        },
        TabRow { flash: false,
            number: 2,
            name: "api".into(),
            active: false,
            has_bell: false,
            display: display(Status::Pending, 0, 1, Some(pending)),
        },
        TabRow { flash: false,
            number: 3,
            name: "worker".into(),
            active: false,
            has_bell: false,
            display: display(Status::Done, 1, 1, Some(done)),
        },
        TabRow { flash: false,
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
        now_epoch_s: 0,
        ledger: Vec::new(),
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
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Running,
    };
    let rows = vec![TabRow { flash: false,
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
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Pending,
    };
    rows.push(TabRow { flash: false,
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
    let row = TabRow { flash: false,
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
        .map(|n| TabRow { flash: false,
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
            task: String::new(),
            kind: Kind::Claude,
            since_tick: 0,
            outcome: None,
            status: Status::Pending,
        };
        rows.push(TabRow { flash: false,
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
        task: String::new(),
        kind: Kind::Claude,
        since_tick: 0,
        outcome: None,
        status: Status::Running,
    };
    let rows = vec![TabRow { flash: false,
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

// ── Bottom region tests (spec §9 budget table) ──

#[test]
fn footer_pins_to_the_floor_with_exact_height() {
    let rows = vec![idle_row(1)];
    let opts = RenderOpts { height: 20, ..ro(24, 0) };
    let s = render(&rows, &opts);
    let lines: Vec<&str> = s.lines().collect();
    assert_eq!(lines.len(), 20, "exact-height invariant: {:?}", s);
    let rule = strip_sgr(lines[17]);
    let tally = strip_sgr(lines[18]);
    let hint = strip_sgr(lines[19]);
    assert!(
        !rule.is_empty() && rule.chars().all(|c| c == '─'),
        "line -3 is the footer rule: {:?}",
        rule
    );
    assert!(
        tally.contains("working") && tally.contains("need you"),
        "line -2 is the tally: {:?}",
        tally
    );
    assert!(hint.contains("alt-[n] jump"), "line -1 is the hint: {:?}", hint);
}

#[test]
fn budget_table_boundaries() {
    let rows = vec![idle_row(1)];
    let content_height = tight(&rows, ro(24, 0)).height; // header(2) + 1 content row = 3
    for leftover in 0..=8usize {
        let opts = RenderOpts { height: content_height + leftover, ..ro(24, 0) };
        let s = render(&rows, &opts);
        let lines: Vec<&str> = s.lines().collect();
        let bottom = &lines[content_height.min(lines.len())..];
        match leftover {
            0 | 1 => assert!(
                bottom.is_empty(),
                "leftover {leftover}: nothing renders: {:?}",
                bottom
            ),
            2 => assert_eq!(
                bottom.len(),
                2,
                "leftover 2: rule + tally: {:?}",
                bottom
            ),
            3 => assert_eq!(
                bottom.len(),
                3,
                "leftover 3: rule + tally + hint: {:?}",
                bottom
            ),
            n => {
                // leftover 4, and every ≥5-with-empty-ledger row, share one
                // shape: (n - 3) filler lines + the 3-line footer.
                assert_eq!(
                    bottom.len(),
                    n,
                    "leftover {n}: filler + footer fills exactly: {:?}",
                    bottom
                );
                assert!(
                    bottom[..n - 3].iter().all(|l| l.is_empty()),
                    "leftover {n}: leading lines are blank filler: {:?}",
                    bottom
                );
                assert!(
                    !bottom[n - 3].is_empty(),
                    "leftover {n}: footer rule follows the filler: {:?}",
                    bottom
                );
            }
        }
    }
}

#[test]
fn tally_counts_running_and_needs_you_not_done() {
    let rows = vec![
        TabRow { flash: false, number: 1, name: "a".into(), active: false, has_bell: false, display: display(Status::Running, 0, 1, None) },
        TabRow { flash: false, number: 2, name: "b".into(), active: false, has_bell: false, display: display(Status::Done, 1, 1, None) },
        TabRow { flash: false, number: 3, name: "c".into(), active: false, has_bell: false, display: display(Status::Error, 0, 1, None) },
    ];
    let content_height = tight(&rows, ro(30, 0)).height;
    let opts = RenderOpts { height: content_height + 3, ..ro(30, 0) };
    let s = render(&rows, &opts);
    let lines: Vec<&str> = s.lines().collect();
    let tally = strip_sgr(lines[lines.len() - 2]);
    // Done never counts toward either tally: 1 Running (spinner) + 1 Error
    // (need-you) only.
    assert_eq!(tally.trim(), "1⠋ working · 1 need you");
}

#[test]
fn tally_renders_zero_working_without_spinner() {
    let rows = vec![idle_row(1)];
    let content_height = tight(&rows, ro(24, 0)).height;
    let opts = RenderOpts { height: content_height + 3, ..ro(24, 0) };
    let s = render(&rows, &opts);
    let lines: Vec<&str> = s.lines().collect();
    let tally = strip_sgr(lines[lines.len() - 2]);
    assert_eq!(tally.trim(), "0 working · 0 need you");
    assert!(!tally.contains('⠋'), "no spinner when 0 working: {:?}", tally);
}

#[test]
fn ledger_entries_render_newest_first_and_click_to_their_tab() {
    let rows = vec![idle_row(1)];
    // Newest first: `web` (age <1m) precedes `gone` (age 15m). `gone` carries
    // no live tab_position — a closed tab's row is click-inert, not dropped.
    let ledger = vec![
        crate::radar_state::LedgerLine {
            at_epoch_s: 950,
            error: false,
            tab_name: "web".into(),
            label: "deploying".into(),
            tab_position: Some(0),
        },
        crate::radar_state::LedgerLine {
            at_epoch_s: 100,
            error: true,
            tab_name: "gone".into(),
            label: "failed".into(),
            tab_position: None,
        },
    ];
    let content_height = tight(&rows, ro(30, 0)).height;
    let opts = RenderOpts {
        height: content_height + 6, // 0 filler + rule(1) + 2 entries + footer(3)
        ledger: ledger.clone(),
        now_epoch_s: 1000,
        ..ro(30, 0)
    };
    let rail = render_rail(&rows, &opts);
    let entry1_line = content_height + 1;
    let entry2_line = content_height + 2;

    assert_eq!(
        rail.target_at_line(entry1_line as isize),
        Some(RailTarget { tab_position: 0, pane_id: None }),
        "the newer, still-open entry is clickable to its tab"
    );
    assert_eq!(
        rail.target_at_line(entry2_line as isize),
        None,
        "a gone tab's ledger row is click-inert"
    );

    let ansi_lines: Vec<&str> = rail.ansi.lines().collect();
    let l1 = strip_sgr(ansi_lines[entry1_line]);
    let l2 = strip_sgr(ansi_lines[entry2_line]);
    assert!(
        l1.contains("<1m") && l1.contains('●') && l1.contains("web") && l1.contains("deploying"),
        "newest entry first: {:?}",
        l1
    );
    assert!(
        l2.contains("15m") && l2.contains('✗') && l2.contains("gone") && l2.contains("failed"),
        "older entry second: {:?}",
        l2
    );
}

#[test]
fn ledger_entry_line_clamps_at_extreme_narrow_widths() {
    // The fixed age+glyph prefix is 6 cols; below that the line must clamp,
    // like every other fixed-prefix renderer in this file (see
    // `emit_pane_line`'s narrow-width fallback and the `truncate(...)` guards
    // on `footer_tally`/`ledger_rule`).
    let line = crate::radar_state::LedgerLine {
        at_epoch_s: 0,
        error: false,
        tab_name: "web".into(),
        label: "deploying".into(),
        tab_position: Some(0),
    };
    for width in 1..=7 {
        let opts = RenderOpts { now_epoch_s: 1000, ..ro(width, 0) };
        let rendered = ledger_entry_line(&line, &opts);
        for text_line in rendered.text.lines() {
            assert!(
                visible_len(text_line) <= width,
                "width {}: line exceeds width: {:?} (visible {})",
                width,
                text_line,
                visible_len(text_line)
            );
        }
    }
}

#[test]
fn cards_never_lose_budget_to_the_bottom_region() {
    // Many URGENT (never idle-foldable) rows, tight height → the overflow
    // compressor packs the plan to fill body_budget exactly, leaving no
    // headroom for the bottom region (leftover 0). The plan renders exactly
    // as it did before Task 13 — no footer squeezed in over dropped rows.
    let rows: Vec<TabRow> = (1..=20)
        .map(|n| TabRow { flash: false,
            number: n,
            name: format!("t{}", n),
            active: false,
            has_bell: false,
            display: display(Status::Pending, 0, 1, None),
        })
        .collect();
    let opts = ro_cards(24, 10);
    let leftover = opts.height.saturating_sub(body_line_count(&rows, &opts));
    assert!(leftover <= 1, "sanity: this scenario must leave no headroom: {leftover}");
    assert!(render_bottom(&rows, leftover, &opts).is_empty());

    let rail = render_rail(&rows, &opts);
    assert_eq!(rail.line_count(), 10, "the overflow plan alone fills the pane");
    assert!(
        !rail.ansi.contains("alt-[n] jump"),
        "no footer should be squeezed in when there's no room: {:?}",
        rail.ansi
    );
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
    /// A sticky task label: empty (task-less panes, the pre-task shape), a
    /// short ASCII phrase, or a wide/CJK string — so detail-line generation
    /// (which is gated on a non-empty task) and wide-glyph width both get
    /// exercised by every proptest that draws panes/rows.
    fn arb_task()(
        t in prop_oneof![
            Just(String::new()),
            "[a-z ]{1,30}",
            Just("修复端到端测试".to_string()),
        ],
    ) -> String {
        t
    }
}

prop_compose! {
    /// An arbitrary pane: ~15% untracked, else a tracked pane with an
    /// arbitrary Kind/Status, a short / long / CJK message, and a fuzzed
    /// sticky task so truncation, wide-glyph width, and the narrow-width
    /// plain fallback all get hit — including the `↳` detail line, which
    /// only appears when the task is non-empty and status is Pending/Error.
    fn arb_pane()(
        id in 1u32..100,
        kind in arb_kind(),
        status in arb_status(),
        untracked in 0u8..100,
        msg_pick in 0u8..3,
        task in arb_task(),
    ) -> PaneDisplay {
        if untracked < 15 {
            PaneDisplay::untracked(id, "term")
        } else {
            let msg = match msg_pick {
                0 => "ok",
                1 => "running a fairly long migration command across the cluster now",
                _ => "日本語のメッセージ表示テスト中です", // CJK wide glyphs
            };
            PaneDisplay::tracked(id, kind, status, msg.to_string(), task, None)
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
        task in arb_task(),
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
                    task,
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
        TabRow { flash: false,
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
            // Height budget holds for EVERY density — not just Cards (whose
            // 1-line header always fit). The 2-line Compact/Comfortable header
            // used to overflow at height 1; the final clamp in `render_rail`
            // fixes it. (Regression: `render_rail` height clamp.)
            prop_assert!(
                s.lines().count() <= height,
                "lines {} > height {} (density {:?}, glyphs {:?})",
                s.lines().count(),
                height,
                density,
                glyphs
            );
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

prop_compose! {
    /// An arbitrary ledger row: a real tab position about half the time
    /// (`None` the other half, simulating a closed tab — still rendered,
    /// just click-inert).
    fn arb_ledger_line()(
        at_epoch_s in 0u64..1_000_000,
        error in any::<bool>(),
        tab_name in "[a-zA-Z0-9_-]{0,15}",
        label in "[a-zA-Z0-9_ -]{0,20}",
        has_tab in any::<bool>(),
        tab_position in 0usize..8,
    ) -> crate::radar_state::LedgerLine {
        crate::radar_state::LedgerLine {
            at_epoch_s,
            error,
            tab_name,
            label,
            tab_position: if has_tab { Some(tab_position) } else { None },
        }
    }
}

fn arb_ledger() -> impl Strategy<Value = Vec<crate::radar_state::LedgerLine>> {
    proptest::collection::vec(arb_ledger_line(), 0..6)
}

proptest! {
    /// Lockstep: the emitted ANSI and the click-target map stay in exact
    /// 1:1 line correspondence, at every width/height/ledger-size the rail
    /// can be drawn at. Also pins the bottom region's exact-height invariant
    /// (spec §9): whenever it renders any lines at all, the total footprint
    /// is exactly `height` — never short, never over (the final `truncate`
    /// only ever bites the degenerate header-taller-than-height case, which
    /// `render_bottom` never contributes to).
    #[test]
    fn render_rail_lockstep_lines_match_targets(
        rows in arb_rows(),
        width in 1usize..=120,
        height in 1usize..=60,
        ledger in arb_ledger(),
    ) {
        let mut opts = ro(width, 0);
        opts.height = height;
        opts.ledger = ledger;
        opts.now_epoch_s = 500_000;
        let rail = render_rail(&rows, &opts);
        // 1:1 correspondence between physical lines and target slots.
        prop_assert_eq!(rail.line_count(), rail.ansi.lines().count());
        // Every in-range line resolves without panic; out-of-range is None.
        for line in 0..rail.line_count() {
            let _ = rail.target_at_line(line as isize);
        }
        prop_assert_eq!(rail.target_at_line(-1), None);
        prop_assert_eq!(rail.target_at_line(rail.line_count() as isize), None);

        if !rows.is_empty() {
            let leftover = height.saturating_sub(body_line_count(&rows, &opts));
            let bottom = render_bottom(&rows, leftover, &opts);
            // Every bottom-region line (rule/entries/footer) must clamp to the
            // rail width, at any width down to 1 — including the ledger entries,
            // which carry a fixed age+glyph prefix like every other fixed-prefix
            // renderer in this file.
            for line in &bottom {
                prop_assert!(
                    visible_len(&line.text) <= width,
                    "bottom line exceeds width {}: {:?} (visible {})",
                    width,
                    line.text,
                    visible_len(&line.text)
                );
            }
            if !bottom.is_empty() {
                prop_assert_eq!(rail.line_count(), height);
            }
        }
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
        TabRow { flash: false, number: 1, name, active, has_bell: false, display }
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
    // Trailing newline popped (like from_lines), so a face ending on a
    // non-blank line has line_count == lines().
    assert_eq!(rail.line_count(), rail.ansi.lines().count());
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
    let row = TabRow { flash: false, number: 1, name: "t".into(), active: false, has_bell: false, display: a };
    assert_eq!(render_row(&row, &opts).len(), 4, "header + 3 pane lines");
}

#[test]
fn single_running_pane_with_detail_is_two_content_lines() {
    // Single-pane Running tab with a non-empty detail msg → 2 content lines
    // (name row + detail row). Mirrors the row_lines assertion from
    // lib.rs::click_mapping_cards_pad_y_and_post_content_row.
    let opts = ro(40, 0);
    let a = display_multi(vec![pe(10, Kind::Claude, Status::Running, "msg")]);
    let row = TabRow { flash: false, number: 1, name: "t".into(), active: false, has_bell: false, display: a };
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
    let row = TabRow { flash: false,
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
    let active_row = TabRow { flash: false,
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
    // The searched substrings ("Ctrl-y", "permission") contain no characters that
    // appear in SGR escape sequences (`\x1b`, `[`, digits, `;`, `m`), so a plain
    // `contains` on the raw ANSI string is valid without stripping SGR first.
    let plain: String = needs.chars().collect();
    assert!(plain.contains("Ctrl-y"), "must tell the user the grant keybind:\n{needs}");
    assert!(plain.to_lowercase().contains("permission"), "must mention permission");
}

#[test]
fn identity_and_detail_rules() {
    use super::identity_and_detail;
    // Task-less panes: today's behavior, no detail line — bit-identical rail.
    assert_eq!(identity_and_detail(Status::Running, "", "editing x.rs"), ("editing x.rs", None));
    assert_eq!(identity_and_detail(Status::Pending, "", "approve?"), ("approve?", None));
    // Task is the identity in every state.
    assert_eq!(identity_and_detail(Status::Running, "fix e2e", "editing x.rs"), ("fix e2e", None));
    assert_eq!(identity_and_detail(Status::Done, "fix e2e", "All tests pass"), ("fix e2e", None));
    // Actionable states get the question as a subordinate detail…
    assert_eq!(identity_and_detail(Status::Pending, "fix e2e", "approve?"), ("fix e2e", Some("approve?")));
    assert_eq!(identity_and_detail(Status::Error, "fix e2e", "boom"), ("fix e2e", Some("boom")));
    // …unless it would duplicate the identity or is blank.
    assert_eq!(identity_and_detail(Status::Pending, "fix e2e", "fix e2e"), ("fix e2e", None));
    assert_eq!(identity_and_detail(Status::Pending, "fix e2e", "  "), ("fix e2e", None));
}

#[test]
fn pending_pane_with_task_renders_identity_plus_question_line() {
    // Multi-pane tab: pending pane shows task on its line and the question on
    // a `↳` line that carries the SAME pane click target (lockstep).
    let row = TabRow { flash: false,
        number: 1,
        name: "review".into(),
        active: false,
        has_bell: false,
        display: TabDisplay {
            status: Status::Pending,
            progress: ProgressCounts { done: 0, total: 2, pending: 1 },
            detail: None,
            panes: vec![
                PaneDisplay::tracked(10, Kind::Claude, Status::Pending, "approve git push?".into(), "migrate schema".into(), None),
                PaneDisplay::tracked(11, Kind::Codex, Status::Running, "editing retry.rs".into(), "write tests".into(), None),
            ],
        },
    };
    let rendered = render_rail(&[row], &ro_comfortable(32, 40));
    let grid = strip_ansi_local(&rendered.ansi); // use the file's existing ANSI-strip helper
    assert!(grid.contains("├ ◆ ✳ migrate schema"), "task is the identity line:\n{grid}");
    assert!(grid.contains("│   ↳ approve git push?"), "question is subordinate:\n{grid}");
    assert!(grid.contains("└ ⠋ ❉ write tests"), "running pane shows task only:\n{grid}");
    // Lockstep: the ↳ line click-jumps to the pending pane.
    let q_line = grid.lines().position(|l| l.contains('↳')).unwrap();
    assert_eq!(
        rendered.target_at_line(q_line as isize),
        Some(RailTarget { tab_position: 0, pane_id: Some(10) }),
    );
}

#[test]
fn tab_name_column_is_fixed_across_active_and_inactive() {
    // One active, one inactive row; strip SGR and compare name columns.
    let mut active = idle_row(1);
    active.name = "alpha".into();
    active.active = true;
    let mut idle = idle_row(2);
    idle.name = "beta".into();
    let opts = ro(24, 0);
    let ansi = render(&[active, idle], &opts);
    // `.find()` returns a BYTE offset; the spine glyph `▌` is a multi-byte
    // UTF-8 char while its inactive stand-in `' '` is one byte, so byte
    // offsets diverge by encoding width even when the visual COLUMN matches.
    // Count chars up to the match to get the actual column.
    let cols: Vec<usize> = ansi
        .lines()
        .map(strip_sgr)
        .filter_map(|l| {
            let byte_idx = l.find("alpha").or_else(|| l.find("beta"))?;
            Some(l[..byte_idx].chars().count())
        })
        .collect();
    assert_eq!(cols[0], cols[1], "active and inactive tab names must start at the same column:\n{ansi}");
}
