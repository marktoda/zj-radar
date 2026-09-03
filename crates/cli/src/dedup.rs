//! Producer-side last-sent dedup: skip re-broadcasting a `running` payload
//! identical to the one this pane just sent.
//!
//! Claude fires PreToolUse AND PostToolUse per tool call, and both derive the
//! same wire payload (`agents/claude.rs` reads only tool_name/tool_input/cwd),
//! so half of tool-loop traffic is an exact duplicate — paid at full host cost
//! (hook process tree, git probes, `zellij pipe` fan-out to every rail
//! instance) and then no-op'd receiver-side. This module remembers the last
//! payload each pane delivered, in a tiny state file, and lets `broadcast`
//! drop a repeat before it spends anything.
//!
//! The rule is deliberately narrow:
//!
//! - **Only `running` is ever skipped.** Every edge (`pending`, `done`,
//!   `error`, `idle`) always goes out — dropping one loses real state, and the
//!   Pending→Running recovery depends on the Notification→pending send
//!   overwriting the record so the PostToolUse→running that follows it differs
//!   and is sent.
//! - **Only within [`DEDUP_TTL_SECS`].** The rail can drop a row without the
//!   producer knowing (`/clear`, the 256-id eviction, a fresh instance with no
//!   snapshot), so an identical `running` is re-sent once the record is older
//!   than the TTL — a bound on how long a rail can disagree with a busy
//!   producer. The TTL sits below the plugin's stale-Running grace, which any
//!   payload cancels: a longer silence would clear a live agent's row.
//!   (A `✓` ack acts on a `Pending` row, so the record is `pending` and the
//!   `running` that follows always differs — no interaction.)
//! - **No sweep.** One ~150-byte file per `(session, pane)` ever used, until
//!   the OS clears the runtime/temp dir; bounded by panes-ever, so a readdir
//!   per hook is not worth it. Pane ids restart when a server is recreated
//!   under the same session name, so a stale record can only suppress a new
//!   pane's first `running` if it is byte-identical and inside the TTL.
//! - **Repo and branch are not in the key.** They are resolved *after* this
//!   check so a skipped send costs no git probe; a branch switch mid-task
//!   surfaces with the next payload that differs in status/msg/task, or at the
//!   TTL. `source` is in the key so two producers sharing a pane never mask
//!   each other.
//!
//! A record is written only after a *confirmed* delivery (the `sh` wrapper's
//! exit status is `zellij pipe`'s, see `core::pipe`); a send killed at its
//! deadline is not recorded, so the next identical payload is retried. Any IO
//! or parse failure fails open: the payload is sent. `ZJ_RADAR_NO_DEDUP=1`
//! disables the whole mechanism (debugging, tests that count sends).

use crate::fsutil::atomic_write;
use crate::status::Status;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// How long an identical `running` payload stays suppressed. Long enough
/// that a tool loop's Pre/Post pairs collapse (a tool call rarely runs 10 s
/// between its two hooks) and rapid-fire identical heartbeats fold; short
/// enough that a rail which silently dropped the row (`/clear`, eviction, a
/// fresh instance with no snapshot) re-converges within a beat. Hard upper
/// bound: the plugin's stale-Running grace — ANY payload for the pane cancels
/// that clock, so a producer quiet for longer would let a live agent's row
/// clear to idle. Pinned below it at compile time.
pub const DEDUP_TTL_SECS: u64 = 10;
const _: () = assert!(DEDUP_TTL_SECS < crate::pipe::RUNNING_QUIET_MAX_SECS);

/// The fields a repeat must match exactly. `status` serializes as its wire
/// token (`core::status`'s lenient serde: an unknown token on disk reads as
/// `Idle`, which is never redundant — fail open).
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
pub struct SentKey {
    pub status: Status,
    pub source: String,
    pub msg: String,
    pub task: String,
}

impl SentKey {
    pub fn new(status: Status, source: &str, msg: &str, task: &str) -> SentKey {
        SentKey {
            status,
            source: source.to_string(),
            msg: msg.to_string(),
            task: task.to_string(),
        }
    }
}

/// What the state file holds: the last delivered key and when it was sent.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
struct Record {
    key: SentKey,
    sent_at: u64,
}

