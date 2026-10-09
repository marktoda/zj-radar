//! `zj-radar state`: a read-only view of every live session's presence file
//! (`zj_radar_core::presence`). Discovery walks Zellij's plugin cache; all
//! grading/dedupe/filtering is pure over [`RawFile`]s so it tests without IO.
//! No plugin query, ever (push-driven rule): freshness is reported as `age_s`.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use zj_radar_core::presence::{Presence, PresenceTab, DEAD_AFTER_SECS, STALE_AFTER_SECS};

const PRESENCE_PREFIX: &str = "zj-radar.presence.";
/// Bound on the plugin-URL path walk (the URL is spelled out as nested dirs).
const MAX_DEPTH: usize = 32;

pub(crate) struct RawFile {
    pub json: String,
    pub age_s: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct SessionView {
    pub name: String,
    pub current: bool,
    pub stale: bool,
    pub age_s: u64,
    pub running: usize,
    pub attention: usize,
    pub attention_tab_position: Option<usize>,
    pub tabs: Vec<PresenceTab>,
    /// The file's format version (0 = pre-v1 writer). Internal: drives the
    /// `--needs-attention` fallback for files without pane `origin`; not
    /// part of the JSON output (the envelope's `v` is a different version).
    #[serde(skip)]
    pub v: u32,
}

/// Every `zj-radar.presence.*.json` under each root's `plugin_cache` dirs.
/// A Zellij cache root holds one dir per plugin URL, spelled as a path under
/// its scheme (`file:/Users/…/zj_radar.wasm/plugin_cache/`), plus per-server
/// UUID dirs and version dirs we skip. A root that is itself a plugin cache
/// (the `/tmp` fallback) is read directly.
pub(crate) fn read_presence_files(roots: &[PathBuf], now: SystemTime) -> Vec<RawFile> {
    let mut dirs = Vec::new();
    for root in roots {
        // A root literally named `zj-radar` is the plugin's `/tmp/zj-radar`
        // fallback, which IS a plugin cache (flat, no URL tree) — read it
        // directly. A Zellij cache root is never named that, so the basename
        // is a safe discriminator for the roots `default_roots` produces.
        if root.file_name().is_some_and(|n| n == "zj-radar") {
            dirs.push(root.clone());
            continue;
        }
        let Ok(entries) = std::fs::read_dir(root) else { continue };
        for entry in entries.flatten() {
            let is_scheme = entry.file_name().to_string_lossy().ends_with(':');
            if is_scheme && entry.file_type().is_ok_and(|t| t.is_dir()) {
                find_plugin_caches(&entry.path(), 0, &mut dirs);
            }
        }
    }
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !(name.starts_with(PRESENCE_PREFIX) && name.ends_with(".json")) {
                continue;
            }
            let Ok(json) = std::fs::read_to_string(entry.path()) else { continue };
            let age_s = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .map_or(0, |d| d.as_secs()); // future mtime → 0
            out.push(RawFile { json, age_s });
        }
    }
    out
}

fn find_plugin_caches(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        // `file_type` doesn't follow symlinks: no cycles.
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        if entry.file_name() == "plugin_cache" {
            out.push(entry.path());
        } else {
            find_plugin_caches(&entry.path(), depth + 1, out);
        }
    }
}

/// Parse, drop dead/garbage, dedupe by name (greatest `updated_epoch_s`,
/// fresher mtime on a tie — the rail's rule), flag stale, sort current-first.
pub(crate) fn collect(files: Vec<RawFile>, current: Option<&str>) -> Vec<SessionView> {
    let mut best: std::collections::BTreeMap<String, (Presence, u64)> = Default::default();
    for f in files {
        if f.age_s > DEAD_AFTER_SECS {
            continue;
        }
        let Some(p) = Presence::parse(&f.json) else { continue };
        let newer = best.get(&p.session_name).is_none_or(|(kept, age)| {
            (p.updated_epoch_s, std::cmp::Reverse(f.age_s)) > (kept.updated_epoch_s, std::cmp::Reverse(*age))
        });
        if newer {
            best.insert(p.session_name.clone(), (p, f.age_s));
        }
    }
    let mut out: Vec<SessionView> = best
        .into_values()
        .map(|(p, age_s)| SessionView {
            current: current == Some(p.session_name.as_str()),
            stale: age_s > STALE_AFTER_SECS,
            age_s,
            running: p.running,
            attention: p.attention,
            attention_tab_position: p.attention_tab_position,
            tabs: p.tabs,
            v: p.v,
            name: p.session_name,
        })
        .collect();
    out.sort_by(|a, b| b.current.cmp(&a.current).then_with(|| a.name.cmp(&b.name)));
    out
}

