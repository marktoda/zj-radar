//! `zj-radar setup [codex|zellij]` — idempotent, conflict-aware local wiring.
//! Claude is handled by the marketplace plugin; Zellij setup installs the wasm
//! at a stable path and manages the `radar` plugin alias in `config.kdl`.

use std::path::{Path, PathBuf};
use toml_edit::{Array, DocumentMut, Item};

/// Our Codex notify invocation — also the idempotency/uninstall marker.
const MARKER: [&str; 3] = ["zj-radar", "notify", "codex"];
const ZELLIJ_ALIAS_BEGIN: &str = "// zj-radar: managed plugin alias begin";
const ZELLIJ_ALIAS_END: &str = "// zj-radar: managed plugin alias end";
const ZELLIJ_LAYOUT_SNIPPET: &str = include_str!("../../examples/radar-template-snippet.kdl");

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
            a.len() == MARKER.len() && a.iter().zip(MARKER).all(|(v, m)| v.as_str() == Some(m))
        })
        .unwrap_or(false)
}

fn our_array() -> Array {
    let mut a = Array::new();
    for m in MARKER {
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
            MARKER[0], MARKER[1], MARKER[2]
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
    if let Some(home) = std::env::var_os("CODEX_HOME") {
        return PathBuf::from(home).join("config.toml");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".codex").join("config.toml")
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
    codex_config_path().exists() && which("codex")
}

/// Entry point for `zj-radar setup`.
pub fn run(
    targets: &[String],
    wasm: Option<&Path>,
    uninstall: bool,
    dry_run: bool,
    yes: bool,
    force: bool,
) {
    let want_codex = (targets.is_empty() && wasm.is_none()) || targets.iter().any(|a| a == "codex");
    let want_zellij = targets.iter().any(|a| a == "zellij") || wasm.is_some();
    for a in targets
        .iter()
        .filter(|a| !matches!(a.as_str(), "codex" | "zellij"))
    {
        eprintln!("zj-radar: setup does not support '{a}' (supported: codex, zellij). Skipping.");
    }
    if want_zellij {
        setup_zellij(wasm, uninstall, dry_run, yes, force);
    }
    if want_codex {
        setup_codex(uninstall, dry_run, yes, force);
    }
}

fn setup_codex(uninstall: bool, dry_run: bool, yes: bool, force: bool) {
    let path = codex_config_path();
    if !uninstall && !codex_installed() {
        if !path.exists() {
            println!("codex: skipped (no config at {})", path.display());
        } else {
            println!("codex: skipped (binary not found on PATH)");
        }
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
        Outcome::Unchanged => println!("codex: already up to date ({})", path.display()),
        Outcome::Conflict => {
            eprintln!(
                "codex: {} already has a different `notify` program. Refusing to overwrite it.\n\
                 Re-run with --force to replace it, or wire zj-radar manually.",
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
                "codex: {} ({})",
                if uninstall { "removed" } else { "installed" },
                path.display()
            );
        }
    }
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
