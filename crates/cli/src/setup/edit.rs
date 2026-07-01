use super::*;

use crate::setup::detect::{
    codex_hook_handler_is_ours, has_unmanaged_radar_alias, is_unmanaged_radar_alias_line, notify_is_ours,
    strip_managed_zellij_alias,
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use toml_edit::{Array, DocumentMut};

#[derive(Debug)]
pub enum Outcome {
    Changed(String),
    Unchanged,
    Conflict,
}

fn our_array() -> Array {
    let mut a = Array::new();
    for m in CODEX_NOTIFY_MARKER {
        a.push(m);
    }
    a
}

/// Pure editor. `install=true` adds/keeps our notify; `install=false` uninstalls.
/// Never clobbers a foreign notify unless `force`. Errors on unparseable TOML.
pub fn edit_codex(existing: &str, install: bool, force: bool) -> Result<Outcome, String> {
    let mut doc = existing
        .parse::<DocumentMut>()
        .map_err(|e| format!("config.toml is not valid TOML: {e}"))?;
    let present = doc.get("notify").is_some();
    let ours = notify_is_ours(doc.get("notify"));

    if install {
        if ours {
            return Ok(Outcome::Unchanged);
        }
        if present && !force {
            return Ok(Outcome::Conflict);
        }
        if present {
            // force overwrite of a foreign notify — in place, position preserved.
            doc["notify"] = toml_edit::value(our_array());
            return Ok(Outcome::Changed(doc.to_string()));
        }
        // Absent: prepend at byte 0 so the key stays top-level (a key appended
        // after an existing [table] would bind to that table). Preserves the
        // rest verbatim.
        let line = format!(
            "notify = [\"{}\", \"{}\", \"{}\"]\n",
            CODEX_NOTIFY_MARKER[0], CODEX_NOTIFY_MARKER[1], CODEX_NOTIFY_MARKER[2]
        );
        return Ok(Outcome::Changed(format!("{line}{existing}")));
    }

    // Uninstall: remove only if it's ours; leave a foreign/absent notify alone.
    if ours {
        doc.as_table_mut().remove("notify");
        Ok(Outcome::Changed(doc.to_string()))
    } else {
        Ok(Outcome::Unchanged)
    }
}

/// Pure editor for Codex `hooks.json`. It strips only marker-owned Radar
/// command hooks, then re-adds the current hook set when installing.
pub fn edit_codex_hooks(existing: &str, install: bool) -> Result<Outcome, String> {
    let mut file = parse_hooks_file(existing)?;
    strip_codex_hooks(&mut file);

    if install {
        add_codex_hooks(&mut file);
    }

    let new = json_pretty(&file)?;
    if normalized_hooks_text(existing) == new {
        Ok(Outcome::Unchanged)
    } else {
        Ok(Outcome::Changed(new))
    }
}

/// Typed view of a Codex `hooks.json`. Deserialization *is* the shape check:
/// the `hooks` map, its event arrays, and each group's optional handler array
/// must have these types or `serde_json` rejects the file — so there is no
/// separate hand-written validator. Foreign keys at every level are preserved
/// verbatim through the flattened `rest`/`meta` maps, and `handlers` stay as raw
/// `Value`s so unknown handler fields round-trip untouched.
///
/// `handlers` is `Option` (not a defaulted `Vec`) so an *absent* `hooks` key and
/// an explicit empty `hooks: []` stay distinct across a round-trip — the strip
/// logic and a preexisting-empty-group must tell them apart.
#[derive(Default, Serialize, Deserialize)]
pub(crate) struct HooksFile {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) hooks: BTreeMap<String, Vec<HookGroup>>,
    #[serde(flatten)]
    rest: Map<String, Value>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct HookGroup {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) hooks: Option<Vec<Value>>,
    #[serde(flatten)]
    meta: Map<String, Value>,
}

