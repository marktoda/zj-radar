//! `zj-radar run` — turnkey: own a Zellij config dir and launch it.
//!
//! Side effects live in `run()`; every decision (session name, launch args,
//! which advisories to print) is pure and lives in `plan_run`, mirroring the
//! pure-editor / thin-IO split that `setup.rs` uses.

use super::fsutil::atomic_write;
use super::setup::CODEX_HOOK_MARKER;
use std::path::{Path, PathBuf};

/// Shown on first run (create OR attach) when the wasm isn't granted. The grant
/// float auto-opens in the common cases — the onboarding layout carries it on
/// CREATE, and `plan_run` dispatches a `launch-or-focus-plugin` action on attach
/// to a LIVE session (see `grant_float_args`). Only a cold resurrect (dead
/// session, no running server to dispatch to) can't auto-open it, so the single
/// honest line names the baked-in Ctrl-y keybind as the fallback rather than
/// branching into a second, dead-end message.
const GRANT_HINT: &str = "First run: a permission prompt opens — press y to enable agent status \
    (if it doesn't appear, press Ctrl-y in the session).";
pub(crate) const PRODUCER_HINT: &str = "Agent status off — no producer wired. Run `zj-radar setup` to enable.";

// ── Pure helpers ─────────────────────────────────────────────────────────────

/// Session name from the cwd basename (sanitized) or an explicit override.
/// Zellij session names allow `[A-Za-z0-9_-]`; other chars fold to `-`. If
/// nothing alphanumeric survives (empty or all-symbol basename), falls back to
/// `"radar"` rather than emitting a degenerate all-dashes name.
pub(crate) fn session_name(cwd: &Path, name_override: Option<&str>) -> String {
    if let Some(n) = name_override {
        return n.to_string();
    }
    let base = cwd.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let sanitized: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' })
        .collect();
    if sanitized.chars().any(|c| c.is_ascii_alphanumeric()) {
        sanitized
    } else {
        "radar".to_string()
    }
}

/// Args to CREATE a new session with the rail layout:
/// `zellij --config-dir <dir> --session <name> --new-session-with-layout radar`.
///
/// `--new-session-with-layout` (NOT `--layout`) is required here: when combined
/// with `--session`, a plain `--layout` is interpreted by Zellij as "add a tab to
/// the EXISTING session <name>", which fails ("session not found") when it
/// doesn't exist yet. `--new-session-with-layout` always starts a new session —
/// even when invoked from inside Zellij — using the named layout resolved from
/// `--config-dir`'s `layouts/`.
pub(crate) fn create_session_args(config_dir: &Path, session: &str, layout: &str) -> Vec<String> {
    vec![
        "--config-dir".into(),
        config_dir.to_string_lossy().into_owned(),
        "--session".into(),
        session.into(),
        "--new-session-with-layout".into(),
        layout.into(),
    ]
}

/// Args to attach to (or resurrect) an existing session:
/// `zellij --config-dir <dir> attach <name>`.
///
/// The layout was applied at creation, but `--config-dir` is still load-bearing
/// here: Zellij config (keybinds included) comes from the ATTACHING client, and
/// a resurrected session is a brand-new server started by that client. Without
/// it, attach reads the user's default `~/.config/zellij` — where the Ctrl-y
/// grant keybind doesn't exist (and a `clear-defaults=true` user config strips
/// any merge path) — so the rail's "press Ctrl-y" advice was a dead end on
/// every resurrect. Passing the owned dir makes the baked-in binds real for
/// every client, not just the creating one.
pub(crate) fn attach_session_args(config_dir: &Path, session: &str) -> Vec<String> {
    vec![
        "--config-dir".into(),
        config_dir.to_string_lossy().into_owned(),
        "attach".into(),
        session.into(),
    ]
}

/// Args to summon the grant float on a LIVE session before we attach:
/// `zellij --session <s> action launch-or-focus-plugin file:<wasm> --floating
/// --move-to-focused-tab --configuration role=onboarding`.
///
/// Mirrors the baked-in Ctrl-y keybind (`run_assets/config.kdl`) but is triggered
/// by `run` itself, so the common re-run-while-live case needs no keypress. The
/// concrete `file:<wasm>` URL (not the `radar` alias) means it works even on a
/// session whose frozen config lacks our alias; `role=onboarding` makes it a
/// distinct instance from the rail, so it opens a new float instead of focusing
/// the rail pane. Requires a running server — a dead/resurrectable session has
/// none, which is why `plan_run` gates this on liveness (keybind covers the rest).
pub(crate) fn grant_float_args(session: &str, wasm_path: &Path) -> Vec<String> {
    vec![
        "--session".into(),
        session.into(),
        "action".into(),
        "launch-or-focus-plugin".into(),
        format!("file:{}", wasm_path.to_string_lossy()),
        "--floating".into(),
        "--move-to-focused-tab".into(),
        "--configuration".into(),
        "role=onboarding".into(),
    ]
}

/// True iff `permissions.kdl` contains a top-level grant block whose quoted key
/// equals `wasm_abs_path`. Zellij keys grants by the literal path string, so an
/// exact match (closing quote included) is correct; the `{` guard skips a bare
/// quoted string that isn't a block header.
pub(crate) fn wasm_is_granted(permissions_kdl: &str, wasm_abs_path: &str) -> bool {
    let needle = format!("\"{wasm_abs_path}\"");
    permissions_kdl
        .lines()
        .map(str::trim_start)
        .any(|l| l.starts_with(&needle) && l.contains('{'))
}