/// Where to look: `ZJ_RADAR_CACHE_DIR` (a Zellij cache root) if set, else the
/// platform's; plus the plugin's `/tmp` fallback root(s).
pub(crate) fn default_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    match std::env::var_os("ZJ_RADAR_CACHE_DIR").filter(|d| !d.is_empty()) {
        Some(dir) => roots.push(PathBuf::from(dir)),
        None => {
            if let Some(cache) = dirs::cache_dir() {
                roots.push(crate::run::zellij_cache_root_in(&cache, cfg!(target_os = "macos")));
            }
            roots.push(PathBuf::from("/tmp/zj-radar"));
        }
    }
    roots
}

pub(crate) struct StateOptions {
    pub json: bool,
    pub needs_attention: bool,
    pub include_stale: bool,
    pub session: Option<String>,
}

/// Apply `--session` and `--needs-attention`. The attention rule is exactly
/// the presence `attention` count's: status-origin panes that need you
/// (pending | error). Stale sessions' attention isn't actionable (the rail
/// won't cycle to them), so they're skipped unless `--include-stale`.
///
/// Pre-v1 files (`v == 0`) carry no pane `origin`, so their origin-less panes
/// count as status-origin (degraded: a v0 command pane in error matches too),
/// and a v0 session whose `attention` count is non-zero matches even with no
/// matching tree pane (kept with `tabs: []`) so the predicate agrees with
/// the count.
pub(crate) fn filter(sessions: Vec<SessionView>, opts: &StateOptions) -> Vec<SessionView> {
    sessions
        .into_iter()
        .filter(|s| opts.session.as_deref().is_none_or(|n| n == s.name))
        .filter_map(|mut s| {
            if !opts.needs_attention {
                return Some(s);
            }
            if s.stale && !opts.include_stale {
                return None;
            }
            let legacy = s.v == 0;
            for tab in &mut s.tabs {
                tab.panes.retain(|p| {
                    let agent = match p.origin.as_deref() {
                        Some(origin) => origin == "status",
                        None => legacy,
                    };
                    agent && zj_radar_core::Status::from_wire(&p.status).needs_you()
                });
            }
            s.tabs.retain(|t| !t.panes.is_empty());
            (!s.tabs.is_empty() || (legacy && s.attention > 0)).then_some(s)
        })
        .collect()
}

#[derive(Serialize)]
struct JsonOut<'a> {
    v: u32,
    sessions: &'a [SessionView],
}

pub(crate) fn render_json(sessions: &[SessionView]) -> String {
    serde_json::to_string(&JsonOut { v: 1, sessions }).unwrap_or_default()
}

pub(crate) fn render_human(sessions: &[SessionView]) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    for s in sessions {
        let mut head = s.name.clone();
        if s.current {
            head.push_str(" (current)");
        }
        if s.running > 0 {
            let _ = write!(head, "  ▶{}", s.running);
        }
        if s.attention > 0 {
            let _ = write!(head, "  !{}", s.attention);
        }
        if s.stale {
            let _ = write!(head, "  (stale, {}s)", s.age_s);
        }
        let _ = writeln!(out, "{head}");
        for tab in &s.tabs {
            let _ = writeln!(out, "  tab {} {}", tab.position, tab.name);
            for p in &tab.panes {
                let id = p.pane_id.map_or_else(|| "-".to_string(), |id| id.to_string());
                let _ = writeln!(out, "    {id:>4} {:<8} {:<8} {}", p.kind, p.status, p.label);
            }
        }
    }
    out
}

