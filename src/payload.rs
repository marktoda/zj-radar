//! Parse + sanitize the zj_radar.status.v1 pipe payload. No zellij-tile dependency.

use crate::status::Status;
use serde::Deserialize;

pub const MAX_PAYLOAD_BYTES: usize = 65536;
pub const MAX_MSG_CHARS: usize = 60;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusPayload {
    pub pane_id: u32,
    pub status: Status,
    pub repo: String,
    pub branch: String,
    pub msg: String,
    pub on_focus: Option<Status>,
    pub seq: Option<u64>,
    pub source: String,
}

#[derive(Deserialize)]
struct RawPane {
    #[serde(rename = "type")]
    kind: String,
    id: u32,
}

#[derive(Deserialize)]
struct Raw {
    pane: RawPane,
    status: String,
    #[serde(default)]
    repo: String,
    #[serde(default)]
    branch: String,
    #[serde(default)]
    msg: String,
    #[serde(default)]
    on_focus: Option<String>,
    #[serde(default)]
    seq: Option<u64>,
    #[serde(default)]
    source: String,
}

/// Strip control/ANSI chars, fold newlines to spaces, truncate to `max_chars`.
pub fn sanitize(s: &str, max_chars: usize) -> String {
    let mut cleaned = String::new();
    let mut in_ansi = false;
    for c in s.chars() {
        if c == '\u{1b}' {
            in_ansi = true;
        } else if in_ansi {
            if c.is_alphabetic() {
                in_ansi = false;
            }
        } else if c == '\n' || c == '\t' {
            cleaned.push(' ');
        } else if !c.is_control() {
            cleaned.push(c);
        }
    }
    cleaned.chars().take(max_chars).collect()
}

/// Parse a broadcast payload. Returns None on oversize, invalid JSON, or a
/// non-terminal pane. Unknown status maps to Idle (never errors).
pub fn parse(raw: &str) -> Option<StatusPayload> {
    if raw.len() > MAX_PAYLOAD_BYTES {
        return None;
    }
    let r: Raw = serde_json::from_str(raw).ok()?;
    if r.pane.kind != "terminal" {
        return None;
    }
    Some(StatusPayload {
        pane_id: r.pane.id,
        status: Status::from_wire(&r.status),
        repo: sanitize(&r.repo, 40),
        branch: sanitize(&r.branch, 40),
        msg: sanitize(&r.msg, MAX_MSG_CHARS),
        on_focus: r.on_focus.as_deref().map(Status::from_wire),
        seq: r.seq,
        source: sanitize(&r.source, 16),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Option<StatusPayload> {
        parse(s)
    }

    #[test]
    fn parses_full_payload() {
        let got = p(r#"{"v":1,"source":"claude","pane":{"type":"terminal","id":12},"status":"running","repo":"pinky","branch":"fix/x","msg":"running tests","on_focus":"idle","seq":42}"#).unwrap();
        assert_eq!(got.pane_id, 12);
        assert_eq!(got.status, Status::Running);
        assert_eq!(got.repo, "pinky");
        assert_eq!(got.on_focus, Some(Status::Idle));
        assert_eq!(got.seq, Some(42));
    }

    #[test]
    fn missing_optionals_default() {
        let got = p(r#"{"pane":{"type":"terminal","id":3},"status":"done"}"#).unwrap();
        assert_eq!(got.pane_id, 3);
        assert_eq!(got.status, Status::Done);
        assert_eq!(got.repo, "");
        assert_eq!(got.on_focus, None);
        assert_eq!(got.seq, None);
    }

    #[test]
    fn rejects_non_terminal_and_garbage_and_oversize() {
        assert!(p(r#"{"pane":{"type":"plugin","id":1},"status":"done"}"#).is_none());
        assert!(p("not json").is_none());
        let big = format!(r#"{{"pane":{{"type":"terminal","id":1}},"status":"done","msg":"{}"}}"#, "x".repeat(MAX_PAYLOAD_BYTES));
        assert!(p(&big).is_none());
    }

    #[test]
    fn unknown_status_is_idle() {
        let got = p(r#"{"pane":{"type":"terminal","id":1},"status":"whatever"}"#).unwrap();
        assert_eq!(got.status, Status::Idle);
    }

    #[test]
    fn sanitize_strips_control_newlines_ansi_and_truncates() {
        let dirty = "a\nb\t\x1b[31mc\x07";
        assert_eq!(sanitize(dirty, 10), "a b c");
        assert_eq!(sanitize("abcdef", 3), "abc");
    }
}