/// Producer-detection advisory: `Some(hint)` when NO producer is wired (Codex
/// hooks lack our marker AND the Claude producer plugin is absent), else `None`.
pub(crate) fn producer_hint(codex_hooks: Option<&str>, claude_present: bool) -> Option<String> {
    let codex = codex_hooks.is_some_and(|h| h.contains(CODEX_HOOK_MARKER));
    if codex || claude_present {
        None
    } else {
        Some(PRODUCER_HINT.to_string())
    }
}

/// True iff Claude Code's installed-plugins manifest lists zj-radar's producer
/// plugin (`zj-radar-claude`). `None`/empty input returns `false`.
pub(crate) fn claude_producer_wired(installed_plugins_json: Option<&str>) -> bool {
    installed_plugins_json.is_some_and(|s| s.contains("zj-radar-claude"))
}

/// Join argv into a copy-pasteable shell command. Every printed command hint
/// (`--print-cmd`, "try running: zellij …") must go through this: on macOS the
/// owned config dir lives under `~/Library/Application Support/…`, so a bare
/// `join(" ")` produces a command that breaks at the space for every user who
/// pastes it. Conservatively single-quotes anything beyond the unambiguous
/// safe set (with embedded `'` as `'\''`, the POSIX idiom).
pub(crate) fn shell_join(args: &[String]) -> String {
    let quoted: Vec<String> = args.iter().map(|a| shell_quote(a)).collect();
    quoted.join(" ")
}

fn shell_quote(arg: &str) -> String {
    let safe = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/' | b':' | b'=' | b',' | b'@' | b'+');
    if !arg.is_empty() && arg.bytes().all(safe) {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

// ── Path locators ────────────────────────────────────────────────────────────

/// The zj-radar–owned Zellij config directory rooted under `data_dir`.
pub(crate) fn owned_config_dir_in(data_dir: &Path) -> PathBuf {
    data_dir.join("zj-radar").join("zellij")
}

/// Zellij's `permissions.kdl` rooted under `cache_dir`. The sub-folder differs
/// between macOS (`org.Zellij-Contributors.Zellij`) and Linux (`zellij`).
pub(crate) fn permissions_path_in(cache_dir: &Path, is_macos: bool) -> PathBuf {
    let folder = if is_macos { "org.Zellij-Contributors.Zellij" } else { "zellij" };
    cache_dir.join(folder).join("permissions.kdl")
}

/// Platform-resolved owned config dir, or `None` if the data dir is unknown.
/// `ZJ_RADAR_DATA_DIR` overrides the platform data dir — the isolation knob
/// `just dev` sets so a locally-built CLI materializes into its own sandbox
/// and can never touch the installed zj-radar's assets.
pub(crate) fn owned_config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("ZJ_RADAR_DATA_DIR").filter(|d| !d.is_empty()) {
        return Some(owned_config_dir_in(Path::new(&dir)));
    }
    dirs::data_dir().map(|d| owned_config_dir_in(&d))
}

/// Platform-resolved path to Zellij's `permissions.kdl`, or `None` if the cache
/// dir is unknown.
pub(crate) fn zellij_permissions_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|c| permissions_path_in(&c, cfg!(target_os = "macos")))
}

// ── Materializer ─────────────────────────────────────────────────────────────

pub(crate) struct Assets {
    pub config_template: &'static str,
    pub layout: &'static str,
    /// First-run layout: the rail plus a floating pane that hosts the grant
    /// prompt legibly. Selected by `plan_run` only when the wasm isn't granted.
    pub onboarding_layout: &'static str,
    /// `Some` when the wasm is baked into the binary (every prebuilt install);
    /// `None` for a from-crates.io `cargo install`, where `run` downloads it.
    pub wasm: Option<&'static [u8]>,
}

pub(crate) struct Materialized {
    pub config_dir: PathBuf,
    pub wasm_path: PathBuf,
}

/// Write the owned config dir idempotently. A no-op when the version marker
/// matches AND all generated files are present (so a deleted file forces a
/// rewrite). Each file is written atomically; the marker is written last, so an
/// interrupted run is re-materialized rather than served half-written.
pub(crate) fn materialize(
    dir: &Path,
    version: &str,
    assets: &Assets,
) -> std::io::Result<Materialized> {
    let wasm_path = dir.join("plugins").join("zj_radar.wasm");
    let config_path = dir.join("config.kdl");
    let layout_path = dir.join("layouts").join("radar.kdl");
    let onboarding_layout_path = dir.join("layouts").join("radar-onboarding.kdl");
    let marker = dir.join(".zj-radar-version");

    let up_to_date = std::fs::read_to_string(&marker).is_ok_and(|v| v == version)
        && wasm_path.exists()
        && config_path.exists()
        && layout_path.exists()
        && onboarding_layout_path.exists();
    if up_to_date {
        return Ok(Materialized { config_dir: dir.to_path_buf(), wasm_path });
    }

    let config = assets.config_template.replace("@WASM@", &wasm_path.to_string_lossy());
    // Write the embedded wasm if we have it; otherwise leave wasm_path for the
    // caller to populate (download). The `up_to_date` check above already gates
    // on wasm_path.exists(), so a not-yet-downloaded wasm never short-circuits.
    if let Some(bytes) = assets.wasm {
        atomic_write(&wasm_path, bytes)?;
    }
    atomic_write(&config_path, config.as_bytes())?;
    atomic_write(&layout_path, assets.layout.as_bytes())?;
    atomic_write(&onboarding_layout_path, assets.onboarding_layout.as_bytes())?;
    atomic_write(&marker, version.as_bytes())?;
    Ok(Materialized { config_dir: dir.to_path_buf(), wasm_path })
}

// ── Embedded assets ──────────────────────────────────────────────────────────

