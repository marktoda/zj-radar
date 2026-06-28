//! `zj-radar setup [codex|zellij]` — idempotent, conflict-aware local wiring.
//! Claude is handled by the marketplace plugin; Zellij setup installs the wasm
//! at a stable path and manages the `radar` plugin alias in `config.kdl`.

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use toml_edit::{Array, DocumentMut, Item};

/// Our legacy Codex notify invocation — also the idempotency/uninstall marker.
const CODEX_NOTIFY_MARKER: [&str; 3] = ["zj-radar", "notify", "codex"];
const CODEX_HOOK_MARKER: &str = "ZJ_RADAR_CODEX_HOOK=v1";
const CODEX_HOOK_COMMAND: &str = "ZJ_RADAR_CODEX_HOOK=v1 zj-radar notify codex";
const CODEX_HOOK_COMMAND_WINDOWS: &str =
    "cmd /C \"set ZJ_RADAR_CODEX_HOOK=v1&& zj-radar notify codex\"";
const CODEX_HOOK_EVENTS: [&str; 7] = [
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "SubagentStart",
    "SubagentStop",
    "Stop",
];
const ZELLIJ_ALIAS_BEGIN: &str = "// zj-radar: managed plugin alias begin";
const ZELLIJ_ALIAS_END: &str = "// zj-radar: managed plugin alias end";
const ZELLIJ_LAYOUT_SNIPPET: &str = include_str!("../../examples/radar-template-snippet.kdl");

pub struct SetupOptions<'a> {
    pub targets: &'a [String],
    pub wasm: Option<&'a Path>,
    pub uninstall: bool,
    pub dry_run: bool,
    pub yes: bool,
    pub check: bool,
    pub legacy_notify: bool,
    pub force: bool,
}

#[derive(Debug)]
pub enum Outcome {
    Changed(String),
    Unchanged,
    Conflict,
}

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
    let mut root = parse_hooks_json(existing)?;
    strip_codex_hooks(&mut root)?;

    if install {
        add_codex_hooks(&mut root)?;
    }

    let new = json_pretty(&root)?;
    if normalized_json_text(existing) == new {
        Ok(Outcome::Unchanged)
    } else {
        Ok(Outcome::Changed(new))
    }
}

fn parse_hooks_json(existing: &str) -> Result<Value, String> {
    if existing.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    let parsed: Value =
        serde_json::from_str(existing).map_err(|e| format!("hooks.json is not valid JSON: {e}"))?;
    if !parsed.is_object() {
        return Err("hooks.json root must be an object".to_string());
    }
    validate_hooks_shape(&parsed)?;
    Ok(parsed)
}

