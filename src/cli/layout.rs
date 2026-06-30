//! Pure Zellij-layout intelligence for `setup`/`run`: the canonical rail
//! fragments, layout analysis, the tailored snippet, and the injection transform.

/// The `default_tab_template` block (indented for nesting inside `layout {}`).
pub(crate) const DEFAULT_TAB_TEMPLATE: &str = r#"    default_tab_template {
        pane split_direction="vertical" {
            pane size=32 borderless=true {
                plugin location="radar"
            }
            children
        }
        pane size=2 borderless=true {
            plugin location="zellij:status-bar"
        }
    }"#;

/// The `new_tab_template` block (indented for nesting inside `layout {}`).
pub(crate) const NEW_TAB_TEMPLATE: &str = r#"    new_tab_template {
        pane split_direction="vertical" {
            pane size=32 borderless=true {
                plugin location="radar"
            }
            pane focus=true
        }
        pane size=2 borderless=true {
            plugin location="zellij:status-bar"
        }
    }"#;

/// The `tab_template name="ui"` block (indented for nesting inside `layout {}`).
pub(crate) const RAIL_UI_TEMPLATE: &str = r#"    tab_template name="ui" {
        pane split_direction="vertical" {
            pane size=32 borderless=true {
                plugin location="radar"
            }
            children
        }
        pane size=2 borderless=true {
            plugin location="zellij:status-bar"
        }
    }"#;

/// The three `swap_tiled_layout` blocks (indented for nesting inside `layout {}`).
pub(crate) const SWAP_BLOCKS: &str = r#"    swap_tiled_layout name="vertical" {
        ui max_panes=5 {
            pane split_direction="vertical" {
                pane
                pane { children; }
            }
        }
        ui max_panes=8 {
            pane split_direction="vertical" {
                pane { children; }
                pane { pane; pane; pane; pane; }
            }
        }
        ui max_panes=12 {
            pane split_direction="vertical" {
                pane { children; }
                pane { pane; pane; pane; pane; }
                pane { pane; pane; pane; pane; }
            }
        }
    }

    swap_tiled_layout name="horizontal" {
        ui max_panes=4 {
            pane
            pane
        }
        ui max_panes=8 {
            pane {
                pane split_direction="vertical" { children; }
                pane split_direction="vertical" { pane; pane; pane; pane; }
            }
        }
        ui max_panes=12 {
            pane {
                pane split_direction="vertical" { children; }
                pane split_direction="vertical" { pane; pane; pane; pane; }
                pane split_direction="vertical" { pane; pane; pane; pane; }
            }
        }
    }

    swap_tiled_layout name="stacked" {
        ui min_panes=5 {
            pane split_direction="vertical" {
                pane
                pane stacked=true { children; }
            }
        }
    }"#;

/// The full rail layout (3 templates + swaps), assembled from the canonical
/// fragments. Single source of truth shared by `run` (embeds it), the tailored
/// snippet, and injection.
pub(crate) fn full_layout() -> String {
    format!(
        "layout {{\n{DEFAULT_TAB_TEMPLATE}\n\n{NEW_TAB_TEMPLATE}\n\n{RAIL_UI_TEMPLATE}\n\n{SWAP_BLOCKS}\n\n    tab name=\"shell\" focus=true {{\n        pane\n    }}\n}}\n"
    )
}

/// Facts inferred from a raw layout string by pure substring scanning.
#[derive(Default)]
pub(crate) struct LayoutFacts {
    pub has_default_template: bool,
    pub has_swaps: bool,
    pub has_top_bar: bool,
    pub has_rail: bool,
    pub has_children_anchor: bool,
}

