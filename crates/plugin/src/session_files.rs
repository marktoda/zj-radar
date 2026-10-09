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
/// Namespace for every file this module owns in the shared root — snapshots,
/// permission markers/locks, notify claims, the presence-writer token (all
/// pid-scoped via `session_prefix`), and presence files (via
/// [`PRESENCE_PREFIX`]).
const SESSION_FILE_PREFIX: &str = "zj-radar.";
const SNAPSHOT_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// How long the first-run permission lock is trusted. The lock prevents every
/// peer sidebar from prompting at once, but its owner can die with the prompt
/// unanswered (the user closes the pane). After this, the next instance assumes
/// the owner is gone and reclaims the lock rather than waiting forever. Generous
/// so a user slowly answering a live prompt is never preempted.
const PERMISSION_LOCK_TTL: Duration = Duration::from_secs(120);
/// A notify claim older than this no longer identifies the same event: the
/// per-tab instances that would duplicate a toast all settle within ~a tick of
/// each other, so a later arrival with the same key is a genuine repeat (e.g.
/// the same question asked again) and may claim anew.
const NOTIFY_CLAIM_TTL: Duration = Duration::from_secs(30);
/// Sweep horizon for spent notify claims — generous multiple of the TTL so a
/// slow peer never watches its evidence get deleted mid-election.
const NOTIFY_CLAIM_SWEEP_AGE: Duration = Duration::from_secs(300);
const PERMISSION_GRANTED_MARKER: &str = "granted";
const PERMISSION_DENIED_MARKER: &str = "denied";
/// Horizon after which a presence file is deleted as debris at `open` time.
/// NOT the primary forgetting: the roster ladder (`docs/design.md`) is
/// fresh (≤90s, `sessions::STALE_AFTER_SECS`) → stale/dimmed (90–300s) →
/// reaped past `sessions::DEAD_AFTER_SECS` (300s), when the runtime's
/// auto-reap unlinks the file (`Effect::DismissPresence`). This sweep is
/// the backstop for debris that reap can't touch — malformed/unparseable
/// files, which never produce a name to dismiss by. Generous: a live
/// session heartbeats its file every 60s, so anything this old is
/// unambiguously abandoned.
const PRESENCE_MAX_AGE: Duration = Duration::from_secs(6 * 60 * 60);
/// Deliberately distinct from the pid-scoped `session_prefix` (`zj-radar.<pid>`) so
/// `is_owned_session_file` / `is_current_session_file` and the snapshot sweep
/// never match a presence file — presence gets its own recognizer and sweep
/// horizon (see `PRESENCE_MAX_AGE`).
const PRESENCE_PREFIX: &str = "zj-radar.presence.";
/// Suffix of the presence-writer token, `zj-radar.<pid>.presence-writer`:
/// the session's one presence *content* writer, by plugin id (decimal).
/// Pid-scoped through `session_prefix` — NOT under [`PRESENCE_PREFIX`] — so
/// no presence read/delete/sweep path ever mistakes it for a peer's
/// presence, and the snapshot sweep owns it (the live session's token is
/// spared, a dead session's is swept with its snapshot). See
/// [`SessionFiles::claim_presence_writer`] for the protocol.
const PRESENCE_WRITER_SUFFIX: &str = "presence-writer";
/// Age below which a presence-writer token naming ANOTHER instance survives
/// a non-preempting (manifest) claim. Why: two clients on different tabs
/// both receive manifests, so plain last-claim-wins turns every alternate
/// manifest into a takeover — a token rewrite plus a full re-entrant
/// republish (`PluginRuntime::presence_writer_acquired`). A token this young
/// was claimed moments ago by a rail that was then receiving manifests
/// itself, so its view is not frozen; letting it keep the token bounds the
/// ping-pong to one takeover per floor. A *preempting* claim (a reveal, the
/// name first learned — a rail that just became the active view) ignores
/// the floor, so a quick tab switch back is never left without the token.
pub(crate) const PRESENCE_WRITER_TAKEOVER_FLOOR: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SessionFileIds {
    pub plugin_id: u32,
    pub zellij_pid: u32,
}

