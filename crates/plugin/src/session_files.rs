//! Session-scoped filesystem coordination for the per-tab sidebar instances.
//!
//! `PluginRuntime` stays pure: it receives an opaque snapshot string and a
//! `PermissionProbe`, then emits effects when state should be persisted. This
//! module owns the filesystem implementation behind those facts and effects.
//! It uses Zellij's plugin-url-scoped `/cache` mount when available, falls back
//! to `/tmp/zj-radar`, and degrades to disabled persistence if neither root is
//! writable. In disabled mode the plugin still runs; late-spawned sidebars just
//! start empty until the next broadcast, and first-run permission prompts cannot
//! be coordinated across tab instances.

use crate::permission::{PermissionMarker, PermissionProbe};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
const CACHE_ROOT: &str = "/cache";
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
const TMP_ROOT: &str = "/tmp/zj-radar";
const SNAPSHOT_PREFIX: &str = "zj-radar.";
const SNAPSHOT_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// How long the first-run permission lock is trusted. The lock prevents every
/// peer sidebar from prompting at once, but its owner can die with the prompt
/// unanswered (the user closes the pane). After this, the next instance assumes
/// the owner is gone and reclaims the lock rather than waiting forever. Generous
/// so a user slowly answering a live prompt is never preempted.
const PERMISSION_LOCK_TTL: Duration = Duration::from_secs(120);
const PERMISSION_GRANTED_MARKER: &str = "granted";
const PERMISSION_DENIED_MARKER: &str = "denied";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SessionFileIds {
    pub plugin_id: u32,
    pub zellij_pid: u32,
}

#[derive(Debug)]
pub(crate) struct SessionFilesOpen {
    pub files: SessionFiles,
    pub snapshot: Option<String>,
    pub permission: PermissionProbe,
}

#[derive(Debug, Default)]
pub(crate) struct SessionFiles {
    paths: Option<SessionPaths>,
}

#[derive(Debug)]
struct SessionPaths {
    root: PathBuf,
    session_prefix: String,
    snapshot: PathBuf,
    snapshot_tmp: PathBuf,
    permission_marker: PathBuf,
    permission_marker_tmp: PathBuf,
    permission_lock: PathBuf,
}

impl SessionFiles {
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub(crate) fn open(ids: SessionFileIds) -> SessionFilesOpen {
        Self::open_with_roots_at(
            ids,
            [PathBuf::from(CACHE_ROOT), PathBuf::from(TMP_ROOT)],
            SystemTime::now(),
            SNAPSHOT_MAX_AGE,
        )
    }

    fn open_with_roots_at<I>(
        ids: SessionFileIds,
        roots: I,
        now: SystemTime,
        max_age: Duration,
    ) -> SessionFilesOpen
    where
        I: IntoIterator<Item = PathBuf>,
    {
        for root in roots {
            let paths = SessionPaths::new(root, ids);
            if !root_is_writable(&paths.root, ids) {
                continue;
            }
            prune_stale_files(&paths, now, max_age);
            let snapshot = std::fs::read_to_string(&paths.snapshot).ok();
            let files = SessionFiles { paths: Some(paths) };
            let permission = files.permission_probe(now);
            return SessionFilesOpen {
                files,
                snapshot,
                permission,
            };
        }

        SessionFilesOpen {
            files: SessionFiles::default(),
            snapshot: None,
            permission: PermissionProbe {
                marker: None,
                lock_acquired: true,
            },
        }
    }

    pub(crate) fn permission_marker(&self) -> Option<PermissionMarker> {
        let paths = self.paths.as_ref()?;
        let raw = std::fs::read_to_string(&paths.permission_marker).ok()?;
        marker_from_str(raw.trim())
    }

    pub(crate) fn snapshot(&self) -> Option<String> {
        let paths = self.paths.as_ref()?;
        std::fs::read_to_string(&paths.snapshot).ok()
    }

    pub(crate) fn persist_permission_marker(&self, marker: PermissionMarker) {
        let Some(paths) = &self.paths else {
            return;
        };
        let raw = match marker {
            PermissionMarker::Granted => PERMISSION_GRANTED_MARKER,
            PermissionMarker::Denied => PERMISSION_DENIED_MARKER,
        };
        if std::fs::write(&paths.permission_marker_tmp, raw.as_bytes()).is_ok() {
            if std::fs::rename(&paths.permission_marker_tmp, &paths.permission_marker).is_err() {
                let _ = std::fs::remove_file(&paths.permission_marker_tmp);
            }
        } else {
            let _ = std::fs::remove_file(&paths.permission_marker_tmp);
        }
    }

