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
    source: String,
}
// Note: the retired clear-on-focus hint key is silently ignored (serde drops
// unknown fields) — no longer consumed, kept tolerated on the wire for back-compat
// with older producers, exactly like the legacy `seq`.

/// Unicode bidi format/override characters. These can visually reorder or hide
/// surrounding text (the "Trojan Source" class) — e.g. an RLO (U+202E) in a
/// `repo`/`branch`/`msg` field could make a rail row read differently than its
/// bytes. They have no legitimate use in the short display strings we render, so
/// they are dropped. `is_control()` does NOT cover them (they are `Cf`, format
/// characters, not `Cc`). The zero-width JOINER (U+200D) is deliberately NOT here:
/// it is load-bearing for emoji sequences, and `render::truncate` already handles
/// a stranded trailing ZWJ.
fn is_bidi_control(c: char) -> bool {
    matches!(c,
        '\u{202A}'..='\u{202E}'   // LRE RLE PDF LRO RLO (embeddings + overrides)
        | '\u{2066}'..='\u{2069}' // LRI RLI FSI PDI (isolates)
        | '\u{200E}' | '\u{200F}' // LRM RLM (marks)
        | '\u{061C}'              // ALM (Arabic letter mark)
    )
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
                    // OSC: consume until BEL (0x07) or ST (ESC \). A well-formed
                    // OSC string is text + terminator with no bare control bytes,
                    // so ANY other C0 control (newline, a fresh ESC, …) ends a
                    // malformed/unterminated OSC WITHOUT being consumed — the outer
                    // loop reprocesses it. This bounds an unterminated OSC: it used
                    // to run to end-of-input and swallow the whole field, blanking
                    // it. (A fully-printable OSC with no terminator anywhere still
                    // consumes to the end — matching how a real terminal waits for a
                    // terminator — but that is the only remaining swallow case.)
                    i += 2;
                    while i < bytes.len() {
                        let b = bytes[i];
                        if b == 0x07 {
                            i += 1; // BEL terminator consumed
                            break;
                        }
                        if b == 0x1b && bytes.get(i + 1).copied() == Some(b'\\') {
                            i += 2; // ST terminator consumed
                            break;
                        }
                        if b < 0x20 {
                            break; // stray control ends the OSC; leave it for the outer loop
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
            // Normal byte — reconstruct as char (handle multi-byte UTF-8).
            // Use `get` instead of direct indexing: ESC-sequence scanners advance
            // byte-by-byte and can leave `i` on a UTF-8 continuation byte (e.g.
            // "\x1b" + lead-byte of a 2-byte char consumed as "ESC + one byte").
            // If `i` is not on a char boundary we skip the stray continuation byte.
            // Also drop Unicode C1 control chars (U+0080–U+009F) whose lead byte
            // (0xC2) passes the ASCII-range check above but are still control chars,
            // and bidi format/override chars (`is_bidi_control`) that could visually
            // reorder or hide rail text (Trojan-Source-style spoofing).
            match s.get(i..) {
                Some(remaining) => match remaining.chars().next() {
                    Some(c) if !c.is_control() && !is_bidi_control(c) => {
                        cleaned.push(c);
                        i += c.len_utf8();
                    }
                    Some(c) => {
                        i += c.len_utf8();
                    }
                    None => {
                        i += 1;
                    }
                },
                None => {
                    i += 1;
                }
            }
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
        source: sanitize(&r.source, 16),
    })
}

/// Serialized form of the `zj_radar.status.v1` payload — the producer mirror of
/// the `Raw` parse struct. `status` serializes through `Status`'s own wire
/// vocabulary, so the produce and parse directions share one token set.
#[derive(serde::Serialize)]
struct Wire<'a> {
    v: u32,
    source: &'a str,
    pane: WirePane,
    status: Status,
    repo: &'a str,
    branch: &'a str,
    msg: &'a str,
}

#[derive(serde::Serialize)]
struct WirePane {
    #[serde(rename = "type")]
    kind: &'static str,
    id: u32,
}