fn validate_hooks_shape(root: &Value) -> Result<(), String> {
    let Some(hooks) = root.get("hooks") else {
        return Ok(());
    };
    let Some(events) = hooks.as_object() else {
        return Err("hooks.json `hooks` must be an object".to_string());
    };
    for (event, groups) in events {
        let Some(groups) = groups.as_array() else {
            return Err(format!("hooks.json `hooks.{event}` must be an array"));
        };
        for group in groups {
            let Some(group) = group.as_object() else {
                return Err(format!(
                    "hooks.json `hooks.{event}` entries must be objects"
                ));
            };
            if let Some(handlers) = group.get("hooks") {
                if !handlers.is_array() {
                    return Err(format!(
                        "hooks.json `hooks.{event}[].hooks` must be an array"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn strip_codex_hooks(root: &mut Value) -> Result<bool, String> {
    let Some(hooks) = root.get_mut("hooks") else {
        return Ok(false);
    };
    let Some(events) = hooks.as_object_mut() else {
        return Err("hooks.json `hooks` must be an object".to_string());
    };

    let mut changed = false;
    let event_names: Vec<String> = events.keys().cloned().collect();
    for event in event_names {
        let Some(groups) = events.get_mut(&event).and_then(Value::as_array_mut) else {
            return Err(format!("hooks.json `hooks.{event}` must be an array"));
        };
        let mut emptied_by_us = Vec::new();
        for group in groups.iter_mut() {
            let Some(group_obj) = group.as_object_mut() else {
                return Err(format!(
                    "hooks.json `hooks.{event}` entries must be objects"
                ));
            };
            let Some(handlers) = group_obj.get_mut("hooks") else {
                emptied_by_us.push(false);
                continue;
            };
            let Some(handlers) = handlers.as_array_mut() else {
                return Err(format!(
                    "hooks.json `hooks.{event}[].hooks` must be an array"
                ));
            };
            let before_handlers = handlers.len();
            handlers.retain(|handler| !codex_hook_handler_is_ours(handler));
            let group_changed = handlers.len() != before_handlers;
            changed |= group_changed;
            emptied_by_us.push(group_changed && handlers.is_empty());
        }
        let before_groups = groups.len();
        let mut idx = 0;
        groups.retain(|group| {
            let remove = group
                .get("hooks")
                .and_then(Value::as_array)
                .is_some_and(|handlers| handlers.is_empty())
                && emptied_by_us.get(idx).copied().unwrap_or(false);
            idx += 1;
            !remove
        });
        changed |= groups.len() != before_groups;
        if groups.is_empty() {
            events.remove(&event);
            changed = true;
        }
    }

    if events.is_empty() {
        root.as_object_mut()
            .expect("root validated as object")
            .remove("hooks");
        changed = true;
    }

    Ok(changed)
}

fn codex_hook_handler_is_ours(handler: &Value) -> bool {
    handler
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|command| command.contains(CODEX_HOOK_MARKER))
        || handler
            .get("commandWindows")
            .and_then(Value::as_str)
            .is_some_and(|command| command.contains(CODEX_HOOK_MARKER))
}

fn add_codex_hooks(root: &mut Value) -> Result<(), String> {
    let root_obj = root
        .as_object_mut()
        .ok_or_else(|| "hooks.json root must be an object".to_string())?;
    let hooks = root_obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| "hooks.json `hooks` must be an object".to_string())?;

    for event in CODEX_HOOK_EVENTS {
        let groups = hooks
            .entry(event.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        let groups = groups
            .as_array_mut()
            .ok_or_else(|| format!("hooks.json `hooks.{event}` must be an array"))?;
        groups.push(codex_hook_group());
    }
    Ok(())
}

fn codex_hook_group() -> Value {
    json!({
        "hooks": [
            {
                "type": "command",
                "command": CODEX_HOOK_COMMAND,
                "commandWindows": CODEX_HOOK_COMMAND_WINDOWS,
                "timeout": 5
            }
        ]
    })
}

fn normalized_json_text(existing: &str) -> String {
    parse_hooks_json(existing)
        .and_then(|v| json_pretty(&v))
        .unwrap_or_else(|_| existing.to_string())
}

fn json_pretty(value: &Value) -> Result<String, String> {
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

fn split_lines(existing: &str) -> Vec<String> {
    existing.lines().map(ToString::to_string).collect()
}

fn join_lines(lines: &[String]) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

fn strip_managed_zellij_alias(lines: &mut Vec<String>) -> bool {
    let mut changed = false;
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() != ZELLIJ_ALIAS_BEGIN {
            i += 1;
            continue;
        }
        let end = lines[i + 1..]
            .iter()
            .position(|line| line.trim() == ZELLIJ_ALIAS_END)
            .map(|offset| i + 1 + offset)
            .unwrap_or(lines.len().saturating_sub(1));
        lines.drain(i..=end);
        changed = true;
    }
    changed
}

fn is_unmanaged_radar_alias_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") || trimmed.starts_with("/-") {
        return false;
    }
    trimmed == "radar"
        || trimmed.starts_with("radar ")
        || trimmed.starts_with("radar\t")
        || trimmed.starts_with("radar{")
}

fn has_unmanaged_radar_alias(lines: &[String]) -> bool {
    lines.iter().any(|line| is_unmanaged_radar_alias_line(line))
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

fn find_plugins_insert(lines: &[String]) -> Option<(usize, String)> {
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || !trimmed.starts_with("plugins") || !line.contains('{') {
            continue;
        }
        let base_indent_len = line.len() - trimmed.len();
        let child_indent = format!("{}    ", &line[..base_indent_len]);
        let mut depth = brace_delta(line);
        for (j, next) in lines.iter().enumerate().skip(i + 1) {
            depth += brace_delta(next);
            if depth <= 0 {
                return Some((j, child_indent));
            }
        }
    }
    None
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

    if !install {
        let new = join_lines(&lines);
        return if new == existing {
            Ok(Outcome::Unchanged)
        } else {
            Ok(Outcome::Changed(new))
        };
    }

    if has_unmanaged_radar_alias(&lines) {
        if !force {
            return Ok(Outcome::Conflict);
        }
        remove_unmanaged_radar_aliases(&mut lines);
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

    let new = join_lines(&lines);
    if new == existing {
        Ok(Outcome::Unchanged)
    } else {
        Ok(Outcome::Changed(new))
    }
}

// ── Thin IO layer (not unit-tested) ──

fn codex_config_path() -> PathBuf {
    codex_home_dir().join("config.toml")
}

fn codex_hooks_path() -> PathBuf {
    codex_home_dir().join("hooks.json")
}

fn codex_home_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("CODEX_HOME") {
        return PathBuf::from(home);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".codex")
}

fn zellij_config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("ZELLIJ_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".config").join("zellij")
}

fn zellij_config_path(config_dir: &Path) -> PathBuf {
    config_dir.join("config.kdl")
}

fn zellij_wasm_dest(config_dir: &Path) -> PathBuf {
    config_dir.join("plugins").join("zj_radar.wasm")
}

fn zellij_plugin_location(path: &Path) -> String {
    let display_path = if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        path.strip_prefix(&home)
            .ok()
            .map(|rel| format!("~/{}", rel.display()))
            .unwrap_or_else(|| path.display().to_string())
    } else {
        path.display().to_string()
    };
    format!("file:{display_path}")
}

fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|p| p.join(bin).is_file()))
        .unwrap_or(false)
}