/// Exit: 0 ok/match, 1 `--needs-attention` matched nothing, 2 error (stdout
/// write failure). An absent cache is an empty answer, not an error.
pub(crate) fn run(opts: StateOptions) -> std::process::ExitCode {
    use std::io::Write;
    let current = std::env::var("ZELLIJ_SESSION_NAME").ok();
    let sessions = filter(collect(read_presence_files(&default_roots(), SystemTime::now()), current.as_deref()), &opts);
    let text = if opts.json { format!("{}\n", render_json(&sessions)) } else { render_human(&sessions) };
    if std::io::stdout().write_all(text.as_bytes()).is_err() {
        return std::process::ExitCode::from(2);
    }
    if opts.needs_attention && sessions.is_empty() {
        std::process::ExitCode::from(1)
    } else {
        std::process::ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn presence(name: &str, updated: u64, panes: &[(&str, &str, &str)]) -> String {
        // panes: (origin, kind, status)
        serde_json::json!({
            "v": 1, "session_name": name, "running": 0, "attention": 0, "updated_epoch_s": updated,
            "tabs": [{"position": 0, "name": "t", "panes": panes.iter().enumerate().map(|(i, (o, k, s))|
                serde_json::json!({"pane_id": i, "origin": o, "kind": k, "status": s, "label": "x"})).collect::<Vec<_>>()}]
        })
        .to_string()
    }
    fn raw(json: String, age_s: u64) -> RawFile {
        RawFile { json, age_s }
    }

    #[test]
    fn dead_and_garbage_are_dropped_stale_is_flagged() {
        let out = collect(
            vec![
                raw(presence("live", 1, &[]), 5),
                raw(presence("old", 1, &[]), 91),
                raw(presence("gone", 1, &[]), 301),
                raw("not json".into(), 0),
            ],
            None,
        );
        let names: Vec<_> = out.iter().map(|s| (s.name.as_str(), s.stale)).collect();
        assert_eq!(names, vec![("live", false), ("old", true)]);
    }

    #[test]
    fn same_name_keeps_the_newest_write() {
        let out = collect(vec![raw(presence("w", 200, &[]), 40), raw(presence("w", 100, &[]), 1)], None);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].age_s, 40, "updated_epoch_s decides, like the rail's dedupe");
    }

    #[test]
    fn current_session_sorts_first() {
        let out = collect(vec![raw(presence("a", 1, &[]), 0), raw(presence("z", 1, &[]), 0)], Some("z"));
        assert_eq!(
            out.iter().map(|s| (s.name.as_str(), s.current)).collect::<Vec<_>>(),
            vec![("z", true), ("a", false)]
        );
    }

    #[test]
    fn walks_only_scheme_dirs_to_plugin_caches() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("file:/Users/x/plugins/zj_radar.wasm/plugin_cache");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("zj-radar.presence.42.json"), presence("w", 1, &[])).unwrap();
        std::fs::write(cache.join("zj-radar.42.json"), "{}").unwrap(); // snapshot, not presence
        std::fs::write(cache.join("zj-radar.presence.42.json.tmp"), "x").unwrap();
        let noise = root.path().join("0b1c-uuid/plugin_cache");
        std::fs::create_dir_all(&noise).unwrap();
        std::fs::write(noise.join("zj-radar.presence.9.json"), presence("noise", 1, &[])).unwrap();

        let files = read_presence_files(&[root.path().to_path_buf()], SystemTime::now());
        assert_eq!(files.len(), 1, "only the file: subtree, only presence json");
        assert!(files[0].json.contains("\"w\""));
    }

    #[test]
    fn future_mtime_clamps_to_zero() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("file:/p/plugin_cache");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("zj-radar.presence.1.json"), presence("w", 1, &[])).unwrap();
        let past = SystemTime::now() - Duration::from_secs(3600);
        assert_eq!(read_presence_files(&[dir.path().to_path_buf()], past)[0].age_s, 0);
    }

    fn opts() -> StateOptions {
        StateOptions { json: false, needs_attention: false, include_stale: false, session: None }
    }

    #[test]
    fn needs_attention_counts_agent_pending_and_error_only() {
        let s = collect(
            vec![raw(
                presence(
                    "w",
                    1,
                    &[("status", "claude", "pending"), ("command", "test", "error"), ("status", "codex", "running")],
                ),
                0,
            )],
            None,
        );
        let out = filter(s, &StateOptions { needs_attention: true, ..opts() });
        let panes = &out[0].tabs[0].panes;
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].kind, "claude");
    }

    #[test]
    fn needs_attention_skips_stale_unless_asked_and_drops_empties() {
        let files = || {
            vec![
                raw(presence("old", 1, &[("status", "claude", "pending")]), 120),
                raw(presence("calm", 1, &[("status", "claude", "running")]), 0),
            ]
        };
        assert!(filter(collect(files(), None), &StateOptions { needs_attention: true, ..opts() }).is_empty());
        let with_stale =
            filter(collect(files(), None), &StateOptions { needs_attention: true, include_stale: true, ..opts() });
        assert_eq!(with_stale.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), vec!["old"]);
    }

    #[test]
    fn needs_attention_matches_a_pre_v1_tree_pane_without_origin() {
        // Pre-v1 writers published no `origin`; treat their panes as
        // status-origin rather than never matching (degraded: a v0 command
        // pane in error matches too — documented in producers.md).
        let v0 = r#"{"session_name":"old","running":0,"attention":1,"tabs":[{"position":0,"name":"t",
            "panes":[{"kind":"claude","status":"pending","label":"x"},{"kind":"claude","status":"running","label":"y"}]}]}"#;
        let out = filter(collect(vec![raw(v0.into(), 0)], None), &StateOptions { needs_attention: true, ..opts() });
        assert_eq!(out.len(), 1, "a v0 pending pane matches");
        assert_eq!(out[0].tabs[0].panes.len(), 1);
        assert_eq!(out[0].tabs[0].panes[0].status, "pending");
    }

    #[test]
    fn needs_attention_keeps_a_pre_v1_count_without_a_tree() {
        // A v0 file may carry only counts: the predicate agrees with the
        // `attention` count, with no pane detail to show.
        let v0 = r#"{"session_name":"old","running":0,"attention":1}"#;
        let calm = r#"{"session_name":"calm","running":1,"attention":0}"#;
        let out = filter(
            collect(vec![raw(v0.into(), 0), raw(calm.into(), 0)], None),
            &StateOptions { needs_attention: true, ..opts() },
        );
        assert_eq!(out.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), vec!["old"]);
        assert!(out[0].tabs.is_empty());
        assert!(!render_json(&out).contains("\"v\":0"), "the file version stays out of the JSON");
    }

    #[test]
    fn v1_panes_without_origin_never_match() {
        let v1 = r#"{"v":1,"session_name":"new","running":0,"attention":0,"tabs":[{"position":0,"name":"t",
            "panes":[{"kind":"claude","status":"pending","label":"x"}]}]}"#;
        let out = filter(collect(vec![raw(v1.into(), 0)], None), &StateOptions { needs_attention: true, ..opts() });
        assert!(out.is_empty(), "the v0 fallback is gated on v == 0");
    }

    #[test]
    fn session_filter_keeps_one() {
        let s = collect(vec![raw(presence("a", 1, &[]), 0), raw(presence("b", 1, &[]), 0)], None);
        let out = filter(s, &StateOptions { session: Some("b".into()), ..opts() });
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn json_and_human_shapes() {
        let s = collect(vec![raw(presence("main", 1, &[("status", "claude", "pending")]), 12)], Some("main"));
        insta::assert_snapshot!(render_json(&s));
        insta::assert_snapshot!(render_human(&s));
    }
}