/// One peer's presence file as read off disk — see
/// [`SessionFiles::read_peer_presences`]'s doc for why `age_secs` is
/// measured off the file's mtime rather than trusted from the JSON content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PeerPresenceFile {
    pub json: String,
    pub age_secs: u64,
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
    presence: PathBuf,
    presence_tmp: PathBuf,
    presence_writer: PathBuf,
    presence_writer_tmp: PathBuf,
    /// This instance's id as written into the token (decimal).
    plugin_id: String,
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

    fn open_with_roots_at<I>(ids: SessionFileIds, roots: I, now: SystemTime, max_age: Duration) -> SessionFilesOpen
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
            return SessionFilesOpen { files, snapshot, permission };
        }

        SessionFilesOpen {
            files: SessionFiles::default(),
            snapshot: None,
            permission: PermissionProbe { marker: None, lock_acquired: true },
        }
    }

    /// One instance's files over a single `root` (cross-module tests drive
    /// the lib.rs glue against real token/presence files).
    #[cfg(test)]
    pub(crate) fn open_for_test(root: &Path, plugin_id: u32, zellij_pid: u32) -> SessionFiles {
        Self::open_with_roots_at(
            SessionFileIds { plugin_id, zellij_pid },
            [root.to_path_buf()],
            SystemTime::now(),
            SNAPSHOT_MAX_AGE,
        )
        .files
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
        write_via_tmp(&paths.permission_marker_tmp, &paths.permission_marker, raw.as_bytes());
    }

    /// Refresh the permission lock's mtime (rewriting it, creating if needed).
    /// Called each tick by the instance whose own request is in-flight, so
    /// `reclaim_if_stale` only ever sees a stale lock when the prompt-owner is
    /// actually gone — never while a user is still answering a live prompt.
    pub(crate) fn heartbeat_permission_lock(&self) {
        let Some(paths) = &self.paths else {
            return;
        };
        let _ = std::fs::write(&paths.permission_lock, b"");
    }

    /// Write the shared snapshot (`Effect::PersistSnapshot`): the existing
    /// record, if any, is handed to `json` for the merge (`snapshot::to_json`
    /// keeps entries another instance persisted), and the result lands via
    /// tmp+rename. Disabled mode is a no-op.
    pub(crate) fn persist_snapshot(&self, json: impl FnOnce(Option<&str>) -> String) {
        let Some(paths) = &self.paths else {
            return;
        };
        let existing = self.snapshot();
        write_via_tmp(&paths.snapshot_tmp, &paths.snapshot, json(existing.as_deref()).as_bytes());
    }

    /// Publish this session's presence for peer sessions' badges and `zj-radar
    /// state` (`Effect::PersistPresence`), gated on the presence-writer token
    /// ([`claim_presence_writer`](Self::claim_presence_writer)):
    ///
    /// - **Holder**: `unless_fresher_than: None` (a content edge) always
    ///   writes; `Some(window)` (the liveness heartbeat) skips when the file's
    ///   mtime is younger than `window`.
    /// - **Non-holder**: either kind is only a *rescue* — it writes when the
    ///   file is at least `rescue_after` old (the holder has gone quiet: its
    ///   tab closed or its instance died), and otherwise skips. A non-holder's
    ///   view of other tabs can be frozen (hidden rails get no manifests), so
    ///   it must never overwrite a live holder's content.
    ///
    /// A missing or unreadable file counts as stale. `json` is lazy, so a skip
    /// costs one token read plus one stat. Same tmp+rename discipline as
    /// `persist_snapshot`; disabled mode is a no-op.
    pub(crate) fn persist_presence(
        &self,
        unless_fresher_than: Option<Duration>,
        rescue_after: Duration,
        json: impl FnOnce() -> String,
    ) {
        self.persist_presence_at(unless_fresher_than, rescue_after, json, SystemTime::now())
    }

    fn persist_presence_at(
        &self,
        unless_fresher_than: Option<Duration>,
        rescue_after: Duration,
        json: impl FnOnce() -> String,
        now: SystemTime,
    ) {
        let Some(paths) = &self.paths else {
            return;
        };
        let min_age = if self.holds_presence_writer() { unless_fresher_than } else { Some(rescue_after) };
        if let Some(min_age) = min_age {
            if age_of(std::fs::metadata(&paths.presence), now).is_some_and(|age| age < min_age) {
                return;
            }
        }
        write_via_tmp(&paths.presence_tmp, &paths.presence, json().as_bytes());
    }

    /// Make this instance the session's presence *content* writer
    /// (`Effect::ClaimPresenceWriter`). Returns whether this call took the
    /// token over (it named another instance, or nobody) — the caller then
    /// re-publishes this instance's view (`PluginRuntime::presence_writer_acquired`).
    /// A claim of a token already ours is one small read and no write, so the
    /// runtime may claim on every manifest.
    ///
    /// Protocol: a named rail claims whenever it is the active view — when
    /// its session name is first learned, on `Visible(true)`, and on every
    /// `TabUpdate`/`PaneUpdate` (which Zellij delivers to the active tab's
    /// plugins only). The most recent claimant has the freshest topology; a
    /// hidden rail's tab/pane view is frozen, so last-claim-wins is exactly
    /// "the freshest view writes". After a detach the last-active rail keeps
    /// the token and keeps publishing from its at-detach topology. Plain
    /// last-writer-wins (tmp+rename), no lock — except that a manifest claim
    /// (`preempt: false`) leaves a foreign token younger than
    /// [`PRESENCE_WRITER_TAKEOVER_FLOOR`] alone: two clients on different
    /// tabs are both fresh, so either holding is fine, and the floor stops
    /// them trading it on every manifest. Returns `false` when the token
    /// write failed (nothing was taken over).
    pub(crate) fn claim_presence_writer(&self, preempt: bool) -> bool {
        self.claim_presence_writer_at(preempt, SystemTime::now())
    }

    fn claim_presence_writer_at(&self, preempt: bool, now: SystemTime) -> bool {
        let Some(paths) = &self.paths else {
            return false;
        };
        if self.holds_presence_writer() {
            return false;
        }
        // A missing token (or an mtime hiccup) is no young holder: take it.
        if !preempt
            && age_of(std::fs::metadata(&paths.presence_writer), now)
                .is_some_and(|age| age < PRESENCE_WRITER_TAKEOVER_FLOOR)
        {
            return false;
        }
        write_via_tmp(&paths.presence_writer_tmp, &paths.presence_writer, paths.plugin_id.as_bytes())
    }

    /// Whether the presence-writer token names this instance. Missing or
    /// unreadable → not held (nobody writes content until the next claim;
    /// the rescue heartbeat keeps the file alive meanwhile).
    pub(crate) fn holds_presence_writer(&self) -> bool {
        let Some(paths) = &self.paths else {
            return false;
        };
        std::fs::read_to_string(&paths.presence_writer).is_ok_and(|raw| raw.trim() == paths.plugin_id)
    }

    /// Raw JSON of every OTHER session's presence file (own pid excluded, tmp
    /// files excluded), paired with how long ago its mtime was last touched.
    /// Parsing/validation is the caller's job (`Presence::parse` skips
    /// corrupt peers) — and so is liveness grading: every peer file found
    /// comes back unconditionally, and `sessions::Sessions::update_presences`
    /// grades its age into the fresh (≤90s) → stale/dimmed (90–300s) →
    /// reaped (>300s, `sessions::DEAD_AFTER_SECS`) ladder without doing its
    /// own filesystem I/O. `age_secs` is measured off THIS session's own
    /// filesystem clock reading the file's mtime — not the peer's
    /// self-reported `updated_epoch_s` inside the JSON, which a corrupt or
    /// merely-slow-to-update peer could get wrong. Read-only regardless:
    /// nothing is ever deleted here (the reap's `Effect::DismissPresence`
    /// and the open-time `PRESENCE_MAX_AGE` sweep own the forgetting). Read
    /// on timer ticks only — bounded by live session count, never per-pane.
    pub(crate) fn read_peer_presences(&self) -> Vec<PeerPresenceFile> {
        self.read_peer_presences_at(SystemTime::now())
    }

    fn read_peer_presences_at(&self, now: SystemTime) -> Vec<PeerPresenceFile> {
        let Some(paths) = &self.paths else {
            return Vec::new();
        };
        let mut out = Vec::new();
        // The own-file skip is the pre-read predicate: no point paying the
        // open+read for a file whose content is discarded by name.
        for_each_presence_file(
            &paths.root,
            |name| !paths.is_own_presence_file(name),
            |entry, json| {
                let age_secs = age_of(entry.metadata(), now).map(|age| age.as_secs()).unwrap_or(0); // metadata/clock hiccup: treat as fresh rather than drop the peer
                out.push(PeerPresenceFile { json, age_secs });
            },
        );
        // Sorted by content, so `sessions::update_presences`'s later-entry-
        // wins dedup tie-break is deterministic across reads. (Unstable is
        // fine: equal keys are byte-identical files.)
        out.sort_unstable_by(|a, b| a.json.cmp(&b.json));
        out
    }

    /// Delete every presence file in the shared root whose raw JSON content
    /// satisfies `matches` — the on-disk half of a dismiss
    /// (`Effect::DismissPresence`, manual or auto-reap). Same file
    /// recognizer as `read_peer_presences` (prefix + `.json`, so tmp files
    /// and snapshots never match) and the same own-file exclusion, applied
    /// at the same pre-read seam: this session's own live file is spared by
    /// *path* identity, so the delete primitive itself can never unlink it
    /// — but its NAME is still matched like any other content, so a dead
    /// pid-keyed corpse carrying our own name (a restarted session) is
    /// reaped here like any other corpse. Every match is deleted, not just
    /// the freshest: a name can have multiple pid-keyed corpses on disk
    /// (see `update_presences`'s dedup doc in `sessions.rs`). One edge two
    /// servers sharing a presence root can hit: a name-matched delete could
    /// unlink a same-named LIVE session under another server — covered by
    /// the non-destructive doctrine (`sessions::Sessions::dismiss`): its
    /// next heartbeat republishes the file and it reappears fresh.
    /// Parse-agnostic on purpose — the predicate receives raw file content,
    /// and the caller (lib.rs's effect handler) supplies a
    /// `Presence::parse`-based closure, so name matching stays exactly as
    /// lenient as every other presence read path without this module
    /// learning about `Presence` at all. A no-op in disabled mode (no
    /// writable root).
    pub(crate) fn remove_presences_matching(&self, matches: impl Fn(&str) -> bool) {
        let Some(paths) = &self.paths else {
            return;
        };
        for_each_presence_file(
            &paths.root,
            |name| !paths.is_own_presence_file(name),
            |entry, json| {
                if matches(&json) {
                    let _ = std::fs::remove_file(entry.path());
                }
            },
        );
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
        PermissionProbe { marker, lock_acquired }
    }

    fn become_permission_request_owner(&self, now: SystemTime) -> bool {
        let Some(paths) = &self.paths else {
            return true;
        };
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&paths.permission_lock) {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                reclaim_if_stale(&paths.permission_lock, now, PERMISSION_LOCK_TTL)
            }
            // If coordination itself fails, prefer one reachable prompt over a
            // session where every instance waits forever.
            Err(_) => true,
        }
    }

    /// Elect this instance to dispatch the notification identified by `key`
    /// (`notify_rules::claim_key`). Every per-tab instance computes the same
    /// edge from the same shared signals and calls this with the same key; the
    /// first to atomically create the claim file dispatches, the rest skip —
    /// one toast per event instead of one per visited tab. A claim past
    /// `NOTIFY_CLAIM_TTL` is a *previous* event that happens to share the key
    /// (same pane, status, and text), so it is reclaimed and fires again.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub(crate) fn claim_notification(&self, key: &str) -> bool {
        self.claim_notification_at(key, SystemTime::now())
    }

    fn claim_notification_at(&self, key: &str, now: SystemTime) -> bool {
        let Some(paths) = &self.paths else {
            // No writable root → no peers to coordinate with either (they
            // could not have opened it); dispatch rather than go silent.
            return true;
        };
        prune_stale_claims(paths, now);
        let claim = paths.notify_claim(key);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&claim) {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => reclaim_if_stale(&claim, now, NOTIFY_CLAIM_TTL),
            // Coordination failed: prefer a duplicate toast over a missed
            // "needs input" — same trade the permission election makes.
            Err(_) => true,
        }
    }
}

