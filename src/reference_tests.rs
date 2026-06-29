//! Doc-as-oracle test harness: parse docs/rail-reference.md, build state,
//! run the real render pipeline, and compare the rendered grid to the doc's
//! expected block.
//!
//! Green regression guard: the renderer matches every `rail-expect` block in
//! docs/rail-reference.md. Editing a `rail-input`/`rail-expect` pair in the doc
//! edits this test — the doc is the single source of truth for rail rendering.

use crate::config::{Density, NamingMode};
use crate::radar_state::{PaneUpdate, RadarState, RadarTab, TabId, TerminalPane};
use crate::render::{GlyphSet, RenderOpts, TabRow};
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
/// DSL grammar (unchanged):
///   width N
///   height N
///   glyphs plain|nerd
///   tab <pos> "<name>" [active]
///     <kind> <status> "<msg>"   ← indented; one line per pane
///
/// For idle panes: Running is applied first (tick=0, same msg) to set
/// `ever_active=true`, then Idle (tick=1, same msg). This preserves the
/// idle-but-tracked behavior required by scenario J.
///
/// Panics on unknown `kind` or `status` tokens with a descriptive message.
fn build(input: &str) -> (Vec<TabRow>, RenderOpts) {
    use crate::kind::Kind;
    use crate::payload::to_wire;

    let mut width: usize = 32;
    let mut height: usize = usize::MAX / 2;
    let mut glyphs = GlyphSet::Plain;
    let mut density = Density::Compact;

    struct PaneSpec {
        pane_id: u32,
        kind: Kind,
        status: Status,
        msg: String,
        /// true = register pane but send NO status (→ PaneDisplay::Untracked)
        untracked: bool,
        /// Some(code) → command-origin path: command_changed + timer + on_exit.
        /// None → status_pipe path (existing behavior).
        exit_code: Option<Option<i32>>,
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

        // Tab declaration: `tab <pos> "<name>" [active] [bell]`
        if let Some(rest) = line.strip_prefix("tab ") {
            let rest = rest.trim();
            // Parse position
            let (pos_str, after_pos) = rest.split_once(' ').unwrap_or((rest, ""));
            let pos = pos_str.parse::<usize>().unwrap_or(0);
            let after_pos = after_pos.trim();
            // Parse name (quoted)
            let (name, after_name) = if let Some(inner) = after_pos.strip_prefix('"') {
                if let Some(end) = inner.find('"') {
                    (&inner[..end], inner[end + 1..].trim())
                } else {
                    (inner, "")
                }
            } else {
                (after_pos, "")
            };
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
                let title = if let Some(inner) = rest.trim().strip_prefix('"') {
                    inner.strip_suffix('"').unwrap_or(inner)
                } else {
                    rest.trim()
                };
                let pane_id = next_pane_id;
                next_pane_id += 1;
                if let Some(idx) = current_tab {
                    tabs[idx].panes.push(PaneSpec {
                        pane_id,
                        kind: Kind::Other,
                        status: Status::Idle,
                        msg: title.to_string(),
                        untracked: true,
                        exit_code: None,
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

            // Parse the (possibly-quoted) message and any trailing `exit <N>|?`.
            // If the msg starts with `"`, find the closing `"`, then look for
            // a trailer after it. Otherwise treat the whole rest as the msg.
            let (msg, exit_trailer) = if let Some(after_open) = rest.strip_prefix('"') {
                if let Some(close_pos) = after_open.find('"') {
                    let msg_inner = &after_open[..close_pos];
                    let after_close = after_open[close_pos + 1..].trim();
                    let trailer = if after_close.is_empty() { None } else { Some(after_close) };
                    (msg_inner, trailer)
                } else {
                    // No closing quote — treat the whole thing as the msg
                    (after_open, None)
                }
            } else {
                (rest, None)
            };

            // Validate kind token
            let valid_kinds = [
                "claude", "codex", "gemini", "command", "build",
                "test", "deploy", "server", "other",
            ];
            if !valid_kinds.contains(&kind_str) {
                panic!("reference DSL: unknown kind '{}' in scenario", kind_str);
            }

            // Validate status token against the single source of truth (the
            // `statuses!` table) so the DSL vocabulary can't drift from `Status`.
            // Kept strict (panic on unknown) to catch scenario typos — unlike the
            // lenient `from_wire` used to parse it below.
            if !Status::ALL.iter().any(|s| s.as_wire() == status_str) {
                panic!("reference DSL: unknown status '{}' in scenario", status_str);
            }

            // Parse optional `exit <N>|?` trailer (appears after the closing quote).
            // Presence of this trailer routes the pane through the command-origin
            // path (command_changed + timer + on_exit) instead of status_pipe.
            let exit_code: Option<Option<i32>> = if let Some(trailer) = exit_trailer {
                if let Some(code_str) = trailer.strip_prefix("exit ") {
                    let code_str = code_str.trim();
                    if code_str == "?" {
                        Some(None)
                    } else if let Ok(n) = code_str.parse::<i32>() {
                        Some(Some(n))
                    } else {
                        panic!(
                            "reference DSL: bad exit code '{}' — must be an integer or '?'",
                            code_str
                        );
                    }
                } else {
                    panic!(
                        "reference DSL: unknown pane trailer '{}' — only 'exit <N>|?' is supported",
                        trailer
                    );
                }
            } else {
                None
            };

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
                    untracked: false,
                    exit_code,
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
    radar.panes_changed(update, 0, NamingMode::Off);

    // 3. Apply status for each tracked pane.
    //
    // Two paths:
    //   a) status_pipe path (exit_code == None): the existing StatusPipe-origin path.
    //      Untracked panes get NO update → PaneDisplay::Untracked.
    //      For idle panes: first apply Running (tick=0) to set ever_active=true,
    //      then apply Idle (tick=1). This preserves scenario J's idle-but-tracked behavior.
    //
    //   b) command-origin path (exit_code == Some(code)): drives the CommandStore path
    //      so that pane_outcome() fires and end-result tags (✓ / (exit N) / ✗) render.
    //      Sequence:
    //        1. command_changed(tick=0) — registers a pending command with the msg as argv.
    //        2. timer(tick=1) — promotes pending→Running (debounce window = 1 tick).
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
                // First prime ever_active with Running, then switch to Idle
                let wire_running = to_wire(
                    pane.pane_id,
                    Status::Running,
                    "",
                    "",
                    &pane.msg,
                    None,
                    source,
                );
                radar.status_pipe(&wire_running, 0, NamingMode::Off);

                let wire_idle = to_wire(
                    pane.pane_id,
                    Status::Idle,
                    "",
                    "",
                    &pane.msg,
                    None,
                    source,
                );
                radar.status_pipe(&wire_idle, 1, NamingMode::Off);
            } else {
                let wire = to_wire(
                    pane.pane_id,
                    pane.status,
                    "",
                    "",
                    &pane.msg,
                    None,
                    source,
                );
                radar.status_pipe(&wire, 0, NamingMode::Off);
            }
        }
    }

    // Command-origin path for panes with an `exit <N>|?` trailer.
    // Step 1: command_changed (tick=0) — registers each pane as a pending command.
    // Step 2: timer(tick=1) — promotes all pending commands to Running (debounce=1).
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
        radar.timer(1);

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
        radar.panes_changed(update2, 2, NamingMode::Off);
    }

    // 4. Build RenderOpts
    let theme = DerivedColors::from_bg_fg((0, 0, 0), (200, 200, 200));
    let opts = RenderOpts {
        width,
        height,
        now_tick: 0,
        glyphs,
        header: true,
        density,
        theme,
    };

    (radar.rows(), opts)
}

// ── vt100 grid helper ────────────────────────────────────────────────────────

/// Render ANSI output through vt100 and return visible rows joined by '\n',
/// each row trimmed of trailing spaces, with trailing blank lines removed.
fn grid(ansi: &str, width: usize) -> String {
    let height = ansi.lines().count().max(1) as u16;
    let w = width as u16;
    let mut parser = vt100::Parser::new(height, w, 0);
    let joined = ansi.replace('\n', "\r\n");
    parser.process(joined.as_bytes());
    let screen = parser.screen();
    let lines: Vec<String> = (0..height)
        .map(|r| {
            (0..w)
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
        .collect();

    // Trim trailing blank lines
    let trimmed_end = lines
        .iter()
        .rposition(|l| !l.is_empty())
        .map(|i| i + 1)
        .unwrap_or(0);
    lines[..trimmed_end].join("\n")
}

// ── The test ─────────────────────────────────────────────────────────────────

#[test]
fn rail_reference_matches() {
    let doc = include_str!("../docs/rail-reference.md");
    let cases = parse_cases(doc);
    eprintln!("Found {} scenarios", cases.len());
    let mut failures = Vec::new();
    for case in &cases {
        let (rows, opts) = build(&case.input);
        let rail = crate::render::render_rail(&rows, &opts);
        let got = grid(&rail.ansi, opts.width);
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