/// Analyze a raw KDL layout string and return its structural facts.
/// No parsing — substring/word scanning only.
pub(crate) fn analyze(layout: &str) -> LayoutFacts {
    LayoutFacts {
        has_default_template: layout.contains("default_tab_template"),
        has_swaps: layout.contains("swap_tiled_layout"),
        has_top_bar: layout.contains("zellij:tab-bar") || layout.contains("zellij:compact-bar"),
        has_rail: layout.contains("// zj-radar: begin")
            || layout.contains("plugin location=\"radar\""),
        has_children_anchor: layout
            .split_whitespace()
            .any(|tok| tok == "children" || tok.starts_with("children;")),
    }
}

/// Generate a situation-aware snippet for the user to paste into their Zellij
/// layout. If the rail is already present, returns a short "already integrated"
/// message. Otherwise assembles the minimal paste block from the canonical
/// fragments, annotated with notes about the user's specific situation.
pub(crate) fn tailored_snippet(facts: &LayoutFacts) -> String {
    if facts.has_rail {
        return "// already integrated, nothing to paste.".to_string();
    }

    let mut lines: Vec<&str> = Vec::new();

    // Situation notes go first so the user sees them before the paste block.
    if facts.has_top_bar {
        lines.push("// Note: the rail includes a status bar — replace your existing top bar pane.");
    }
    if facts.has_swaps {
        lines.push("// Note: swap layouts already present; the templates below slot into them.");
    } else {
        lines.push("// Note: swap layouts included below (enables Alt+] cycling between layouts).");
    }

    lines.push(DEFAULT_TAB_TEMPLATE);
    lines.push("");
    lines.push(NEW_TAB_TEMPLATE);

    if !facts.has_swaps {
        lines.push("");
        lines.push(RAIL_UI_TEMPLATE);
        lines.push("");
        lines.push(SWAP_BLOCKS);
    }

    lines.join("\n")
}

/// The rail vertical-split that wraps a tab's `children` anchor. Single source
/// of truth for the wrap shape — matches the body of `DEFAULT_TAB_TEMPLATE`.
/// Lines are joined and re-indented to the anchor's column at splice time.
const RAIL_PANE_WRAP: &[&str] = &[
    "pane split_direction=\"vertical\" {",
    "    pane size=32 borderless=true {",
    "        plugin location=\"radar\"",
    "    }",
    "    children",
    "}",
    "pane size=2 borderless=true {",
    "    plugin location=\"zellij:status-bar\"",
    "}",
];

/// Why an `inject` call declined to transform a layout. Injection is
/// fail-closed: any uncertainty about a layout's shape returns a `Refusal`
/// rather than risking a mangled config.
#[derive(Debug)]
pub(crate) enum Refusal {
    /// The layout is not valid KDL — the `kdl` parser rejected it.
    Unparseable(String),
    /// The layout parsed, but its shape isn't one we know how to transform
    /// safely (no `default_tab_template` and no top-level `children` anchor).
    Unrecognized(String),
}