/// Build a `zj_radar.status.v1` JSON payload (inverse of `parse`). Shared by the
/// CLI producer and tested against `parse` so the two can never drift.
pub fn to_wire(
    pane_id: u32,
    status: Status,
    repo: &str,
    branch: &str,
    msg: &str,
    source: &str,
) -> String {
    serde_json::to_string(&Wire {
        v: 1,
        source,
        pane: WirePane {
            kind: "terminal",
            id: pane_id,
        },
        status,
        repo,
        branch,
        msg,
    })
    .expect("status payload of plain fields always serializes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn p(s: &str) -> Option<StatusPayload> {
        parse(s)
    }

    #[test]
    fn parses_full_payload() {
        // The trailing "seq":42 is an unknown field now (seq was removed) — it
        // must be ignored, not rejected, so older producers stay compatible.
        let got = p(r#"{"v":1,"source":"claude","pane":{"type":"terminal","id":12},"status":"running","repo":"pinky","branch":"fix/x","msg":"running tests","seq":42}"#).unwrap();
        assert_eq!(got.pane_id, 12);
        assert_eq!(got.status, Status::Running);
        assert_eq!(got.repo, "pinky");
    }

    #[test]
    fn missing_optionals_default() {
        let got = p(r#"{"pane":{"type":"terminal","id":3},"status":"done"}"#).unwrap();
        assert_eq!(got.pane_id, 3);
        assert_eq!(got.status, Status::Done);
        assert_eq!(got.repo, "");
    }

    #[test]
    fn rejects_non_terminal_and_garbage_and_oversize() {
        assert!(p(r#"{"pane":{"type":"plugin","id":1},"status":"done"}"#).is_none());
        assert!(p("not json").is_none());
        let big = format!(
            r#"{{"pane":{{"type":"terminal","id":1}},"status":"done","msg":"{}"}}"#,
            "x".repeat(MAX_PAYLOAD_BYTES)
        );
        assert!(p(&big).is_none());
    }

    #[test]
    fn unknown_status_is_idle() {
        let got = p(r#"{"pane":{"type":"terminal","id":1},"status":"whatever"}"#).unwrap();
        assert_eq!(got.status, Status::Idle);
    }

    #[test]
    fn truncates_each_field_to_its_own_cap() {
        // Each text field has a distinct cap (repo/branch 40, msg MAX_MSG_CHARS,
        // source 16). Pin all four so nudging any cap in `parse` is caught here —
        // the wire boundary is the only thing standing between a hostile producer
        // and an unbounded row.
        let json = format!(
            r#"{{"pane":{{"type":"terminal","id":1}},"status":"running","repo":"{r}","branch":"{b}","msg":"{m}","source":"{s}"}}"#,
            r = "r".repeat(100),
            b = "b".repeat(100),
            m = "m".repeat(200),
            s = "s".repeat(100),
        );
        let got = p(&json).unwrap();
        assert_eq!(got.repo.chars().count(), 40);
        assert_eq!(got.branch.chars().count(), 40);
        assert_eq!(got.msg.chars().count(), MAX_MSG_CHARS);
        assert_eq!(got.source.chars().count(), 16);
    }

    #[test]
    fn parses_pane_id_boundaries() {
        // 0 and u32::MAX are both valid pane ids — neither overflows nor is special.
        assert_eq!(
            p(r#"{"pane":{"type":"terminal","id":0},"status":"done"}"#).unwrap().pane_id,
            0
        );
        assert_eq!(
            p(r#"{"pane":{"type":"terminal","id":4294967295},"status":"done"}"#).unwrap().pane_id,
            u32::MAX
        );
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
    fn sanitize_unterminated_osc_is_bounded_by_a_control() {
        // An OSC with no BEL/ST terminator used to run to end-of-input and swallow
        // the whole field. A stray control (here a newline) now ends it, so the
        // trailing text survives (the newline folds to a space).
        assert_eq!(sanitize("\x1b]0;title\nreal text", 100), " real text");
        // A fresh ESC-introduced CSI inside an unterminated OSC also ends it; the
        // CSI is then stripped and its trailing text survives.
        assert_eq!(sanitize("\x1b]0;title\x1b[0mkept", 100), "kept");
    }

    #[test]
    fn sanitize_fully_printable_unterminated_osc_still_drops() {
        // The one remaining swallow case: an OSC with no terminator AND no control
        // byte anywhere. A real terminal also waits indefinitely for a terminator,
        // so dropping (rather than emitting the raw OSC payload as text) is the
        // defensive choice. Pinned so the bound above isn't mistaken for a full fix.
        assert_eq!(sanitize("\x1b]0;title with no terminator", 100), "");
    }

    #[test]
    fn sanitize_strips_bidi_controls_but_keeps_zwj() {
        // Trojan-Source-style bidi overrides/marks/isolates are dropped so a
        // producer can't visually reorder a rail row (e.g. spoof "error" as "done").
        assert_eq!(sanitize("abc\u{202e}def", 100), "abcdef"); // RLO
        assert_eq!(sanitize("a\u{200f}b\u{200e}c", 100), "abc"); // RLM / LRM
        assert_eq!(sanitize("x\u{2066}y\u{2069}z", 100), "xyz"); // LRI / PDI isolates
        // The zero-width JOINER is preserved — it is load-bearing for emoji.
        let zwj = "👩\u{200d}💻";
        assert_eq!(sanitize(zwj, 100), zwj);
    }

    #[test]
    fn to_wire_round_trips_through_parse() {
        use crate::status::Status;
        let json = to_wire(
            12,
            Status::Running,
            "pinky",
            "fix/x",
            "running tests",
            "claude",
        );
        let got = parse(&json).expect("to_wire output must parse");
        assert_eq!(got.pane_id, 12);
        assert_eq!(got.status, Status::Running);
        assert_eq!(got.repo, "pinky");
        assert_eq!(got.branch, "fix/x");
        assert_eq!(got.msg, "running tests");
        assert_eq!(got.source, "claude");
    }

    #[test]
    fn as_wire_round_trips_for_all_statuses() {
        use crate::status::Status;
        for &s in Status::ALL {
            assert_eq!(Status::from_wire(s.as_wire()), s);
        }
    }

    // ── defense-in-depth tests (ported from harness branch) ──

    #[test]
    fn rejects_oversized_payload() {
        let big = format!(
            r#"{{"v":1,"pane":{{"type":"terminal","id":1}},"status":"running","msg":"{}"}}"#,
            "x".repeat(MAX_PAYLOAD_BYTES)
        );
        assert!(parse(&big).is_none());
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(parse("{not json").is_none());
        assert!(parse("").is_none());
        assert!(parse("null").is_none());
    }

    #[test]
    fn sanitize_strips_control_and_truncates() {
        let dirty = "\x1b[31mred\x07\nbeep\ttab";
        let clean = sanitize(dirty, MAX_MSG_CHARS);
        assert!(!clean.contains('\x1b'));
        assert!(!clean.contains('\x07'));
        assert!(!clean.contains('\n'));
        assert!(!clean.contains('\t'), "tab should be folded to space");
        assert!(clean.chars().count() <= MAX_MSG_CHARS);
    }

    proptest::proptest! {
        #[test]
        fn sanitize_never_emits_control_or_overlong(input in ".{0,500}", max in 1usize..120) {
            let out = sanitize(&input, max);
            prop_assert!(out.chars().count() <= max, "len {} > max {}", out.chars().count(), max);
            for ch in out.chars() {
                prop_assert!(ch != '\x1b', "ESC leaked");
                prop_assert!(!ch.is_control(), "control char leaked: {:?}", ch);
            }
        }

        #[test]
        fn parse_to_wire_round_trip(
            pane in any::<u32>(),
            status in proptest::sample::select(Status::ALL.to_vec()),
            repo in "[a-z]{0,15}",
            branch in "[a-z/]{0,15}",
            msg in "[a-zA-Z0-9 ]{0,40}",
            source in "[a-z]{0,12}",
        ) {
            // to_wire and parse must be inverses: a round-trip through the wire
            // format must preserve EVERY field parse surfaces — across all statuses,
            // the full pane-id range, and msg/source (the fields the old version
            // silently dropped). Only printable ASCII within each field's cap is
            // generated, so sanitize does not alter any field.
            let wire = to_wire(pane, status, &repo, &branch, &msg, &source);
            let got = parse(&wire).expect("our own wire output must parse");
            prop_assert_eq!(got.pane_id, pane);
            prop_assert_eq!(got.status, status);
            prop_assert_eq!(got.repo, repo);
            prop_assert_eq!(got.branch, branch);
            prop_assert_eq!(got.msg, msg);
            prop_assert_eq!(got.source, source);
        }

        #[test]
        fn parse_never_panics_on_arbitrary_input(raw in ".{0,2000}") {
            // parse is the untrusted-input boundary: whatever a producer broadcasts,
            // it must resolve to Some/None, never panic (no slice on a non-char
            // boundary, no overflow). The result is intentionally ignored.
            let _ = parse(&raw);
        }
    }
}