const CONFIG_TEMPLATE: &str = include_str!("run_assets/config.kdl");
const LAYOUT: &str = include_str!("run_assets/radar.kdl");
const ONBOARDING_LAYOUT: &str = include_str!("run_assets/radar-onboarding.kdl");

// build.rs sets `embedded_wasm` (+ ZJ_RADAR_WASM_PATH) when it has a wasm to
// bake in — true for every prebuilt binary (curl|sh, binstall, nix) and any
// in-workspace build. A from-crates.io `cargo install` has no wasm to embed, so
// WASM is None and `run` downloads the matching release on first use.
#[cfg(embedded_wasm)]
const WASM: Option<&[u8]> = Some(include_bytes!(env!("ZJ_RADAR_WASM_PATH")));
#[cfg(not(embedded_wasm))]
const WASM: Option<&[u8]> = None;

fn embedded_assets() -> Assets {
    Assets {
        config_template: CONFIG_TEMPLATE,
        layout: LAYOUT,
        onboarding_layout: ONBOARDING_LAYOUT,
        wasm: WASM,
    }
}

// ── Orchestration: pure plan + thin IO ───────────────────────────────────────

/// Inputs gathered from the environment, separated from the decision so that
/// `plan_run` is pure and unit-testable.
struct RunFacts {
    /// Resolved session name (cwd basename or override).
    session: String,
    config_dir: PathBuf,
    wasm_path: PathBuf,
    /// Whether a Zellij session of this name already exists (attach vs create).
    session_exists: bool,
    /// Whether that session has a RUNNING server (not merely resurrectable). Only
    /// a running server can receive the pre-attach grant-float dispatch.
    session_running: bool,
    /// Whether we're invoked from inside a Zellij session (`run` can't nest).
    inside_zellij: bool,
    /// Whether the DEAD session's cached resurrection layout carries
    /// `defer_permission "true"` rails (see `session_layout_defers`). A
    /// resurrected onboarding-era session rebuilds those rails with the float
    /// long gone — the marker they wait on will never land without help.
    resurrect_layout_defers: bool,
    permissions_kdl: Option<String>,
    codex_hooks: Option<String>,
    installed_plugins: Option<String>,
}

/// What to launch and what to advise — the pure result of `plan_run`.
struct RunPlan {
    args: Vec<String>,
    /// When `Some`, a `zellij` action to dispatch (best-effort) BEFORE `args`,
    /// to summon the grant float on a live session so no keypress is needed.
    pre_attach: Option<Vec<String>>,
    /// When `Some`, the same grant-float action, to dispatch AFTER the session's
    /// server comes up — the resurrect path. A dead session can't receive an
    /// action, so the caller watches for liveness in the background while
    /// `attach` resurrects, then fires this once. Covers both the ungranted
    /// resurrect (float hosts the real prompt) and the granted-but-deferring
    /// resurrect (float auto-resolves against the cached grant and writes the
    /// marker the frozen `defer_permission` rails are stuck waiting on).
    post_attach_watch: Option<Vec<String>>,
    advisories: Vec<String>,
    /// When set, the caller must NOT launch (we're nested) — show guidance.
    nested: bool,
}

/// Pure decision: attach if the session exists, otherwise create with the rail
/// layout; collect the ordered advisory lines (grant hint before producer hint);
/// surface whether we're nested.
fn plan_run(facts: &RunFacts) -> RunPlan {
    let granted = facts
        .permissions_kdl
        .as_deref()
        .is_some_and(|kdl| wasm_is_granted(kdl, &facts.wasm_path.to_string_lossy()));

    let args = if facts.session_exists {
        attach_session_args(&facts.config_dir, &facts.session)
    } else {
        // First run (ungranted) opens the onboarding layout — its floating pane
        // hosts Zellij's grant prompt legibly. Once granted, later runs use the
        // plain rail layout so no transient pane ever flashes.
        let layout = if granted { "radar" } else { "radar-onboarding" };
        create_session_args(&facts.config_dir, &facts.session, layout)
    };

    // Attaching to an existing LIVE ungranted session: summon the grant float
    // ourselves (no keypress). Creation carries the float in its layout; a
    // dead/resurrectable session has no server to dispatch to, so it falls back
    // to the baked-in Ctrl-y keybind (named in GRANT_HINT).
    let pre_attach = (!granted && facts.session_exists && facts.session_running)
        .then(|| grant_float_args(&facts.session, &facts.wasm_path));

    // Attaching to a DEAD session (resurrect): no server to dispatch to yet,
    // so plan a post-attach watch instead — fired once the resurrected server
    // is up. Needed when the wasm is ungranted (the float hosts the prompt) OR
    // when the cached layout resurrects deferring rails (granted or not, they
    // are stuck until a float writes the marker).
    let post_attach_watch = (facts.session_exists
        && !facts.session_running
        && (!granted || facts.resurrect_layout_defers))
        .then(|| grant_float_args(&facts.session, &facts.wasm_path));

    let mut advisories = Vec::new();
    if !granted {
        advisories.push(GRANT_HINT.to_string());
    }
    let claude = claude_producer_wired(facts.installed_plugins.as_deref());
    if let Some(hint) = producer_hint(facts.codex_hooks.as_deref(), claude) {
        advisories.push(hint);
    }
    RunPlan { args, pre_attach, post_attach_watch, advisories, nested: facts.inside_zellij }
}

pub struct RunOptions {
    pub name: Option<String>,
    pub print_cmd: bool,
}

/// Read `~/<rel>` if present. Producer/grant probes are strictly read-only.
fn read_under_home(rel: &str) -> Option<String> {
    dirs::home_dir().and_then(|h| std::fs::read_to_string(h.join(rel)).ok())
}

