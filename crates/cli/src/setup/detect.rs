use super::*;

use serde_json::Value;
use std::path::{Path, PathBuf};
use toml_edit::Item;

/// True iff `notify` exists and equals our exact marker array.
pub fn notify_is_ours(item: Option<&Item>) -> bool {
    item.and_then(|i| i.as_array())
        .map(|a| {
            a.len() == CODEX_NOTIFY_MARKER.len()
                && a.iter()
                    .zip(CODEX_NOTIFY_MARKER)
                    .all(|(v, m)| v.as_str() == Some(m))
        })
        .unwrap_or(false)
}

pub(crate) fn codex_hook_handler_is_ours(handler: &Value) -> bool {
    handler
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|command| command.contains(CODEX_HOOK_MARKER))
        || handler
            .get("commandWindows")
            .and_then(Value::as_str)
            .is_some_and(|command| command.contains(CODEX_HOOK_MARKER))
}

/// The layout name a `config.kdl` selects via `default_layout "name"`, or
/// `None` when unset. Line-scan like the other config detectors: the node at
/// line start (not commented out), its first argument quoted or bare. This is
/// what makes `setup`/`--check` operate on the layout Zellij will actually
/// load — hardcoding `default.kdl` injected the rail into a file a
/// `default_layout "main"` user never sees.
pub(crate) fn default_layout_name(config_text: &str) -> Option<String> {
    for line in config_text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with("/-") {
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("default_layout") else {
            continue;
        };
        // Node name must end here (not e.g. `default_layout_x`).
        if !rest.starts_with([' ', '\t']) {
            continue;
        }
        let rest = rest.trim_start();
        let name = if let Some(quoted) = rest.strip_prefix('"') {
            quoted.split('"').next().unwrap_or("")
        } else {
            rest.split(|c: char| c.is_whitespace() || c == '{' || c == '/')
                .next()
                .unwrap_or("")
        };
        if !name.is_empty() {
            return Some(name.to_string());
        }
    }
    None
}

/// The layout name `setup`/`--check` should operate on: an explicit `--layout`
/// wins, else the config's `default_layout`, else Zellij's built-in `default`.
pub(crate) fn resolve_layout_name(explicit: Option<&str>, config_text: Option<&str>) -> String {
    explicit
        .map(str::to_string)
        .or_else(|| config_text.and_then(default_layout_name))
        .unwrap_or_else(|| "default".to_string())
}

/// The layout FILE `setup`/`--check` should operate on:
/// `<config_dir>/layouts/<name>.kdl` for the resolved layout name (see
/// [`resolve_layout_name`]). One resolution shared by the install path and the
/// doctor, so both inspect the layout Zellij actually loads (and the one a
/// `--layout` install just wrote).
pub(crate) fn resolve_layout_path(
    config_dir: &Path,
    explicit: Option<&str>,
    config_text: Option<&str>,
) -> PathBuf {
    config_dir
        .join("layouts")
        .join(format!("{}.kdl", resolve_layout_name(explicit, config_text)))
}

pub(crate) fn strip_managed_zellij_alias(lines: &mut Vec<String>) -> bool {
    let mut changed = false;
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() != ZELLIJ_ALIAS_BEGIN {
            i += 1;
            continue;
        }
        let Some(end) = lines[i + 1..]
            .iter()
            .position(|line| line.trim() == ZELLIJ_ALIAS_END)
            .map(|offset| i + 1 + offset)
        else {
            // Malformed block: a BEGIN with no matching END (a hand-edited or
            // truncated config). Skip it — draining to EOF here would delete
            // every user line below the stray marker. Fail closed: a destructive
            // op defaults to removing *nothing*, never *everything*.
            i += 1;
            continue;
        };
        lines.drain(i..=end);
        changed = true;
    }
    changed
}

pub(crate) fn is_unmanaged_radar_alias_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") || trimmed.starts_with("/-") {
        return false;
    }
    trimmed == "radar"
        || trimmed.starts_with("radar ")
        || trimmed.starts_with("radar\t")
        || trimmed.starts_with("radar{")
}