/// Conservatively inject the rail into a user's Zellij layout.
///
/// Strategy: locate the `children` anchor via the KDL parser, then splice text
/// by byte offset so the rest of the file keeps its exact formatting. The
/// injected additions are fenced with `// zj-radar: begin`/`// zj-radar: end`
/// markers so they round-trip through idempotency and Task 7's uninstall.
///
/// Fail-closed: an unparseable layout or an unrecognized shape returns a
/// `Refusal`; we never edit on uncertainty.
pub(crate) fn inject(layout: &str, facts: &LayoutFacts) -> Result<String, Refusal> {
    // Already integrated — exact no-op. This is what makes re-injection
    // idempotent: a second pass sees `has_rail` and returns the input verbatim.
    if facts.has_rail {
        return Ok(layout.to_string());
    }

    let doc = layout
        .parse::<kdl::KdlDocument>()
        .map_err(|e| Refusal::Unparseable(e.to_string()))?;

    // Shape gate: the transform needs a template or a `children` anchor to wrap.
    // `analyze` already scanned for both; if neither is present this isn't a
    // shape we touch — fail-closed. (Checked post-parse so a malformed layout
    // is reported as `Unparseable`, not `Unrecognized`.)
    if !facts.has_children_anchor && !facts.has_default_template {
        return Err(Refusal::Unrecognized(
            "no `default_tab_template` or `children` anchor to wrap".into(),
        ));
    }

    // The transform only knows the `layout { ... }` shape.
    let layout_node = doc
        .nodes()
        .iter()
        .find(|n| n.name().value() == "layout")
        .ok_or_else(|| Refusal::Unrecognized("no top-level `layout` node".into()))?;
    let body = layout_node
        .children()
        .ok_or_else(|| Refusal::Unrecognized("`layout` has no body".into()))?;

    // We require a recognized shape: a `default_tab_template` whose body holds a
    // `children` anchor, or a top-level `children` anchor directly in `layout`.
    // Anything else (e.g. a hand-tiled tab) is refused — fail-closed.
    let anchor = body
        .nodes()
        .iter()
        .find(|n| n.name().value() == "default_tab_template")
        .and_then(|n| n.children())
        .and_then(find_children_anchor)
        .or_else(|| find_children_anchor(body))
        .ok_or_else(|| {
            Refusal::Unrecognized(
                "no `default_tab_template` or top-level `children` anchor to wrap".into(),
            )
        })?;

    // Byte ranges to splice. KDL spans index into the original source, so we
    // can edit text directly and preserve every other byte.
    let anchor_span = anchor.span();
    let anchor_start = anchor_span.offset();
    let anchor_end = anchor_start + anchor_span.len();

    // The anchor's indentation = the run of spaces/tabs preceding it on its line.
    let indent = line_indent(layout, anchor_start);

    // The layout's closing `}` is the last byte of the layout node's span.
    let layout_span = layout_node.span();
    let close_brace = layout_span.offset() + layout_span.len() - 1;
    debug_assert_eq!(&layout[close_brace..close_brace + 1], "}");

    // 1. Wrap the `children` anchor in the rail vertical split, re-indented to
    //    the anchor's column. Reuses the canonical `RAIL_PANE_WRAP`. The fence
    //    markers and body are indented to the anchor; the first line replaces
    //    the anchor in place (already at the right column).
    let wrap = indent_block(&fenced_lines(RAIL_PANE_WRAP), &indent);

    // 2. Assemble the additions appended before the closing brace: the `ui`
    //    template, the swap blocks, and a `new_tab_template` when absent.
    let mut additions = format!("{RAIL_UI_TEMPLATE}\n\n{SWAP_BLOCKS}");
    if !body
        .nodes()
        .iter()
        .any(|n| n.name().value() == "new_tab_template")
    {
        additions = format!("{NEW_TAB_TEMPLATE}\n\n{additions}");
    }
    let additions = fence(&additions);

    // Rebuild the source around the two edit points: replace the anchor with
    // the wrap, and insert the additions just before the closing brace. Both
    // offsets index the original `layout`, so a single forward pass is correct.
    let mut out = String::with_capacity(layout.len() + wrap.len() + additions.len() + 32);
    out.push_str(&layout[..anchor_start]);
    out.push_str(&wrap);
    out.push_str(&layout[anchor_end..close_brace]);
    out.push('\n');
    out.push_str(&additions);
    out.push('\n');
    out.push_str(&layout[close_brace..]);
    Ok(out)
}

/// Find a bare `children` anchor (no args, no body) that is a **direct child**
/// of `block`. Only direct children are considered — a `children` that lives
/// inside a user-defined split pane is NOT a template/top-level anchor and must
/// not be wrapped (that would break the user's own layout). A `children` that
/// carries arguments or a child block is not a plain anchor and is skipped.
fn find_children_anchor(block: &kdl::KdlDocument) -> Option<&kdl::KdlNode> {
    block.nodes().iter().find(|node| {
        node.name().value() == "children"
            && node.entries().is_empty()
            && node.children().is_none()
    })
}