    pub(crate) fn persist_snapshot(&self, json: &str) {
        let Some(paths) = &self.paths else {
            return;
        };
        if std::fs::write(&paths.snapshot_tmp, json.as_bytes()).is_ok() {
            if std::fs::rename(&paths.snapshot_tmp, &paths.snapshot).is_err() {
                let _ = std::fs::remove_file(&paths.snapshot_tmp);
            }
        } else {
            let _ = std::fs::remove_file(&paths.snapshot_tmp);
        }
    }

    /// Re-probe the permission state for a timer tick: re-read the marker and,
    /// if still unmarked, re-attempt lock ownership (reclaiming a now-stale
    /// lock). This lets a waiting peer take over a prompt whose owner has gone,
    /// not just newly-opened instances.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub(crate) fn refresh_permission_probe(&self) -> PermissionProbe {
        self.permission_probe(SystemTime::now())
    }

    fn permission_probe(&self, now: SystemTime) -> PermissionProbe {
        let marker = self.permission_marker();
        let lock_acquired = marker.is_none() && self.become_permission_request_owner(now);
        PermissionProbe {
            marker,
            lock_acquired,
        }
    }

    fn become_permission_request_owner(&self, now: SystemTime) -> bool {
        let Some(paths) = &self.paths else {
            return true;
        };
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&paths.permission_lock)
        {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                reclaim_if_stale(&paths.permission_lock, now)
            }
            // If coordination itself fails, prefer one reachable prompt over a
            // session where every instance waits forever.
            Err(_) => true,
        }
    }
}

/// A peer found the permission lock already held. If it has outlived
/// `PERMISSION_LOCK_TTL` its owner likely died with the prompt unanswered, so
/// remove it and try to take it. Best-effort: if another peer wins the recreate
/// race we defer to it (returns false) — consistent with preferring at least one
/// reachable prompt over a deadlock.
fn reclaim_if_stale(lock: &Path, now: SystemTime) -> bool {
    let stale = std::fs::metadata(lock)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|modified| now.duration_since(modified).ok())
        .is_some_and(|age| age > PERMISSION_LOCK_TTL);
    if !stale {
        return false;
    }
    let _ = std::fs::remove_file(lock);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(lock)
        .is_ok()
}

impl SessionPaths {
    fn new(root: PathBuf, ids: SessionFileIds) -> Self {
        let session_prefix = format!("{SNAPSHOT_PREFIX}{}", ids.zellij_pid);
        let snapshot = root.join(format!("{session_prefix}.json"));
        let snapshot_tmp = root.join(format!("{session_prefix}.json.{}.tmp", ids.plugin_id));
        let permission_marker = root.join(format!("{session_prefix}.permissions"));
        let permission_marker_tmp = root.join(format!(
            "{session_prefix}.permissions.{}.tmp",
            ids.plugin_id
        ));
        let permission_lock = root.join(format!("{session_prefix}.permissions.lock"));
        Self {
            root,
            session_prefix,
            snapshot,
            snapshot_tmp,
            permission_marker,
            permission_marker_tmp,
            permission_lock,
        }
    }

    fn is_current_session_file(&self, name: &str) -> bool {
        name == format!("{}.json", self.session_prefix)
            || name == format!("{}.permissions", self.session_prefix)
            || name == format!("{}.permissions.lock", self.session_prefix)
            || (name.starts_with(&format!("{}.json.", self.session_prefix))
                && name.ends_with(".tmp"))
            || (name.starts_with(&format!("{}.permissions.", self.session_prefix))
                && name.ends_with(".tmp"))
    }
}

fn root_is_writable(root: &Path, ids: SessionFileIds) -> bool {
    if std::fs::create_dir_all(root).is_err() {
        return false;
    }
    let probe = root.join(format!(
        ".zj-radar.probe.{}.{}",
        ids.zellij_pid, ids.plugin_id
    ));
    if std::fs::write(&probe, b"").is_err() {
        return false;
    }
    let _ = std::fs::remove_file(probe);
    true
}