/// Per-line mask: is line `i` inside a `plugins { … }` block? The radar-alias
/// detectors scope to this — a user node that happens to be named `radar` in
/// some OTHER block (a keybind, a theme) is not a plugin alias, must not trip
/// `Outcome::Conflict`, and above all must never be deleted by `--force`'s
/// alias replacement. String- and comment-aware via `edit::brace_delta`.
pub(crate) fn in_plugins_block_mask(lines: &[String]) -> Vec<bool> {
    let mut mask = vec![false; lines.len()];
    let mut i = 0;
    while i < lines.len() {
        if !super::edit::is_plugins_node_line(&lines[i]) {
            i += 1;
            continue;
        }
        let mut depth = super::edit::brace_delta(&lines[i]);
        if depth <= 0 {
            // Self-contained `plugins {}` one-liner: nothing inside it.
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < lines.len() {
            depth += super::edit::brace_delta(&lines[j]);
            if depth <= 0 {
                break;
            }
            mask[j] = true;
            j += 1;
        }
        i = j + 1;
    }
    mask
}

/// An unmanaged `radar` alias *inside a `plugins` block*. Lines outside any
/// plugins block never count, however radar-shaped they look.
pub(crate) fn has_unmanaged_radar_alias(lines: &[String]) -> bool {
    let mask = in_plugins_block_mask(lines);
    lines
        .iter()
        .zip(&mask)
        .any(|(line, in_plugins)| *in_plugins && is_unmanaged_radar_alias_line(line))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_top_level_notify_is_ours(toml: &str) {
        let doc = toml.parse::<toml_edit::DocumentMut>().expect("valid toml");
        assert!(
            notify_is_ours(doc.get("notify")),
            "notify must be top-level and ours:\n{toml}"
        );
    }

    #[test]
    fn notify_is_ours_matches_exact_marker_array() {
        let existing = "notify = [\"zj-radar\", \"notify\", \"codex\"]\n";
        assert_top_level_notify_is_ours(existing);
    }

    #[test]
    fn notify_is_ours_rejects_foreign_array() {
        let doc = "notify = [\"/other\", \"turn-ended\"]\n"
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert!(!notify_is_ours(doc.get("notify")));
    }

    #[test]
    fn notify_is_ours_rejects_absent() {
        let doc = "model = \"x\"\n".parse::<toml_edit::DocumentMut>().unwrap();
        assert!(!notify_is_ours(doc.get("notify")));
    }

    #[test]
    fn codex_hook_handler_is_ours_matches_command_or_windows_variant() {
        let ours = serde_json::json!({"command": format!("{CODEX_HOOK_MARKER} zj-radar notify codex")});
        assert!(codex_hook_handler_is_ours(&ours));
        let ours_windows = serde_json::json!({"commandWindows": format!("{CODEX_HOOK_MARKER} zj-radar notify codex")});
        assert!(codex_hook_handler_is_ours(&ours_windows));
        let foreign = serde_json::json!({"command": "echo foreign"});
        assert!(!codex_hook_handler_is_ours(&foreign));
    }

    #[test]
    fn strip_managed_zellij_alias_removes_marker_block_only() {
        let mut lines: Vec<String> = vec![
            "plugins {".to_string(),
            ZELLIJ_ALIAS_BEGIN.to_string(),
            "    radar location=\"file:/x.wasm\" {".to_string(),
            "        naming \"managed\"".to_string(),
            "    }".to_string(),
            ZELLIJ_ALIAS_END.to_string(),
            "    tab-bar location=\"zellij:tab-bar\"".to_string(),
            "}".to_string(),
        ];
        let changed = strip_managed_zellij_alias(&mut lines);
        assert!(changed);
        assert!(!lines.iter().any(|l| l.contains("radar location")));
        assert!(lines.iter().any(|l| l.contains("tab-bar")));
    }

    #[test]
    fn strip_managed_zellij_alias_leaves_unmatched_begin_and_content_intact() {
        // A BEGIN marker with no matching END (hand-edited / truncated config)
        // must NOT drain to EOF — every user line below the stray marker stays.
        let mut lines: Vec<String> = vec![
            "plugins {".to_string(),
            ZELLIJ_ALIAS_BEGIN.to_string(),
            "    radar location=\"file:/x.wasm\"".to_string(),
            "    tab-bar location=\"zellij:tab-bar\"".to_string(),
            "}".to_string(),
            "keybinds {}".to_string(),
        ];
        let before = lines.clone();
        let changed = strip_managed_zellij_alias(&mut lines);
        assert!(!changed, "an unmatched BEGIN is malformed → no change");
        assert_eq!(lines, before, "no user content is deleted");
    }

    #[test]
    fn strip_managed_zellij_alias_noop_when_absent() {
        let mut lines: Vec<String> =
            vec!["plugins {".to_string(), "    tab-bar location=\"zellij:tab-bar\"".to_string(), "}".to_string()];
        let changed = strip_managed_zellij_alias(&mut lines);
        assert!(!changed);
        assert_eq!(lines.len(), 3);
    }

    fn in_plugins(lines: &[&str]) -> Vec<String> {
        let mut v = vec!["plugins {".to_string()];
        v.extend(lines.iter().map(|l| format!("    {l}")));
        v.push("}".to_string());
        v
    }

    #[test]
    fn has_unmanaged_radar_alias_detects_bare_and_block_forms() {
        assert!(has_unmanaged_radar_alias(&in_plugins(&["radar"])));
        assert!(has_unmanaged_radar_alias(&in_plugins(&["radar location=\"x\""])));
        assert!(has_unmanaged_radar_alias(&in_plugins(&["radar{"])));
    }

    #[test]
    fn has_unmanaged_radar_alias_ignores_comments_and_unrelated_lines() {
        assert!(!has_unmanaged_radar_alias(&in_plugins(&["// radar"])));
        assert!(!has_unmanaged_radar_alias(&in_plugins(&["/- radar"])));
        assert!(!has_unmanaged_radar_alias(&in_plugins(&["tab-bar location=\"zellij:tab-bar\""])));
    }

    #[test]
    fn radar_nodes_outside_a_plugins_block_are_not_aliases() {
        // A user's own node that happens to be named `radar` — in a keybind
        // block, a theme, or at top level — must not read as a plugin alias:
        // it would trip Outcome::Conflict, and `--force` would DELETE it.
        let keybind_radar: Vec<String> = vec![
            "keybinds {".into(),
            "    shared_except \"locked\" {".into(),
            "        radar location=\"whatever\"".into(),
            "    }".into(),
            "}".into(),
            "radar".into(), // top-level stray
        ];
        assert!(!has_unmanaged_radar_alias(&keybind_radar));

        // The same node INSIDE plugins still counts.
        let mut with_alias = keybind_radar.clone();
        with_alias.extend(in_plugins(&["radar location=\"file:/x.wasm\""]));
        assert!(has_unmanaged_radar_alias(&with_alias));
    }

    #[test]
    fn in_plugins_block_mask_tracks_nesting_and_one_liners() {
        let lines: Vec<String> = vec![
            "plugins {".into(),         // 0: opener, not inside
            "    radar {".into(),       // 1: inside
            "        naming \"x\"".into(), // 2: inside (nested)
            "    }".into(),             // 3: inside
            "}".into(),                 // 4: closer, not inside
            "plugins {}".into(),        // 5: one-liner, nothing inside
            "radar".into(),             // 6: outside
        ];
        assert_eq!(
            in_plugins_block_mask(&lines),
            vec![false, true, true, true, false, false, false],
        );
    }

    #[test]
    fn default_layout_name_parses_quoted_bare_and_ignores_comments() {
        assert_eq!(
            default_layout_name("theme \"nord\"\ndefault_layout \"main\"\n"),
            Some("main".to_string())
        );
        assert_eq!(default_layout_name("default_layout compact\n"), Some("compact".to_string()));
        // Commented-out (both KDL comment forms) and absent → None.
        assert_eq!(default_layout_name("// default_layout \"main\"\n"), None);
        assert_eq!(default_layout_name("/- default_layout \"main\"\n"), None);
        assert_eq!(default_layout_name("theme \"nord\"\n"), None);
        // A different node whose name merely starts with it doesn't match.
        assert_eq!(default_layout_name("default_layout_dir \"/tmp\"\n"), None);
        // Trailing comment after a bare name is not part of the name.
        assert_eq!(
            default_layout_name("default_layout main // my layout\n"),
            Some("main".to_string())
        );
    }

    #[test]
    fn resolve_layout_name_precedence_is_flag_config_default() {
        let config = Some("default_layout \"main\"\n");
        assert_eq!(resolve_layout_name(Some("mine"), config), "mine");
        assert_eq!(resolve_layout_name(None, config), "main");
        assert_eq!(resolve_layout_name(None, Some("theme \"nord\"\n")), "default");
        assert_eq!(resolve_layout_name(None, None), "default");
    }
}