/// True iff a Zellij session named `name` exists (alive or resurrectable), per
/// `zellij list-sessions --short` (plain names, one per line). Any error
/// (zellij missing, no server) is treated as "does not exist" → create path.
fn session_is_live(name: &str) -> bool {
    std::process::Command::new("zellij")
        .args(["list-sessions", "--short"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| String::from_utf8_lossy(&o.stdout).lines().any(|l| l.trim() == name))
}

/// True iff a session named `name` has a RUNNING server right now (not merely
/// resurrectable). `--short` can't tell the two apart, so we parse the full
/// listing: a resurrectable session's line contains `EXITED`, a live one does
/// not (it may carry `(current)`). Only a running server can receive the
/// `grant_float_args` action, so this gates the pre-attach dispatch. Any error is
/// "not running" → fall back to the keybind.
fn session_is_running(name: &str) -> bool {
    std::process::Command::new("zellij")
        .args(["list-sessions", "--no-formatting"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| {
            String::from_utf8_lossy(&o.stdout).lines().any(|l| {
                l.split_whitespace().next() == Some(name) && !l.contains("EXITED")
            })
        })
}

/// True iff a cached resurrection layout will rebuild `defer_permission "true"`
/// rails. Zellij snapshots plugin config verbatim into `session-layout.kdl`, so
/// a session created from the onboarding layout carries the flag forever — even
/// after the float granted and closed. Substring match is enough: the value is
/// only ever written as the exact `defer_permission "true"` pair by our own
/// layouts, and a false positive merely summons a float that auto-resolves and
/// closes itself.
pub(crate) fn session_layout_defers(session_layout_kdl: &str) -> bool {
    session_layout_kdl.contains("defer_permission \"true\"")
}

/// Read the cached resurrection layout for `session`, if Zellij kept one:
/// `<zellij cache>/<contract dir>/session_info/<session>/session-layout.kdl`.
/// The contract dir's name is a Zellij internal (`contract_version_1` today),
/// so scan for any directory that has our session under `session_info` rather
/// than hardcoding it. Best-effort: any error reads as `None` (fail-open — the
/// plugin's own patience escalation still self-heals, just slower).
fn cached_session_layout(session: &str) -> Option<String> {
    let base = zellij_permissions_path()?.parent()?.to_path_buf();
    let entries = std::fs::read_dir(base).ok()?;
    entries.flatten().find_map(|entry| {
        let candidate = entry
            .path()
            .join("session_info")
            .join(session)
            .join("session-layout.kdl");
        std::fs::read_to_string(candidate).ok()
    })
}

/// Fire `dispatch` (a `zellij …` invocation) once the session's server is up,
/// polling from a detached thread while the foreground `attach` resurrects it.
/// Output is discarded — the terminal belongs to the attached client by then —
/// and every failure mode is best-effort by design: the watcher gives up after
/// ~30s, and the plugin's own patience escalation remains the backstop.
fn dispatch_when_running(session: String, dispatch: Vec<String>) {
    std::thread::spawn(move || {
        for _ in 0..60 {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if session_is_running(&session) {
                // One settling beat: the server lists as running slightly
                // before it accepts actions.
                std::thread::sleep(std::time::Duration::from_millis(500));
                let _ = std::process::Command::new("zellij")
                    .args(&dispatch)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
                return;
            }
        }
    });
}

pub fn run(opts: RunOptions) {
    let Some(dir) = owned_config_dir() else {
        crate::exit::fail_report("zj-radar", "could not resolve a data directory");
        return;
    };
    let materialized = match materialize(&dir, env!("CARGO_PKG_VERSION"), &embedded_assets()) {
        Ok(m) => m,
        Err(e) => {
            crate::exit::fail_report("zj-radar", format!("failed to set up config dir {}: {e}", dir.display()));
            return;
        }
    };

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let session = session_name(&cwd, opts.name.as_deref());
    let session_exists = session_is_live(&session);
    let session_running = session_exists && session_is_running(&session);

    // Local-wasm override (`just dev`): copy the freshly-built artifact over
    // the materialized path on every run, so the session always loads the
    // build under test — never the embedded or downloaded release wasm.
    if let Some(wasm) = std::env::var_os("ZJ_RADAR_WASM").filter(|w| !w.is_empty()) {
        if let Err(e) = std::fs::copy(&wasm, &materialized.wasm_path) {
            crate::exit::fail_report(
                "zj-radar",
                format!("copying ZJ_RADAR_WASM ({}) failed — {e}", Path::new(&wasm).display()),
            );
            return;
        }
    }

    // No embedded wasm (a from-crates.io `cargo install`) and none cached yet —
    // fetch the matching release once. Prebuilt binaries embed the wasm, so this
    // path is inert for them.
    if !materialized.wasm_path.exists() {
        let version = super::setup::wasm_download_version();
        if let Err(e) = super::setup::download_wasm_to(&version, &materialized.wasm_path) {
            crate::exit::fail_report(
                "zj-radar",
                format!(
                    "no embedded wasm and the download failed — {e}\n\
                     Fetch it once while online with: zj-radar setup zellij --download"
                ),
            );
            return;
        }
    }

    // Only a dead-but-existing session resurrects from a cached layout, so
    // only that case needs the defer probe.
    let resurrect_layout_defers = session_exists
        && !session_running
        && cached_session_layout(&session).is_some_and(|kdl| session_layout_defers(&kdl));
    let facts = RunFacts {
        session,
        config_dir: materialized.config_dir,
        wasm_path: materialized.wasm_path,
        session_exists,
        session_running,
        inside_zellij: std::env::var_os("ZELLIJ").is_some(),
        resurrect_layout_defers,
        permissions_kdl: zellij_permissions_path().and_then(|p| std::fs::read_to_string(p).ok()),
        codex_hooks: crate::setup::codex_hooks_text(),
        installed_plugins: read_under_home(".claude/plugins/installed_plugins.json"),
    };
    let plan = plan_run(&facts);

    // Advisories are guidance, which belongs on stderr (see the `exit` module
    // doc) — and `--print-cmd`'s stdout must stay machine-readable: a shell
    // doing `$(zj-radar run --print-cmd)` must capture the command, not prose.
    for advisory in &plan.advisories {
        eprintln!("{advisory}");
    }
    if opts.print_cmd {
        if let Some(dispatch) = &plan.pre_attach {
            println!("zellij {}", shell_join(dispatch));
        }
        println!("zellij {}", shell_join(&plan.args));
        return;
    }
    if plan.nested {
        crate::exit::fail_report(
            "zj-radar",
            "you're already inside Zellij. `run` starts a NEW session — detach first \
             (Ctrl-o d by default) and re-run, or use `zj-radar setup` to add the rail to your \
             current Zellij config.",
        );
        return;
    }
    // Best-effort: summon the grant float on the live session before we hand the
    // terminal to `attach`. Failure is non-fatal — the Ctrl-y keybind and the
    // rail's own grant prompt remain as fallbacks — so we ignore the result.
    if let Some(dispatch) = &plan.pre_attach {
        let _ = std::process::Command::new("zellij").args(dispatch).status();
    }
    // Resurrect path: the server only exists once `attach` below brings it up,
    // so watch for it from the background and fire the float dispatch then.
    // The thread dies with this process (which outlives the attached client).
    if let Some(dispatch) = plan.post_attach_watch.clone() {
        dispatch_when_running(facts.session.clone(), dispatch);
    }
    if let Err(e) = std::process::Command::new("zellij").args(&plan.args).status() {
        crate::exit::fail_report("zj-radar", format!("failed to launch zellij: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn session_name_sanitizes_falls_back_and_overrides() {
        assert_eq!(session_name(Path::new("/Users/m/dev/zj-radar"), None), "zj-radar");
        assert_eq!(session_name(Path::new("/Users/m/dev/My Proj!"), None), "My-Proj-");
        assert_eq!(session_name(Path::new("/"), None), "radar");
        // All-symbol basename: nothing alphanumeric survives -> fall back, not "---".
        assert_eq!(session_name(Path::new("/Users/m/%%%"), None), "radar");
        assert_eq!(session_name(Path::new("/Users/m/dev/foo"), Some("bar")), "bar");
    }

    #[test]
    fn create_and_attach_args_are_exact() {
        assert_eq!(
            create_session_args(Path::new("/cfg"), "foo", "radar"),
            vec!["--config-dir", "/cfg", "--session", "foo", "--new-session-with-layout", "radar"]
        );
        // --config-dir on attach is load-bearing: a resurrected session is a
        // NEW server whose config (Ctrl-y grant keybind included) comes from
        // the attaching client, not from the session's creation.
        assert_eq!(
            attach_session_args(Path::new("/cfg"), "foo"),
            vec!["--config-dir", "/cfg", "attach", "foo"]
        );
    }

    #[test]
    fn session_layout_defers_detects_frozen_onboarding_rails() {
        assert!(session_layout_defers(
            "layout {\n  pane {\n    plugin location=\"file:/x.wasm\" {\n      defer_permission \"true\"\n    }\n  }\n}\n"
        ));
        assert!(!session_layout_defers("layout { pane { plugin location=\"file:/x.wasm\" } }\n"));
        // The granted-era plain layout never carries the flag.
        assert!(!session_layout_defers(LAYOUT));
        // Our onboarding layout always does — the pair this probe exists for.
        assert!(session_layout_defers(ONBOARDING_LAYOUT));
    }

    const SAMPLE: &str = r#"
"/nix/store/abc-room.wasm" {
    ReadApplicationState
}
"/Users/m/Library/Application Support/zj-radar/zellij/plugins/zj_radar.wasm" {
    ReadApplicationState
    ReadCliPipes
    ChangeApplicationState
}
"#;

    #[test]
    fn shell_join_quotes_what_a_shell_would_split_or_expand() {
        let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // Plain flags and paths pass through bare.
        assert_eq!(shell_join(&s(&["attach", "--config-dir", "/cfg", "foo"])),
                   "attach --config-dir /cfg foo");
        // The macOS owned-config path — the case this helper exists for.
        assert_eq!(
            shell_join(&s(&["--config-dir", "/Users/m/Library/Application Support/zj-radar/zellij"])),
            "--config-dir '/Users/m/Library/Application Support/zj-radar/zellij'"
        );
        // Embedded single quote uses the POSIX '\'' idiom; empty arg stays visible.
        assert_eq!(shell_join(&s(&["it's", ""])), r"'it'\''s' ''");
        // Shell metacharacters (globs, vars, semicolons) get quoted, not trusted.
        assert_eq!(shell_join(&s(&["a b", "$HOME", "*;rm"])), "'a b' '$HOME' '*;rm'");
    }

    #[test]
    fn grant_detection_matches_block_headers_only() {
        let p = "/Users/m/Library/Application Support/zj-radar/zellij/plugins/zj_radar.wasm";
        assert!(wasm_is_granted(SAMPLE, p));
        assert!(!wasm_is_granted(SAMPLE, "/some/other/zj_radar.wasm"));
        assert!(!wasm_is_granted("", p));
        // A quoted path with no opening brace is not a grant block.
        assert!(!wasm_is_granted("\"/x/zj_radar.wasm\"\n", "/x/zj_radar.wasm"));
        // The closing quote in the needle prevents matching a longer path it prefixes.
        assert!(!wasm_is_granted("\"/x/zj_radar.wasm.bak\" {\n}\n", "/x/zj_radar.wasm"));
    }

    #[test]
    fn locators_compose_expected_paths() {
        assert_eq!(owned_config_dir_in(Path::new("/data")), Path::new("/data/zj-radar/zellij"));
        assert_eq!(
            permissions_path_in(Path::new("/cache"), true),
            Path::new("/cache/org.Zellij-Contributors.Zellij/permissions.kdl")
        );
        assert_eq!(
            permissions_path_in(Path::new("/cache"), false),
            Path::new("/cache/zellij/permissions.kdl")
        );
    }

    fn test_assets() -> Assets {
        Assets {
            config_template: "plugins { radar location=\"file:@WASM@\" {} }\n",
            layout: "layout { default_tab_template { children } tab { pane } }\n",
            onboarding_layout: "layout { tab { pane; floating_panes { pane { plugin location=\"radar\" { role \"onboarding\" } } } } }\n",
            wasm: Some(b"\0asm-dummy"),
        }
    }

    #[test]
    fn materialize_without_embedded_wasm_writes_config_but_not_wasm() {
        let d = tempdir().unwrap();
        let dir = d.path().join("c");
        let assets = Assets {
            config_template: "plugins { radar location=\"file:@WASM@\" {} }\n",
            layout: "layout {}\n",
            onboarding_layout: "layout {}\n",
            wasm: None,
        };
        let m = materialize(&dir, "0.1.0", &assets).unwrap();
        // No embedded wasm → materialize must not fabricate the file; `run`
        // downloads it. Config/layout/marker are still written.
        assert!(!m.wasm_path.exists(), "must not create a wasm file when none is embedded");
        assert!(dir.join("config.kdl").exists(), "config still written");
        assert!(dir.join("layouts/radar.kdl").exists(), "layout still written");
        assert!(dir.join("layouts/radar-onboarding.kdl").exists(), "onboarding layout written");
        assert!(dir.join(".zj-radar-version").exists(), "marker still written");
    }

    #[test]
    fn materialize_writes_all_files_and_substitutes_wasm_path() {
        let d = tempdir().unwrap();
        let dir = d.path().join("zj-radar/zellij");
        let m = materialize(&dir, "0.1.0", &test_assets()).unwrap();
        assert_eq!(m.config_dir, dir);
        assert_eq!(m.wasm_path, dir.join("plugins/zj_radar.wasm"));
        assert_eq!(std::fs::read(&m.wasm_path).unwrap(), b"\0asm-dummy");
        let cfg = std::fs::read_to_string(dir.join("config.kdl")).unwrap();
        assert!(cfg.contains(&format!("file:{}", m.wasm_path.display())));
        assert!(!cfg.contains("@WASM@"));
        assert!(dir.join("layouts/radar.kdl").exists());
        assert_eq!(std::fs::read_to_string(dir.join(".zj-radar-version")).unwrap(), "0.1.0");
    }

    #[test]
    fn materialize_is_noop_on_matching_version() {
        let d = tempdir().unwrap();
        let dir = d.path().join("c");
        materialize(&dir, "0.1.0", &test_assets()).unwrap();
        let first_layout = std::fs::read_to_string(dir.join("layouts/radar.kdl")).unwrap();
        let sentinel = Assets {
            config_template: "SENTINEL-CONFIG-SHOULD-NOT-BE-WRITTEN\n",
            layout: "SENTINEL-SHOULD-NOT-BE-WRITTEN\n",
            onboarding_layout: "SENTINEL-ONBOARDING-SHOULD-NOT-BE-WRITTEN\n",
            wasm: Some(b"SENTINEL-WASM"),
        };
        materialize(&dir, "0.1.0", &sentinel).unwrap();
        let after_layout = std::fs::read_to_string(dir.join("layouts/radar.kdl")).unwrap();
        assert_eq!(after_layout, first_layout, "matching version must be a no-op");
        assert!(!after_layout.contains("SENTINEL"), "sentinel must not appear in layout file");
    }

    #[test]
    fn materialize_rewrites_when_a_file_is_missing_despite_matching_marker() {
        let d = tempdir().unwrap();
        let dir = d.path().join("c");
        materialize(&dir, "0.1.0", &test_assets()).unwrap();
        std::fs::remove_file(dir.join("config.kdl")).unwrap();
        materialize(&dir, "0.1.0", &test_assets()).unwrap();
        assert!(dir.join("config.kdl").exists(), "deleted file must be restored");
    }

    #[test]
    fn materialize_rewrites_on_version_change() {
        let d = tempdir().unwrap();
        let dir = d.path().join("c");
        materialize(&dir, "0.1.0", &test_assets()).unwrap();
        materialize(&dir, "0.2.0", &test_assets()).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(".zj-radar-version")).unwrap(), "0.2.0");
    }

    #[test]
    fn claude_producer_detection() {
        let with_plugin = r#"{"plugins":["zj-radar-claude","some-other-plugin"]}"#;
        assert!(claude_producer_wired(Some(with_plugin)));
        let without_plugin = r#"{"plugins":["some-other-plugin","another-one"]}"#;
        assert!(!claude_producer_wired(Some(without_plugin)));
        assert!(!claude_producer_wired(None));
    }

    #[test]
    fn producer_hint_only_when_none_wired() {
        let wired = format!("{CODEX_HOOK_MARKER} zj-radar notify codex");
        assert!(producer_hint(Some(&wired), false).is_none());
        assert!(producer_hint(None, true).is_none());
        assert!(producer_hint(None, false).unwrap().contains("zj-radar setup"));
    }

    #[test]
    fn bundled_layout_has_swaps_and_alias() {
        assert!(LAYOUT.contains("swap_tiled_layout"), "rail layout must declare swaps");
        assert!(LAYOUT.contains("location=\"radar\""), "rail must use the radar alias");
        assert!(CONFIG_TEMPLATE.contains("@WASM@"), "config template needs the @WASM@ token");
    }

    #[test]
    fn bundled_config_has_grant_keybind() {
        // The Ctrl-y escape hatch is the manual grant path from ANY session
        // state. Keybinds come from each client's config, so it's real only
        // because `attach_session_args` passes `--config-dir` back to this
        // owned config — creation and every later attach/resurrect alike. It
        // must launch the radar plugin floating in the onboarding role.
        assert!(CONFIG_TEMPLATE.contains("bind \"Ctrl y\""), "config must bind the grant escape hatch");
        assert!(
            CONFIG_TEMPLATE.contains("LaunchOrFocusPlugin \"radar\""),
            "keybind must launch the radar plugin"
        );
        assert!(CONFIG_TEMPLATE.contains("floating true"), "grant pane must be floating to be legible");
        assert!(
            CONFIG_TEMPLATE.contains("role \"onboarding\""),
            "grant float must use the onboarding role so it owns the prompt and closes on grant"
        );
        // The rail's needs-permission face only advertises Ctrl-y when its
        // config claims the bind exists. Zellij merges alias config into every
        // launch of the `radar` alias, so one key on the alias covers layout
        // rails, the keybind float, and resurrection snapshots alike.
        assert!(
            CONFIG_TEMPLATE.contains("grant_hint \"ctrl-y\""),
            "the alias must claim the Ctrl-y hint this config's keybind makes true"
        );
    }

    #[test]
    fn bundled_config_binds_alt_n_tab_jumps() {
        // The rail numbers its rows after tabs, so the owned config supplies
        // Alt-1..9 → GoToTab as an unadvertised nicety (Zellij owns keybinds,
        // not the plugin).
        for n in 1..=9 {
            assert!(
                CONFIG_TEMPLATE.contains(&format!("bind \"Alt {n}\"")),
                "config must bind Alt {n} → GoToTab (the rail numbers rows after tabs)"
            );
        }
        assert!(CONFIG_TEMPLATE.contains("GoToTab 1"));
        // Deliberately NOT advertised: no `jump_hint` on the alias, so the
        // footer never renders ` alt-[n] jump`. Alt+digit is commonly claimed
        // upstream of Zellij (window-manager workspace hotkeys, macOS Option
        // typing `¡`), and the rail can't detect interception — a promise the
        // binds alone can't keep. `JumpHint` stays config-gated (default
        // hidden) for users whose setups do deliver the chord.
        assert!(
            CONFIG_TEMPLATE.lines().all(|l| !l.trim_start().starts_with("jump_hint")),
            "the alias must not advertise the alt-[n] chord — interception \
             upstream of Zellij makes the hint machine-dependent"
        );
    }

    #[test]
    fn cli_version_bumped_past_alt_n_config_change() {
        // materialize() is a no-op once the on-disk `.zj-radar-version` marker
        // equals the running binary's CARGO_PKG_VERSION (see materialize() above,
        // called with env!("CARGO_PKG_VERSION") at the real call site). Landing a
        // config.kdl content change WITHOUT bumping the crate version would strand
        // every existing `zj-radar run` install on the stale pre-alt-n binds
        // forever, since their marker already matches. Pin the version past the
        // last release that shipped without alt-n binds so this landing forces
        // re-materialization. (One-off landing gate, not an evergreen regression
        // test — the baseline below is expected to go stale once released.)
        assert_ne!(
            env!("CARGO_PKG_VERSION"),
            "0.1.0",
            "bump the workspace version so the .zj-radar-version marker changes \
             and existing owned configs re-materialize with the new alt-n binds"
        );
        // (A second gate briefly lived here for the `jump_hint "alt-n"` alias
        // key, but that key was added and removed entirely within unreleased
        // 0.1.2 — no released binary ever materialized it, so there is no
        // stale-marker population to force past.)
    }

    // ── plan_run decision matrix ──
    // `granted`/`codex`/`claude` toggle whether each input signals "already set up".
    // Defaults: session "proj", does not exist (create path), not running, not nested.
    fn facts(granted: bool, codex: bool, claude: bool) -> RunFacts {
        let wasm = "/data/zj-radar/zellij/plugins/zj_radar.wasm";
        RunFacts {
            session: "proj".to_string(),
            config_dir: PathBuf::from("/data/zj-radar/zellij"),
            wasm_path: PathBuf::from(wasm),
            session_exists: false,
            session_running: false,
            inside_zellij: false,
            resurrect_layout_defers: false,
            permissions_kdl: granted.then(|| format!("\"{wasm}\" {{\n}}\n")),
            codex_hooks: codex.then(|| format!("{CODEX_HOOK_MARKER} zj-radar notify codex")),
            installed_plugins: claude.then(|| "zj-radar-claude".to_string()),
        }
    }

    #[test]
    fn plan_run_creates_new_session_when_absent() {
        let p = plan_run(&facts(true, true, false)); // granted → plain rail layout
        assert_eq!(
            p.args,
            create_session_args(Path::new("/data/zj-radar/zellij"), "proj", "radar")
        );
        assert!(!p.nested);
    }

    #[test]
    fn plan_run_uses_onboarding_layout_when_ungranted() {
        // First run: launch the onboarding layout so the floating pane hosts the
        // grant prompt legibly. The layout carries the float, so there's no
        // pre-attach dispatch on the create path.
        let p = plan_run(&facts(false, true, false));
        assert_eq!(
            p.args,
            create_session_args(Path::new("/data/zj-radar/zellij"), "proj", "radar-onboarding")
        );
        assert!(p.pre_attach.is_none(), "create path carries the float in its layout, not a dispatch");
    }

    #[test]
    fn plan_run_attaches_when_session_exists() {
        let mut f = facts(true, true, false);
        f.session_exists = true;
        let p = plan_run(&f);
        assert_eq!(p.args, attach_session_args(Path::new("/data/zj-radar/zellij"), "proj"));
    }

    #[test]
    fn plan_run_flags_nested_when_inside_zellij() {
        let mut f = facts(true, true, false);
        f.inside_zellij = true;
        assert!(plan_run(&f).nested);
    }

    #[test]
    fn plan_run_advises_grant_when_ungranted() {
        let p = plan_run(&facts(false, true, false)); // producer wired, not granted
        assert_eq!(p.advisories.len(), 1);
        assert!(p.advisories[0].contains("press y"));
    }

    #[test]
    fn plan_run_dispatches_grant_float_on_live_ungranted_attach() {
        // Attaching to an existing LIVE ungranted session: `run` summons the grant
        // float itself (no keypress) by dispatching the launch-or-focus action to
        // the running server, then attaches.
        let mut f = facts(false, true, false); // ungranted, producer wired
        f.session_exists = true;
        f.session_running = true;
        let p = plan_run(&f);
        assert_eq!(
            p.args,
            attach_session_args(Path::new("/data/zj-radar/zellij"), "proj"),
            "ungranted + existing session still attaches"
        );
        assert_eq!(
            p.pre_attach.as_deref(),
            Some(grant_float_args("proj", Path::new("/data/zj-radar/zellij/plugins/zj_radar.wasm")).as_slice()),
            "live attach dispatches the grant float before attaching"
        );
        assert!(p.post_attach_watch.is_none(), "a live server needs no post-attach watch");
    }

    #[test]
    fn plan_run_watches_dead_ungranted_attach_for_resurrection() {
        // A dead/resurrectable session has no server to receive an action NOW —
        // so the plan is a post-attach watch: once `attach` resurrects the
        // server, the caller fires the same grant-float dispatch.
        let mut f = facts(false, true, false); // ungranted
        f.session_exists = true;
        f.session_running = false; // resurrectable, not running
        let p = plan_run(&f);
        assert_eq!(p.args, attach_session_args(Path::new("/data/zj-radar/zellij"), "proj"));
        assert!(p.pre_attach.is_none(), "no live server → nothing to dispatch to yet");
        assert_eq!(
            p.post_attach_watch.as_deref(),
            Some(grant_float_args("proj", Path::new("/data/zj-radar/zellij/plugins/zj_radar.wasm")).as_slice()),
            "dead ungranted attach must plan the post-resurrect float dispatch"
        );
    }

    #[test]
    fn plan_run_watches_granted_resurrect_whose_layout_defers() {
        // The resurrect deadlock: permissions.kdl says granted, but the cached
        // session layout rebuilds `defer_permission "true"` rails with no float
        // — they'd wait (patience-long) for a marker nothing writes. The watch
        // summons the float, which auto-resolves against the cached grant and
        // writes the marker immediately.
        let mut f = facts(true, true, false); // granted
        f.session_exists = true;
        f.session_running = false;
        f.resurrect_layout_defers = true;
        let p = plan_run(&f);
        assert!(p.post_attach_watch.is_some(), "granted + deferring layout still needs the float");
        // Same facts but a healthy (non-deferring) cached layout: no watch, no
        // float flash on plain granted resurrects.
        f.resurrect_layout_defers = false;
        assert!(plan_run(&f).post_attach_watch.is_none());
    }

    #[test]
    fn plan_run_no_dispatch_when_granted_even_if_live() {
        // Already granted: nothing to grant, so a live attach never dispatches.
        let mut f = facts(true, true, false);
        f.session_exists = true;
        f.session_running = true;
        assert!(plan_run(&f).pre_attach.is_none());
    }

    #[test]
    fn plan_run_grant_hint_is_unified_and_names_the_keybind_fallback() {
        // One honest message for every ungranted path: promises the prompt (it
        // auto-opens on create + live-attach) and names Ctrl-y as the fallback for
        // the cold-resurrect case. No second, "center"-promising dead-end message.
        let p = plan_run(&facts(false, true, false));
        assert_eq!(p.advisories.len(), 1);
        assert!(p.advisories[0].contains("press y"), "names the grant key");
        assert!(p.advisories[0].contains("Ctrl-y"), "names the resurrect fallback");
        assert!(!p.advisories[0].contains("center"), "no stale center-float promise");
    }

    #[test]
    fn plan_run_advises_producer_when_none_wired() {
        let p = plan_run(&facts(true, false, false)); // granted, no producer
        assert_eq!(p.advisories.len(), 1);
        assert!(p.advisories[0].contains("zj-radar setup"));
    }

    #[test]
    fn plan_run_silent_when_granted_and_wired() {
        assert!(plan_run(&facts(true, false, true)).advisories.is_empty()); // granted + claude
        assert!(plan_run(&facts(true, true, false)).advisories.is_empty()); // granted + codex
    }

    #[test]
    fn plan_run_advises_both_when_nothing_set_up() {
        let p = plan_run(&facts(false, false, false));
        assert_eq!(p.advisories.len(), 2);
        assert!(p.advisories[0].contains("press y"), "grant hint comes first");
        assert!(p.advisories[1].contains("zj-radar setup"));
    }
}
