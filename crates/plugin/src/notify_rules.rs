//! Pure notification decisions: diff the observable per-pane status against the
//! previous baseline and emit a notification for each background transition INTO
//! an attention status (done/error/pending). Host-independent and fully unit
//! tested; the wasm side only dispatches the resulting argv via `run_command`.

use crate::config::Config;
use crate::observation::TrackedObservation;
use crate::status::Status;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub pane_id: u32,
    pub status: Status,
    pub title: String,
    pub body: String,
}

/// Filesystem-safe identity of one notification *event*, identical across
/// per-tab plugin instances: every instance sees the same broadcast, computes
/// the same edge, and builds the same key — which is what lets a shared claim
/// file elect exactly one dispatcher (`SessionFiles::claim_notification`).
/// The text hash keeps two different messages on the same pane+status (e.g. a
/// second question) as distinct events.
pub fn claim_key(n: &Notification) -> String {
    format!("p{}.{}.{:08x}", n.pane_id, n.status.as_wire(), fnv1a(&n.title, &n.body))
}

/// FNV-1a over title+body. Stability across *builds* is irrelevant — every
/// instance in a session runs the same wasm — it only has to be deterministic
/// within one, which a hand-rolled FNV is (unlike `DefaultHasher`'s seeds).
fn fnv1a(title: &str, body: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for b in title.bytes().chain([0u8]).chain(body.bytes()) {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// User-facing phrase per attention status. `Pending` reads as "needs input"
/// (cmux parity); non-attention statuses have no phrase and never notify.
fn phrase(status: Status) -> Option<&'static str> {
    match status {
        Status::Done => Some("done"),
        Status::Error => Some("error"),
        Status::Pending => Some("needs input"),
        Status::Running | Status::Idle => None,
    }
}

/// Whether this status's per-status toggle (and the master switch) is enabled.
fn enabled(status: Status, cfg: &Config) -> bool {
    cfg.notify
        && match status {
            Status::Done => cfg.notify_done,
            Status::Error => cfg.notify_error,
            Status::Pending => cfg.notify_pending,
            Status::Running | Status::Idle => false,
        }
}

fn build(pane_id: u32, o: &TrackedObservation, status: Status) -> Notification {
    let title = if o.branch.is_empty() {
        o.repo.clone()
    } else {
        format!("{} · {}", o.repo, o.branch)
    };
    let phrase = phrase(status).unwrap_or("");
    let body = if o.msg.is_empty() {
        phrase.to_string()
    } else {
        format!("{phrase} — {}", o.msg)
    };
    Notification { pane_id, status, title, body }
}

/// Emit a notification for each pane that transitioned INTO an attention status
/// since `prev`, is not the focused pane (unless `notify_when_focused`), and whose
/// status toggle is enabled. A pane absent from `prev` is treated as `Idle`.
pub fn diff(
    prev: &BTreeMap<u32, Status>,
    current: &BTreeMap<u32, &TrackedObservation>,
    focused: Option<u32>,
    cfg: &Config,
) -> Vec<Notification> {
    let mut out = Vec::new();
    for (&pane_id, &o) in current {
        let new = o.status;
        let was = prev.get(&pane_id).copied().unwrap_or(Status::Idle);
        if !new.needs_attention() || new == was {
            continue;
        }
        if !enabled(new, cfg) {
            continue;
        }
        if focused == Some(pane_id) && !cfg.notify_when_focused {
            continue;
        }
        out.push(build(pane_id, o, new));
    }
    out
}

/// The next baseline: the current observable status of every live pane.
pub fn status_map(current: &BTreeMap<u32, &TrackedObservation>) -> BTreeMap<u32, Status> {
    current.iter().map(|(&id, &o)| (id, o.status)).collect()
}

/// `run_command` argv that shows a desktop notification on the host — portably,
/// without the plugin (wasm) knowing which OS it runs on: the host `sh` does the
/// dispatch. Prefer macOS `osascript` (so existing macOS behaviour is unchanged),
/// else Linux `notify-send` (libnotify). If neither is on `PATH` the command is a
/// silent no-op, matching the best-effort-cosmetic contract.
///
/// Title and body ride as the shell's positional parameters (`$1`/`$2`), never
/// interpolated into the script, so arbitrary notification text needs no escaping
/// and cannot break out of the command. `osascript` receives them as its own
/// `argv` (`on run argv`) for the same reason — retiring the old hand-rolled
/// AppleScript string escaper. The `--` guards a title/body that begins with `-`.
pub fn notify_command(title: &str, body: &str) -> Vec<String> {
    // $0 is a label; $1 = title, $2 = body.
    const DISPATCH: &str = concat!(
        "if command -v osascript >/dev/null 2>&1; then ",
        "exec osascript -e 'on run argv' ",
        "-e 'display notification (item 2 of argv) with title (item 1 of argv)' ",
        "-e 'end run' -- \"$1\" \"$2\"; ",
        "elif command -v notify-send >/dev/null 2>&1; then ",
        "exec notify-send -- \"$1\" \"$2\"; ",
        "fi",
    );
    vec![
        "sh".to_string(),
        "-c".to_string(),
        DISPATCH.to_string(),
        "zj-radar".to_string(), // $0
        title.to_string(),      // $1
        body.to_string(),       // $2
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(status: Status, repo: &str, branch: &str, msg: &str) -> TrackedObservation {
        let mut o = TrackedObservation::command(status, repo.to_string(), msg.to_string(), crate::kind::Kind::Other, 1);
        o.branch = branch.to_string();
        o
    }

    fn current<'a>(pairs: &'a [(u32, &'a TrackedObservation)]) -> BTreeMap<u32, &'a TrackedObservation> {
        pairs.iter().copied().collect()
    }

    #[test]
    fn background_done_notifies_once() {
        let o = obs(Status::Done, "pinky", "main", "cargo build");
        let pairs = [(7, &o)];
        let cur = current(&pairs);
        let prev = BTreeMap::from([(7, Status::Running)]);
        let n = diff(&prev, &cur, None, &Config::default());
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].title, "pinky · main");
        assert_eq!(n[0].body, "done — cargo build");
    }

    #[test]
    fn no_edge_when_status_unchanged() {
        let o = obs(Status::Done, "pinky", "main", "cargo build");
        let pairs = [(7, &o)];
        let cur = current(&pairs);
        let prev = BTreeMap::from([(7, Status::Done)]);
        assert!(diff(&prev, &cur, None, &Config::default()).is_empty());
    }

    #[test]
    fn focused_pane_suppressed_by_default() {
        let o = obs(Status::Pending, "pinky", "main", "needs you");
        let pairs = [(7, &o)];
        let cur = current(&pairs);
        let prev = BTreeMap::from([(7, Status::Running)]);
        // focused == 7 → suppressed
        assert!(diff(&prev, &cur, Some(7), &Config::default()).is_empty());
        // a different focused pane does not suppress
        assert_eq!(diff(&prev, &cur, Some(9), &Config::default()).len(), 1);
    }

    #[test]
    fn notify_when_focused_overrides_gate() {
        let o = obs(Status::Done, "pinky", "main", "build");
        let pairs = [(7, &o)];
        let cur = current(&pairs);
        let prev = BTreeMap::from([(7, Status::Running)]);
        let cfg = Config { notify_when_focused: true, ..Config::default() };
        assert_eq!(diff(&prev, &cur, Some(7), &cfg).len(), 1);
    }

    #[test]
    fn per_status_toggle_and_master_switch() {
        let o = obs(Status::Error, "pinky", "main", "boom");
        let pairs = [(7, &o)];
        let cur = current(&pairs);
        let prev = BTreeMap::from([(7, Status::Running)]);
        let no_error = Config { notify_error: false, ..Config::default() };
        assert!(diff(&prev, &cur, None, &no_error).is_empty());
        let off = Config { notify: false, ..Config::default() };
        assert!(diff(&prev, &cur, None, &off).is_empty());
    }

    #[test]
    fn notify_done_false_suppresses_running_to_done_edge() {
        let o = obs(Status::Done, "pinky", "main", "cargo build");
        let pairs = [(7, &o)];
        let cur = current(&pairs);
        let prev = BTreeMap::from([(7, Status::Running)]);
        let cfg = Config { notify_done: false, ..Config::default() };
        assert!(
            diff(&prev, &cur, None, &cfg).is_empty(),
            "notify_done:false must suppress a Running→Done edge"
        );
    }

    #[test]
    fn notify_pending_false_suppresses_running_to_pending_edge() {
        let o = obs(Status::Pending, "pinky", "main", "needs you");
        let pairs = [(7, &o)];
        let cur = current(&pairs);
        let prev = BTreeMap::from([(7, Status::Running)]);
        let cfg = Config { notify_pending: false, ..Config::default() };
        assert!(
            diff(&prev, &cur, None, &cfg).is_empty(),
            "notify_pending:false must suppress a Running→Pending edge"
        );
    }

    #[test]
    fn running_and_idle_never_notify() {
        let r = obs(Status::Running, "pinky", "main", "work");
        let i = obs(Status::Idle, "pinky", "main", "");
        let pairs = [(7, &r), (8, &i)];
        let cur = current(&pairs);
        let prev = BTreeMap::from([(7, Status::Idle), (8, Status::Done)]);
        assert!(diff(&prev, &cur, None, &Config::default()).is_empty());
    }

    #[test]
    fn new_pane_with_no_prev_uses_idle_baseline() {
        // A pane absent from prev is treated as having been Idle → a fresh Done notifies.
        let o = obs(Status::Done, "pinky", "main", "build");
        let pairs = [(7, &o)];
        let cur = current(&pairs);
        let prev = BTreeMap::new();
        assert_eq!(diff(&prev, &cur, None, &Config::default()).len(), 1);
    }

    #[test]
    fn body_without_msg_is_status_phrase_only() {
        let mut o = obs(Status::Pending, "pinky", "", "");
        o.msg = String::new();
        let pairs = [(7, &o)];
        let cur = current(&pairs);
        let prev = BTreeMap::from([(7, Status::Running)]);
        let n = diff(&prev, &cur, None, &Config::default());
        assert_eq!(n[0].title, "pinky");
        assert_eq!(n[0].body, "needs input");
    }

    #[test]
    fn status_map_extracts_statuses() {
        let a = obs(Status::Done, "r", "b", "m");
        let pairs = [(7, &a)];
        let cur = current(&pairs);
        assert_eq!(status_map(&cur), BTreeMap::from([(7, Status::Done)]));
    }

    #[test]
    fn claim_key_is_deterministic_and_distinguishes_events() {
        let o = obs(Status::Pending, "pinky", "main", "approve git push?");
        let pairs = [(7, &o)];
        let cur = current(&pairs);
        let prev = BTreeMap::from([(7, Status::Running)]);
        let n = diff(&prev, &cur, None, &Config::default());
        let key = claim_key(&n[0]);
        // Deterministic (what makes the cross-instance election work) and
        // filesystem-safe (it becomes a /cache claim filename).
        assert_eq!(key, claim_key(&n[0]));
        assert!(key.starts_with("p7.pending."));
        assert!(key.chars().all(|c| c.is_ascii_alphanumeric() || c == '.'));

        // A different question on the same pane+status is a different event.
        let o2 = obs(Status::Pending, "pinky", "main", "approve rm -rf?");
        let pairs2 = [(7, &o2)];
        let cur2 = current(&pairs2);
        let n2 = diff(&prev, &cur2, None, &Config::default());
        assert_ne!(key, claim_key(&n2[0]));
    }

    #[test]
    fn notify_command_dispatches_both_backends_with_positional_args() {
        let argv = notify_command("pinky · main", "done — build");
        assert_eq!(argv[0], "sh");
        assert_eq!(argv[1], "-c");
        // The host script tries macOS first, then the Linux fallback.
        assert!(argv[2].contains("osascript"), "macOS branch present: {}", argv[2]);
        assert!(argv[2].contains("notify-send"), "Linux fallback present: {}", argv[2]);
        // Title/body ride as $1/$2 — argv[4]/argv[5], after the $0 label.
        assert_eq!(argv[3], "zj-radar");
        assert_eq!(argv[4], "pinky · main");
        assert_eq!(argv[5], "done — build");
    }

    #[test]
    fn notify_command_passes_hostile_text_verbatim_without_escaping() {
        // Quotes, backslashes, `$(…)`, `;`, and a leading dash are passed as argv,
        // never spliced into the script, so nothing can inject into or break out
        // of the command (the reason the old string-literal escaper is gone). The
        // `--` in the script is what protects a leading-dash title/body.
        let argv = notify_command("-rf \"$(reboot)\"", "a\\b\" ; rm -rf /");
        assert_eq!(argv[4], "-rf \"$(reboot)\"");
        assert_eq!(argv[5], "a\\b\" ; rm -rf /");
        // The payload text never appears in the script body itself.
        assert!(!argv[2].contains("reboot"));
        assert!(!argv[2].contains("rm -rf"));
    }
}
