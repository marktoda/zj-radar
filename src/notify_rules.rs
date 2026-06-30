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
    pub title: String,
    pub body: String,
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

fn build(o: &TrackedObservation, status: Status) -> Notification {
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
    Notification { title, body }
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
        out.push(build(o, new));
    }
    out
}

/// The next baseline: the current observable status of every live pane.
pub fn status_map(current: &BTreeMap<u32, &TrackedObservation>) -> BTreeMap<u32, Status> {
    current.iter().map(|(&id, &o)| (id, o.status)).collect()
}

/// `run_command` argv that fires a macOS notification. AppleScript string
/// literals escape `\` and `"`; everything else passes through.
pub fn osascript_command(n: &Notification) -> Vec<String> {
    fn quote(s: &str) -> String {
        let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    }
    let script = format!(
        "display notification {} with title {}",
        quote(&n.body),
        quote(&n.title)
    );
    vec!["osascript".to_string(), "-e".to_string(), script]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(status: Status, repo: &str, branch: &str, msg: &str) -> TrackedObservation {
        let mut o = TrackedObservation::command(status, repo.to_string(), msg.to_string(), "shell".to_string(), 1);
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
    fn osascript_command_quotes_and_escapes() {
        let n = Notification { title: "re\"po".to_string(), body: "a\\b".to_string() };
        let argv = osascript_command(&n);
        assert_eq!(argv[0], "osascript");
        assert_eq!(argv[1], "-e");
        // body and title become AppleScript double-quoted literals with \ and " escaped
        assert_eq!(argv[2], "display notification \"a\\\\b\" with title \"re\\\"po\"");
    }
}
