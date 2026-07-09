//! Doc-as-oracle test harness: parse docs/rail-reference.md, build state,
//! run the real render pipeline, and compare the rendered grid to the doc's
//! expected block.
//!
//! Green regression guard: the renderer matches every `rail-expect` block in
//! docs/rail-reference.md. Editing a `rail-input`/`rail-expect` pair in the doc
//! edits this test — the doc is the single source of truth for rail rendering.

use crate::command::DEBOUNCE_TICKS;
use crate::config::{Density, NamingMode};
use crate::radar_state::{PaneUpdate, RadarState, RadarTab, TabId};
use crate::render::{GlyphSet, RenderOpts};
use crate::rollup::{TabRow, TerminalPane};
use crate::status::Status;
use crate::theme::DerivedColors;
use std::collections::{HashMap, HashSet};

// ── Case type ───────────────────────────────────────────────────────────────

struct Case {
    id: String,
    input: String,
    expect: String,
}

// ── Parser ──────────────────────────────────────────────────────────────────

/// Scan `doc` for `## <heading>` markers followed by ```rail-input``` and
/// ```rail-expect``` fenced blocks. Each heading that has BOTH blocks yields
/// one `Case`.
fn parse_cases(doc: &str) -> Vec<Case> {
    let mut cases = Vec::new();
    let mut current_id: Option<String> = None;
    let mut current_input: Option<String> = None;
    let mut current_expect: Option<String> = None;

    let mut in_input = false;
    let mut in_expect = false;
    let mut buf = String::new();

    for raw_line in doc.lines() {
        // Detect heading
        if raw_line.starts_with("## ") && !in_input && !in_expect {
            // Flush any pending case before moving to next heading
            if let (Some(id), Some(inp), Some(exp)) = (
                current_id.take(),
                current_input.take(),
                current_expect.take(),
            ) {
                cases.push(Case { id, input: inp, expect: exp });
            }
            current_id = Some(raw_line[3..].trim().to_string());
            current_input = None;
            current_expect = None;
            continue;
        }

        // Detect fenced block openings
        if raw_line.trim_start() == "```rail-input" && !in_input && !in_expect {
            in_input = true;
            buf.clear();
            continue;
        }
        if raw_line.trim_start() == "```rail-expect" && !in_input && !in_expect {
            in_expect = true;
            buf.clear();
            continue;
        }

        // Detect closing fence
        if raw_line.trim_start() == "```" {
            if in_input {
                current_input = Some(buf.clone());
                in_input = false;
                buf.clear();
            } else if in_expect {
                current_expect = Some(buf.clone());
                in_expect = false;
                buf.clear();
                // If we have both blocks under the current heading, record it.
                if let (Some(id), Some(inp), Some(exp)) = (
                    current_id.clone(),
                    current_input.clone(),
                    current_expect.take(),
                ) {
                    cases.push(Case { id, input: inp, expect: exp });
                    current_input = None;
                }
            }
            continue;
        }

        // Accumulate block content
        if in_input || in_expect {
            if !buf.is_empty() {
                buf.push('\n');
            }
            buf.push_str(raw_line);
        }
    }

    // Final flush (end-of-file)
    if let (Some(id), Some(inp), Some(exp)) = (current_id, current_input, current_expect) {
        cases.push(Case { id, input: inp, expect: exp });
    }

    cases
}

// ── DSL builder ─────────────────────────────────────────────────────────────