/// The tmp-write→rename discipline every persisted file in this module uses:
/// write the instance-scoped `tmp` file, then atomically rename it over
/// `dest`. On any failure the tmp is removed and the existing `dest` is left
/// untouched; errors are otherwise swallowed (persistence is best-effort —
/// see the module doc's disabled-mode story).
/// Returns whether `dest` now holds `bytes` (callers that only care about
/// best effort ignore it).
fn write_via_tmp(tmp: &Path, dest: &Path, bytes: &[u8]) -> bool {
    if std::fs::write(tmp, bytes).is_ok() && std::fs::rename(tmp, dest).is_ok() {
        return true;
    }
    let _ = std::fs::remove_file(tmp);
    false
}

/// THE presence-file recognizer every read/delete path shares: prefix plus
/// the `.json` extension, so tmp files and snapshots never match. (The
/// open-time sweep, `prune_stale_files`, deliberately does NOT use it — see
/// the prefix-only match there.)
fn is_peer_presence_file(name: &str) -> bool {
    name.starts_with(PRESENCE_PREFIX) && name.ends_with(".json")
}

/// Visit every presence file in `root` ([`is_peer_presence_file`]) whose file
/// name passes `keep`, handing each entry with its raw JSON content. `keep`
/// runs BEFORE the read, so a name-based exclusion (the peer read's own-file
/// skip) costs nothing — filtering it in the callback instead would pay a
/// wasted wasi open+read per scan. Unreadable files are skipped; content
/// filtering stays the callback's job.
fn for_each_presence_file(root: &Path, keep: impl Fn(&str) -> bool, mut f: impl FnMut(&std::fs::DirEntry, String)) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !is_peer_presence_file(&name) || !keep(&name) {
            continue;
        }
        if let Ok(json) = std::fs::read_to_string(entry.path()) {
            f(&entry, json);
        }
    }
}

/// How long ago (relative to `now`) the file behind `meta` was last modified.
/// `None` on any metadata/clock hiccup — including an mtime in the future —
/// so each caller decides its own conservative fallback (treat as fresh,
/// don't reclaim, don't prune).
fn age_of(meta: std::io::Result<std::fs::Metadata>, now: SystemTime) -> Option<Duration> {
    meta.and_then(|m| m.modified()).ok().and_then(|modified| now.duration_since(modified).ok())
}