pub(crate) fn parse_hooks_file(existing: &str) -> Result<HooksFile, String> {
    if existing.trim().is_empty() {
        return Ok(HooksFile::default());
    }
    serde_json::from_str(existing).map_err(|e| format!("hooks.json is not valid JSON: {e}"))
}

/// Remove only Radar-owned handlers, then collapse any group/event we emptied.
/// A group is dropped only when *we* emptied it (it held our handlers and now
/// holds none) — a preexisting empty `hooks: []` or a group with no handler
/// array is left untouched.
fn strip_codex_hooks(file: &mut HooksFile) {
    for groups in file.hooks.values_mut() {
        groups.retain_mut(|group| {
            let Some(handlers) = group.hooks.as_mut() else {
                return true; // no handler array — not ours to touch
            };
            let before = handlers.len();
            handlers.retain(|handler| !codex_hook_handler_is_ours(handler));
            // Drop the group only if removing our handlers emptied it.
            !(handlers.len() != before && handlers.is_empty())
        });
    }
    // Drop events whose groups are all gone; an empty `hooks` map serializes away.
    file.hooks.retain(|_, groups| !groups.is_empty());
}

fn add_codex_hooks(file: &mut HooksFile) {
    for event in CODEX_HOOK_EVENTS {
        file.hooks
            .entry(event.to_string())
            .or_default()
            .push(codex_hook_group());
    }
}

fn codex_hook_group() -> HookGroup {
    HookGroup {
        hooks: Some(vec![json!({
            "type": "command",
            "command": CODEX_HOOK_COMMAND,
            "commandWindows": CODEX_HOOK_COMMAND_WINDOWS,
            "timeout": 5
        })]),
        meta: Map::new(),
    }
}

fn normalized_hooks_text(existing: &str) -> String {
    parse_hooks_file(existing)
        .and_then(|f| json_pretty(&f))
        .unwrap_or_else(|_| existing.to_string())
}

fn json_pretty<T: Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string_pretty(value)
        .map(|mut s| {
            s.push('\n');
            s
        })
        .map_err(|e| format!("hooks.json serialization failed: {e}"))
}

fn zellij_alias_lines(location: &str, indent: &str) -> Vec<String> {
    let escaped = kdl_string(location);
    vec![
        format!("{indent}{ZELLIJ_ALIAS_BEGIN}"),
        format!("{indent}radar location=\"{escaped}\" {{"),
        format!("{indent}    naming \"managed\""),
        format!("{indent}}}"),
        format!("{indent}{ZELLIJ_ALIAS_END}"),
    ]
}

fn kdl_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

pub(crate) fn split_lines(existing: &str) -> Vec<String> {
    existing.lines().map(ToString::to_string).collect()
}

fn join_lines(lines: &[String]) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

fn brace_delta(line: &str) -> isize {
    let mut delta = 0;
    let mut chars = line.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if !in_string && c == '/' && chars.peek() == Some(&'/') {
            break;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => delta += 1,
            '}' => delta -= 1,
            _ => {}
        }
    }
    delta
}

fn remove_unmanaged_radar_aliases(lines: &mut Vec<String>) -> bool {
    let mut changed = false;
    let mut i = 0;
    while i < lines.len() {
        if !is_unmanaged_radar_alias_line(&lines[i]) {
            i += 1;
            continue;
        }

        let mut end = i;
        let mut depth = brace_delta(&lines[i]);
        while depth > 0 && end + 1 < lines.len() {
            end += 1;
            depth += brace_delta(&lines[end]);
        }
        lines.drain(i..=end);
        changed = true;
    }
    changed
}

/// Is this line the opening of the KDL `plugins` block — not a sibling node
/// whose name merely starts with "plugins" (e.g. `plugins_extra {`)? The node
/// name must be followed by whitespace or the opening brace.
fn is_plugins_node_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    !trimmed.starts_with("//")
        && line.contains('{')
        && trimmed
            .strip_prefix("plugins")
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(['{', ' ', '\t']))
}