fn codex_installed() -> bool {
    which("codex") || codex_config_path().exists() || codex_hooks_path().exists()
}

/// Entry point for `zj-radar setup`.
pub fn run(options: SetupOptions<'_>) {
    let want_codex = (options.targets.is_empty() && options.wasm.is_none())
        || options.targets.iter().any(|a| a == "codex");
    let want_zellij = options.targets.iter().any(|a| a == "zellij") || options.wasm.is_some();
    for a in options
        .targets
        .iter()
        .filter(|a| !matches!(a.as_str(), "codex" | "zellij"))
    {
        eprintln!("zj-radar: setup does not support '{a}' (supported: codex, zellij). Skipping.");
    }
    if options.check {
        if want_zellij {
            check_zellij();
        }
        if want_codex {
            check_codex(options.legacy_notify);
        }
        return;
    }
    if want_zellij {
        setup_zellij(
            options.wasm,
            options.uninstall,
            options.dry_run,
            options.yes,
            options.force,
        );
    }
    if want_codex {
        setup_codex(
            options.uninstall,
            options.dry_run,
            options.yes,
            options.legacy_notify,
            options.force,
        );
    }
}

#[derive(Debug, PartialEq, Eq)]
enum CheckLevel {
    Ok,
    Warn,
    Missing,
}

#[derive(Debug, PartialEq, Eq)]
struct CheckItem {
    level: CheckLevel,
    name: &'static str,
    detail: String,
}

impl CheckItem {
    fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            level: CheckLevel::Ok,
            name,
            detail: detail.into(),
        }
    }

    fn warn(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            level: CheckLevel::Warn,
            name,
            detail: detail.into(),
        }
    }

    fn missing(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            level: CheckLevel::Missing,
            name,
            detail: detail.into(),
        }
    }
}

fn check_codex(legacy_notify: bool) {
    let config = std::fs::read_to_string(codex_config_path()).ok();
    let hooks = std::fs::read_to_string(codex_hooks_path()).ok();
    let items = codex_check_items(
        which("codex"),
        which("zj-radar"),
        config.as_deref(),
        hooks.as_deref(),
        legacy_notify,
    );
    println!("codex:");
    print_check_items(&items);
}