fn marker_from_str(raw: &str) -> Option<PermissionMarker> {
    match raw {
        PERMISSION_GRANTED_MARKER => Some(PermissionMarker::Granted),
        PERMISSION_DENIED_MARKER => Some(PermissionMarker::Denied),
        _ => None,
    }
}

fn prune_stale_files(paths: &SessionPaths, now: SystemTime, max_age: Duration) {
    let Ok(entries) = std::fs::read_dir(&paths.root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !is_owned_session_file(&name) || paths.is_current_session_file(&name) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > max_age);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn is_owned_session_file(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(SNAPSHOT_PREFIX) else {
        return false;
    };
    let Some((pid, suffix)) = rest.split_once('.') else {
        return false;
    };
    if !is_digits(pid) {
        return false;
    }

    if matches!(suffix, "json" | "permissions" | "permissions.lock") {
        return true;
    }
    suffix
        .strip_prefix("json.")
        .and_then(|rest| rest.strip_suffix(".tmp"))
        .is_some_and(is_digits)
        || suffix
            .strip_prefix("permissions.")
            .and_then(|rest| rest.strip_suffix(".tmp"))
            .is_some_and(is_digits)
}

fn is_digits(raw: &str) -> bool {
    !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(name: &str) -> Self {
            let n = NEXT_DIR.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir().join(format!(
                "zj-radar-session-files-{name}-{}-{n}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }

        fn join(&self, child: &str) -> PathBuf {
            self.path.join(child)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn ids(plugin_id: u32, zellij_pid: u32) -> SessionFileIds {
        SessionFileIds {
            plugin_id,
            zellij_pid,
        }
    }

    fn open(root: &Path, ids: SessionFileIds) -> SessionFilesOpen {
        SessionFiles::open_with_roots_at(
            ids,
            [root.to_path_buf()],
            SystemTime::now(),
            SNAPSHOT_MAX_AGE,
        )
    }

    #[test]
    fn open_reads_snapshot_as_opaque_text_and_acquires_first_lock() {
        let dir = TempDir::new("open");
        std::fs::write(dir.join("zj-radar.42.json"), b"not json").unwrap();

        let opened = open(dir.path(), ids(7, 42));

        assert_eq!(opened.snapshot.as_deref(), Some("not json"));
        assert_eq!(opened.permission.marker, None);
        assert!(opened.permission.lock_acquired);
        assert!(dir.join("zj-radar.42.permissions.lock").exists());
    }

    #[test]
    fn peer_without_marker_waits_on_existing_lock() {
        let dir = TempDir::new("lock");

        let owner = open(dir.path(), ids(1, 42));
        let peer = open(dir.path(), ids(2, 42));

        assert!(owner.permission.lock_acquired);
        assert_eq!(peer.permission.marker, None);
        assert!(!peer.permission.lock_acquired);
    }

    #[test]
    fn stale_permission_lock_is_reclaimed() {
        let dir = TempDir::new("stale-lock");
        let now = SystemTime::now();
        let root = || [dir.path().to_path_buf()];

        // Owner takes the lock; a peer arriving while it's fresh must wait.
        let owner = SessionFiles::open_with_roots_at(ids(1, 42), root(), now, SNAPSHOT_MAX_AGE);
        assert!(owner.permission.lock_acquired);
        let fresh_peer = SessionFiles::open_with_roots_at(ids(2, 42), root(), now, SNAPSHOT_MAX_AGE);
        assert!(
            !fresh_peer.permission.lock_acquired,
            "a fresh lock must still make peers wait"
        );

        // Once the lock outlives the TTL (owner presumed gone with the prompt
        // unanswered) the next instance reclaims it instead of waiting forever.
        let later = now + PERMISSION_LOCK_TTL + Duration::from_secs(60);
        let reclaimer = SessionFiles::open_with_roots_at(ids(3, 42), root(), later, SNAPSHOT_MAX_AGE);
        assert!(
            reclaimer.permission.lock_acquired,
            "a stale lock must be reclaimed so peers aren't stranded forever"
        );
    }

    #[test]
    fn marker_short_circuits_lock_election() {
        let dir = TempDir::new("marker");
        let owner = open(dir.path(), ids(1, 42));
        owner
            .files
            .persist_permission_marker(PermissionMarker::Granted);

        let peer = open(dir.path(), ids(2, 42));

        assert_eq!(peer.permission.marker, Some(PermissionMarker::Granted));
        assert!(!peer.permission.lock_acquired);
        assert_eq!(
            peer.files.permission_marker(),
            Some(PermissionMarker::Granted)
        );
        assert!(!dir.join("zj-radar.42.permissions.1.tmp").exists());
    }

    #[test]
    fn invalid_marker_is_treated_as_missing() {
        let dir = TempDir::new("invalid-marker");
        std::fs::write(dir.join("zj-radar.42.permissions"), b"maybe").unwrap();

        let opened = open(dir.path(), ids(1, 42));

        assert_eq!(opened.permission.marker, None);
        assert!(opened.permission.lock_acquired);
    }

    #[test]
    fn failed_marker_temp_write_keeps_existing_marker() {
        let dir = TempDir::new("marker-failure");
        std::fs::write(dir.join("zj-radar.42.permissions"), b"granted").unwrap();
        std::fs::create_dir(dir.join("zj-radar.42.permissions.9.tmp")).unwrap();
        let opened = open(dir.path(), ids(9, 42));

        opened
            .files
            .persist_permission_marker(PermissionMarker::Denied);

        assert_eq!(
            std::fs::read_to_string(dir.join("zj-radar.42.permissions")).unwrap(),
            "granted"
        );
        assert_eq!(
            opened.files.permission_marker(),
            Some(PermissionMarker::Granted)
        );
    }

    #[test]
    fn persist_snapshot_writes_through_instance_tmp_then_rename() {
        let dir = TempDir::new("snapshot");
        let opened = open(dir.path(), ids(9, 42));

        opened.files.persist_snapshot(r#"{"v":1}"#);

        assert_eq!(
            std::fs::read_to_string(dir.join("zj-radar.42.json")).unwrap(),
            r#"{"v":1}"#
        );
        assert_eq!(opened.files.snapshot().as_deref(), Some(r#"{"v":1}"#));
        assert!(!dir.join("zj-radar.42.json.9.tmp").exists());
    }

    #[test]
    fn failed_snapshot_temp_write_keeps_existing_snapshot() {
        let dir = TempDir::new("snapshot-failure");
        std::fs::write(dir.join("zj-radar.42.json"), "old").unwrap();
        std::fs::create_dir(dir.join("zj-radar.42.json.9.tmp")).unwrap();
        let opened = open(dir.path(), ids(9, 42));

        opened.files.persist_snapshot("new");

        assert_eq!(
            std::fs::read_to_string(dir.join("zj-radar.42.json")).unwrap(),
            "old"
        );
    }

    #[test]
    fn root_selection_falls_back_to_next_writable_root() {
        let dir = TempDir::new("fallback");
        let broken = dir.join("cache-as-file");
        let fallback = dir.join("tmp-root");
        std::fs::write(&broken, b"not a dir").unwrap();

        let opened = SessionFiles::open_with_roots_at(
            ids(3, 42),
            [broken.clone(), fallback.clone()],
            SystemTime::now(),
            SNAPSHOT_MAX_AGE,
        );
        opened.files.persist_snapshot("seed");

        assert!(!broken.join("zj-radar.42.json").exists());
        assert_eq!(
            std::fs::read_to_string(fallback.join("zj-radar.42.json")).unwrap(),
            "seed"
        );
    }

    #[test]
    fn disabled_mode_is_nonfatal_and_ignores_writes() {
        let dir = TempDir::new("disabled");
        let broken_a = dir.join("cache-as-file");
        let broken_b = dir.join("tmp-as-file");
        std::fs::write(&broken_a, b"not a dir").unwrap();
        std::fs::write(&broken_b, b"not a dir").unwrap();

        let opened = SessionFiles::open_with_roots_at(
            ids(3, 42),
            [broken_a.clone(), broken_b.clone()],
            SystemTime::now(),
            SNAPSHOT_MAX_AGE,
        );
        opened.files.persist_snapshot("ignored");
        opened
            .files
            .persist_permission_marker(PermissionMarker::Denied);

        assert_eq!(opened.snapshot, None);
        assert_eq!(opened.permission.marker, None);
        assert!(opened.permission.lock_acquired);
        assert_eq!(opened.files.permission_marker(), None);
    }

    #[test]
    fn stale_pruning_removes_old_session_snapshot_marker_and_lock() {
        let dir = TempDir::new("prune");
        for name in [
            "zj-radar.1.json",
            "zj-radar.1.permissions",
            "zj-radar.1.permissions.lock",
        ] {
            std::fs::write(dir.join(name), b"old").unwrap();
        }
        for name in [
            "zj-radar.2.json",
            "zj-radar.2.permissions",
            "zj-radar.2.permissions.lock",
        ] {
            std::fs::write(dir.join(name), b"current").unwrap();
        }

        let now = SystemTime::now() + SNAPSHOT_MAX_AGE + Duration::from_secs(1);
        let _ = SessionFiles::open_with_roots_at(
            ids(3, 2),
            [dir.path().to_path_buf()],
            now,
            SNAPSHOT_MAX_AGE,
        );

        assert!(!dir.join("zj-radar.1.json").exists());
        assert!(!dir.join("zj-radar.1.permissions").exists());
        assert!(!dir.join("zj-radar.1.permissions.lock").exists());
        assert!(dir.join("zj-radar.2.json").exists());
        assert!(dir.join("zj-radar.2.permissions").exists());
        assert!(dir.join("zj-radar.2.permissions.lock").exists());
    }

    #[test]
    fn stale_pruning_does_not_keep_numeric_prefix_collisions() {
        let dir = TempDir::new("prune-prefix");
        for name in [
            "zj-radar.20.json",
            "zj-radar.20.json.8.tmp",
            "zj-radar.20.permissions",
            "zj-radar.20.permissions.8.tmp",
            "zj-radar.20.permissions.lock",
        ] {
            std::fs::write(dir.join(name), b"old").unwrap();
        }
        for name in [
            "zj-radar.2.json",
            "zj-radar.2.json.3.tmp",
            "zj-radar.2.permissions",
            "zj-radar.2.permissions.3.tmp",
            "zj-radar.2.permissions.lock",
        ] {
            std::fs::write(dir.join(name), b"current").unwrap();
        }

        let now = SystemTime::now() + SNAPSHOT_MAX_AGE + Duration::from_secs(1);
        let _ = SessionFiles::open_with_roots_at(
            ids(3, 2),
            [dir.path().to_path_buf()],
            now,
            SNAPSHOT_MAX_AGE,
        );

        assert!(!dir.join("zj-radar.20.json").exists());
        assert!(!dir.join("zj-radar.20.json.8.tmp").exists());
        assert!(!dir.join("zj-radar.20.permissions").exists());
        assert!(!dir.join("zj-radar.20.permissions.8.tmp").exists());
        assert!(!dir.join("zj-radar.20.permissions.lock").exists());
        assert!(dir.join("zj-radar.2.json").exists());
        assert!(dir.join("zj-radar.2.json.3.tmp").exists());
        assert!(dir.join("zj-radar.2.permissions").exists());
        assert!(dir.join("zj-radar.2.permissions.3.tmp").exists());
        assert!(dir.join("zj-radar.2.permissions.lock").exists());
    }

    #[test]
    fn stale_pruning_ignores_unknown_zj_radar_files() {
        let dir = TempDir::new("prune-unknown");
        for name in [
            "zj-radar.notes",
            "zj-radar.abc.json",
            "zj-radar.1.unknown",
            "zj-radar.1.json.tmp",
            "zj-radar.1.permissions.tmp",
        ] {
            std::fs::write(dir.join(name), b"not ours").unwrap();
        }

        let now = SystemTime::now() + SNAPSHOT_MAX_AGE + Duration::from_secs(1);
        let _ = SessionFiles::open_with_roots_at(
            ids(3, 2),
            [dir.path().to_path_buf()],
            now,
            SNAPSHOT_MAX_AGE,
        );

        assert!(dir.join("zj-radar.notes").exists());
        assert!(dir.join("zj-radar.abc.json").exists());
        assert!(dir.join("zj-radar.1.unknown").exists());
        assert!(dir.join("zj-radar.1.json.tmp").exists());
        assert!(dir.join("zj-radar.1.permissions.tmp").exists());
    }
}