fn find_plugins_insert(lines: &[String]) -> Option<(usize, String)> {
    for (i, line) in lines.iter().enumerate() {
        if !is_plugins_node_line(line) {
            continue;
        }
        let trimmed = line.trim_start();
        let base_indent_len = line.len() - trimmed.len();
        let child_indent = format!("{}    ", &line[..base_indent_len]);
        let mut depth = brace_delta(line);
        // A self-contained one-liner (`plugins {}`) has depth 0 on its own line;
        // scanning forward from it would return some LATER block's closing
        // brace and splice the alias into the wrong scope. The caller expands
        // such lines first (`expand_one_line_plugins_block`), so skip here.
        if depth <= 0 {
            continue;
        }
        for (j, next) in lines.iter().enumerate().skip(i + 1) {
            depth += brace_delta(next);
            if depth <= 0 {
                return Some((j, child_indent));
            }
        }
    }
    None
}

/// Byte positions of a self-contained one-line block on `line`: the `{` that
/// opens depth 1 and the `}` that closes back to depth 0, string- and
/// comment-aware like [`brace_delta`]. `None` when the line doesn't both open
/// and close a block, or opens another one after closing.
fn one_line_block_spans(line: &str) -> Option<(usize, usize)> {
    let (mut open, mut close) = (None, None);
    let mut depth = 0isize;
    let mut in_string = false;
    let mut escaped = false;
    let mut chars = line.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if !in_string && c == '/' && chars.peek().map(|&(_, c)| c) == Some('/') {
            break;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => {
                depth += 1;
                if depth == 1 {
                    if close.is_some() {
                        return None; // a second block opens after the first closed
                    }
                    open.get_or_insert(i);
                }
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                }
            }
            _ => {}
        }
    }
    match (open, close, depth) {
        (Some(o), Some(c), 0) => Some((o, c)),
        _ => None,
    }
}

/// Expand a one-line `plugins { … }` node into a multi-line block so the alias
/// can be spliced in as a child: `plugins {}` becomes `plugins {` / `}`, with
/// any inline content moved to its own (still valid, `;`-separated) line and a
/// trailing comment kept on the closing line. Returns whether a line changed.
fn expand_one_line_plugins_block(lines: &mut Vec<String>) -> bool {
    for i in 0..lines.len() {
        if !is_plugins_node_line(&lines[i]) {
            continue;
        }
        let Some((open, close)) = one_line_block_spans(&lines[i]) else {
            continue;
        };
        let line = lines[i].clone();
        let indent: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        let inner = line[open + 1..close].trim().to_string();
        let mut repl = vec![line[..=open].to_string()];
        if !inner.is_empty() {
            repl.push(format!("{indent}    {inner}"));
        }
        repl.push(format!("{indent}{}", &line[close..]));
        lines.splice(i..=i, repl);
        return true;
    }
    false
}

/// Does any top-level `plugins` block contain a `radar` child? The post-edit
/// semantic check for an install: the alias must have landed where Zellij will
/// actually read it.
fn plugins_block_has_radar(doc: &kdl::KdlDocument) -> bool {
    doc.nodes()
        .iter()
        .filter(|n| n.name().value() == "plugins")
        .filter_map(|n| n.children())
        .any(|c| c.nodes().iter().any(|n| n.name().value() == "radar"))
}