/// Parse a `rail-input` DSL block into `(Vec<TabRow>, RenderOpts)` by routing
/// through the real `RadarState` aggregator.
///
/// DSL grammar:
///   width N
///   height N
///   glyphs plain|nerd
///   jump_hint            ← footer advertises `alt-[n] jump` (default: hidden)
///   tab <pos> "<name>" [active]
///     <kind> <status> "<msg>" [task "<text>"] [exit <N>|?]   ← indented; one line per pane
///
/// For idle panes: Running is applied first (tick=0, same msg) to set
/// `ever_active=true`, then Idle (tick=1, same msg). This preserves the
/// idle-but-tracked behavior required by scenario J.
///
/// Panics on unknown `kind` or `status` tokens with a descriptive message.
fn build(input: &str) -> (Vec<TabRow>, Vec<crate::rollup::LedgerLine>, RenderOpts) {
    use crate::kind::Kind;
    use crate::payload::{to_wire, StatusPayload};

    let mut width: usize = 32;
    let mut height: usize = usize::MAX / 2;
    let mut explicit_height = false;
    let mut glyphs = GlyphSet::Plain;
    let mut density = Density::Compact;
    let mut jump_hint = false;
    let mut ledger_lines: Vec<crate::rollup::LedgerLine> = Vec::new();
    // A fixed "now" for the `ledger` directive's age math, so a scenario's
    // round `age_secs` numbers produce deterministic, doc-readable
    // `format_age` text (e.g. 90 → "1m").
    const LEDGER_NOW_EPOCH_S: u64 = 1_000_000;

    /// Split a leading `"quoted"` token off `s`, returning (contents, rest).
    /// THE quote splitter for every DSL site that reads a quoted string —
    /// the `ledger` directive, tab names, pane msgs, untracked titles, and
    /// the `task "…"` trailer. Lenient on malformed input: an unquoted token
    /// yields the whole (trim-started) remainder, an unterminated quote
    /// everything after the opening `"` — both with an empty rest.
    fn take_quoted(s: &str) -> (&str, &str) {
        let s = s.trim_start();
        match s.strip_prefix('"') {
            Some(inner) => match inner.find('"') {
                Some(end) => (&inner[..end], &inner[end + 1..]),
                None => (inner, ""),
            },
            None => (s, ""),
        }
    }

    struct PaneSpec {
        pane_id: u32,
        kind: Kind,
        status: Status,
        msg: String,
        /// Sticky task label (empty = no task). Set via the `task "<text>"` trailer.
        task: String,
        /// true = register pane but send NO status (→ PaneDisplay::Untracked)
        untracked: bool,
        /// Some(code) → command-origin path: command_changed + timer + on_exit.
        /// None → status_pipe path (existing behavior).
        exit_code: Option<Option<i32>>,
        /// Minutes this pane has been waiting on the user (Pending only). Set
        /// via the `waiting <N>m` trailer; backdates the apply epoch so the
        /// `· Nm` wait tag renders. 0 = applied "now" (no tag).
        waiting_m: u64,
    }

    struct TabSpec {
        pos: usize,   // 1-based DSL position (e.g. `tab 1 "shell"` -> pos=1)
        name: String,
        active: bool,
        has_bell: bool,
        panes: Vec<PaneSpec>,
    }

    let mut tabs: Vec<TabSpec> = Vec::new();
    let mut current_tab: Option<usize> = None; // index into tabs
    let mut next_pane_id: u32 = 1;

    for line in input.lines() {
        // Width / height / glyphs / density options (non-indented)
        if let Some(rest) = line.strip_prefix("width ") {
            if let Ok(n) = rest.trim().parse::<usize>() {
                width = n;
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("height ") {
            if let Ok(n) = rest.trim().parse::<usize>() {
                height = n;
                explicit_height = true;
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("glyphs ") {
            glyphs = match rest.trim() {
                "nerd" => GlyphSet::Nerd,
                _ => GlyphSet::Plain,
            };
            continue;
        }
        if let Some(rest) = line.strip_prefix("density ") {
            density = match rest.trim() {
                "compact" => Density::Compact,
                "comfortable" => Density::Comfortable,
                "cards" => Density::Cards,
                other => panic!("reference DSL: unknown density '{}' in scenario", other),
            };
            continue;
        }
        // Footer `alt-[n] jump` hint: config-driven honesty (opt-in for setups
        // where Alt+digit actually reaches Zellij; no in-tree config sets it)
        // — default hidden, mirroring `JumpHint`.
        if line.trim() == "jump_hint" {
            jump_hint = true;
            continue;
        }

        // Completion-ledger row (spec §9 bottom region): `ledger <age_secs>
        // done|error "<tab_name>" "<label>"`. `tab_position` is always `None`
        // here — the DSL has no live `RadarState` recede path to seed a real
        // one through, and it makes no visual difference (only click targets,
        // which this doc doesn't assert).
        if let Some(rest) = line.strip_prefix("ledger ") {
            let rest = rest.trim();
            let mut parts = rest.splitn(3, ' ');
            let age_str = parts.next().unwrap_or("0");
            let outcome_str = parts.next().unwrap_or("done");
            let remainder = parts.next().unwrap_or("");
            let age_secs: u64 = age_str.parse().unwrap_or_else(|_| {
                panic!(
                    "reference DSL: bad ledger age '{}' — must be an integer number of seconds",
                    age_str
                )
            });
            let error = match outcome_str {
                "done" => false,
                "error" => true,
                other => panic!(
                    "reference DSL: unknown ledger outcome '{}' — must be 'done' or 'error'",
                    other
                ),
            };
            let (tab_name, after) = take_quoted(remainder);
            let (label, _) = take_quoted(after);
            ledger_lines.push(crate::rollup::LedgerLine {
                at_epoch_s: LEDGER_NOW_EPOCH_S.saturating_sub(age_secs),
                error,
                tab_name: tab_name.to_string(),
                label: label.to_string(),
                tab_position: None,
            });
            continue;
        }

        // Tab declaration: `tab <pos> "<name>" [active] [bell]`
        if let Some(rest) = line.strip_prefix("tab ") {
            let rest = rest.trim();
            // Parse position
            let (pos_str, after_pos) = rest.split_once(' ').unwrap_or((rest, ""));
            let pos = pos_str.parse::<usize>().unwrap_or(0);
            let after_pos = after_pos.trim();
            // Parse name (quoted)
            let (name, after_name) = take_quoted(after_pos);
            let active = after_name.contains("active");
            let has_bell = after_name.contains("bell");
            let idx = tabs.len();
            tabs.push(TabSpec {
                pos,
                name: name.to_string(),
                active,
                has_bell,
                panes: Vec::new(),
            });
            current_tab = Some(idx);
            continue;
        }

        // Pane line: indented `  <kind> <status> "<msg>"` OR `  untracked "<title>"`
        if (line.starts_with("  ") || line.starts_with('\t')) && current_tab.is_some() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Untracked pane form: `untracked "<title>"`
            if let Some(rest) = trimmed.strip_prefix("untracked ") {
                let (title, _) = take_quoted(rest);
                let pane_id = next_pane_id;
                next_pane_id += 1;
                if let Some(idx) = current_tab {
                    tabs[idx].panes.push(PaneSpec {
                        pane_id,
                        kind: Kind::Other,
                        status: Status::Idle,
                        msg: title.to_string(),
                        task: String::new(),
                        untracked: true,
                        exit_code: None,
                        waiting_m: 0,
                    });
                }
                continue;
            }

            // Parse: kind status "msg" [exit <N>|?]
            // We split on the first two spaces to get kind and status, then parse
            // the remainder manually to correctly handle quoted messages that may
            // contain spaces, followed by an optional `exit <N>|?` trailer.
            let mut parts = trimmed.splitn(3, ' ');
            let kind_str = parts.next().unwrap_or("other");
            let status_str = parts.next().unwrap_or("idle");
            let rest = parts.next().unwrap_or("\"\"");

            // Parse the (possibly-quoted) message; whatever follows the close
            // quote is the trailer chain (`task`/`waiting`/`exit`). An
            // unquoted msg consumes the whole rest, so it carries no trailers.
            let (msg, after_msg) = take_quoted(rest);

            // Validate kind token against the single source of truth (the
            // `kinds!` table): a token is valid iff it round-trips through
            // `from_source`/`as_source` — a new agent added via the
            // compiler-guided flow is accepted by the DSL automatically, while
            // a typo (which `from_source` folds to `Other`) still panics.
            if Kind::from_source(kind_str).as_source() != kind_str {
                panic!("reference DSL: unknown kind '{}' in scenario", kind_str);
            }

            // Validate status token against the single source of truth (the
            // `statuses!` table) so the DSL vocabulary can't drift from `Status`.
            // Kept strict (panic on unknown) to catch scenario typos — unlike the
            // lenient `from_wire` used to parse it below.
            if !Status::ALL.iter().any(|s| s.as_wire() == status_str) {
                panic!("reference DSL: unknown status '{}' in scenario", status_str);
            }

            // Trailers after the closing quote: `task "<text>"`, `waiting <N>m`,
            // and/or `exit <N>|?`.
            let mut task = String::new();
            let mut exit_code: Option<Option<i32>> = None;
            let mut waiting_m: u64 = 0;
            let mut trailer = after_msg.trim();
            while !trailer.is_empty() {
                if trailer.starts_with("task \"") {
                    let (text, remainder) = take_quoted(&trailer["task ".len()..]);
                    task = text.to_string();
                    trailer = remainder.trim();
                } else if let Some(rest) = trailer.strip_prefix("waiting ") {
                    let (mins, remainder) = rest.split_once(' ').unwrap_or((rest, ""));
                    let mins = mins.strip_suffix('m').unwrap_or_else(|| {
                        panic!("reference DSL: bad waiting trailer '{trailer}' — expected 'waiting <N>m'")
                    });
                    waiting_m = mins.parse().unwrap_or_else(|_| {
                        panic!("reference DSL: bad waiting minutes '{mins}' — must be an integer")
                    });
                    trailer = remainder.trim();
                } else if let Some(code_str) = trailer.strip_prefix("exit ") {
                    let code_str = code_str.trim();
                    if code_str == "?" {
                        exit_code = Some(None);
                    } else if let Ok(n) = code_str.parse::<i32>() {
                        exit_code = Some(Some(n));
                    } else {
                        panic!("reference DSL: bad exit code '{code_str}' — must be an integer or '?'");
                    }
                    trailer = "";
                } else {
                    panic!("reference DSL: unknown pane trailer '{trailer}' — only 'task \"<text>\"', 'waiting <N>m', and 'exit <N>|?' are supported");
                }
            }

            let kind = Kind::from_source(kind_str);
            let status = Status::from_wire(status_str);
            let pane_id = next_pane_id;
            next_pane_id += 1;

            if let Some(idx) = current_tab {
                tabs[idx].panes.push(PaneSpec {
                    pane_id,
                    kind,
                    status,
                    msg: msg.to_string(),
                    task,
                    untracked: false,
                    exit_code,
                    waiting_m,
                });
            }
            continue;
        }
    }

    // ── Build RadarState ──────────────────────────────────────────────────────

    let mut radar = RadarState::default();

    // 1. tabs_changed: one RadarTab per DSL tab.
    // DSL pos is 1-based (e.g. `tab 1 "shell"`); rows() returns number = position+1,
    // so we store position = pos - 1 to get the right display number.
    let radar_tabs: Vec<RadarTab> = tabs.iter().map(|spec| RadarTab {
        id: TabId::new(spec.pos),
        position: spec.pos.saturating_sub(1),
        name: spec.name.clone(),
        active: spec.active,
        has_bell: spec.has_bell,
    }).collect();
    radar.tabs_changed(radar_tabs);

    // 2. panes_changed: register all panes as live terminal panes.
    let mut tab_panes: HashMap<usize, Vec<TerminalPane>> = HashMap::new();
    let mut live: HashSet<u32> = HashSet::new();

    for spec in &tabs {
        let position = spec.pos.saturating_sub(1);
        let terminal_panes: Vec<TerminalPane> = spec.panes.iter().map(|p| {
            live.insert(p.pane_id);
            TerminalPane {
                id: p.pane_id,
                title: p.msg.clone(),
                focused_in_tab: false,
            }
        }).collect();
        if !terminal_panes.is_empty() {
            tab_panes.insert(position, terminal_panes);
        }
    }

    let update = PaneUpdate {
        tab_panes,
        live,
        theme: None,
        exits: Vec::new(),
    };
    radar.panes_changed(update, 0, 0, NamingMode::Off);

    // 3. Apply status for each tracked pane.
    //
    // Two paths:
    //   a) status_pipe path (exit_code == None): the existing StatusPipe-origin path.
    //      Untracked panes get NO update → PaneDisplay::Untracked.
    //      For idle panes: first apply Running (tick=0) to set ever_active=true,
    //      then apply Idle (tick=1). This preserves scenario J's idle-but-tracked behavior.
    //
    //   b) command-origin path (exit_code == Some(code)): drives the CommandStore path
    //      so that pane_outcome() fires and end-result tags (no tag / exit N / ✗) render.
    //      Sequence:
    //        1. command_changed(tick=0) — registers a pending command with the msg as argv.
    //        2. timer(tick=DEBOUNCE_TICKS) — promotes pending→Running once the debounce
    //           window elapses.
    //        3. panes_changed with exits vec containing the exit code — calls on_exit,
    //           which sets status Done/Error and exit_code on the resolved entry.

    // Collect command-exit panes so we can feed them as a batch to panes_changed.
    let mut command_exits: Vec<(u32, Option<i32>)> = Vec::new();

    for spec in &tabs {
        for pane in &spec.panes {
            if pane.untracked || pane.exit_code.is_some() {
                continue; // untracked + command-exit panes handled separately
            }

            // kind -> wire source name (inverse of the `from_source` the DSL used)
            let source = pane.kind.as_source();

            if pane.status == Status::Idle {
                // First prime ever_active (and the task) with Running, then switch
                // to Idle with task="" — the idle apply clears it, matching real
                // `/clear` semantics.
                let wire_running = to_wire(&StatusPayload {
                    pane_id: pane.pane_id,
                    status: Status::Running,
                    repo: "".into(),
                    branch: "".into(),
                    msg: pane.msg.clone(),
                    task: pane.task.clone(),
                    source: source.to_string(),
                    ack: false,
                });
                radar.status_pipe(&wire_running, 0, 0, NamingMode::Off);

                let wire_idle = to_wire(&StatusPayload {
                    pane_id: pane.pane_id,
                    status: Status::Idle,
                    repo: "".into(),
                    branch: "".into(),
                    msg: pane.msg.clone(),
                    task: "".into(),
                    source: source.to_string(),
                    ack: false,
                });
                radar.status_pipe(&wire_idle, 1, 0, NamingMode::Off);
            } else {
                let wire = to_wire(&StatusPayload {
                    pane_id: pane.pane_id,
                    status: pane.status,
                    repo: "".into(),
                    branch: "".into(),
                    msg: pane.msg.clone(),
                    task: pane.task.clone(),
                    source: source.to_string(),
                    ack: false,
                });
                // Applied "now" relative to the render epoch, backdated by the
                // `waiting <N>m` trailer — how the doc's pending scenarios earn
                // (or, at 0, deliberately do not earn) the `· Nm` wait tag.
                let apply_epoch = LEDGER_NOW_EPOCH_S.saturating_sub(pane.waiting_m * 60);
                radar.status_pipe(&wire, 0, apply_epoch, NamingMode::Off);
            }
        }
    }

    // Command-origin path for panes with an `exit <N>|?` trailer.
    // Step 1: command_changed (tick=0) — registers each pane as a pending command.
    // Step 2: timer(tick=DEBOUNCE_TICKS) — promotes all pending commands to Running.
    // Step 3: panes_changed with exits — on_exit sets Done/Error + exit_code.
    for spec in &tabs {
        for pane in &spec.panes {
            if let Some(code) = pane.exit_code {
                // Build argv from the msg string so the CommandStore compacts it
                // into the display message (e.g. "cargo build" stays "cargo build").
                let argv: Vec<String> = pane.msg.split_whitespace()
                    .map(|s| s.to_string())
                    .collect();
                radar.command_changed(pane.pane_id, &argv, true, 0);
                command_exits.push((pane.pane_id, code));
            }
        }
    }

    if !command_exits.is_empty() {
        // Promote all pending commands to Running so the msg is set before exit.
        radar.timer(DEBOUNCE_TICKS, 0);

        // Now deliver the exits. Re-register the live pane set (unchanged) along
        // with the exits vec so that panes_changed → on_exit sets Done/Error.
        let mut tab_panes2: HashMap<usize, Vec<TerminalPane>> = HashMap::new();
        let mut live2: HashSet<u32> = HashSet::new();
        for spec in &tabs {
            let position = spec.pos.saturating_sub(1);
            let terminal_panes: Vec<TerminalPane> = spec.panes.iter().map(|p| {
                live2.insert(p.pane_id);
                TerminalPane {
                    id: p.pane_id,
                    title: p.msg.clone(),
                    focused_in_tab: false,
                }
            }).collect();
            if !terminal_panes.is_empty() {
                tab_panes2.insert(position, terminal_panes);
            }
        }
        let update2 = PaneUpdate {
            tab_panes: tab_panes2,
            live: live2,
            theme: None,
            exits: command_exits,
        };
        radar.panes_changed(update2, 2, 0, NamingMode::Off);
    }

    // 4. Build RenderOpts
    let theme = DerivedColors::from_bg_fg((0, 0, 0), (200, 200, 200));
    // Fixtures render a settled state, not the instant a pane just flipped to
    // Pending — pass a tick well past any flash window armed during setup
    // above (which uses small hardcoded ticks like 0/1/DEBOUNCE_TICKS) so a
    // `claude pending ...` scenario never spuriously renders the flash tint.
    let rows = radar.rows(10_000);
    let mut opts = RenderOpts {
        width,
        height,
        now_tick: 0,
        glyphs,
        header: true,
        density,
        theme,
        now_epoch_s: LEDGER_NOW_EPOCH_S,
        jump_hint,
        // The reference doc's scenarios are all single-session — the badge
        // stays invisible (`render_session_badge`'s `len() <= 1` gate), same
        // as every rail-reference.md fixture predates this field.
        badge: vec![],
    };
    // Scenarios that don't declare an explicit `height` used the old
    // "unboundedly large" sentinel to mean "enough to fit, no overflow, no
    // padding" — a meaning the bottom region has changed: any
    // height taller than the content now pads down to a pinned footer, and
    // `usize::MAX / 2` worth of filler lines would never finish building.
    // Recompute that default as the session's exact natural content height
    // (leftover 0 ⇒ no bottom region), so every pre-existing scenario keeps
    // rendering exactly what it always did; only a scenario that opts into an
    // explicit `height` can exercise the footer/ledger region.
    if !explicit_height {
        opts.height = crate::render::body_line_count(&rows, &ledger_lines, &opts);
    }

    (rows, ledger_lines, opts)
}

// ── vt100 grid helper ────────────────────────────────────────────────────────

// The vt100 `grid` oracle is shared with the insta snapshot suite — see
// `render::test_util`. One helper, two oracles: the doc spec and the
// snapshots always judge the renderer by the same visible grid.
use crate::render::test_util::grid;

// ── The test ─────────────────────────────────────────────────────────────────

#[test]
fn rail_reference_matches() {
    let doc = include_str!("../../../docs/rail-reference.md");
    let cases = parse_cases(doc);
    eprintln!("Found {} scenarios", cases.len());

    // `parse_cases` is lenient: a heading missing either fenced block silently
    // yields no Case, so a typo'd fence (```rail-expect → ```rail-expct) would
    // de-register a scenario while this test stayed green. Pin the parse
    // structurally: every fence opener in the doc must have landed in exactly
    // one Case. Count openers the way the parser does (the whole trimmed line
    // is the fence tag) so the inline mentions in the doc's prose — set off in
    // four-backtick spans, never alone on a line — don't count.
    let fence_openers =
        |tag: &str| doc.lines().filter(|l| l.trim_start() == tag).count();
    for tag in ["```rail-input", "```rail-expect"] {
        assert_eq!(
            cases.len(),
            fence_openers(tag),
            "parsed {} scenario(s) but docs/rail-reference.md has {} `{}` fences — \
             a scenario is malformed (typo'd fence tag, a block missing its \
             ```rail-input/```rail-expect partner, or a pair with no `## <id>` \
             heading above it)",
            cases.len(),
            fence_openers(tag),
            tag,
        );
    }
    assert!(!cases.is_empty(), "docs/rail-reference.md yielded zero scenarios");

    let mut failures = Vec::new();
    for case in &cases {
        let (rows, ledger, opts) = build(&case.input);
        let rail = crate::render::render_rail(&rows, &ledger, &opts);
        let got = grid(&rail.ansi, opts.width as u16);
        if got.trim_end() == case.expect.trim_end() {
            eprintln!("PASS: {}", case.id);
        } else {
            eprintln!("FAIL: {}", case.id);
            failures.push(format!(
                "### {}\n--- expected ---\n{}\n--- got ---\n{}",
                case.id, case.expect, got
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} scenario(s) mismatch:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