fn check_zellij() {
    println!("zellij:");
    let config_path = zellij_config_path(&zellij_config_dir());
    let wasm_path = zellij_wasm_dest(&zellij_config_dir());
    let items = [
        if config_path.exists() {
            CheckItem::ok("config", config_path.display().to_string())
        } else {
            CheckItem::missing("config", format!("not found at {}", config_path.display()))
        },
        if wasm_path.exists() {
            CheckItem::ok("wasm", wasm_path.display().to_string())
        } else {
            CheckItem::missing("wasm", format!("not found at {}", wasm_path.display()))
        },
    ];
    print_check_items(&items);
}

fn print_check_items(items: &[CheckItem]) {
    for item in items {
        let status = match item.level {
            CheckLevel::Ok => "ok",
            CheckLevel::Warn => "warn",
            CheckLevel::Missing => "missing",
        };
        println!("  {status} {}: {}", item.name, item.detail);
    }
}

fn codex_check_items(
    codex_on_path: bool,
    zj_radar_on_path: bool,
    config: Option<&str>,
    hooks: Option<&str>,
    legacy_notify: bool,
) -> Vec<CheckItem> {
    let mut items = Vec::new();
    items.push(if codex_on_path {
        CheckItem::ok("codex binary", "found on PATH")
    } else {
        CheckItem::missing("codex binary", "not found on PATH")
    });
    items.push(if zj_radar_on_path {
        CheckItem::ok("zj-radar binary", "found on PATH")
    } else {
        CheckItem::missing("zj-radar binary", "not found on PATH")
    });

    match config.map(codex_hooks_disabled_in_config).transpose() {
        Ok(Some(true)) => items.push(CheckItem::warn(
            "hooks feature",
            "`[features].hooks = false` disables Codex hooks",
        )),
        Ok(_) => items.push(CheckItem::ok(
            "hooks feature",
            "enabled or unset in config.toml",
        )),
        Err(e) => items.push(CheckItem::warn("config.toml", e)),
    }

    if legacy_notify {
        items.push(check_legacy_notify(config));
    } else {
        items.push(check_hooks_json(hooks));
        if config
            .and_then(|text| text.parse::<DocumentMut>().ok())
            .as_ref()
            .and_then(|doc| doc.get("notify"))
            .is_some_and(|notify| !notify_is_ours(Some(notify)))
        {
            items.push(CheckItem::ok(
                "legacy notify",
                "foreign notify is preserved; hooks do not use the notify slot",
            ));
        }
    }

    if !legacy_notify {
        items.push(CheckItem::warn(
            "hook trust",
            "run `/hooks` in Codex after install or hook changes",
        ));
    }
    items
}

fn check_hooks_json(hooks: Option<&str>) -> CheckItem {
    let Some(hooks) = hooks else {
        return CheckItem::missing("hooks.json", "zj-radar Codex hooks are not installed");
    };
    match codex_owned_hook_event_count(hooks) {
        Ok(count) if count == CODEX_HOOK_EVENTS.len() => {
            CheckItem::ok("hooks.json", "all zj-radar Codex hooks installed")
        }
        Ok(count) if count > 0 => CheckItem::warn(
            "hooks.json",
            format!(
                "partial zj-radar hook install ({count}/{})",
                CODEX_HOOK_EVENTS.len()
            ),
        ),
        Ok(_) => CheckItem::missing("hooks.json", "zj-radar Codex hooks are not installed"),
        Err(e) => CheckItem::warn("hooks.json", e),
    }
}

fn check_legacy_notify(config: Option<&str>) -> CheckItem {
    let Some(config) = config else {
        return CheckItem::missing("legacy notify", "config.toml not found");
    };
    match config.parse::<DocumentMut>() {
        Ok(doc) if notify_is_ours(doc.get("notify")) => {
            CheckItem::ok("legacy notify", "zj-radar owns Codex notify")
        }
        Ok(doc) if doc.get("notify").is_some() => {
            CheckItem::warn("legacy notify", "another command owns Codex notify")
        }
        Ok(_) => CheckItem::missing("legacy notify", "Codex notify is not installed"),
        Err(e) => CheckItem::warn("config.toml", format!("config.toml is not valid TOML: {e}")),
    }
}