/// The leading whitespace (spaces/tabs) on the line containing `offset`.
fn line_indent(src: &str, offset: usize) -> String {
    let line_start = src[..offset].rfind('\n').map_or(0, |i| i + 1);
    src[line_start..offset]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// Wrap the canonical wrap lines in the `// zj-radar:` marker fence so the
/// injected block can be recognized for idempotency and cleanly removed on
/// uninstall. Returns the fenced block as individual lines for re-indentation.
fn fenced_lines<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    let mut out = Vec::with_capacity(lines.len() + 2);
    out.push("// zj-radar: begin");
    out.extend_from_slice(lines);
    out.push("// zj-radar: end");
    out
}

/// Join `lines`, prefixing each with `indent` (blank lines stay blank). The
/// first line is *not* prefixed — it replaces the anchor in place, which
/// already sits at the right column.
fn indent_block(lines: &[&str], indent: &str) -> String {
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push('\n');
            if !line.is_empty() {
                out.push_str(indent);
            }
        }
        out.push_str(line);
    }
    out
}

/// Wrap a block of injected text in the `// zj-radar:` marker fence so it can be
/// recognized for idempotency and cleanly removed on uninstall.
fn fence(block: &str) -> String {
    format!("// zj-radar: begin\n{block}\n// zj-radar: end")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_layout_has_templates_swaps_and_alias() {
        let l = full_layout();
        assert!(l.contains("default_tab_template"));
        assert!(l.contains("new_tab_template"));
        assert!(l.contains("tab_template name=\"ui\""));
        assert_eq!(l.matches("swap_tiled_layout").count(), 3);
        assert!(l.contains("plugin location=\"radar\""));
    }

    #[test]
    fn snippet_is_situation_aware() {
        let none = LayoutFacts { has_default_template: true, has_swaps: false, has_top_bar: false, has_rail: false, has_children_anchor: true };
        let s = tailored_snippet(&none);
        assert!(s.contains("swap_tiled_layout"), "must include swaps when absent");

        let with_swaps = LayoutFacts { has_swaps: true, ..none };
        assert!(tailored_snippet(&with_swaps).to_lowercase().contains("already"), "note existing swaps");

        let with_bar = LayoutFacts { has_top_bar: true, ..none };
        assert!(tailored_snippet(&with_bar).to_lowercase().contains("replace"), "note the rail replaces the bar");

        let injected = LayoutFacts { has_rail: true, ..none };
        assert!(tailored_snippet(&injected).to_lowercase().contains("already integrated"));
    }

    #[test]
    fn analyze_detects_shape() {
        let clean = "layout {\n    default_tab_template {\n        children\n    }\n    tab { pane }\n}\n";
        let f = analyze(clean);
        assert!(f.has_default_template && f.has_children_anchor);
        assert!(!f.has_swaps && !f.has_rail);

        let with_bar = "layout {\n    default_tab_template {\n        pane size=1 { plugin location=\"zellij:compact-bar\" }\n        children\n    }\n}\n";
        assert!(analyze(with_bar).has_top_bar);

        let injected = format!("layout {{\n// zj-radar: begin\n{}\n// zj-radar: end\n}}\n", super::RAIL_UI_TEMPLATE);
        assert!(analyze(&injected).has_rail);
    }

    #[test]
    fn inject_wraps_children_and_adds_marked_swaps() {
        let clean = "layout {\n    default_tab_template {\n        children\n    }\n    tab { pane }\n}\n";
        let out = inject(clean, &analyze(clean)).unwrap();
        assert!(out.contains("// zj-radar: begin") && out.contains("// zj-radar: end"));
        assert!(out.contains("plugin location=\"radar\""));
        assert!(out.contains("swap_tiled_layout"));
        // re-analyze the output: now has the rail.
        assert!(analyze(&out).has_rail);
    }

    #[test]
    fn inject_is_idempotent() {
        let clean = "layout {\n    default_tab_template {\n        children\n    }\n    tab { pane }\n}\n";
        let once = inject(clean, &analyze(clean)).unwrap();
        let twice = inject(&once, &analyze(&once)).unwrap();
        assert_eq!(once, twice, "re-injecting must be a no-op");
    }

    #[test]
    fn inject_refuses_unparseable() {
        assert!(matches!(inject("layout { oops", &LayoutFacts::default()), Err(Refusal::Unparseable(_))));
    }

    #[test]
    fn inject_refuses_unrecognized_shape() {
        // No default_tab_template and no top-level children anchor.
        let weird = "layout {\n    tab { pane split_direction=\"vertical\" { pane; pane } }\n}\n";
        assert!(matches!(inject(weird, &analyze(weird)), Err(Refusal::Unrecognized(_))));
    }

    /// A realistic Zellij layout using bare booleans (`borderless=true`,
    /// `focus=true`) exactly as real users write them. Before the KDL v1-fallback
    /// fix this returned `Refusal::Unparseable` because the v2 parser rejects bare
    /// boolean values. After the fix it must succeed and produce valid KDL output.
    #[test]
    fn inject_realistic_layout_with_bare_booleans() {
        // Multi-line block style matches how Zellij layouts are actually written
        // (and what the kdl v1 parser accepts). Inline `{ pane }` is a kdl-v1
        // parser limitation, not a real Zellij layout style.
        let input = "\
layout {
    default_tab_template {
        pane size=1 borderless=true {
            plugin location=\"zellij:tab-bar\"
        }
        children
        pane size=2 borderless=true {
            plugin location=\"zellij:status-bar\"
        }
    }
    tab focus=true {
        pane
    }
}
";
        let facts = analyze(input);
        assert!(facts.has_default_template, "must detect default_tab_template");
        assert!(facts.has_children_anchor, "must detect children anchor");

        let out = inject(input, &facts).expect("inject must succeed on a realistic Zellij layout");

        assert!(out.contains("// zj-radar: begin"), "must contain begin marker");
        assert!(out.contains("// zj-radar: end"), "must contain end marker");
        assert!(out.contains("plugin location=\"radar\""), "must inject radar plugin");
        assert!(out.contains("swap_tiled_layout"), "must inject swap layouts");

        // The output must be parseable by the same KDL parser (v1-fallback).
        out.parse::<kdl::KdlDocument>()
            .expect("injected output must be valid KDL");
    }

    /// A layout whose only `children` anchor is nested inside a user-defined
    /// split pane — not a direct child of `default_tab_template` or `layout`.
    /// Injection must refuse with `Unrecognized` (fail-closed: wrapping the wrong
    /// node would silently corrupt the user's layout).
    #[test]
    fn inject_refuses_children_nested_in_user_split() {
        let weird = "\
layout {
    tab {
        pane split_direction=\"vertical\" {
            pane
            children
        }
    }
}
";
        // analyze sees the children token but there is no direct-child anchor.
        let facts = analyze(weird);
        assert!(facts.has_children_anchor, "analyze must see the nested children");
        assert!(!facts.has_default_template);

        assert!(
            matches!(inject(weird, &facts), Err(Refusal::Unrecognized(_))),
            "inject must refuse a layout whose only children is nested in a user split"
        );
    }

    /// Drift guard: `full_layout()` must stay byte-equal to `run_assets/radar.kdl`
    /// — the file `run` embeds and materializes. If the fragment consts diverge
    /// from the file, this test breaks loudly so we fix one authoritative source
    /// instead of silently shipping two layouts that disagree.
    #[test]
    fn full_layout_matches_run_asset() {
        assert_eq!(
            full_layout(),
            include_str!("run_assets/radar.kdl"),
            "`full_layout()` must be byte-equal to src/cli/run_assets/radar.kdl; \
             adjust the fragment consts or the file to re-sync"
        );
    }
}
