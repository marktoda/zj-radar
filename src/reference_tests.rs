//! Doc-as-oracle test harness: parse docs/rail-reference.md, build state,
//! run the real render pipeline, and compare the rendered grid to the doc's
//! expected block.
//!
//! Green regression guard: the renderer matches every `rail-expect` block in
//! docs/rail-reference.md. Editing a `rail-input`/`rail-expect` pair in the doc
//! edits this test — the doc is the single source of truth for rail rendering.

use crate::config::Density;
use crate::kind::Kind;
use crate::render::{GlyphSet, PaneDisplay, PrimaryDetail, ProgressCounts, RenderOpts, TabDisplay, TabRow};
use crate::status::Status;
use crate::theme::DerivedColors;

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

/// Parse a `rail-input` DSL block into `(Vec<TabRow>, RenderOpts)`.
///
/// Defaults: width=32, height=usize::MAX/2, glyphs=Plain, density=Compact, header=true.
///
/// For each tab, builds `TabDisplay` directly:
///   - `total` = number of tracked panes; `done` = count Done; `pending` = count Pending.
///   - `best_status` (the tab status): start Idle; for each pane where status.is_active(),
///     if severity > best OR (== severity AND this pane's since_tick >= current best's
///     since_tick), set best = this status and detail = Some(PrimaryDetail{...}).
///   - since_tick for each pane = its 0-based index in the tab's pane list.
fn build(input: &str) -> (Vec<TabRow>, RenderOpts) {
    let mut width: usize = 32;
    let mut height: usize = usize::MAX / 2;
    let mut glyphs = GlyphSet::Plain;

    struct PaneSpec {
        pane_id: u32,
        kind: Kind,
        status: Status,
        msg: String,
        since_tick: u64,
    }

    struct TabSpec {
        pos: usize,
        name: String,
        active: bool,
        panes: Vec<PaneSpec>,
    }

    let mut tabs: Vec<TabSpec> = Vec::new();
    let mut current_tab: Option<usize> = None; // index into tabs
    let mut next_pane_id: u32 = 1;

    for line in input.lines() {
        // Width / height / glyphs options (non-indented)
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

        // Tab declaration: `tab <pos> "<name>" [active]`
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
            let idx = tabs.len();
            tabs.push(TabSpec {
                pos,
                name: name.to_string(),
                active,
                panes: Vec::new(),
            });
            current_tab = Some(idx);
            continue;
        }

        // Pane line: indented `  <kind> <status> "<msg>"`
        if (line.starts_with("  ") || line.starts_with('\t')) && current_tab.is_some() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            // Parse: kind status "msg"
            let mut parts = trimmed.splitn(3, ' ');
            let kind_str = parts.next().unwrap_or("other");
            let status_str = parts.next().unwrap_or("idle");
            let msg_part = parts.next().unwrap_or("\"\"");
            // Strip surrounding quotes from msg
            let msg = if let Some(inner) = msg_part.strip_prefix('"') {
                inner.strip_suffix('"').unwrap_or(inner)
            } else {
                msg_part
            };

            let kind = Kind::from_source(kind_str);
            let status = Status::from_wire(status_str);
            let pane_id = next_pane_id;
            next_pane_id += 1;

            if let Some(idx) = current_tab {
                let since_tick = tabs[idx].panes.len() as u64;
                tabs[idx].panes.push(PaneSpec {
                    pane_id,
                    kind,
                    status,
                    msg: msg.to_string(),
                    since_tick,
                });
            }
            continue;
        }
    }

    // Build TabRow vec from collected specs
    let theme = DerivedColors::from_bg_fg((0, 0, 0), (200, 200, 200));
    let rows: Vec<TabRow> = tabs
        .into_iter()
        .map(|spec| {
            let display = if spec.panes.is_empty() {
                TabDisplay {
                    status: Status::Idle,
                    progress: ProgressCounts { done: 0, total: 0, pending: 0 },
                    detail: None,
                    panes: vec![],
                }
            } else {
                // Compute aggregated status + detail from panes
                let total = spec.panes.len();
                let done = spec.panes.iter().filter(|p| p.status == Status::Done).count();
                let pending = spec.panes.iter().filter(|p| p.status == Status::Pending).count();

                let mut best_status = Status::Idle;
                let mut best_tick: u64 = 0;
                let mut detail: Option<PrimaryDetail> = None;

                for pane in &spec.panes {
                    if pane.status.is_active() {
                        let is_better = if pane.status.severity() > best_status.severity() {
                            true
                        } else if pane.status.severity() == best_status.severity() {
                            pane.since_tick >= best_tick
                        } else {
                            false
                        };
                        if is_better {
                            best_status = pane.status;
                            best_tick = pane.since_tick;
                            detail = Some(PrimaryDetail {
                                repo: String::new(),
                                branch: String::new(),
                                msg: pane.msg.clone(),
                                since_tick: pane.since_tick,
                                status: pane.status,
                                kind: pane.kind,
                            });
                        }
                    }
                }

                let pane_displays: Vec<PaneDisplay> = spec.panes
                    .iter()
                    .map(|p| PaneDisplay::tracked(p.pane_id, p.kind, p.status, p.msg.clone()))
                    .collect();

                TabDisplay {
                    status: best_status,
                    // NOTE: these counts assume every listed pane is ever_active
                    // (tracked). The real `tab_display` gates done/total on
                    // `ever_active`. That divergence is inert today because the
                    // renderer reads no `progress` field (the right-slot was
                    // dropped). If ⟦D1⟧ revives done/total in the slot, mirror
                    // the `ever_active` gating here or the oracle could pass on
                    // wrong counts.
                    progress: ProgressCounts { done, total, pending },
                    detail,
                    panes: pane_displays,
                }
            };

            TabRow {
                number: spec.pos as u32,
                name: spec.name,
                active: spec.active,
                has_bell: false,
                display,
            }
        })
        .collect();

    let opts = RenderOpts {
        width,
        height,
        now_tick: 0,
        glyphs,
        header: true,
        density: Density::Compact,
        theme,
    };

    (rows, opts)
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