fn codex_owned_hook_event_count(existing: &str) -> Result<usize, String> {
    let root = parse_hooks_json(existing)?;
    let Some(hooks) = root.get("hooks").and_then(Value::as_object) else {
        return Ok(0);
    };
    Ok(CODEX_HOOK_EVENTS
        .iter()
        .filter(|event| {
            hooks
                .get(**event)
                .and_then(Value::as_array)
                .is_some_and(|groups| {
                    groups
                        .iter()
                        .filter_map(|group| group.get("hooks").and_then(Value::as_array))
                        .flat_map(|handlers| handlers.iter())
                        .any(codex_hook_handler_is_ours)
                })
        })
        .count())
}

fn codex_hooks_disabled_in_config(existing: &str) -> Result<bool, String> {
    let doc = existing
        .parse::<DocumentMut>()
        .map_err(|e| format!("config.toml is not valid TOML: {e}"))?;
    Ok(doc
        .get("features")
        .and_then(Item::as_table_like)
        .and_then(|features| {
            features
                .get("hooks")
                .or_else(|| features.get("codex_hooks"))
                .and_then(Item::as_bool)
        })
        == Some(false))
}

fn setup_codex(uninstall: bool, dry_run: bool, yes: bool, legacy_notify: bool, force: bool) {
    if legacy_notify {
        setup_codex_notify(uninstall, dry_run, yes, force);
    } else {
        setup_codex_hooks(uninstall, dry_run, yes);
    }
}

fn setup_codex_hooks(uninstall: bool, dry_run: bool, yes: bool) {
    let path = codex_hooks_path();
    if !uninstall && !codex_installed() {
        println!("codex: skipped (binary/config not found)");
        return;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let outcome = match edit_codex_hooks(&existing, !uninstall) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("codex: refused — {e}");
            return;
        }
    };
    match outcome {
        Outcome::Unchanged if uninstall => {
            println!("codex: hooks already removed ({})", path.display())
        }
        Outcome::Unchanged => {
            println!("codex: hooks already up to date ({})", path.display());
            print_codex_hook_guidance();
        }
        Outcome::Conflict => unreachable!("codex hooks editor has no conflict outcome"),
        Outcome::Changed(new) => {
            if dry_run {
                println!("--- {} (dry-run) ---\n{new}", path.display());
                if !uninstall {
                    print_codex_hook_guidance();
                }
                return;
            }
            if !yes && !confirm(&format!("Write {}?", path.display())) {
                println!("codex: skipped (declined)");
                return;
            }
            if let Err(e) = write_atomic(&path, &new) {
                eprintln!("codex: write failed — {e}");
                return;
            }
            println!(
                "codex: hooks {} ({})",
                if uninstall { "removed" } else { "installed" },
                path.display()
            );
            if !uninstall {
                print_codex_hook_guidance();
            }
        }
    }
}

fn setup_codex_notify(uninstall: bool, dry_run: bool, yes: bool, force: bool) {
    let path = codex_config_path();
    if !uninstall && !codex_installed() {
        println!("codex: skipped (binary/config not found)");
        return;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let outcome = match edit_codex(&existing, !uninstall, force) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("codex: refused — {e}");
            return;
        }
    };
    match outcome {
        Outcome::Unchanged => println!(
            "codex: legacy notify already up to date ({})",
            path.display()
        ),
        Outcome::Conflict => {
            eprintln!(
                "codex: {} already has a different `notify` program. Refusing to overwrite it.\n\
                 Re-run with --legacy-notify --force to replace it, or use hook setup without --legacy-notify.",
                path.display()
            );
        }
        Outcome::Changed(new) => {
            if dry_run {
                println!("--- {} (dry-run) ---\n{new}", path.display());
                return;
            }
            if !yes && !confirm(&format!("Write {}?", path.display())) {
                println!("codex: skipped (declined)");
                return;
            }
            if let Err(e) = write_atomic(&path, &new) {
                eprintln!("codex: write failed — {e}");
                return;
            }
            println!(
                "codex: legacy notify {} ({})",
                if uninstall { "removed" } else { "installed" },
                path.display()
            );
        }
    }
}

fn print_codex_hook_guidance() {
    if codex_hooks_disabled() {
        eprintln!(
            "codex: warning — hooks appear disabled in {} (`[features].hooks = false`)",
            codex_config_path().display()
        );
    }
    println!("codex: run `/hooks` in Codex to review and trust the zj-radar command hook.");
}