/// Pure editor for `~/.config/zellij/config.kdl`. It manages only the
/// marker-tagged `radar` plugin alias; layout templates remain user-owned.
pub fn edit_zellij(
    existing: &str,
    location: &str,
    install: bool,
    force: bool,
) -> Result<Outcome, String> {
    let mut lines = split_lines(existing);
    strip_managed_zellij_alias(&mut lines);

    if install {
        if has_unmanaged_radar_alias(&lines) {
            if !force {
                return Ok(Outcome::Conflict);
            }
            remove_unmanaged_radar_aliases(&mut lines);
        }

        // A one-line `plugins {}`/`plugins { … }` can't take a child as-is —
        // expand it to a multi-line block first so the insert scan finds it.
        if find_plugins_insert(&lines).is_none() {
            expand_one_line_plugins_block(&mut lines);
        }
        if let Some((idx, indent)) = find_plugins_insert(&lines) {
            let alias = zellij_alias_lines(location, &indent);
            lines.splice(idx..idx, alias);
        } else {
            if !lines.is_empty() && !lines.last().is_some_and(|line| line.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push("plugins {".to_string());
            lines.extend(zellij_alias_lines(location, "    "));
            lines.push("}".to_string());
        }
    }

    let new = join_lines(&lines);
    if new == existing {
        return Ok(Outcome::Unchanged);
    }

    // Fail-closed backstop: the edits above are line-level, so any config shape
    // they mis-model must surface as an error, never as a corrupt write. Only
    // enforced when the ORIGINAL parses — a dialect our (v1-fallback) parser
    // can't read at all is out of scope for the guard, and refusing on it would
    // regress configs we edited fine before.
    if existing.parse::<kdl::KdlDocument>().is_ok() {
        let doc: kdl::KdlDocument = new.parse().map_err(|e| {
            format!("refusing to write config.kdl: the edited result is no longer valid KDL ({e}) — please report this")
        })?;
        if install && !plugins_block_has_radar(&doc) {
            return Err(
                "refusing to write config.kdl: the radar alias did not land inside a `plugins` block — please report this"
                    .to_string(),
            );
        }
    }
    Ok(Outcome::Changed(new))
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
    fn fresh_file_installs_our_notify() {
        let out = edit_codex("", true, false).unwrap();
        match out {
            Outcome::Changed(s) => assert_top_level_notify_is_ours(&s),
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn installs_above_existing_tables_stays_top_level() {
        let existing = "[marketplaces.x]\nsource = \"local\"\n";
        let out = edit_codex(existing, true, false).unwrap();
        match out {
            Outcome::Changed(s) => {
                assert_top_level_notify_is_ours(&s);
                assert!(
                    s.contains("[marketplaces.x]"),
                    "must preserve the user's table"
                );
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn idempotent_when_already_ours() {
        let existing = "notify = [\"zj-radar\", \"notify\", \"codex\"]\n";
        assert!(matches!(
            edit_codex(existing, true, false).unwrap(),
            Outcome::Unchanged
        ));
    }

    #[test]
    fn foreign_notify_refuses_without_force() {
        let existing = "notify = [\"/some/other/notifier\", \"turn-ended\"]\n";
        assert!(matches!(
            edit_codex(existing, true, false).unwrap(),
            Outcome::Conflict
        ));
    }

    #[test]
    fn foreign_notify_overwritten_with_force_preserves_rest() {
        let existing = "model = \"gpt-5.5\"\nnotify = [\"/other\", \"turn-ended\"]\n";
        match edit_codex(existing, true, true).unwrap() {
            Outcome::Changed(s) => {
                assert_top_level_notify_is_ours(&s);
                assert!(
                    s.contains("model = \"gpt-5.5\""),
                    "must preserve other keys"
                );
                assert!(!s.contains("/other"), "foreign notifier must be gone");
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn uninstall_removes_only_ours() {
        let ours = "notify = [\"zj-radar\", \"notify\", \"codex\"]\nmodel = \"x\"\n";
        match edit_codex(ours, false, false).unwrap() {
            Outcome::Changed(s) => {
                assert!(!s.contains("notify"));
                assert!(s.contains("model = \"x\""));
            }
            o => panic!("{o:?}"),
        }
        let foreign = "notify = [\"/other\", \"turn-ended\"]\n";
        assert!(matches!(
            edit_codex(foreign, false, false).unwrap(),
            Outcome::Unchanged
        ));
    }

    #[test]
    fn malformed_toml_is_refused() {
        assert!(edit_codex("this = = not toml", true, false).is_err());
    }

    fn hooks_value(json_text: &str) -> serde_json::Value {
        serde_json::from_str(json_text).unwrap()
    }

    fn hook_handler_count(json_text: &str) -> usize {
        let v = hooks_value(json_text);
        v.get("hooks")
            .and_then(Value::as_object)
            .map(|events| {
                events
                    .values()
                    .filter_map(Value::as_array)
                    .flat_map(|groups| groups.iter())
                    .filter_map(|group| group.get("hooks").and_then(Value::as_array))
                    .map(Vec::len)
                    .sum()
            })
            .unwrap_or(0)
    }

    #[test]
    fn codex_hooks_fresh_file_installs_all_events() {
        match edit_codex_hooks("", true).unwrap() {
            Outcome::Changed(s) => {
                let v = hooks_value(&s);
                for event in CODEX_HOOK_EVENTS {
                    assert!(
                        v.pointer(&format!("/hooks/{event}/0/hooks/0/command"))
                            .and_then(Value::as_str)
                            .is_some_and(|command| command.contains(CODEX_HOOK_MARKER)),
                        "missing owned hook for {event}:\n{s}"
                    );
                }
                assert_eq!(hook_handler_count(&s), CODEX_HOOK_EVENTS.len());
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn codex_hooks_are_idempotent_after_pretty_install() {
        let once = match edit_codex_hooks("", true).unwrap() {
            Outcome::Changed(s) => s,
            o => panic!("{o:?}"),
        };
        assert!(matches!(
            edit_codex_hooks(&once, true).unwrap(),
            Outcome::Unchanged
        ));
    }

    #[test]
    fn codex_hooks_preserve_foreign_hooks_and_replaces_ours() {
        let existing = r#"{
          "hooks": {
            "PreToolUse": [
              {
                "matcher": "Bash",
                "hooks": [
                  {
                    "type": "command",
                    "command": "echo foreign"
                  },
                  {
                    "type": "command",
                    "command": "ZJ_RADAR_CODEX_HOOK=v1 old-zj-radar notify codex"
                  }
                ]
              }
            ]
          }
        }"#;
        match edit_codex_hooks(existing, true).unwrap() {
            Outcome::Changed(s) => {
                assert!(s.contains("echo foreign"));
                assert!(!s.contains("old-zj-radar"));
                assert_eq!(hook_handler_count(&s), CODEX_HOOK_EVENTS.len() + 1);
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn codex_hooks_uninstall_removes_only_ours() {
        let installed = match edit_codex_hooks(
            r#"{
              "hooks": {
                "Stop": [
                  {
                    "hooks": [
                      {
                        "type": "command",
                        "command": "echo foreign"
                      },
                      {
                        "type": "command",
                        "command": "ZJ_RADAR_CODEX_HOOK=v1 zj-radar notify codex"
                      }
                    ]
                  }
                ]
              }
            }"#,
            false,
        )
        .unwrap()
        {
            Outcome::Changed(s) => s,
            o => panic!("{o:?}"),
        };
        assert!(installed.contains("echo foreign"));
        assert!(!installed.contains(CODEX_HOOK_MARKER));
    }

    #[test]
    fn codex_hooks_uninstall_only_ours_collapses_empty_container() {
        let installed = match edit_codex_hooks("", true).unwrap() {
            Outcome::Changed(s) => s,
            o => panic!("{o:?}"),
        };
        match edit_codex_hooks(&installed, false).unwrap() {
            Outcome::Changed(s) => assert_eq!(hooks_value(&s), json!({})),
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn codex_hooks_preserve_preexisting_empty_groups() {
        let existing = r#"{
          "hooks": {
            "Stop": [
              {
                "matcher": "Bash",
                "hooks": []
              }
            ]
          }
        }"#;
        match edit_codex_hooks(existing, false).unwrap() {
            Outcome::Unchanged => {}
            o => panic!("{o:?}"),
        }
        match edit_codex_hooks(existing, true).unwrap() {
            Outcome::Changed(s) => {
                let empty = hooks_value(&s)
                    .pointer("/hooks/Stop/0/hooks")
                    .and_then(Value::as_array)
                    .is_some_and(Vec::is_empty);
                assert!(empty, "preexisting empty group should remain:\n{s}");
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn codex_hooks_preserve_foreign_top_level_and_group_keys() {
        // Foreign top-level keys and group-level metadata (e.g. `matcher`) must
        // survive a round-trip — they flow through the flattened `rest`/`meta`.
        let existing = r#"{
          "model": "gpt-5",
          "hooks": {
            "Stop": [
              {
                "matcher": "Bash",
                "hooks": [
                  { "type": "command", "command": "echo foreign" }
                ]
              }
            ]
          }
        }"#;
        let s = match edit_codex_hooks(existing, true).unwrap() {
            Outcome::Changed(s) => s,
            o => panic!("{o:?}"),
        };
        let v = hooks_value(&s);
        assert_eq!(v.pointer("/model").and_then(Value::as_str), Some("gpt-5"));
        assert_eq!(
            v.pointer("/hooks/Stop/0/matcher").and_then(Value::as_str),
            Some("Bash"),
            "foreign group metadata must be preserved:\n{s}"
        );
        assert!(s.contains("echo foreign"), "foreign handler must survive:\n{s}");
        assert!(s.contains(CODEX_HOOK_MARKER), "our hook must be added:\n{s}");
    }

    #[test]
    fn codex_hooks_reject_malformed_json_and_bad_shapes() {
        assert!(edit_codex_hooks("not json", true).is_err());
        assert!(edit_codex_hooks("[]", true).is_err());
        assert!(edit_codex_hooks(r#"{"hooks":[]}"#, true).is_err());
        assert!(edit_codex_hooks(r#"{"hooks":{"Stop":{}}}"#, true).is_err());
        assert!(edit_codex_hooks(r#"{"hooks":{"Stop":[{"hooks":{}}]}}"#, true).is_err());
    }

    #[test]
    fn zellij_fresh_config_adds_plugins_alias_block() {
        match edit_zellij(
            "",
            "file:~/.config/zellij/plugins/zj_radar.wasm",
            true,
            false,
        )
        .unwrap()
        {
            Outcome::Changed(s) => {
                assert!(s.contains("plugins {"));
                assert!(s.contains(ZELLIJ_ALIAS_BEGIN));
                assert!(
                    s.contains("radar location=\"file:~/.config/zellij/plugins/zj_radar.wasm\"")
                );
                assert!(s.contains("naming \"managed\""));
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn zellij_existing_plugins_block_gets_alias_child() {
        let existing = "keybinds {}\n\nplugins {\n    tab-bar location=\"zellij:tab-bar\"\n}\n";
        match edit_zellij(existing, "file:/tmp/zj_radar.wasm", true, false).unwrap() {
            Outcome::Changed(s) => {
                assert!(s.contains("tab-bar location=\"zellij:tab-bar\""));
                assert!(s.contains("    radar location=\"file:/tmp/zj_radar.wasm\""));
                assert!(s.contains("plugins {\n    tab-bar"));
            }
            o => panic!("{o:?}"),
        }
    }

    fn assert_radar_inside_plugins(config: &str) {
        let doc: kdl::KdlDocument = config
            .parse()
            .expect("edited config must stay valid KDL");
        assert!(
            plugins_block_has_radar(&doc),
            "radar alias must land inside a `plugins` block:\n{config}"
        );
        // The alias must not have leaked into any OTHER top-level block.
        for node in doc.nodes().iter().filter(|n| n.name().value() != "plugins") {
            if let Some(children) = node.children() {
                assert!(
                    !children.nodes().iter().any(|n| n.name().value() == "radar"),
                    "radar alias leaked into `{}`:\n{config}",
                    node.name().value()
                );
            }
        }
    }

    /// Regression: a one-line `plugins {}` followed by another block used to
    /// splice the alias into the WRONG scope (the next block's interior, or top
    /// level), writing a config Zellij rejects at startup.
    #[test]
    fn zellij_one_line_plugins_block_followed_by_sibling_block() {
        let existing = "plugins {}\nkeybinds {\n    normal {\n    }\n}\n";
        match edit_zellij(existing, "file:/tmp/zj_radar.wasm", true, false).unwrap() {
            Outcome::Changed(s) => {
                assert_radar_inside_plugins(&s);
                assert!(s.contains("keybinds"), "keybinds block preserved");
            }
            other => panic!("expected Changed, got {other:?}"),
        }
    }

    #[test]
    fn zellij_one_line_plugins_block_variants_get_alias_inside_plugins() {
        // Empty, inline-content (`;`-separated), and trailing-comment
        // one-liners — each must end with the alias inside `plugins`.
        for existing in [
            "plugins {}\n",
            "plugins { tab-bar location=\"zellij:tab-bar\"; }\n",
            "plugins {} // aliases live here\n",
        ] {
            match edit_zellij(existing, "file:/tmp/zj_radar.wasm", true, false).unwrap() {
                Outcome::Changed(s) => assert_radar_inside_plugins(&s),
                other => panic!("expected Changed for {existing:?}, got {other:?}"),
            }
        }
    }

    /// The one-liner expansion must not break idempotency: installing twice
    /// over an expanded block is byte-identical after the first install.
    #[test]
    fn zellij_one_line_plugins_block_install_is_idempotent() {
        let once = match edit_zellij("plugins {}\n", "file:/tmp/zj_radar.wasm", true, false).unwrap() {
            Outcome::Changed(s) => s,
            other => panic!("expected Changed, got {other:?}"),
        };
        assert!(matches!(
            edit_zellij(&once, "file:/tmp/zj_radar.wasm", true, false).unwrap(),
            Outcome::Unchanged
        ));
    }

    #[test]
    fn zellij_managed_alias_is_idempotent() {
        let once = match edit_zellij("", "file:/tmp/zj_radar.wasm", true, false).unwrap() {
            Outcome::Changed(s) => s,
            o => panic!("{o:?}"),
        };
        assert!(matches!(
            edit_zellij(&once, "file:/tmp/zj_radar.wasm", true, false).unwrap(),
            Outcome::Unchanged
        ));
    }

    #[test]
    fn zellij_unmanaged_radar_alias_conflicts_without_force() {
        let existing = "plugins {\n    radar location=\"file:/other.wasm\"\n}\n";
        assert!(matches!(
            edit_zellij(existing, "file:/tmp/zj_radar.wasm", true, false).unwrap(),
            Outcome::Conflict
        ));
    }

    #[test]
    fn zellij_force_replaces_unmanaged_radar_alias() {
        let existing = "plugins {\n    radar location=\"file:/other.wasm\" {\n        x \"y\"\n    }\n    tab-bar location=\"zellij:tab-bar\"\n}\n";
        match edit_zellij(existing, "file:/tmp/zj_radar.wasm", true, true).unwrap() {
            Outcome::Changed(s) => {
                assert!(!s.contains("/other.wasm"));
                assert!(s.contains("tab-bar location=\"zellij:tab-bar\""));
                assert!(s.contains("radar location=\"file:/tmp/zj_radar.wasm\""));
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn zellij_uninstall_removes_only_managed_alias() {
        let installed = match edit_zellij(
            "plugins {\n    tab-bar location=\"zellij:tab-bar\"\n}\n",
            "file:/tmp/zj_radar.wasm",
            true,
            false,
        )
        .unwrap()
        {
            Outcome::Changed(s) => s,
            o => panic!("{o:?}"),
        };
        match edit_zellij(&installed, "file:/tmp/zj_radar.wasm", false, false).unwrap() {
            Outcome::Changed(s) => {
                assert!(!s.contains("zj_radar.wasm"));
                assert!(s.contains("tab-bar location=\"zellij:tab-bar\""));
            }
            o => panic!("{o:?}"),
        }
    }
}
