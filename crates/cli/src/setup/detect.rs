use super::*;

use serde_json::Value;
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

pub(crate) fn has_unmanaged_radar_alias(lines: &[String]) -> bool {
    lines.iter().any(|line| is_unmanaged_radar_alias_line(line))
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

    #[test]
    fn has_unmanaged_radar_alias_detects_bare_and_block_forms() {
        assert!(has_unmanaged_radar_alias(&["radar".to_string()]));
        assert!(has_unmanaged_radar_alias(&["radar location=\"x\"".to_string()]));
        assert!(has_unmanaged_radar_alias(&["radar{".to_string()]));
    }

    #[test]
    fn has_unmanaged_radar_alias_ignores_comments_and_unrelated_lines() {
        assert!(!has_unmanaged_radar_alias(&["// radar".to_string()]));
        assert!(!has_unmanaged_radar_alias(&["/- radar".to_string()]));
        assert!(!has_unmanaged_radar_alias(&["tab-bar location=\"zellij:tab-bar\"".to_string()]));
    }
}