/// The pure rule: is `key` (about to be sent) a redundant repeat of `last`,
/// as of `now`? Only `running` can be redundant; everything else is an edge
/// and always sends.
fn is_redundant(key: &SentKey, last: &Record, now: u64) -> bool {
    key.status == Status::Running
        && last.key == *key
        && now.saturating_sub(last.sent_at) < DEDUP_TTL_SECS
}

/// Handle to one pane's last-sent record. Built by [`LastSent::from_env`] on
/// the live path (which may decline, see there) or [`LastSent::at`] in tests.
pub struct LastSent {
    path: PathBuf,
}

impl LastSent {
    /// The record for `pane_id` in the current Zellij session, or `None` when
    /// dedup is off: `ZJ_RADAR_NO_DEDUP` is set, or there is no session
    /// identity (`ZELLIJ_SESSION_NAME`) to scope the file by. Pane ids are
    /// per session, so the file is keyed on both.
    pub fn from_env(pane_id: u32) -> Option<LastSent> {
        if std::env::var_os("ZJ_RADAR_NO_DEDUP").is_some_and(|v| !v.is_empty()) {
            return None;
        }
        let session = std::env::var("ZELLIJ_SESSION_NAME").ok().filter(|s| !s.is_empty())?;
        Some(LastSent::at(&state_dir(), &session, pane_id))
    }

    /// The record file for (`session`, `pane_id`) under `dir`.
    pub fn at(dir: &Path, session: &str, pane_id: u32) -> LastSent {
        LastSent {
            path: dir.join(format!("last-sent.{}.{pane_id}.json", sanitize(session))),
        }
    }

    /// True when sending `key` now would repeat the last confirmed delivery
    /// (per [`is_redundant`]). A missing, unreadable, or malformed record
    /// means "not a repeat".
    pub fn is_duplicate(&self, key: &SentKey, now: u64) -> bool {
        let Some(last) = self.read() else {
            return false;
        };
        is_redundant(key, &last, now)
    }

    /// Record `key` as delivered at `now`. Temp-file + rename so a concurrent
    /// hook (parallel tool calls) sees the old record or the new one, never a
    /// torn file. Write failures are ignored: the worst case is one redundant
    /// send next time.
    pub fn record(&self, key: &SentKey, now: u64) {
        let record = Record {
            key: key.clone(),
            sent_at: now,
        };
        if let Ok(body) = serde_json::to_vec(&record) {
            let _ = atomic_write(&self.path, &body);
        }
    }

    fn read(&self) -> Option<Record> {
        let body = std::fs::read(&self.path).ok()?;
        serde_json::from_slice(&body).ok()
    }
}

/// Seconds since the epoch, saturating at 0 on a pre-1970 clock so a skewed
/// host can't panic a hook.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Where the per-user state files live. `$XDG_RUNTIME_DIR` when set (per-user,
/// tmpfs, cleared at logout — the ideal home for a seconds-long cache), else
/// the process temp dir (`$TMPDIR`, per-user on macOS; `/tmp` on Linux
/// without XDG, where a foreign dir just makes every write fail open). Its
/// own leaf, not `zj-radar/`: on Linux that is the plugin's `/tmp/zj-radar`
/// session-file root, whose presence scans read the directory.
fn state_dir() -> PathBuf {
    dirs::runtime_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("zj-radar-dedup")
}