fn codex_hooks_disabled() -> bool {
    let Ok(existing) = std::fs::read_to_string(codex_config_path()) else {
        return false;
    };
    codex_hooks_disabled_in_config(&existing).unwrap_or(false)
}

fn setup_zellij(wasm: Option<&Path>, uninstall: bool, dry_run: bool, yes: bool, force: bool) {
    let config_dir = zellij_config_dir();
    let config_path = zellij_config_path(&config_dir);
    let wasm_dest = zellij_wasm_dest(&config_dir);
    let location = zellij_plugin_location(&wasm_dest);

    if !uninstall {
        let Some(src) = wasm else {
            eprintln!("zellij: refused — pass --wasm <path-to-zj_radar.wasm>");
            return;
        };
        if !src.is_file() {
            eprintln!("zellij: refused — wasm not found at {}", src.display());
            return;
        }
    }

    let existing = std::fs::read_to_string(&config_path).unwrap_or_default();
    let outcome = match edit_zellij(&existing, &location, !uninstall, force) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("zellij: refused — {e}");
            return;
        }
    };

    match outcome {
        Outcome::Unchanged if uninstall => {
            println!("zellij: already removed ({})", config_path.display());
        }
        Outcome::Unchanged => {
            println!(
                "zellij: config already up to date ({})",
                config_path.display()
            );
            println_layout_snippet();
        }
        Outcome::Conflict => {
            eprintln!(
                "zellij: {} already has an unmanaged `radar` plugin alias. Refusing to overwrite it.\n\
                 Re-run with --force to replace it, or wire zj-radar manually.",
                config_path.display()
            );
        }
        Outcome::Changed(new) => {
            if dry_run {
                if !uninstall {
                    if let Some(src) = wasm {
                        println!(
                            "zellij: would copy {} -> {}",
                            src.display(),
                            wasm_dest.display()
                        );
                    }
                }
                println!("--- {} (dry-run) ---\n{new}", config_path.display());
                if !uninstall {
                    println_layout_snippet();
                }
                return;
            }
            let prompt = if uninstall {
                format!("Update {}?", config_path.display())
            } else {
                format!(
                    "Copy wasm to {} and update {}?",
                    wasm_dest.display(),
                    config_path.display()
                )
            };
            if !yes && !confirm(&prompt) {
                println!("zellij: skipped (declined)");
                return;
            }
            if !uninstall {
                if let Some(parent) = wasm_dest.parent() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        eprintln!("zellij: create plugin dir failed — {e}");
                        return;
                    }
                }
                let Some(src) = wasm else {
                    eprintln!("zellij: refused — pass --wasm <path-to-zj_radar.wasm>");
                    return;
                };
                if let Err(e) = std::fs::copy(src, &wasm_dest) {
                    eprintln!("zellij: wasm copy failed — {e}");
                    return;
                }
            }
            if let Err(e) = write_atomic(&config_path, &new) {
                eprintln!("zellij: config write failed — {e}");
                return;
            }
            println!(
                "zellij: {} ({})",
                if uninstall { "removed" } else { "installed" },
                config_path.display()
            );
            if !uninstall {
                println!("zellij: wasm installed at {}", wasm_dest.display());
                println_layout_snippet();
            }
        }
    }
}

fn println_layout_snippet() {
    println!(
        "\nAdd the sidebar to a Zellij layout with:\n\n{}",
        ZELLIJ_LAYOUT_SNIPPET.trim_end()
    );
}

