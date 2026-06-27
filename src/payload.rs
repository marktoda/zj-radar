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

/// Strip control/ANSI chars, fold newlines/tabs/CR to spaces, truncate to `max_chars`.
///
/// Stripped sequences:
/// - CSI (`\x1b[` … final byte in 0x40–0x7E)
/// - OSC (`\x1b]` … terminated by BEL `\x07` or ST `\x1b\`)
/// - Any other ESC-introduced 2-byte sequence (`\x1b` + one byte)
/// - C0 control chars (0x00–0x1F) — `\n`, `\t`, `\r` become a single space; all others dropped
/// - DEL (0x7F) — dropped
pub fn sanitize(s: &str, max_chars: usize) -> String {
    let mut cleaned = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1b {
            // ESC — decide what kind of sequence follows
            let next = bytes.get(i + 1).copied();
            match next {
                Some(b'[') => {
                    // CSI: consume until a byte in 0x40–0x7E (inclusive)
                    i += 2;
                    while i < bytes.len() {
                        let fb = bytes[i];
                        i += 1;
                        if (0x40..=0x7e).contains(&fb) {
                            break; // final byte consumed
                        }
                    }
                }
                Some(b']') => {
                    // OSC: consume until BEL (0x07) or ST (ESC \)
                    i += 2;
                    while i < bytes.len() {
                        if bytes[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if bytes[i] == 0x1b && bytes.get(i + 1).copied() == Some(b'\\') {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                Some(_) => {
                    // Any other ESC + one byte: skip both
                    i += 2;
                }
                None => {
                    // Lone ESC at end of string: skip it
                    i += 1;
                }
            }
        } else if b == b'\n' || b == b'\t' || b == b'\r' {
            cleaned.push(' ');
            i += 1;
        } else if b < 0x20 || b == 0x7f {
            // Other C0 control chars and DEL: drop
            i += 1;
        } else {
            // Normal byte — reconstruct as char (handle multi-byte UTF-8)
            // Find the char boundary and push the whole char
            let ch_len = {
                let remaining = &s[i..];
                remaining.chars().next().map(|c| c.len_utf8()).unwrap_or(1)
            };
            // Safety: we know `s` is valid UTF-8 and `i` is a char boundary
            // (we only advance by 1 for single ASCII bytes or by ch_len here)
            if let Some(c) = s[i..].chars().next() {
                cleaned.push(c);
            }
            i += ch_len;
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

/// Build a `zj_radar.status.v1` JSON payload (inverse of `parse`). `on_focus` is
/// omitted entirely when `None`. Shared by the CLI producer and tested against
/// `parse` so the two can never drift.
pub fn to_wire(
    pane_id: u32,
    status: Status,
    repo: &str,
    branch: &str,
    msg: &str,
    on_focus: Option<Status>,
    source: &str,
) -> String {
    let mut obj = serde_json::json!({
        "v": 1,
        "source": source,
        "pane": { "type": "terminal", "id": pane_id },
        "status": status.as_wire(),
        "repo": repo,
        "branch": branch,
        "msg": msg,
    });
    if let Some(f) = on_focus {
        obj["on_focus"] = serde_json::Value::String(f.as_wire().to_string());
    }
    obj.to_string()
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

    // ── hardening tests (FIX 1) ──

    #[test]
    fn sanitize_strips_truecolor_csi() {
        // CSI 38;2;R;G;Bm (truecolor foreground) + reset — only the text survives
        let input = "\x1b[38;2;255;0;0mred\x1b[0m";
        assert_eq!(sanitize(input, 100), "red");
    }

    #[test]
    fn sanitize_strips_osc_bel_terminated() {
        // OSC 0 ; title BEL — the sequence is removed, trailing text survives
        let input = "\x1b]0;evil title\x07ok";
        assert_eq!(sanitize(input, 100), "ok");
    }

    #[test]
    fn sanitize_converts_newline_to_space() {
        assert_eq!(sanitize("a\nb", 100), "a b");
    }

    #[test]
    fn sanitize_drops_bel_control_char() {
        // BEL (0x07) not preceded by ESC+] should be dropped entirely
        assert_eq!(sanitize("a\x07b", 100), "ab");
    }

    #[test]
    fn sanitize_length_cap_still_applies_after_hardening() {
        // A truecolor sequence followed by many chars — cap must still hold
        let input = format!("\x1b[38;2;0;128;0m{}\x1b[0m", "x".repeat(200));
        let out = sanitize(&input, 50);
        assert_eq!(out.chars().count(), 50);
        assert_eq!(out, "x".repeat(50));
    }

    #[test]
    fn to_wire_round_trips_through_parse() {
        use crate::status::Status;
        let json = to_wire(12, Status::Running, "pinky", "fix/x", "running tests", Some(Status::Idle), "claude");
        let got = parse(&json).expect("to_wire output must parse");
        assert_eq!(got.pane_id, 12);
        assert_eq!(got.status, Status::Running);
        assert_eq!(got.repo, "pinky");
        assert_eq!(got.branch, "fix/x");
        assert_eq!(got.msg, "running tests");
        assert_eq!(got.on_focus, Some(Status::Idle));
        assert_eq!(got.source, "claude");
    }

    #[test]
    fn to_wire_omits_on_focus_when_none() {
        use crate::status::Status;
        let json = to_wire(3, Status::Done, "r", "b", "m", None, "codex");
        assert!(!json.contains("on_focus"));
        assert_eq!(parse(&json).unwrap().on_focus, None);
    }

    #[test]
    fn as_wire_round_trips_for_all_statuses() {
        use crate::status::Status;
        for s in [Status::Idle, Status::Running, Status::Pending, Status::Done, Status::Error] {
            assert_eq!(Status::from_wire(s.as_wire()), s);
        }
    }
}
