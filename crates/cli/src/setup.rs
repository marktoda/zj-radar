//! `zj-radar setup [codex|zellij]` — idempotent, conflict-aware local wiring.
//! Claude is handled by the marketplace plugin; Zellij setup installs the wasm
//! at a stable path and manages the `radar` plugin alias in `config.kdl`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use toml_edit::{Array, DocumentMut, Item};

/// Our legacy Codex notify invocation — also the idempotency/uninstall marker.
const CODEX_NOTIFY_MARKER: [&str; 3] = ["zj-radar", "notify", "codex"];
// Also used by `run`'s producer detection so the two agree on what marks a wired
// Codex producer (shared single source of truth).
pub(crate) const CODEX_HOOK_MARKER: &str = "ZJ_RADAR_CODEX_HOOK=v1";
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

pub struct SetupOptions<'a> {
    pub targets: &'a [String],
    pub wasm: Option<&'a Path>,
    /// Fetch the wasm matching this CLI's version instead of passing `--wasm`.
    pub download: bool,
    pub uninstall: bool,
    pub dry_run: bool,
    pub yes: bool,
    pub check: bool,
    pub legacy_notify: bool,
    pub force: bool,
    /// Non-interactive consent to inject the rail into the target layout.
    pub inject: bool,
    /// Layout name to inject into (`<config_dir>/layouts/<name>.kdl`).
    /// `None` means `default`.
    pub layout: Option<&'a str>,
    /// Open the plugin in a focused floating pane so Zellij can prompt for
    /// permissions (one-time grant). Exits after launching; skips wasm/alias/inject.
    pub grant: bool,
}

/// Where the wasm artifact comes from. A total type so "both --wasm and
/// --download" is a refusal at one place, not a runtime check inside the
/// orchestrator.
pub(crate) enum WasmSource {
    None,
    Path(PathBuf),
    Download,
}

pub(crate) fn wasm_source(wasm: Option<&Path>, download: bool) -> Result<WasmSource, String> {
    match (wasm, download) {
        (Some(_), true) => Err("pass either --wasm <path> or --download, not both".to_string()),
        (Some(p), false) => Ok(WasmSource::Path(p.to_path_buf())),
        (None, true)     => Ok(WasmSource::Download),
        (None, false)    => Ok(WasmSource::None),
    }
}

struct ZellijSetupOpts<'a> {
    wasm_source: WasmSource,
    force:       bool,
    inject:      bool,
    layout:      Option<&'a str>,
    dry_run:     bool,
    yes:         bool,
}

struct CodexSetupOpts {
    legacy_notify: bool,
    force:         bool,
    dry_run:       bool,
    yes:           bool,
}

/// Decision about how to handle layout injection for a given invocation.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InjectMode {
    /// Inject without prompting (`--inject` was passed).
    Inject,
    /// Ask the user interactively (default N — no mutation without explicit y).
    Prompt,
    /// Print the tailored snippet and skip injection. The safe non-mutating
    /// default when `--yes` is in effect or when stdin is not a tty.
    Snippet,
}

/// Pure decision: given the CLI flags and whether stdin is a tty, decide how
/// the layout injection step should behave. The rules are:
///
/// 1. `--inject` → `Inject` (unconditional explicit consent).
/// 2. `--yes` → `Snippet`  (take the safe default; never mutate silently).
/// 3. Not a tty → `Snippet` (no way to ask).
/// 4. Otherwise → `Prompt`  (interactive).
pub(crate) fn inject_mode(inject_flag: bool, yes: bool, is_tty: bool) -> InjectMode {
    if inject_flag {
        return InjectMode::Inject;
    }
    if yes || !is_tty {
        return InjectMode::Snippet;
    }
    InjectMode::Prompt
}

#[derive(Debug)]
pub enum Outcome {
    Changed(String),
    Unchanged,
    Conflict,
}

/// The single operation a `setup` invocation performs. Resolving this once makes
/// the precedence (grant > check > uninstall > install) explicit instead of
/// implicit in the order of `if` blocks.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Grant,
    Check,
    Uninstall,
    Install,
}