/// Zellij session names are free text; fold anything outside a filename-safe
/// set to `_` and cap the length so the path stays short (macOS `sun_path`
/// budgets taught this repo to respect short runtime paths).
fn sanitize(session: &str) -> String {
    session
        .chars()
        .take(64)
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running(msg: &str) -> SentKey {
        SentKey::new(Status::Running, "claude", msg, "fix auth")
    }

    fn last(key: SentKey, sent_at: u64) -> Record {
        Record { key, sent_at }
    }

    #[test]
    fn identical_running_within_ttl_is_redundant() {
        let key = running("editing auth.rs");
        assert!(is_redundant(&key, &last(key.clone(), 1000), 1000));
        assert!(is_redundant(&key, &last(key.clone(), 1000), 1000 + DEDUP_TTL_SECS - 1));
    }

    #[test]
    fn identical_running_past_ttl_is_sent_again() {
        // The TTL bounds how long a rail that silently dropped the row can
        // disagree with a busy producer.
        let key = running("editing auth.rs");
        assert!(!is_redundant(&key, &last(key.clone(), 1000), 1000 + DEDUP_TTL_SECS));
    }

    #[test]
    fn any_field_change_is_sent() {
        let key = running("editing auth.rs");
        for other in [
            running("reading auth.rs"),
            SentKey::new(Status::Running, "claude", "editing auth.rs", "other task"),
            SentKey::new(Status::Running, "generic", "editing auth.rs", "fix auth"),
        ] {
            assert!(!is_redundant(&key, &last(other.clone(), 1000), 1000), "{other:?}");
        }
    }

    #[test]
    fn edges_are_never_redundant() {
        // Dropping an edge loses real state; only running heartbeats collapse.
        for status in Status::ALL.iter().copied().filter(|s| *s != Status::Running) {
            let key = SentKey::new(status, "claude", "same", "same");
            assert!(!is_redundant(&key, &last(key.clone(), 1000), 1000), "{status:?}");
        }
    }

    #[test]
    fn clock_skew_never_panics_and_fails_open() {
        // A record from the "future" reads as age 0 → still within TTL; the
        // saturating sub is what keeps the hook from panicking.
        let key = running("x");
        assert!(is_redundant(&key, &last(key.clone(), 5000), 1000));
    }

    #[test]
    fn record_round_trips_through_the_state_file() {
        let dir = tempfile::tempdir().unwrap();
        let ls = LastSent::at(dir.path(), "my-session", 7);
        let key = running("editing auth.rs");
        assert!(!ls.is_duplicate(&key, 1000), "no record yet");
        ls.record(&key, 1000);
        assert!(ls.is_duplicate(&key, 1000 + DEDUP_TTL_SECS / 2));
        assert!(!ls.is_duplicate(&running("other"), 1000 + DEDUP_TTL_SECS / 2));
        assert!(!ls.is_duplicate(&key, 1000 + DEDUP_TTL_SECS));
        // No temp file left behind — the record is the directory's only entry.
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, vec!["last-sent.my-session.7.json".to_string()]);
    }

    #[test]
    fn pending_overwrites_the_record_so_the_recovery_running_is_sent() {
        // The Pending→Running recovery edge: Notification→pending passes
        // through and overwrites last-sent, so the PostToolUse→running that
        // follows differs from the record and IS sent — then its own repeat
        // collapses.
        let dir = tempfile::tempdir().unwrap();
        let ls = LastSent::at(dir.path(), "s", 7);
        let run = running("running cargo");
        ls.record(&run, 1000);
        assert!(ls.is_duplicate(&run, 1001), "pre-tool repeat collapses");
        let pending = SentKey::new(Status::Pending, "claude", "Allow cargo?", "");
        assert!(!ls.is_duplicate(&pending, 1002));
        ls.record(&pending, 1002);
        assert!(!ls.is_duplicate(&run, 1003), "recovery running must be sent");
        ls.record(&run, 1003);
        assert!(ls.is_duplicate(&run, 1004));
    }

    #[test]
    fn malformed_or_foreign_record_fails_open() {
        let dir = tempfile::tempdir().unwrap();
        let ls = LastSent::at(dir.path(), "s", 7);
        std::fs::write(dir.path().join("last-sent.s.7.json"), b"not json").unwrap();
        assert!(!ls.is_duplicate(&running("x"), 1000));
        std::fs::write(dir.path().join("last-sent.s.7.json"), br#"{"key":{"status":"running"},"sent_at":1}"#).unwrap();
        assert!(!ls.is_duplicate(&running("x"), 1000), "missing fields → no repeat");
    }

    #[test]
    fn unwritable_dir_fails_open() {
        // A dir that can't be created (a file in its place) makes record() a
        // no-op and is_duplicate() false — the payload is simply sent.
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"").unwrap();
        let ls = LastSent::at(&blocker, "s", 7);
        let key = running("x");
        ls.record(&key, 1000);
        assert!(!ls.is_duplicate(&key, 1000));
    }

    #[test]
    fn session_names_are_filename_safe_and_short() {
        assert_eq!(sanitize("triangular-stegosaurus"), "triangular-stegosaurus");
        assert_eq!(sanitize("my session/../x"), "my_session_.._x");
        assert_eq!(sanitize(&"a".repeat(200)).len(), 64);
        let ls = LastSent::at(Path::new("/d"), "a/b", 3);
        assert_eq!(ls.path, PathBuf::from("/d/last-sent.a_b.3.json"));
    }
}
