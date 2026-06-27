//! `zj-radar setup [codex]` — idempotent, conflict-aware agent config wiring.
//! v1 supports Codex only (Claude is handled by the marketplace plugin).

use std::path::PathBuf;
use toml_edit::{Array, DocumentMut, Item};

/// Our Codex notify invocation — also the idempotency/uninstall marker.
const MARKER: [&str; 3] = ["zj-radar", "notify", "codex"];

#[derive(Debug)]
pub enum Outcome {
    Changed(String),
    Unchanged,
    Conflict,
}

/// True iff `notify` exists and equals our exact marker array.
pub fn notify_is_ours(item: Option<&Item>) -> bool {
    item.and_then(|i| i.as_array())
        .map(|a| a.len() == MARKER.len() && a.iter().zip(MARKER).all(|(v, m)| v.as_str() == Some(m)))
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
        let line = format!("notify = [\"{}\", \"{}\", \"{}\"]\n", MARKER[0], MARKER[1], MARKER[2]);
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

// ── Thin IO layer (not unit-tested) ──

fn codex_config_path() -> PathBuf {
    if let Some(home) = std::env::var_os("CODEX_HOME") {
        return PathBuf::from(home).join("config.toml");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".codex").join("config.toml")
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
pub fn run(agents: &[String], uninstall: bool, dry_run: bool, yes: bool, force: bool) {
    let want_codex = agents.is_empty() || agents.iter().any(|a| a == "codex");
    for a in agents.iter().filter(|a| a.as_str() != "codex") {
        eprintln!("zj-radar: setup does not support '{a}' (v1 supports: codex). Skipping.");
    }
    if !want_codex {
        return;
    }
    setup_codex(uninstall, dry_run, yes, force);
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
            println!("codex: {} ({})", if uninstall { "removed" } else { "installed" }, path.display());
        }
    }
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
        let _ = std::fs::copy(path, path.with_extension("toml.zj-radar.bak"));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("toml.zj-radar.tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_top_level_notify_is_ours(toml: &str) {
        let doc = toml.parse::<toml_edit::DocumentMut>().expect("valid toml");
        assert!(notify_is_ours(doc.get("notify")), "notify must be top-level and ours:\n{toml}");
    }

    #[test]
    fn fresh_file_installs_our_notify() {
        let out = edit_codex("", true, false).unwrap();
        match out { Outcome::Changed(s) => assert_top_level_notify_is_ours(&s), o => panic!("{o:?}") }
    }

    #[test]
    fn installs_above_existing_tables_stays_top_level() {
        let existing = "[marketplaces.x]\nsource = \"local\"\n";
        let out = edit_codex(existing, true, false).unwrap();
        match out {
            Outcome::Changed(s) => {
                assert_top_level_notify_is_ours(&s);
                assert!(s.contains("[marketplaces.x]"), "must preserve the user's table");
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn idempotent_when_already_ours() {
        let existing = "notify = [\"zj-radar\", \"notify\", \"codex\"]\n";
        assert!(matches!(edit_codex(existing, true, false).unwrap(), Outcome::Unchanged));
    }

    #[test]
    fn foreign_notify_refuses_without_force() {
        let existing = "notify = [\"/some/other/notifier\", \"turn-ended\"]\n";
        assert!(matches!(edit_codex(existing, true, false).unwrap(), Outcome::Conflict));
    }

    #[test]
    fn foreign_notify_overwritten_with_force_preserves_rest() {
        let existing = "model = \"gpt-5.5\"\nnotify = [\"/other\", \"turn-ended\"]\n";
        match edit_codex(existing, true, true).unwrap() {
            Outcome::Changed(s) => {
                assert_top_level_notify_is_ours(&s);
                assert!(s.contains("model = \"gpt-5.5\""), "must preserve other keys");
                assert!(!s.contains("/other"), "foreign notifier must be gone");
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn uninstall_removes_only_ours() {
        let ours = "notify = [\"zj-radar\", \"notify\", \"codex\"]\nmodel = \"x\"\n";
        match edit_codex(ours, false, false).unwrap() {
            Outcome::Changed(s) => { assert!(!s.contains("notify")); assert!(s.contains("model = \"x\"")); }
            o => panic!("{o:?}"),
        }
        let foreign = "notify = [\"/other\", \"turn-ended\"]\n";
        assert!(matches!(edit_codex(foreign, false, false).unwrap(), Outcome::Unchanged));
    }

    #[test]
    fn malformed_toml_is_refused() {
        assert!(edit_codex("this = = not toml", true, false).is_err());
    }
}