/// Remove spent notify claims so a long-lived session doesn't accrete one file
/// per notification event forever. Runs on each claim attempt — notifications
/// are edge-rate (not output-rate), so the readdir is negligible.
fn prune_stale_claims(paths: &SessionPaths, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(&paths.root) else {
        return;
    };
    let prefix = format!("{}.notify.", paths.session_prefix);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(&prefix) {
            continue;
        }
        let stale = age_of(entry.metadata(), now).is_some_and(|age| age > NOTIFY_CLAIM_SWEEP_AGE);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// A peer found the lock already held. If it has outlived `ttl` its owner is
/// no longer relevant (a dead prompt owner, or a notify claim from a previous
/// event), so remove it and try to take it. Best-effort: if another peer wins
/// the recreate race we defer to it (returns false) — consistent with
/// preferring at least one reachable prompt/toast over a deadlock.
fn reclaim_if_stale(lock: &Path, now: SystemTime, ttl: Duration) -> bool {
    let stale = age_of(std::fs::metadata(lock), now).is_some_and(|age| age > ttl);
    if !stale {
        return false;
    }
    let _ = std::fs::remove_file(lock);
    std::fs::OpenOptions::new().write(true).create_new(true).open(lock).is_ok()
}

impl SessionPaths {
    fn new(root: PathBuf, ids: SessionFileIds) -> Self {
        let session_prefix = format!("{SESSION_FILE_PREFIX}{}", ids.zellij_pid);
        let snapshot = root.join(format!("{session_prefix}.json"));
        let snapshot_tmp = root.join(format!("{session_prefix}.json.{}.tmp", ids.plugin_id));
        let permission_marker = root.join(format!("{session_prefix}.permissions"));
        let permission_marker_tmp = root.join(format!("{session_prefix}.permissions.{}.tmp", ids.plugin_id));
        let permission_lock = root.join(format!("{session_prefix}.permissions.lock"));
        let presence = root.join(format!("{PRESENCE_PREFIX}{}.json", ids.zellij_pid));
        let presence_tmp = root.join(format!("{PRESENCE_PREFIX}{}.json.{}.tmp", ids.zellij_pid, ids.plugin_id));
        let presence_writer = root.join(format!("{session_prefix}.{PRESENCE_WRITER_SUFFIX}"));
        let presence_writer_tmp = root.join(format!("{session_prefix}.{PRESENCE_WRITER_SUFFIX}.{}.tmp", ids.plugin_id));
        Self {
            root,
            session_prefix,
            snapshot,
            snapshot_tmp,
            permission_marker,
            permission_marker_tmp,
            permission_lock,
            presence,
            presence_tmp,
            presence_writer,
            presence_writer_tmp,
            plugin_id: ids.plugin_id.to_string(),
        }
    }

    fn notify_claim(&self, key: &str) -> PathBuf {
        self.root.join(format!("{}.notify.{key}", self.session_prefix))
    }

    fn is_current_session_file(&self, name: &str) -> bool {
        name == format!("{}.json", self.session_prefix)
            || name == format!("{}.permissions", self.session_prefix)
            || name == format!("{}.permissions.lock", self.session_prefix)
            || (name.starts_with(&format!("{}.json.", self.session_prefix)) && name.ends_with(".tmp"))
            || (name.starts_with(&format!("{}.permissions.", self.session_prefix)) && name.ends_with(".tmp"))
            || name.starts_with(&format!("{}.notify.", self.session_prefix))
            || name.starts_with(&format!("{}.{PRESENCE_WRITER_SUFFIX}", self.session_prefix))
    }

    fn is_own_presence_file(&self, name: &str) -> bool {
        self.presence.file_name().is_some_and(|n| n.to_string_lossy() == name)
            || self.presence_tmp.file_name().is_some_and(|n| n.to_string_lossy() == name)
    }
}

fn root_is_writable(root: &Path, ids: SessionFileIds) -> bool {
    if std::fs::create_dir_all(root).is_err() {
        return false;
    }
    let probe = root.join(format!(".zj-radar.probe.{}.{}", ids.zellij_pid, ids.plugin_id));
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
        // Deliberately looser than `is_peer_presence_file`: prefix-only, no
        // `.json` requirement, so presence *tmp debris* (an orphaned
        // `...json.<plugin>.tmp` from a crashed rename) takes this branch and
        // is swept on the shorter 6h horizon — it matches nothing below
        // (`is_owned_session_file` requires a numeric pid) and would
        // otherwise accumulate forever.
        if name.starts_with(PRESENCE_PREFIX) {
            if paths.is_own_presence_file(&name) {
                continue;
            }
            let stale = age_of(entry.metadata(), now).is_some_and(|age| age > PRESENCE_MAX_AGE);
            if stale {
                let _ = std::fs::remove_file(entry.path());
            }
            continue;
        }
        if !is_owned_session_file(&name) || paths.is_current_session_file(&name) {
            continue;
        }
        let stale = age_of(liveness_metadata(&paths.root, &name, &entry), now).is_some_and(|age| age > max_age);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The metadata whose mtime says how long ago the owner of `name` was last
/// alive. Usually the file's own, but a presence-writer token is written
/// only on a takeover — a stable holder never touches it — so a session
/// older than the sweep horizon would lose its live token to ANY session's
/// plugin load. Its session's presence file (`zj-radar.presence.<pid>.json`,
/// heartbeated about every 60 s) is the liveness signal instead; the
/// token's own mtime is the fallback only when that file is missing.
fn liveness_metadata(root: &Path, name: &str, entry: &std::fs::DirEntry) -> std::io::Result<std::fs::Metadata> {
    let pid = name
        .strip_prefix(SESSION_FILE_PREFIX)
        .and_then(|rest| rest.strip_suffix(&format!(".{PRESENCE_WRITER_SUFFIX}")))
        .filter(|pid| is_digits(pid));
    if let Some(pid) = pid {
        if let Ok(meta) = std::fs::metadata(root.join(format!("{PRESENCE_PREFIX}{pid}.json"))) {
            return Ok(meta);
        }
    }
    entry.metadata()
}

fn is_owned_session_file(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(SESSION_FILE_PREFIX) else {
        return false;
    };
    let Some((pid, suffix)) = rest.split_once('.') else {
        return false;
    };
    if !is_digits(pid) {
        return false;
    }

    if matches!(suffix, "json" | "permissions" | "permissions.lock" | PRESENCE_WRITER_SUFFIX) {
        return true;
    }
    suffix.strip_prefix("json.").and_then(|rest| rest.strip_suffix(".tmp")).is_some_and(is_digits)
        || suffix.strip_prefix("permissions.").and_then(|rest| rest.strip_suffix(".tmp")).is_some_and(is_digits)
        || suffix
            .strip_prefix(PRESENCE_WRITER_SUFFIX)
            .and_then(|rest| rest.strip_prefix('.'))
            .and_then(|rest| rest.strip_suffix(".tmp"))
            .is_some_and(is_digits)
        || suffix.starts_with("notify.")
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
            let path = std::env::temp_dir().join(format!("zj-radar-session-files-{name}-{}-{n}", std::process::id()));
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
        SessionFileIds { plugin_id, zellij_pid }
    }

    fn open(root: &Path, ids: SessionFileIds) -> SessionFilesOpen {
        SessionFiles::open_with_roots_at(ids, [root.to_path_buf()], SystemTime::now(), SNAPSHOT_MAX_AGE)
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
        assert!(!fresh_peer.permission.lock_acquired, "a fresh lock must still make peers wait");

        // Once the lock outlives the TTL (owner presumed gone with the prompt
        // unanswered) the next instance reclaims it instead of waiting forever.
        let later = now + PERMISSION_LOCK_TTL + Duration::from_secs(60);
        let reclaimer = SessionFiles::open_with_roots_at(ids(3, 42), root(), later, SNAPSHOT_MAX_AGE);
        assert!(reclaimer.permission.lock_acquired, "a stale lock must be reclaimed so peers aren't stranded forever");
    }

    #[test]
    fn marker_short_circuits_lock_election() {
        let dir = TempDir::new("marker");
        let owner = open(dir.path(), ids(1, 42));
        owner.files.persist_permission_marker(PermissionMarker::Granted);

        let peer = open(dir.path(), ids(2, 42));

        assert_eq!(peer.permission.marker, Some(PermissionMarker::Granted));
        assert!(!peer.permission.lock_acquired);
        assert_eq!(peer.files.permission_marker(), Some(PermissionMarker::Granted));
        assert!(!dir.join("zj-radar.42.permissions.1.tmp").exists());
    }

    #[test]
    fn heartbeat_rewrites_the_lock_and_is_inert_when_disabled() {
        let dir = TempDir::new("heartbeat");
        let owner = open(dir.path(), ids(1, 42));
        let lock = dir.join("zj-radar.42.permissions.lock");
        assert!(lock.exists());

        // A heartbeat rewrites the lock in place — and restores it if a peer's
        // stale-reclaim raced its owner and deleted it mid-prompt.
        std::fs::remove_file(&lock).unwrap();
        owner.files.heartbeat_permission_lock();
        assert!(lock.exists(), "heartbeat must (re)create the lock it owns");

        // Disabled persistence: no paths, no panic, no writes.
        SessionFiles::default().heartbeat_permission_lock();
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

        opened.files.persist_permission_marker(PermissionMarker::Denied);

        assert_eq!(std::fs::read_to_string(dir.join("zj-radar.42.permissions")).unwrap(), "granted");
        assert_eq!(opened.files.permission_marker(), Some(PermissionMarker::Granted));
    }

    #[test]
    fn persist_snapshot_writes_through_instance_tmp_then_rename() {
        let dir = TempDir::new("snapshot");
        let opened = open(dir.path(), ids(9, 42));

        opened.files.persist_snapshot(|_| r#"{"v":1}"#.into());

        assert_eq!(std::fs::read_to_string(dir.join("zj-radar.42.json")).unwrap(), r#"{"v":1}"#);
        assert_eq!(opened.files.snapshot().as_deref(), Some(r#"{"v":1}"#));
        assert!(!dir.join("zj-radar.42.json.9.tmp").exists());
    }

    #[test]
    fn failed_snapshot_temp_write_keeps_existing_snapshot() {
        let dir = TempDir::new("snapshot-failure");
        std::fs::write(dir.join("zj-radar.42.json"), "old").unwrap();
        std::fs::create_dir(dir.join("zj-radar.42.json.9.tmp")).unwrap();
        let opened = open(dir.path(), ids(9, 42));

        opened.files.persist_snapshot(|_| "new".into());

        assert_eq!(std::fs::read_to_string(dir.join("zj-radar.42.json")).unwrap(), "old");
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
        opened.files.persist_snapshot(|_| "seed".into());

        assert!(!broken.join("zj-radar.42.json").exists());
        assert_eq!(std::fs::read_to_string(fallback.join("zj-radar.42.json")).unwrap(), "seed");
    }

    #[test]
    fn notify_claim_elects_exactly_one_dispatcher() {
        let dir = TempDir::new("notify-claim");
        let now = SystemTime::now();
        let a = open(dir.path(), ids(1, 42)).files;
        let b = open(dir.path(), ids(2, 42)).files;

        // Both instances compute the same event key; only the first fires.
        assert!(a.claim_notification_at("p7.done.a1b2c3d4", now));
        assert!(!b.claim_notification_at("p7.done.a1b2c3d4", now));
        // A different event on the same pane claims independently.
        assert!(b.claim_notification_at("p7.pending.99887766", now));
    }

    #[test]
    fn notify_claim_past_ttl_is_a_genuine_repeat_and_fires_again() {
        let dir = TempDir::new("notify-ttl");
        let now = SystemTime::now();
        let files = open(dir.path(), ids(1, 42)).files;

        assert!(files.claim_notification_at("p7.pending.aa", now));
        // Within the TTL the key still identifies the already-fired event.
        let soon = now + Duration::from_secs(5);
        assert!(!files.claim_notification_at("p7.pending.aa", soon));
        // Past the TTL the same key is a new event (same question re-asked).
        let later = now + NOTIFY_CLAIM_TTL + Duration::from_secs(1);
        assert!(files.claim_notification_at("p7.pending.aa", later));
    }

    #[test]
    fn spent_notify_claims_are_swept_on_later_claims() {
        let dir = TempDir::new("notify-sweep");
        let now = SystemTime::now();
        let files = open(dir.path(), ids(1, 42)).files;

        assert!(files.claim_notification_at("old", now));
        assert!(dir.join("zj-radar.42.notify.old").exists());
        let later = now + NOTIFY_CLAIM_SWEEP_AGE + Duration::from_secs(1);
        assert!(files.claim_notification_at("new", later));
        assert!(
            !dir.join("zj-radar.42.notify.old").exists(),
            "spent claim swept so a long session doesn't accrete files"
        );
    }

    #[test]
    fn notify_claim_without_writable_root_prefers_duplicate_over_silence() {
        let dir = TempDir::new("notify-disabled");
        let broken = dir.join("cache-as-file");
        std::fs::write(&broken, b"not a dir").unwrap();
        let opened =
            SessionFiles::open_with_roots_at(ids(3, 42), [broken.clone(), broken], SystemTime::now(), SNAPSHOT_MAX_AGE);
        // No coordination possible → every instance dispatches (the pre-claim
        // behavior), because a missed "needs input" is worse than a dup toast.
        assert!(opened.files.claim_notification_at("k", SystemTime::now()));
    }

    #[test]
    fn stale_session_sweep_owns_notify_claims_but_spares_current_session() {
        let dir = TempDir::new("notify-owned");
        // A dead session's claim (pid 41) is prunable; ours (pid 42) is not.
        assert!(is_owned_session_file("zj-radar.41.notify.p7.done.aa"));
        let paths = SessionPaths::new(dir.path().to_path_buf(), ids(1, 42));
        assert!(paths.is_current_session_file("zj-radar.42.notify.p7.done.aa"));
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
        opened.files.persist_snapshot(|_| "ignored".into());
        opened.files.persist_permission_marker(PermissionMarker::Denied);

        assert_eq!(opened.snapshot, None);
        assert_eq!(opened.permission.marker, None);
        assert!(opened.permission.lock_acquired);
        assert_eq!(opened.files.permission_marker(), None);
    }

    #[test]
    fn stale_pruning_removes_old_session_snapshot_marker_and_lock() {
        let dir = TempDir::new("prune");
        for name in ["zj-radar.1.json", "zj-radar.1.permissions", "zj-radar.1.permissions.lock"] {
            std::fs::write(dir.join(name), b"old").unwrap();
        }
        for name in ["zj-radar.2.json", "zj-radar.2.permissions", "zj-radar.2.permissions.lock"] {
            std::fs::write(dir.join(name), b"current").unwrap();
        }

        let now = SystemTime::now() + SNAPSHOT_MAX_AGE + Duration::from_secs(1);
        let _ = SessionFiles::open_with_roots_at(ids(3, 2), [dir.path().to_path_buf()], now, SNAPSHOT_MAX_AGE);

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
            "zj-radar.20.json", "zj-radar.20.json.8.tmp", "zj-radar.20.permissions", "zj-radar.20.permissions.8.tmp",
            "zj-radar.20.permissions.lock",
        ] {
            std::fs::write(dir.join(name), b"old").unwrap();
        }
        for name in [
            "zj-radar.2.json", "zj-radar.2.json.3.tmp", "zj-radar.2.permissions", "zj-radar.2.permissions.3.tmp",
            "zj-radar.2.permissions.lock",
        ] {
            std::fs::write(dir.join(name), b"current").unwrap();
        }

        let now = SystemTime::now() + SNAPSHOT_MAX_AGE + Duration::from_secs(1);
        let _ = SessionFiles::open_with_roots_at(ids(3, 2), [dir.path().to_path_buf()], now, SNAPSHOT_MAX_AGE);

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
            "zj-radar.notes", "zj-radar.abc.json", "zj-radar.1.unknown", "zj-radar.1.json.tmp",
            "zj-radar.1.permissions.tmp",
        ] {
            std::fs::write(dir.join(name), b"not ours").unwrap();
        }

        let now = SystemTime::now() + SNAPSHOT_MAX_AGE + Duration::from_secs(1);
        let _ = SessionFiles::open_with_roots_at(ids(3, 2), [dir.path().to_path_buf()], now, SNAPSHOT_MAX_AGE);

        assert!(dir.join("zj-radar.notes").exists());
        assert!(dir.join("zj-radar.abc.json").exists());
        assert!(dir.join("zj-radar.1.unknown").exists());
        assert!(dir.join("zj-radar.1.json.tmp").exists());
        assert!(dir.join("zj-radar.1.permissions.tmp").exists());
    }

    #[test]
    fn persist_snapshot_hands_the_existing_record_to_the_merge() {
        let dir = tempfile::tempdir().unwrap();
        let files = SessionFiles::open_with_roots_at(
            SessionFileIds { plugin_id: 1, zellij_pid: 42 },
            [dir.path().to_path_buf()],
            SystemTime::now(),
            SNAPSHOT_MAX_AGE,
        )
        .files;
        let snapshot = dir.path().join("zj-radar.42.json");
        files.persist_snapshot(|existing| {
            assert!(existing.is_none());
            "first".into()
        });
        assert_eq!(std::fs::read_to_string(&snapshot).unwrap(), "first");
        files.persist_snapshot(|existing| format!("{}+second", existing.unwrap()));
        assert_eq!(std::fs::read_to_string(&snapshot).unwrap(), "first+second");
    }

    #[test]
    fn heartbeat_presence_skips_a_fresh_file_and_rewrites_a_stale_or_missing_one() {
        let dir = tempfile::tempdir().unwrap();
        let files = SessionFiles::open_with_roots_at(
            SessionFileIds { plugin_id: 1, zellij_pid: 100 },
            [dir.path().to_path_buf()],
            SystemTime::now(),
            SNAPSHOT_MAX_AGE,
        )
        .files;
        let presence = dir.path().join("zj-radar.presence.100.json");
        let min_age = Duration::from_secs(30);

        // Missing: a heartbeat writes.
        files.claim_presence_writer(false);
        files.persist_presence_at(Some(min_age), RESCUE, || "first".into(), SystemTime::now());
        assert_eq!(std::fs::read_to_string(&presence).unwrap(), "first");

        // Just written (by this or any sibling instance): skipped, and the
        // JSON closure is never even evaluated.
        files.persist_presence_at(
            Some(min_age),
            RESCUE,
            || unreachable!("fresh file must not be serialized"),
            SystemTime::now(),
        );
        assert_eq!(std::fs::read_to_string(&presence).unwrap(), "first");

        // A content edge (no window) always writes, fresh or not.
        files.persist_presence_at(None, RESCUE, || "edge".into(), SystemTime::now());
        assert_eq!(std::fs::read_to_string(&presence).unwrap(), "edge");

        // Older than the skip window: rewritten.
        files.persist_presence_at(Some(min_age), RESCUE, || "second".into(), SystemTime::now() + min_age);
        assert_eq!(std::fs::read_to_string(&presence).unwrap(), "second");
    }

    #[test]
    fn presence_round_trips_between_two_session_roots() {
        let dir = tempfile::tempdir().unwrap();
        let a = SessionFiles::open_with_roots_at(
            SessionFileIds { plugin_id: 1, zellij_pid: 100 },
            [dir.path().to_path_buf()],
            SystemTime::now(),
            SNAPSHOT_MAX_AGE,
        );
        let b = SessionFiles::open_with_roots_at(
            SessionFileIds { plugin_id: 7, zellij_pid: 200 },
            [dir.path().to_path_buf()],
            SystemTime::now(),
            SNAPSHOT_MAX_AGE,
        );
        a.files.claim_presence_writer(false);
        a.files.persist_presence(None, RESCUE, || r#"{"session_name":"alpha"}"#.into());
        b.files.claim_presence_writer(false);
        b.files.persist_presence(None, RESCUE, || r#"{"session_name":"beta"}"#.into());
        // Each session sees the OTHER's presence, never its own.
        let a_json: Vec<String> = a.files.read_peer_presences().into_iter().map(|p| p.json).collect();
        let b_json: Vec<String> = b.files.read_peer_presences().into_iter().map(|p| p.json).collect();
        assert_eq!(a_json, vec![r#"{"session_name":"beta"}"#.to_string()]);
        assert_eq!(b_json, vec![r#"{"session_name":"alpha"}"#.to_string()]);
    }

    #[test]
    fn open_sweeps_stale_presence_but_keeps_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let stale = dir.path().join("zj-radar.presence.999.json");
        std::fs::write(&stale, "{}").unwrap();
        let old = SystemTime::now() + PRESENCE_MAX_AGE + Duration::from_secs(60);
        // Re-open "later": the sweep runs against `old` as now.
        let s = SessionFiles::open_with_roots_at(
            SessionFileIds { plugin_id: 1, zellij_pid: 100 },
            [dir.path().to_path_buf()],
            old,
            SNAPSHOT_MAX_AGE,
        );
        assert!(!stale.exists(), "stale presence swept at open");
        s.files.claim_presence_writer(false);
        s.files.persist_presence(None, RESCUE, || r#"{"session_name":"me"}"#.into());
        let fresh = dir.path().join("zj-radar.presence.100.json");
        assert!(fresh.exists());
    }

    #[test]
    fn read_peer_presences_never_drops_an_old_mtime_peer_and_reports_its_age() {
        // The read path grades nothing: this method used to skip a peer
        // once its file's mtime drifted past a liveness TTL; now it must
        // keep returning that peer unconditionally with an honest age, so
        // the caller (`sessions::Sessions`) owns the whole fresh → stale →
        // reaped ladder — dropping happens there (`DEAD_AFTER_SECS`) or at
        // `open`'s much-longer `PRESENCE_MAX_AGE` debris sweep, never here.
        let dir = tempfile::tempdir().unwrap();
        let reader = SessionFiles::open_with_roots_at(
            SessionFileIds { plugin_id: 1, zellij_pid: 100 },
            [dir.path().to_path_buf()],
            SystemTime::now(),
            SNAPSHOT_MAX_AGE,
        );
        let fresh_peer = SessionFiles::open_with_roots_at(
            SessionFileIds { plugin_id: 2, zellij_pid: 200 },
            [dir.path().to_path_buf()],
            SystemTime::now(),
            SNAPSHOT_MAX_AGE,
        );
        let old_peer = SessionFiles::open_with_roots_at(
            SessionFileIds { plugin_id: 3, zellij_pid: 300 },
            [dir.path().to_path_buf()],
            SystemTime::now(),
            SNAPSHOT_MAX_AGE,
        );
        fresh_peer.files.claim_presence_writer(false);
        fresh_peer.files.persist_presence(None, RESCUE, || r#"{"session_name":"fresh"}"#.into());
        old_peer.files.claim_presence_writer(false);
        old_peer.files.persist_presence(None, RESCUE, || r#"{"session_name":"old"}"#.into());

        // Backdate only the "old" peer's file — a dead server's file just
        // sitting there with an old mtime, not something any sweep has
        // touched (well short of `PRESENCE_MAX_AGE`, so `open` would not
        // have reaped it either).
        let old_path = dir.path().join("zj-radar.presence.300.json");
        let old_age = Duration::from_secs(400); // well past sessions::STALE_AFTER_SECS (90s)
        std::fs::File::open(&old_path).unwrap().set_modified(SystemTime::now() - old_age).unwrap();

        let mut peers = reader.files.read_peer_presences();
        peers.sort_by(|a, b| a.json.cmp(&b.json));
        assert_eq!(peers.len(), 2, "both peers must still be returned regardless of mtime age");
        let fresh = peers.iter().find(|p| p.json.contains("fresh")).expect("fresh peer present");
        let old = peers.iter().find(|p| p.json.contains("\"old\"")).expect("old-mtime peer still present, not dropped");
        assert!(fresh.age_secs < 5, "freshly-written peer's age should read ~0s, got {}", fresh.age_secs);
        assert!(
            old.age_secs >= old_age.as_secs(),
            "backdated peer's age must reflect its real mtime, got {}",
            old.age_secs
        );
        assert!(
            old_path.exists(),
            "the read path must never delete anything — only the dismiss/reap and the open-time sweep do"
        );
    }

    #[test]
    fn read_peer_presences_ignores_tmp_and_non_presence_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("zj-radar.presence.300.json.9.tmp"), "x").unwrap();
        std::fs::write(dir.path().join("zj-radar.300.json"), "snapshot-not-presence").unwrap();
        let s = SessionFiles::open_with_roots_at(
            SessionFileIds { plugin_id: 1, zellij_pid: 100 },
            [dir.path().to_path_buf()],
            SystemTime::now(),
            SNAPSHOT_MAX_AGE,
        );
        assert!(s.files.read_peer_presences().is_empty());
    }

    #[test]
    fn remove_presences_matching_deletes_every_file_satisfying_the_predicate_and_spares_others() {
        let dir = tempfile::tempdir().unwrap();
        let reader = SessionFiles::open_with_roots_at(
            SessionFileIds { plugin_id: 1, zellij_pid: 100 },
            [dir.path().to_path_buf()],
            SystemTime::now(),
            SNAPSHOT_MAX_AGE,
        );
        let alpha_a = SessionFiles::open_with_roots_at(
            SessionFileIds { plugin_id: 2, zellij_pid: 200 },
            [dir.path().to_path_buf()],
            SystemTime::now(),
            SNAPSHOT_MAX_AGE,
        );
        let alpha_b = SessionFiles::open_with_roots_at(
            SessionFileIds { plugin_id: 3, zellij_pid: 300 },
            [dir.path().to_path_buf()],
            SystemTime::now(),
            SNAPSHOT_MAX_AGE,
        );
        let beta = SessionFiles::open_with_roots_at(
            SessionFileIds { plugin_id: 4, zellij_pid: 400 },
            [dir.path().to_path_buf()],
            SystemTime::now(),
            SNAPSHOT_MAX_AGE,
        );
        alpha_a.files.claim_presence_writer(false);
        alpha_a.files.persist_presence(None, RESCUE, || r#"{"session_name":"alpha","running":1,"attention":0}"#.into());
        alpha_b.files.claim_presence_writer(false);
        alpha_b.files.persist_presence(None, RESCUE, || r#"{"session_name":"alpha","running":2,"attention":0}"#.into());
        beta.files.claim_presence_writer(false);
        beta.files.persist_presence(None, RESCUE, || r#"{"session_name":"beta","running":1,"attention":0}"#.into());

        reader.files.remove_presences_matching(|json| json.contains(r#""alpha""#));

        assert!(!dir.path().join("zj-radar.presence.200.json").exists());
        assert!(!dir.path().join("zj-radar.presence.300.json").exists());
        assert!(dir.path().join("zj-radar.presence.400.json").exists());
    }

    #[test]
    fn remove_presences_matching_spares_the_own_file_but_reaps_same_name_corpses() {
        // The own-file exclusion lives at the delete primitive itself, by
        // *path* identity: a dismiss of this session's own NAME (a dead
        // pid-keyed corpse of a restarted session) unlinks the corpse's
        // file while the live file this instance keeps heartbeating is
        // structurally unreachable — which is what lets the runtime's
        // auto-reap treat its own name like any other dead name.
        let dir = tempfile::tempdir().unwrap();
        let me = SessionFiles::open_with_roots_at(
            SessionFileIds { plugin_id: 1, zellij_pid: 100 },
            [dir.path().to_path_buf()],
            SystemTime::now(),
            SNAPSHOT_MAX_AGE,
        );
        me.files.claim_presence_writer(false);
        me.files.persist_presence(None, RESCUE, || r#"{"session_name":"alpha","running":1,"attention":0}"#.into());
        // A corpse of a previous server incarnation, same name, older pid.
        std::fs::write(
            dir.path().join("zj-radar.presence.99.json"),
            r#"{"session_name":"alpha","running":0,"attention":0}"#,
        )
        .unwrap();

        me.files.remove_presences_matching(|json| json.contains(r#""alpha""#));

        assert!(
            dir.path().join("zj-radar.presence.100.json").exists(),
            "the own live file must be spared by path identity even when its name matches"
        );
        assert!(
            !dir.path().join("zj-radar.presence.99.json").exists(),
            "a same-name corpse under another pid must be reaped"
        );
    }

    #[test]
    fn remove_presences_matching_is_a_noop_in_disabled_mode() {
        SessionFiles::default().remove_presences_matching(|_| true);
    }

    // ── Presence-writer token ──

    const RESCUE: Duration = Duration::from_secs(75);
    const HOLDER_SKIP: Duration = Duration::from_secs(15);

    /// Two rails of ONE session (same zellij pid, distinct plugin ids)
    /// sharing a root.
    fn two_rails(dir: &Path) -> (SessionFiles, SessionFiles) {
        (open(dir, ids(1, 100)).files, open(dir, ids(2, 100)).files)
    }

    fn presence_100(dir: &Path) -> Option<String> {
        std::fs::read_to_string(dir.join("zj-radar.presence.100.json")).ok()
    }

    #[test]
    fn claiming_the_presence_writer_token_moves_it_to_the_claimant() {
        let dir = TempDir::new("writer-claim");
        let (a, b) = two_rails(dir.path());
        assert!(!a.holds_presence_writer() && !b.holds_presence_writer(), "nobody holds before a claim");

        assert!(a.claim_presence_writer(false), "first claim takes the token over");
        assert!(a.holds_presence_writer());
        assert!(!b.holds_presence_writer());
        assert!(!a.claim_presence_writer(false), "re-claiming a held token is no takeover (and no write)");

        assert!(b.claim_presence_writer(true), "another rail's preempting claim takes it away");
        assert!(b.holds_presence_writer());
        assert!(!a.holds_presence_writer(), "the previous holder no longer holds");
        assert_eq!(std::fs::read_to_string(dir.join("zj-radar.100.presence-writer")).unwrap(), "2");
    }

    #[test]
    fn content_write_needs_the_writer_token() {
        let dir = TempDir::new("writer-content");
        let (a, b) = two_rails(dir.path());
        b.claim_presence_writer(false);
        b.persist_presence(None, RESCUE, || "holder".into());
        assert_eq!(presence_100(dir.path()).as_deref(), Some("holder"));

        // A non-holder's content edge over a fresh file: skipped, unserialized.
        a.persist_presence(None, RESCUE, || unreachable!("a non-holder must not serialize a content edge"));
        assert_eq!(presence_100(dir.path()).as_deref(), Some("holder"));

        a.claim_presence_writer(true);
        a.persist_presence(None, RESCUE, || "new holder".into());
        assert_eq!(presence_100(dir.path()).as_deref(), Some("new holder"), "the holder's content edge writes");
    }

    #[test]
    fn non_holder_heartbeat_writes_only_as_a_rescue() {
        let dir = TempDir::new("writer-rescue");
        let (a, b) = two_rails(dir.path());
        b.claim_presence_writer(false);
        b.persist_presence(None, RESCUE, || "holder".into());
        let now = SystemTime::now();

        // Past the holder's skip window but short of the rescue age: a
        // non-holder leaves liveness to the holder (no stale-view write).
        a.persist_presence_at(Some(HOLDER_SKIP), RESCUE, || unreachable!("not yet a rescue"), now + HOLDER_SKIP * 2);
        a.persist_presence_at(None, RESCUE, || unreachable!("not yet a rescue"), now + RESCUE - Duration::from_secs(2));
        assert_eq!(presence_100(dir.path()).as_deref(), Some("holder"));

        // The holder went quiet past the rescue age: any rail rescues.
        a.persist_presence_at(Some(HOLDER_SKIP), RESCUE, || "rescue".into(), now + RESCUE + Duration::from_secs(1));
        assert_eq!(presence_100(dir.path()).as_deref(), Some("rescue"));

        // The holder's own heartbeat keeps the short skip window.
        b.persist_presence_at(Some(HOLDER_SKIP), RESCUE, || "beat".into(), SystemTime::now() + HOLDER_SKIP);
        assert_eq!(presence_100(dir.path()).as_deref(), Some("beat"));
    }

    #[test]
    fn presence_writer_token_is_neither_a_peer_presence_nor_debris() {
        let dir = TempDir::new("writer-files");
        let (a, _) = two_rails(dir.path());
        a.claim_presence_writer(false);
        // Another session's token + an orphaned tmp: owned, so swept once stale.
        std::fs::write(dir.join("zj-radar.200.presence-writer"), "9").unwrap();
        std::fs::write(dir.join("zj-radar.200.presence-writer.9.tmp"), "9").unwrap();

        let peer = open(dir.path(), ids(5, 300)).files;
        assert!(peer.read_peer_presences().is_empty(), "a token is never read as a peer presence");
        assert!(a.read_peer_presences().is_empty());

        let later = SystemTime::now() + SNAPSHOT_MAX_AGE + Duration::from_secs(1);
        let reopened =
            SessionFiles::open_with_roots_at(ids(3, 100), [dir.path().to_path_buf()], later, SNAPSHOT_MAX_AGE);
        assert!(dir.join("zj-radar.100.presence-writer").exists(), "the live session's token survives the sweep");
        assert!(!reopened.files.holds_presence_writer(), "and still names its holder, not the reopener");
        assert!(!dir.join("zj-radar.200.presence-writer").exists(), "a dead session's token is swept");
        assert!(!dir.join("zj-radar.200.presence-writer.9.tmp").exists(), "and so is its tmp debris");
    }

    fn backdate(path: &Path, by: Duration) {
        std::fs::File::options().write(true).open(path).unwrap().set_modified(SystemTime::now() - by).unwrap();
    }

    #[test]
    fn a_young_foreign_token_is_only_taken_over_by_a_preempting_claim() {
        // Two clients on different tabs both receive manifests: without a
        // floor, every alternate manifest is a takeover (token rewrite +
        // full republish). A token younger than the floor names a holder
        // that is itself receiving manifests — a manifest claim leaves it.
        let dir = TempDir::new("writer-floor");
        let (a, b) = two_rails(dir.path());
        assert!(a.claim_presence_writer(false), "nobody holds: a claim takes it");
        let now = SystemTime::now();
        assert!(!b.claim_presence_writer_at(false, now), "a fresh foreign token is not taken by a manifest claim");
        assert!(a.holds_presence_writer(), "the young holder keeps it");
        assert!(
            b.claim_presence_writer_at(false, now + PRESENCE_WRITER_TAKEOVER_FLOOR + Duration::from_secs(1)),
            "past the floor a manifest claim takes over"
        );
        assert!(b.holds_presence_writer());
        // A preempting claim (a reveal: this rail just became the active
        // view) ignores the floor.
        assert!(a.claim_presence_writer_at(true, SystemTime::now()), "a reveal preempts a young token");
        assert!(a.holds_presence_writer());
        // Our own token is never rewritten, young or old.
        let token = dir.join("zj-radar.100.presence-writer");
        backdate(&token, Duration::from_secs(60));
        let mtime = std::fs::metadata(&token).unwrap().modified().unwrap();
        assert!(!a.claim_presence_writer(true), "own token: no takeover");
        assert_eq!(std::fs::metadata(&token).unwrap().modified().unwrap(), mtime, "and no rewrite");
    }

    #[test]
    fn a_failed_token_write_is_not_a_takeover() {
        // A directory squatting on the token path: the tmp write succeeds,
        // the rename fails. Reporting that as a takeover would re-enter the
        // republish on every manifest without ever holding the token.
        let dir = TempDir::new("writer-fail");
        let (a, _) = two_rails(dir.path());
        std::fs::create_dir(dir.join("zj-radar.100.presence-writer")).unwrap();
        assert!(!a.claim_presence_writer(true), "the write failed, so nothing was taken over");
        assert!(!a.holds_presence_writer());
    }

    #[test]
    fn a_long_lived_sessions_token_is_aged_by_its_presence_file() {
        // A stable holder never rewrites its token, so the token's own mtime
        // says nothing about liveness past a day; the session's presence
        // file (heartbeated every ~60 s) does.
        let dir = TempDir::new("writer-sweep");
        let live_token = dir.join("zj-radar.200.presence-writer");
        std::fs::write(&live_token, "9").unwrap();
        backdate(&live_token, SNAPSHOT_MAX_AGE + Duration::from_secs(3600));
        std::fs::write(dir.join("zj-radar.presence.200.json"), "{}").unwrap();

        let dead_token = dir.join("zj-radar.300.presence-writer");
        let dead_presence = dir.join("zj-radar.presence.300.json");
        std::fs::write(&dead_token, "9").unwrap();
        std::fs::write(&dead_presence, "{}").unwrap();
        backdate(&dead_token, SNAPSHOT_MAX_AGE + Duration::from_secs(3600));
        backdate(&dead_presence, SNAPSHOT_MAX_AGE + Duration::from_secs(3600));

        let orphan_token = dir.join("zj-radar.400.presence-writer");
        std::fs::write(&orphan_token, "9").unwrap();
        backdate(&orphan_token, SNAPSHOT_MAX_AGE + Duration::from_secs(3600));

        let _ = open(dir.path(), ids(1, 100));
        assert!(live_token.exists(), "a day-old token of a session with a fresh presence file survives");
        assert!(!dead_token.exists(), "token and presence both old: swept");
        assert!(!orphan_token.exists(), "no presence file: aged by its own mtime");
    }

    #[test]
    fn presence_writer_is_inert_when_disabled() {
        let files = SessionFiles::default();
        assert!(!files.claim_presence_writer(true));
        assert!(!files.holds_presence_writer());
        files.persist_presence(None, RESCUE, || unreachable!("disabled mode writes nothing"));
    }
}