pub(crate) fn mode_from_flags(grant: bool, check: bool, uninstall: bool) -> Mode {
    if grant {
        Mode::Grant
    } else if check {
        Mode::Check
    } else if uninstall {
        Mode::Uninstall
    } else {
        Mode::Install
    }
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
struct HooksFile {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    hooks: BTreeMap<String, Vec<HookGroup>>,
    #[serde(flatten)]
    rest: Map<String, Value>,
}

#[derive(Serialize, Deserialize)]
struct HookGroup {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hooks: Option<Vec<Value>>,
    #[serde(flatten)]
    meta: Map<String, Value>,
}

fn parse_hooks_file(existing: &str) -> Result<HooksFile, String> {
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

/// The release URL for the wasm artifact built from a given crate version.
/// `setup zellij --download` fetches the wasm matching the CLI's own version so
/// the two halves shipped from one tag can't drift across Zellij's unstable
/// plugin ABI (a CLI and a hand-downloaded wasm of different versions otherwise
/// can). Pure so the version→asset mapping is unit-tested; the fetch itself is
/// thin IO below.
fn wasm_release_url(version: &str) -> String {
    format!("https://github.com/marktoda/zj-radar/releases/download/v{version}/zj_radar.wasm")
}

// ── Grant helper ──

/// Pure: the argument vector for `zellij plugin --floating --width 90 --height
/// 28 file:<wasm_path>`. Unit-tested so the exec call stays thin.
pub(crate) fn grant_args(wasm_path: &Path) -> Vec<String> {
    vec![
        "plugin".to_string(),
        "--floating".to_string(),
        "--width".to_string(),
        "90".to_string(),
        "--height".to_string(),
        "28".to_string(),
        format!("file:{}", wasm_path.display()),
    ]
}

/// Exec `zellij plugin --floating … file:<wasm_dest>` for the one-time
/// permission grant. Reports the error but does not exit — callers may choose.
fn run_grant(config_dir: &Path) {
    use std::process::Command;
    let wasm_dest = zellij_wasm_dest(config_dir);
    let args = grant_args(&wasm_dest);
    match Command::new("zellij").args(&args).status() {
        Ok(status) if status.success() => {}
        Ok(status) => {
            eprintln!(
                "zj-radar: zellij plugin exited with {status}; \
                 try running: zellij {}",
                args.join(" ")
            );
        }
        Err(e) => {
            eprintln!(
                "zj-radar: failed to launch zellij for grant — {e}; \
                 try running: zellij {}",
                args.join(" ")
            );
        }
    }
}

// ── Thin IO layer (not unit-tested) ──

/// Fetch the wasm matching `version` to `dest` (creating its parent dir). Shells
/// out to curl (or wget) rather than linking a Rust TLS stack — keeping the host
/// build free of openssl/rustls, and curl is already assumed by the install flow.
/// Shared by `setup zellij --download` and `run`'s first-use fallback (when the
/// CLI shipped without an embedded wasm).
pub(crate) fn download_wasm_to(version: &str, dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create dir failed — {e}"))?;
    }
    let url = wasm_release_url(version);
    println!("zj-radar: downloading wasm {version} from {url}");
    run_download(&url, dest)?;
    if !dest.is_file() {
        return Err(format!("download reported success but {} is missing", dest.display()));
    }
    Ok(())
}

/// Fetch the wasm matching `version` to a temp file and return its path.
fn download_wasm(version: &str) -> Result<PathBuf, String> {
    let dest = std::env::temp_dir().join(format!("zj_radar-{version}.wasm"));
    download_wasm_to(version, &dest)?;
    Ok(dest)
}

/// HTTPS-only download via curl, falling back to wget only when curl is absent
/// (so a curl HTTP error surfaces as itself rather than a confusing wget retry).
fn run_download(url: &str, dest: &Path) -> Result<(), String> {
    use std::process::Command;
    if which("curl") {
        let status = Command::new("curl")
            .args(["--proto", "=https", "--tlsv1.2", "-fL", url, "-o"])
            .arg(dest)
            .status()
            .map_err(|e| format!("failed to run curl — {e}"))?;
        return if status.success() {
            Ok(())
        } else {
            Err(format!(
                "curl failed for {url} — is v{} released? See https://github.com/marktoda/zj-radar/releases",
                env!("CARGO_PKG_VERSION")
            ))
        };
    }
    if which("wget") {
        let status = Command::new("wget")
            .args(["--https-only", "-O"])
            .arg(dest)
            .arg(url)
            .status()
            .map_err(|e| format!("failed to run wget — {e}"))?;
        return if status.success() {
            Ok(())
        } else {
            Err(format!("wget failed for {url}"))
        };
    }
    Err("need curl or wget on PATH to --download".to_string())
}

/// The wasm release tag to fetch: `ZJ_RADAR_VERSION` (a leading `v` is optional)
/// overrides, else this CLI's own version — the version-skew-safe default.
pub(crate) fn wasm_download_version() -> String {
    std::env::var("ZJ_RADAR_VERSION")
        .ok()
        .map(|v| v.trim_start_matches('v').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

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

/// Returns `true` when `path` is a symlink — the hallmark of a Nix / home-manager
/// managed file that we must not overwrite. Uses `symlink_metadata` so the query
/// does not follow the link (a broken symlink still returns `true`).
pub(crate) fn config_is_managed(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
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
    let mode = mode_from_flags(options.grant, options.check, options.uninstall);

    if mode == Mode::Grant {
        run_grant(&zellij_config_dir());
        return;
    }

    let want_codex = (options.targets.is_empty() && options.wasm.is_none() && !options.download)
        || options.targets.iter().any(|a| a == "codex");
    let want_zellij = options.targets.iter().any(|a| a == "zellij")
        || options.wasm.is_some()
        || options.download;
    for a in options
        .targets
        .iter()
        .filter(|a| !matches!(a.as_str(), "codex" | "zellij"))
    {
        eprintln!("zj-radar: setup does not support '{a}' (supported: codex, zellij). Skipping.");
    }

    if mode == Mode::Check {
        if want_zellij {
            check_zellij();
        }
        if want_codex {
            check_codex(options.legacy_notify);
        }
        return;
    }

    let uninstall = mode == Mode::Uninstall;
    if want_zellij {
        let wasm_source = if uninstall {
            WasmSource::None
        } else {
            match wasm_source(options.wasm, options.download) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("zellij: refused — {e}");
                    return;
                }
            }
        };
        setup_zellij(
            uninstall,
            ZellijSetupOpts {
                wasm_source,
                force:   options.force,
                inject:  options.inject,
                layout:  options.layout,
                dry_run: options.dry_run,
                yes:     options.yes,
            },
        );
    }
    if want_codex {
        setup_codex(
            uninstall,
            CodexSetupOpts {
                legacy_notify: options.legacy_notify,
                force:         options.force,
                dry_run:       options.dry_run,
                yes:           options.yes,
            },
        );
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CheckLevel {
    Ok,
    Warn,
    Missing,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CheckItem {
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
    let env = CodexEnv {
        codex_on_path:    which("codex"),
        zj_radar_on_path: which("zj-radar"),
        config_text:      std::fs::read_to_string(codex_config_path()).ok(),
        hooks_text:       std::fs::read_to_string(codex_hooks_path()).ok(),
    };
    let items = codex_check_items(&analyze_codex(&env), legacy_notify);
    println!("codex:");
    print_check_items(&items);
}

/// Raw, already-read environment for Zellij setup. The ONLY layer that touched
/// the filesystem — `analyze_zellij` is pure over this struct.
pub(crate) struct ZellijEnv {
    pub config_text:           Option<String>,
    pub layout_text:           Option<String>,
    pub permissions_text:      Option<String>,
    pub codex_hooks_text:      Option<String>,
    pub installed_plugins_text: Option<String>,
    pub wasm_present:          bool,
    pub config_managed:        bool,
    pub wasm_path:             String,
}

/// Every derived fact about the current Zellij setup state, in one place. Both
/// `check` (renders) and `install` (gates) read these; the derivation is here so
/// "is our alias present?" has exactly one definition.
pub(crate) struct ZellijFacts {
    pub managed_alias_present:   bool,
    pub unmanaged_alias_present: bool,
    pub alias_is_store_path:     bool,
    pub wasm_present:            bool,
    pub has_rail:                Option<bool>,
    pub granted:                 Option<bool>,
    pub producer_wired:          bool,
    pub config_managed:          bool,
}

/// Pure: derive every Zellij setup fact from already-read inputs. No I/O.
pub(crate) fn analyze_zellij(env: &ZellijEnv) -> ZellijFacts {
    let lines: Vec<String> = env.config_text.as_deref().map(split_lines).unwrap_or_default();
    let managed_alias_present = lines.iter().any(|l| l.trim() == ZELLIJ_ALIAS_BEGIN);
    let mut lines_without_managed = lines.clone();
    strip_managed_zellij_alias(&mut lines_without_managed);
    let unmanaged_alias_present = has_unmanaged_radar_alias(&lines_without_managed);
    let alias_is_store_path =
        env.config_text.as_deref().is_some_and(|t| t.contains("/nix/store/"));
    let has_rail = env.layout_text.as_deref().map(|t| super::layout::analyze(t).has_rail);
    let granted = env
        .permissions_text
        .as_deref()
        .map(|t| super::run::wasm_is_granted(t, &env.wasm_path));
    let claude_present = super::run::claude_producer_wired(env.installed_plugins_text.as_deref());
    let producer_wired =
        super::run::producer_hint(env.codex_hooks_text.as_deref(), claude_present).is_none();
    ZellijFacts {
        managed_alias_present,
        unmanaged_alias_present,
        alias_is_store_path,
        wasm_present: env.wasm_present,
        has_rail,
        granted,
        producer_wired,
        config_managed: env.config_managed,
    }
}

/// `CheckItem`s for `zj-radar setup zellij --check`. Pure over fully-derived
/// `ZellijFacts`; the derivation lives in `analyze_zellij`.
pub(crate) fn zellij_check_items(f: &ZellijFacts) -> Vec<CheckItem> {
    let mut items = Vec::new();

    // 1. alias — "present" means managed marker OR an unmanaged alias line.
    let alias_present = f.managed_alias_present || f.unmanaged_alias_present;
    items.push(match (alias_present, f.alias_is_store_path) {
        (false, _) => CheckItem::missing("alias", "radar plugin alias not found in config.kdl"),
        (true, true) => CheckItem::warn(
            "alias",
            "alias points at /nix/store/ path — grant won't persist across rebuilds; run `setup zellij` after each rebuild",
        ),
        (true, false) => CheckItem::ok("alias", "radar plugin alias present in config.kdl"),
    });

    // 2. wasm
    items.push(if f.wasm_present {
        CheckItem::ok("wasm", "wasm plugin file present")
    } else {
        CheckItem::missing(
            "wasm",
            "wasm plugin file not found — run `zj-radar setup zellij --wasm <path>` or `--download`",
        )
    });

    // 3. layout (rail)
    items.push(match f.has_rail {
        None => CheckItem::warn("layout", "no default layout found"),
        Some(true) => CheckItem::ok("layout", "default layout has the radar rail"),
        Some(false) => CheckItem::missing(
            "layout",
            "default layout does not have the radar rail — run `zj-radar setup zellij` or paste the snippet",
        ),
    });

    // 4. grant
    items.push(match f.granted {
        None => CheckItem::warn("grant", "no permissions.kdl found"),
        Some(true) => CheckItem::ok("grant", "wasm is granted in permissions.kdl"),
        Some(false) => CheckItem::missing("grant", "wasm not granted — run `zj-radar setup zellij --grant`"),
    });

    // 5. producer
    items.push(if f.producer_wired {
        CheckItem::ok("producer", "a producer is wired (Codex hooks or Claude plugin)")
    } else {
        CheckItem::missing(
            "producer",
            "no producer detected — run `zj-radar setup codex` or enable the Claude plugin",
        )
    });

    // 6. managed config (only emit when true)
    if f.config_managed {
        items.push(CheckItem::warn(
            "managed config",
            "config.kdl is managed (symlink); edits may be overwritten",
        ));
    }

    items
}

fn check_zellij() {
    let config_dir = zellij_config_dir();
    let config_path = zellij_config_path(&config_dir);
    let wasm_dest = zellij_wasm_dest(&config_dir);
    let layout_path = config_dir.join("layouts").join("default.kdl");
    let env = ZellijEnv {
        config_text: std::fs::read_to_string(&config_path).ok(),
        layout_text: std::fs::read_to_string(&layout_path).ok(),
        permissions_text: super::run::zellij_permissions_path()
            .and_then(|p| std::fs::read_to_string(p).ok()),
        codex_hooks_text: dirs::home_dir()
            .and_then(|h| std::fs::read_to_string(h.join(".codex/hooks.json")).ok()),
        installed_plugins_text: dirs::home_dir()
            .and_then(|h| std::fs::read_to_string(h.join(".claude/plugins/installed_plugins.json")).ok()),
        wasm_present: wasm_dest.is_file(),
        config_managed: config_is_managed(&config_path),
        wasm_path: wasm_dest.to_string_lossy().into_owned(),
    };
    let items = zellij_check_items(&analyze_zellij(&env));
    println!("zellij:");
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

/// Derived state of Codex's `[features].hooks` switch.
pub(crate) enum CodexHooksFeature {
    EnabledOrUnset,
    Disabled,
    ConfigError(String),
}

/// Derived state of the legacy `notify` slot in Codex `config.toml`.
pub(crate) enum CodexNotifyState {
    ConfigAbsent,
    NotInstalled,
    Ours,
    Foreign,
    ConfigError(String),
}

/// Raw, already-read environment for Codex setup. The only IO layer.
pub(crate) struct CodexEnv {
    pub codex_on_path:    bool,
    pub zj_radar_on_path: bool,
    pub config_text:      Option<String>,
    pub hooks_text:       Option<String>,
}

/// Every derived fact about Codex setup state. The legacy-vs-hooks choice is a
/// flag the consumer projects on — NOT a fact — so both surfaces are observed.
pub(crate) struct CodexFacts {
    pub codex_on_path:     bool,
    pub zj_radar_on_path:  bool,
    pub hooks_feature:     CodexHooksFeature,
    pub notify:            CodexNotifyState,
    /// `None` = hooks.json absent; `Some(Ok(n))` = n marker-owned events; `Some(Err)` = parse error.
    pub owned_hook_events: Option<Result<usize, String>>,
}

/// Pure: derive every Codex setup fact from already-read inputs. No I/O.
pub(crate) fn analyze_codex(env: &CodexEnv) -> CodexFacts {
    let hooks_feature = match env.config_text.as_deref().map(codex_hooks_disabled_in_config) {
        Some(Ok(true)) => CodexHooksFeature::Disabled,
        Some(Ok(false)) | None => CodexHooksFeature::EnabledOrUnset,
        Some(Err(e)) => CodexHooksFeature::ConfigError(e),
    };
    let notify = match env.config_text.as_deref() {
        None => CodexNotifyState::ConfigAbsent,
        Some(text) => match text.parse::<DocumentMut>() {
            Ok(doc) if notify_is_ours(doc.get("notify")) => CodexNotifyState::Ours,
            Ok(doc) if doc.get("notify").is_some() => CodexNotifyState::Foreign,
            Ok(_) => CodexNotifyState::NotInstalled,
            Err(e) => CodexNotifyState::ConfigError(e.to_string()),
        },
    };
    let owned_hook_events = env.hooks_text.as_deref().map(codex_owned_hook_event_count);
    CodexFacts {
        codex_on_path: env.codex_on_path,
        zj_radar_on_path: env.zj_radar_on_path,
        hooks_feature,
        notify,
        owned_hook_events,
    }
}

pub(crate) fn codex_check_items(f: &CodexFacts, legacy_notify: bool) -> Vec<CheckItem> {
    let mut items = Vec::new();
    items.push(if f.codex_on_path {
        CheckItem::ok("codex binary", "found on PATH")
    } else {
        CheckItem::missing("codex binary", "not found on PATH")
    });
    items.push(if f.zj_radar_on_path {
        CheckItem::ok("zj-radar binary", "found on PATH")
    } else {
        CheckItem::missing("zj-radar binary", "not found on PATH")
    });

    items.push(match &f.hooks_feature {
        CodexHooksFeature::Disabled => {
            CheckItem::warn("hooks feature", "`[features].hooks = false` disables Codex hooks")
        }
        CodexHooksFeature::EnabledOrUnset => {
            CheckItem::ok("hooks feature", "enabled or unset in config.toml")
        }
        CodexHooksFeature::ConfigError(e) => CheckItem::warn("config.toml", e.clone()),
    });

    if legacy_notify {
        items.push(match &f.notify {
            CodexNotifyState::ConfigAbsent => {
                CheckItem::missing("legacy notify", "config.toml not found")
            }
            CodexNotifyState::Ours => CheckItem::ok("legacy notify", "zj-radar owns Codex notify"),
            CodexNotifyState::Foreign => {
                CheckItem::warn("legacy notify", "another command owns Codex notify")
            }
            CodexNotifyState::NotInstalled => {
                CheckItem::missing("legacy notify", "Codex notify is not installed")
            }
            CodexNotifyState::ConfigError(e) => CheckItem::warn(
                "config.toml",
                format!("config.toml is not valid TOML: {e}"),
            ),
        });
    } else {
        items.push(match &f.owned_hook_events {
            None => CheckItem::missing("hooks.json", "zj-radar Codex hooks are not installed"),
            Some(Ok(count)) if *count == CODEX_HOOK_EVENTS.len() => {
                CheckItem::ok("hooks.json", "all zj-radar Codex hooks installed")
            }
            Some(Ok(count)) if *count > 0 => CheckItem::warn(
                "hooks.json",
                format!("partial zj-radar hook install ({count}/{})", CODEX_HOOK_EVENTS.len()),
            ),
            Some(Ok(_)) => {
                CheckItem::missing("hooks.json", "zj-radar Codex hooks are not installed")
            }
            Some(Err(e)) => CheckItem::warn("hooks.json", e.clone()),
        });
        if matches!(f.notify, CodexNotifyState::Foreign) {
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

fn codex_owned_hook_event_count(existing: &str) -> Result<usize, String> {
    let file = parse_hooks_file(existing)?;
    Ok(CODEX_HOOK_EVENTS
        .iter()
        .filter(|event| {
            file.hooks.get(**event).is_some_and(|groups| {
                groups
                    .iter()
                    .filter_map(|group| group.hooks.as_ref())
                    .flatten()
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

fn setup_codex(uninstall: bool, opts: CodexSetupOpts) {
    if opts.legacy_notify {
        setup_codex_notify(uninstall, opts.dry_run, opts.yes, opts.force);
    } else {
        setup_codex_hooks(uninstall, opts.dry_run, opts.yes);
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
            let prompt = format!("Write {}?", path.display());
            if !confirm_and_write("codex", &path, &new, yes, &prompt, || Ok(())) {
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
            let prompt = format!("Write {}?", path.display());
            if !confirm_and_write("codex", &path, &new, yes, &prompt, || Ok(())) {
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
    let env = CodexEnv {
        codex_on_path:    false,
        zj_radar_on_path: false,
        config_text:      std::fs::read_to_string(codex_config_path()).ok(),
        hooks_text:       None,
    };
    matches!(analyze_codex(&env).hooks_feature, CodexHooksFeature::Disabled)
}

fn setup_zellij(uninstall: bool, opts: ZellijSetupOpts<'_>) {
    let (wasm, download): (Option<&Path>, bool) = match &opts.wasm_source {
        WasmSource::Path(p) => (Some(p.as_path()), false),
        WasmSource::Download => (None, true),
        WasmSource::None    => (None, false),
    };
    let dry_run     = opts.dry_run;
    let yes         = opts.yes;
    let force       = opts.force;
    let inject_flag = opts.inject;
    let layout_name = opts.layout;
    let config_dir = zellij_config_dir();
    let config_path = zellij_config_path(&config_dir);
    let wasm_dest = zellij_wasm_dest(&config_dir);
    let location = zellij_plugin_location(&wasm_dest);

    // Resolve the target layout path up front (needed whether or not a managed
    // config short-circuits): `--layout <name>` → `<config_dir>/layouts/<name>.kdl`,
    // else `<config_dir>/layouts/default.kdl`.
    let layout_path = config_dir
        .join("layouts")
        .join(format!("{}.kdl", layout_name.unwrap_or("default")));

    // One derivation, shared with `check`: read current state into Facts. The
    // config text is reused below for the `edit_zellij` splice.
    let config_text = std::fs::read_to_string(&config_path).ok();
    let facts = analyze_zellij(&ZellijEnv {
        config_text:            config_text.clone(),
        layout_text:            None, // install only consults `config_managed`; the layout is read later by the inject flow
        permissions_text:       None,
        codex_hooks_text:       None,
        installed_plugins_text: None,
        wasm_present:           wasm_dest.is_file(),
        config_managed:         config_is_managed(&config_path),
        wasm_path:              wasm_dest.to_string_lossy().into_owned(),
    });

    // Refuse to clobber a managed (symlinked) config.kdl: print the layout snippet
    // for guidance, then return early. A Nix/home-manager user gets the wasm + alias
    // via their config, not from us.
    if !uninstall && facts.config_managed {
        eprintln!(
            "zellij: config.kdl at {} is a symlink (managed by Nix / home-manager).\n\
             zj-radar will not overwrite a managed config — add the plugin alias via\n\
             your Nix config instead. See docs/install.md for the home-manager snippet.",
            config_path.display()
        );
        print_snippet_for(&layout_path);
        return;
    }

    // Resolve the wasm source: an explicit --wasm path, or --download (fetch the
    // wasm matching this CLI's version). `downloaded` outlives the borrow in `src`.
    let downloaded: PathBuf;
    let src: Option<&Path> = if uninstall {
        None
    } else if download {
        match download_wasm(&wasm_download_version()) {
            Ok(path) => {
                downloaded = path;
                Some(downloaded.as_path())
            }
            Err(e) => {
                eprintln!("zellij: refused — {e}");
                return;
            }
        }
    } else {
        wasm
    };

    // When `--inject` is set (or `--yes` is set for a non-mutating snippet) but
    // no wasm source is given, skip the wasm/alias step and go directly to the
    // layout step. This makes `setup zellij --inject` and `setup zellij --yes`
    // usable and testable without a wasm artifact while preserving the existing
    // "refused — pass --wasm" error for bare `setup zellij` invocations.
    let layout_only_install = src.is_none() && !uninstall && (inject_flag || yes);
    if layout_only_install {
        run_layout_inject(&layout_path, inject_flag, yes, dry_run);
        return;
    }
    // `--uninstall` with no wasm/config: layout-only uninstall.
    if uninstall && src.is_none() && !config_path.exists() {
        run_layout_uninstall(&layout_path, dry_run);
        return;
    }

    if !uninstall {
        let Some(src) = src else {
            eprintln!("zellij: refused — pass --wasm <path-to-zj_radar.wasm> or --download");
            return;
        };
        if !src.is_file() {
            eprintln!("zellij: refused — wasm not found at {}", src.display());
            return;
        }
    }

    let existing = config_text.unwrap_or_default();
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
            // uninstall: also try to remove the injected rail from the layout.
            run_layout_uninstall(&layout_path, dry_run);
        }
        Outcome::Unchanged => {
            println!(
                "zellij: config already up to date ({})",
                config_path.display()
            );
            // alias already up to date — still offer injection.
            run_layout_inject(&layout_path, inject_flag, yes, dry_run);
            print_grant_hint();
            print_producer_hint_if_needed();
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
                    if let Some(src) = src {
                        println!(
                            "zellij: would copy {} -> {}",
                            src.display(),
                            wasm_dest.display()
                        );
                    }
                }
                println!("--- {} (dry-run) ---\n{new}", config_path.display());
                if uninstall {
                    run_layout_uninstall(&layout_path, dry_run);
                } else {
                    run_layout_inject(&layout_path, inject_flag, yes, dry_run);
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
            // Pre-write side effect: stage the wasm (mkdir + copy) before the
            // config write, only when installing.
            let copy_wasm = || -> Result<(), String> {
                if uninstall {
                    return Ok(());
                }
                if let Some(parent) = wasm_dest.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("create plugin dir failed — {e}"))?;
                }
                let src = src.ok_or("refused — pass --wasm <path-to-zj_radar.wasm> or --download")?;
                std::fs::copy(src, &wasm_dest).map_err(|e| format!("wasm copy failed — {e}"))?;
                Ok(())
            };
            if !confirm_and_write("zellij", &config_path, &new, yes, &prompt, copy_wasm) {
                return;
            }
            println!(
                "zellij: {} ({})",
                if uninstall { "removed" } else { "installed" },
                config_path.display()
            );
            if uninstall {
                run_layout_uninstall(&layout_path, dry_run);
            } else {
                println!("zellij: wasm installed at {}", wasm_dest.display());
                run_layout_inject(&layout_path, inject_flag, yes, dry_run);
                print_grant_hint();
                print_producer_hint_if_needed();
            }
        }
    }
}

fn print_grant_hint() {
    // The rail can't show Zellij's grant prompt legibly (it's a small borderless
    // pane — Zellij #4749). On first launch the user grants by focusing the rail
    // and pressing y; `--grant` offers an explicit floating prompt instead. The
    // turnkey `zj-radar run` handles this automatically. One coherent line — the
    // merge with main's onboarding work otherwise printed two overlapping notes.
    println!(
        "zellij: on first launch, focus the RADAR rail (the left column) and press y to \
         allow access — or run `zj-radar setup zellij --grant` to grant via a floating \
         pane. Zellij asks once, then remembers."
    );
}

/// Emit a producer hint at the tail of `setup zellij` when no producer is wired.
/// Checks Codex hooks and the Claude plugin manifest, same as `run`'s detection.
fn print_producer_hint_if_needed() {
    let codex_hooks = dirs::home_dir()
        .and_then(|h| std::fs::read_to_string(h.join(".codex/hooks.json")).ok());
    let installed_plugins = dirs::home_dir()
        .and_then(|h| {
            std::fs::read_to_string(h.join(".claude/plugins/installed_plugins.json")).ok()
        });
    let claude_present = super::run::claude_producer_wired(installed_plugins.as_deref());
    if let Some(hint) = super::run::producer_hint(codex_hooks.as_deref(), claude_present) {
        println!("zellij: {hint}");
    }
}


/// Print the tailored snippet for a given layout path (empty string → default facts).
fn print_snippet_for(layout_path: &Path) {
    let text = std::fs::read_to_string(layout_path).unwrap_or_default();
    let facts = super::layout::analyze(&text);
    let snippet = super::layout::tailored_snippet(&facts);
    println!("\nAdd the sidebar to a Zellij layout with:\n\n{snippet}");
}

/// Handle layout injection after the alias step. Reads `layout_path`, decides
/// the mode, and either injects (writing a `.zj-radar.bak` backup first) or
/// prints the tailored snippet. A missing layout → snippet only (safe fallback).
fn run_layout_inject(layout_path: &Path, inject_flag: bool, yes: bool, dry_run: bool) {
    use std::io::IsTerminal;
    let is_tty = std::io::stdin().is_terminal();
    let mode = inject_mode(inject_flag, yes, is_tty);

    let text = match std::fs::read_to_string(layout_path) {
        Ok(t) => t,
        Err(_) => {
            // Layout not found — just print the snippet, no failure.
            let facts = super::layout::analyze("");
            let snippet = super::layout::tailored_snippet(&facts);
            println!(
                "zellij: layout not found at {} — add the rail manually:\n\n{snippet}",
                layout_path.display()
            );
            return;
        }
    };

    let facts = super::layout::analyze(&text);

    // Already injected → idempotent no-op for Inject/Prompt; snippet still accurate.
    if facts.has_rail {
        println!("zellij: layout already has the rail ({})", layout_path.display());
        return;
    }

    match mode {
        InjectMode::Snippet => {
            // --yes or non-tty: print snippet, never mutate.
            let snippet = super::layout::tailored_snippet(&facts);
            println!("\nAdd the sidebar to a Zellij layout with:\n\n{snippet}");
        }
        InjectMode::Prompt => {
            let prompt = format!("Inject the rail into {}?", layout_path.display());
            if !confirm(&prompt) {
                let snippet = super::layout::tailored_snippet(&facts);
                println!("\nAdd the sidebar to a Zellij layout with:\n\n{snippet}");
                return;
            }
            do_inject(layout_path, &text, &facts, dry_run);
        }
        InjectMode::Inject => {
            do_inject(layout_path, &text, &facts, dry_run);
        }
    }
}

/// Perform the actual inject: call `layout::inject`, write backup + new text.
/// On `Refusal`, print the reason + tailored snippet (fail-closed).
fn do_inject(layout_path: &Path, text: &str, facts: &super::layout::LayoutFacts, dry_run: bool) {
    match super::layout::inject(text, facts) {
        Ok(new_text) => {
            if dry_run {
                println!(
                    "zellij: would inject rail into {} (dry-run)\n--- layout (dry-run) ---\n{new_text}",
                    layout_path.display()
                );
                return;
            }
            // Back up then atomically write.
            if layout_path.exists() {
                let _ = std::fs::copy(
                    layout_path,
                    path_with_suffix(layout_path, ".zj-radar.bak"),
                );
            }
            match super::fsutil::atomic_write(layout_path, new_text.as_bytes()) {
                Ok(()) => println!(
                    "zellij: rail injected into {} (backup: {}.zj-radar.bak)",
                    layout_path.display(),
                    layout_path.display()
                ),
                Err(e) => eprintln!("zellij: write failed — {e}"),
            }
        }
        Err(super::layout::Refusal::Unparseable(msg)) => {
            eprintln!("zellij: layout could not be parsed — {msg}");
            eprintln!("        Add the rail manually using the snippet below.");
            let snippet = super::layout::tailored_snippet(facts);
            println!("\n{snippet}");
        }
        Err(super::layout::Refusal::Unrecognized(msg)) => {
            eprintln!("zellij: layout shape not recognized — {msg}");
            eprintln!("        Add the rail manually using the snippet below.");
            let snippet = super::layout::tailored_snippet(facts);
            println!("\n{snippet}");
        }
    }
}

/// Handle `--uninstall` for the layout: strip the injected rail if present.
fn run_layout_uninstall(layout_path: &Path, dry_run: bool) {
    let text = match std::fs::read_to_string(layout_path) {
        Ok(t) => t,
        Err(_) => return, // layout not found — nothing to uninstall
    };
    match super::layout::uninstall(&text) {
        None => {
            // no injected rail present — nothing to do
        }
        Some(new_text) => {
            if dry_run {
                println!(
                    "zellij: would remove rail from {} (dry-run)\n--- layout (dry-run) ---\n{new_text}",
                    layout_path.display()
                );
                return;
            }
            if layout_path.exists() {
                let _ = std::fs::copy(
                    layout_path,
                    path_with_suffix(layout_path, ".zj-radar.bak"),
                );
            }
            match super::fsutil::atomic_write(layout_path, new_text.as_bytes()) {
                Ok(()) => println!(
                    "zellij: rail removed from {} (backup: {}.zj-radar.bak)",
                    layout_path.display(),
                    layout_path.display()
                ),
                Err(e) => eprintln!("zellij: write failed — {e}"),
            }
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

/// The shared "commit an edit" tail for every `setup_*` step: prompt (unless
/// `--yes`), run any `pre_write` side effects (e.g. copying the wasm), then write
/// `new` to `path` atomically — emitting the standard `skipped`/`failed`
/// diagnostics under `label`. Returns whether the file was written, so the caller
/// can print its success epilogue. Callers keep `--dry-run` handling and prompt
/// wording, which differ per target. A `pre_write` error is reported as
/// `{label}: {e}`, so its message should read as a sentence without the prefix.
fn confirm_and_write(
    label: &str,
    path: &Path,
    new: &str,
    yes: bool,
    prompt: &str,
    pre_write: impl FnOnce() -> Result<(), String>,
) -> bool {
    if !yes && !confirm(prompt) {
        println!("{label}: skipped (declined)");
        return false;
    }
    if let Err(e) = pre_write() {
        eprintln!("{label}: {e}");
        return false;
    }
    if let Err(e) = write_atomic(path, new) {
        eprintln!("{label}: write failed — {e}");
        return false;
    }
    true
}

/// Back up the existing file, then write atomically (temp file + rename via the
/// shared `fsutil::atomic_write`). The `.bak` is specific to `setup` editing the
/// user's own files; `run` writes its owned dir without one.
fn write_atomic(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    if path.exists() {
        let _ = std::fs::copy(path, path_with_suffix(path, ".zj-radar.bak"));
    }
    super::fsutil::atomic_write(path, contents.as_bytes())
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

    #[test]
    fn wasm_source_rejects_both_path_and_download() {
        let p = std::path::Path::new("/x.wasm");
        assert!(matches!(wasm_source(Some(p), false), Ok(WasmSource::Path(_))));
        assert!(matches!(wasm_source(None, true), Ok(WasmSource::Download)));
        assert!(matches!(wasm_source(None, false), Ok(WasmSource::None)));
        assert!(wasm_source(Some(p), true).is_err(), "both --wasm and --download must refuse");
    }

    #[test]
    fn wasm_release_url_points_at_versioned_asset() {
        assert_eq!(
            wasm_release_url("0.1.0"),
            "https://github.com/marktoda/zj-radar/releases/download/v0.1.0/zj_radar.wasm"
        );
    }

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
    fn codex_check_reports_hook_setup_ready_with_trust_reminder() {
        let hooks = match edit_codex_hooks("", true).unwrap() {
            Outcome::Changed(s) => s,
            o => panic!("{o:?}"),
        };
        let facts = analyze_codex(&CodexEnv {
            codex_on_path:    true,
            zj_radar_on_path: true,
            config_text:      Some("model = \"x\"\n".to_string()),
            hooks_text:       Some(hooks.to_string()),
        });
        let items = codex_check_items(&facts, false);
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
        let facts = analyze_codex(&CodexEnv {
            codex_on_path:    true,
            zj_radar_on_path: true,
            config_text:      Some("[features]\nhooks = false\n".to_string()),
            hooks_text:       Some(hooks.to_string()),
        });
        let items = codex_check_items(&facts, false);
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
        let facts = analyze_codex(&CodexEnv {
            codex_on_path:    true,
            zj_radar_on_path: true,
            config_text:      None,
            hooks_text:       Some(partial.to_string()),
        });
        let items = codex_check_items(&facts, false);
        assert!(items.iter().any(|item| item.name == "hooks.json"
            && item.level == CheckLevel::Warn
            && item.detail.contains("partial")));

        let facts = analyze_codex(&CodexEnv {
            codex_on_path:    true,
            zj_radar_on_path: true,
            config_text:      None,
            hooks_text:       Some("not json".to_string()),
        });
        let items = codex_check_items(&facts, false);
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
        let facts = analyze_codex(&CodexEnv {
            codex_on_path:    true,
            zj_radar_on_path: true,
            config_text:      Some(config.to_string()),
            hooks_text:       Some(hooks.to_string()),
        });
        let items = codex_check_items(&facts, false);
        assert!(items.iter().any(|item| item.name == "legacy notify"
            && item.level == CheckLevel::Ok
            && item.detail.contains("preserved")));
    }

    #[test]
    fn codex_check_legacy_notify_mode_reports_notify_slot() {
        let facts = analyze_codex(&CodexEnv {
            codex_on_path:    true,
            zj_radar_on_path: true,
            config_text:      Some("notify = [\"zj-radar\", \"notify\", \"codex\"]\n".to_string()),
            hooks_text:       None,
        });
        let items = codex_check_items(&facts, true);
        assert!(items.contains(&CheckItem::ok(
            "legacy notify",
            "zj-radar owns Codex notify"
        )));

        let facts = analyze_codex(&CodexEnv {
            codex_on_path:    true,
            zj_radar_on_path: true,
            config_text:      Some("notify = [\"/other\"]\n".to_string()),
            hooks_text:       None,
        });
        let items = codex_check_items(&facts, true);
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
    #[cfg(unix)]
    fn detects_symlinked_config_as_managed() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.kdl");
        std::fs::write(&real, "").unwrap();
        let link = dir.path().join("config.kdl");
        symlink(&real, &link).unwrap();
        assert!(config_is_managed(&link), "symlink should be managed");
        assert!(!config_is_managed(&real), "regular file should not be managed");
        // non-existent path is also not managed
        assert!(!config_is_managed(&dir.path().join("missing.kdl")));
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

    // ── inject_mode decision tests ───────────────────────────────────────────

    #[test]
    fn inject_flag_forces_inject() {
        assert_eq!(inject_mode(true, false, false), InjectMode::Inject);
        assert_eq!(inject_mode(true, false, true), InjectMode::Inject);
        assert_eq!(inject_mode(true, true, false), InjectMode::Inject);
        assert_eq!(inject_mode(true, true, true), InjectMode::Inject);
    }

    #[test]
    fn yes_takes_safe_default_snippet() {
        // --yes without --inject → Snippet regardless of tty
        assert_eq!(inject_mode(false, true, true),  InjectMode::Snippet);
        assert_eq!(inject_mode(false, true, false), InjectMode::Snippet);
    }

    #[test]
    fn non_tty_takes_safe_default_snippet() {
        // non-tty without --inject or --yes → Snippet
        assert_eq!(inject_mode(false, false, false), InjectMode::Snippet);
    }

    #[test]
    fn prompt_when_interactive() {
        // interactive tty, no --inject, no --yes → Prompt
        assert_eq!(inject_mode(false, false, true), InjectMode::Prompt);
    }

    // ── grant_args tests ─────────────────────────────────────────────────────

    #[test]
    fn grant_args_produces_exact_zellij_plugin_command() {
        let path = std::path::Path::new("/home/user/.config/zellij/plugins/zj_radar.wasm");
        assert_eq!(
            grant_args(path),
            vec![
                "plugin",
                "--floating",
                "--width",
                "90",
                "--height",
                "28",
                "file:/home/user/.config/zellij/plugins/zj_radar.wasm",
            ]
        );
    }

    // ── zellij_check_items unit tests ────────────────────────────────────────

    /// Helper: all-good `ZellijFacts` so tests override only the dimension they
    /// care about. (The raw-text→fact derivation is tested in `analyze_zellij_*`.)
    fn all_good_facts() -> ZellijFacts {
        ZellijFacts {
            managed_alias_present:   false,
            unmanaged_alias_present: true,
            alias_is_store_path:     false,
            wasm_present:            true,
            has_rail:                Some(true),
            granted:                 Some(true),
            producer_wired:          true,
            config_managed:          false,
        }
    }

    fn all_good_check_items() -> Vec<CheckItem> {
        zellij_check_items(&all_good_facts())
    }

    #[test]
    fn zellij_check_items_all_ok() {
        let items = all_good_check_items();
        assert!(items.iter().any(|i| i.name == "alias" && i.level == CheckLevel::Ok));
        assert!(items.iter().any(|i| i.name == "wasm" && i.level == CheckLevel::Ok));
        assert!(items.iter().any(|i| i.name == "layout" && i.level == CheckLevel::Ok));
        assert!(items.iter().any(|i| i.name == "grant" && i.level == CheckLevel::Ok));
        assert!(items.iter().any(|i| i.name == "producer" && i.level == CheckLevel::Ok));
        // managed config not emitted when false
        assert!(!items.iter().any(|i| i.name == "managed config"));
    }

    #[test]
    fn zellij_check_items_nix_store_alias_warns() {
        let mut f = all_good_facts();
        f.alias_is_store_path = true;
        let items = zellij_check_items(&f);
        let alias = items.iter().find(|i| i.name == "alias").expect("alias item");
        assert_eq!(alias.level, CheckLevel::Warn, "nix-store alias must warn");
        assert!(alias.detail.contains("nix/store"), "warn detail must mention nix/store");
        assert!(alias.detail.contains("rebuild"), "warn detail must mention rebuild");
    }

    #[test]
    fn zellij_check_items_rail_less_layout_is_missing() {
        let mut f = all_good_facts();
        f.has_rail = Some(false);
        let items = zellij_check_items(&f);
        let layout_item = items.iter().find(|i| i.name == "layout").expect("layout item");
        assert_eq!(layout_item.level, CheckLevel::Missing, "layout without rail must be missing");
        assert!(layout_item.detail.contains("setup zellij"), "hint must mention setup zellij");
    }

    #[test]
    fn zellij_check_items_ungranted_wasm_is_missing() {
        let mut f = all_good_facts();
        f.granted = Some(false);
        let items = zellij_check_items(&f);
        let grant = items.iter().find(|i| i.name == "grant").expect("grant item");
        assert_eq!(grant.level, CheckLevel::Missing, "ungranted wasm must be missing");
        assert!(grant.detail.contains("--grant"), "hint must mention --grant");
    }

    #[test]
    fn zellij_check_items_managed_config_warns() {
        let mut f = all_good_facts();
        f.config_managed = true;
        let items = zellij_check_items(&f);
        let managed = items.iter().find(|i| i.name == "managed config").expect("managed config item");
        assert_eq!(managed.level, CheckLevel::Warn, "managed config must warn");
        assert!(managed.detail.contains("symlink"), "warn detail must mention symlink");
    }

    #[test]
    fn zellij_check_items_missing_alias_is_missing() {
        let mut f = all_good_facts();
        f.unmanaged_alias_present = false;
        let items = zellij_check_items(&f);
        let alias = items.iter().find(|i| i.name == "alias").expect("alias item");
        assert_eq!(alias.level, CheckLevel::Missing);
    }

    #[test]
    fn zellij_check_items_no_layout_warns() {
        let mut f = all_good_facts();
        f.has_rail = None;
        let items = zellij_check_items(&f);
        let layout_item = items.iter().find(|i| i.name == "layout").expect("layout item");
        assert_eq!(layout_item.level, CheckLevel::Warn, "missing layout file should warn");
    }

    #[test]
    fn zellij_check_items_no_permissions_warns() {
        let mut f = all_good_facts();
        f.granted = None;
        let items = zellij_check_items(&f);
        let grant = items.iter().find(|i| i.name == "grant").expect("grant item");
        assert_eq!(grant.level, CheckLevel::Warn, "no permissions.kdl should warn");
    }

    #[test]
    fn zellij_check_items_no_producer_is_missing() {
        let mut f = all_good_facts();
        f.producer_wired = false;
        let items = zellij_check_items(&f);
        let producer = items.iter().find(|i| i.name == "producer").expect("producer item");
        assert_eq!(producer.level, CheckLevel::Missing);
        assert!(producer.detail.contains("setup codex"), "hint must mention setup codex");
    }

    #[test]
    fn zellij_check_items_order_is_stable() {
        let items = all_good_check_items();
        let names: Vec<&str> = items.iter().map(|i| i.name).collect();
        assert_eq!(names, &["alias", "wasm", "layout", "grant", "producer"]);
    }

    #[test]
    fn analyze_zellij_derives_managed_and_unmanaged_alias_separately() {
        // Managed alias block present, no unmanaged line.
        let managed = format!("plugins {{\n{ZELLIJ_ALIAS_BEGIN}\n    radar location=\"file:/x.wasm\"\n{ZELLIJ_ALIAS_END}\n}}\n");
        let env = ZellijEnv {
            config_text: Some(managed),
            layout_text: None,
            permissions_text: None,
            codex_hooks_text: None,
            installed_plugins_text: None,
            wasm_present: false,
            config_managed: false,
            wasm_path: "/x.wasm".to_string(),
        };
        let f = analyze_zellij(&env);
        assert!(f.managed_alias_present, "managed marker must be detected");
        assert!(!f.unmanaged_alias_present, "no unmanaged alias here");
    }

    #[test]
    fn analyze_zellij_derives_has_rail_and_grant_from_text() {
        let wasm_path = "/home/user/.config/zellij/plugins/zj_radar.wasm";
        let layout = "layout {\n    plugin location=\"radar\"\n}\n";
        let perms = format!("\"{wasm_path}\" {{\n    ReadApplicationState\n}}\n");
        let env = ZellijEnv {
            config_text: None,
            layout_text: Some(layout.to_string()),
            permissions_text: Some(perms),
            codex_hooks_text: None,
            installed_plugins_text: None,
            wasm_present: true,
            config_managed: false,
            wasm_path: wasm_path.to_string(),
        };
        let f = analyze_zellij(&env);
        assert_eq!(f.has_rail, Some(true), "layout text with radar plugin has rail");
        assert_eq!(f.granted, Some(true), "permissions naming the wasm path is granted");
        assert!(f.wasm_present);
    }

    #[test]
    fn analyze_zellij_absent_files_are_none_not_false() {
        let env = ZellijEnv {
            config_text: None,
            layout_text: None,
            permissions_text: None,
            codex_hooks_text: None,
            installed_plugins_text: None,
            wasm_present: false,
            config_managed: false,
            wasm_path: "/x.wasm".to_string(),
        };
        let f = analyze_zellij(&env);
        assert_eq!(f.has_rail, None, "no layout file -> None, distinct from Some(false)");
        assert_eq!(f.granted, None, "no permissions file -> None");
        assert!(!f.producer_wired, "no codex hooks and no claude plugin -> not wired");
    }

    #[test]
    fn analyze_codex_classifies_notify_states() {
        let ours = "notify = [\"zj-radar\", \"notify\", \"codex\"]\n";
        let foreign = "notify = [\"other\"]\n";
        let mk = |cfg: Option<&str>| analyze_codex(&CodexEnv {
            codex_on_path: true,
            zj_radar_on_path: true,
            config_text: cfg.map(str::to_string),
            hooks_text: None,
        });
        assert!(matches!(mk(Some(ours)).notify, CodexNotifyState::Ours));
        assert!(matches!(mk(Some(foreign)).notify, CodexNotifyState::Foreign));
        assert!(matches!(mk(Some("a = 1\n")).notify, CodexNotifyState::NotInstalled));
        assert!(matches!(mk(None).notify, CodexNotifyState::ConfigAbsent));
    }

    #[test]
    fn mode_precedence_grant_beats_check_beats_uninstall() {
        assert!(matches!(mode_from_flags(true, true, true), Mode::Grant));
        assert!(matches!(mode_from_flags(false, true, true), Mode::Check));
        assert!(matches!(mode_from_flags(false, false, true), Mode::Uninstall));
        assert!(matches!(mode_from_flags(false, false, false), Mode::Install));
    }

    #[test]
    fn analyze_codex_hooks_feature_and_event_count() {
        let cfg_disabled = "[features]\nhooks = false\n";
        let f = analyze_codex(&CodexEnv {
            codex_on_path: true,
            zj_radar_on_path: true,
            config_text: Some(cfg_disabled.to_string()),
            hooks_text: None,
        });
        assert!(matches!(f.hooks_feature, CodexHooksFeature::Disabled));
        assert!(f.owned_hook_events.is_none(), "no hooks.json -> None");
    }
}