fn confirm(prompt: &str) -> bool {
    use std::io::Write;
    print!("{prompt} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Back up the existing file, then write atomically (temp file + rename).
fn write_atomic(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    if path.exists() {
        let _ = std::fs::copy(path, path_with_suffix(path, ".zj-radar.bak"));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path_with_suffix(path, ".zj-radar.tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

fn path_with_suffix(path: &std::path::Path, suffix: &str) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|name| format!("{}{}", name.to_string_lossy(), suffix))
        .unwrap_or_else(|| format!("config{suffix}"));
    path.with_file_name(file_name)
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
    fn codex_hooks_reject_malformed_json_and_bad_shapes() {
        assert!(edit_codex_hooks("not json", true).is_err());
        assert!(edit_codex_hooks("[]", true).is_err());
        assert!(edit_codex_hooks(r#"{"hooks":[]}"#, true).is_err());
        assert!(edit_codex_hooks(r#"{"hooks":{"Stop":{}}}"#, true).is_err());
        assert!(edit_codex_hooks(r#"{"hooks":{"Stop":[{"hooks":{}}]}}"#, true).is_err());
    }

    #[test]
    fn codex_check_reports_hook_setup_ready_with_trust_reminder() {
        let hooks = match edit_codex_hooks("", true).unwrap() {
            Outcome::Changed(s) => s,
            o => panic!("{o:?}"),
        };
        let items = codex_check_items(true, true, Some("model = \"x\"\n"), Some(&hooks), false);
        assert!(items.contains(&CheckItem::ok("codex binary", "found on PATH")));
        assert!(items.contains(&CheckItem::ok("zj-radar binary", "found on PATH")));
        assert!(items.contains(&CheckItem::ok(
            "hooks feature",
            "enabled or unset in config.toml"
        )));
        assert!(items.contains(&CheckItem::ok(
            "hooks.json",
            "all zj-radar Codex hooks installed"
        )));
        assert!(items.iter().any(|item| item.name == "hook trust"
            && item.level == CheckLevel::Warn
            && item.detail.contains("/hooks")));
    }

    #[test]
    fn codex_check_warns_when_hooks_feature_is_disabled() {
        let hooks = match edit_codex_hooks("", true).unwrap() {
            Outcome::Changed(s) => s,
            o => panic!("{o:?}"),
        };
        let items = codex_check_items(
            true,
            true,
            Some("[features]\nhooks = false\n"),
            Some(&hooks),
            false,
        );
        assert!(items.iter().any(|item| item.name == "hooks feature"
            && item.level == CheckLevel::Warn
            && item.detail.contains("hooks = false")));
    }

    #[test]
    fn codex_check_reports_partial_or_malformed_hooks() {
        let partial = r#"{
          "hooks": {
            "Stop": [
              {
                "hooks": [
                  {
                    "type": "command",
                    "command": "ZJ_RADAR_CODEX_HOOK=v1 zj-radar notify codex"
                  }
                ]
              }
            ]
          }
        }"#;
        let items = codex_check_items(true, true, None, Some(partial), false);
        assert!(items.iter().any(|item| item.name == "hooks.json"
            && item.level == CheckLevel::Warn
            && item.detail.contains("partial")));

        let items = codex_check_items(true, true, None, Some("not json"), false);
        assert!(items.iter().any(|item| item.name == "hooks.json"
            && item.level == CheckLevel::Warn
            && item.detail.contains("not valid JSON")));
    }

    #[test]
    fn codex_check_notes_foreign_notify_is_preserved_for_hooks() {
        let hooks = match edit_codex_hooks("", true).unwrap() {
            Outcome::Changed(s) => s,
            o => panic!("{o:?}"),
        };
        let config = "notify = [\"/other\", \"turn-ended\"]\n";
        let items = codex_check_items(true, true, Some(config), Some(&hooks), false);
        assert!(items.iter().any(|item| item.name == "legacy notify"
            && item.level == CheckLevel::Ok
            && item.detail.contains("preserved")));
    }

    #[test]
    fn codex_check_legacy_notify_mode_reports_notify_slot() {
        let items = codex_check_items(
            true,
            true,
            Some("notify = [\"zj-radar\", \"notify\", \"codex\"]\n"),
            None,
            true,
        );
        assert!(items.contains(&CheckItem::ok(
            "legacy notify",
            "zj-radar owns Codex notify"
        )));

        let items = codex_check_items(true, true, Some("notify = [\"/other\"]\n"), None, true);
        assert!(items.iter().any(|item| item.name == "legacy notify"
            && item.level == CheckLevel::Warn
            && item.detail.contains("another command")));
        assert!(
            !items.iter().any(|item| item.name == "hook trust"),
            "legacy notify mode should not ask users to trust hooks"
        );
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
