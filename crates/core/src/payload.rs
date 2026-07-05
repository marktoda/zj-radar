//! Parse + sanitize the zj_radar.status.v1 pipe payload. No zellij-tile dependency.

use crate::status::Status;
use serde::Deserialize;

/// Public-contract limit: payloads larger than this are rejected outright by
/// `parse` (returns `None`) before JSON parsing is even attempted.
pub const MAX_PAYLOAD_BYTES: usize = 65536;
/// Public-contract limit: `msg` truncates to this many chars on parse.
pub const MAX_MSG_CHARS: usize = 60;
/// Public-contract limit: `task` truncates to this many chars on parse.
pub const MAX_TASK_CHARS: usize = 60;
/// Public-contract limit: `repo` truncates to this many chars on parse.
pub const MAX_REPO_CHARS: usize = 40;
/// Public-contract limit: `branch` truncates to this many chars on parse.
pub const MAX_BRANCH_CHARS: usize = 40;
/// Intake cap for tab names and pane titles. Not a pipe-payload field — these
/// arrive from the Zellij host (tab/pane updates) and the plugin sanitizes
/// them to this many chars at intake, the same discipline as the wire caps
/// above (and the same 40 as `repo`/`branch`, the other row-column strings).
pub const MAX_TAB_NAME_CHARS: usize = 40;
/// Public-contract limit: `source` truncates to this many chars on parse.
pub const MAX_SOURCE_CHARS: usize = 16;

/// The versioned pipe name that binds every producer to the plugin — the one
/// string that must never drift between them. The pipe *name* carries the
/// contract version: a breaking change means a new name (`…v2`), so old plugins
/// simply never see payloads they can't parse. The payload's `v` field is
/// informational only: [`to_wire`] stamps it, `parse` ignores it.
pub const STATUS_PIPE_NAME: &str = "zj_radar.status.v1";
/// Stamped into the payload's `v` field by [`to_wire`]; never read on parse.
pub const STATUS_VERSION: u32 = 1;

/// One `zj_radar.status.v1` broadcast, parsed and sanitized — what a producer
/// says about one pane. Producers build one of these (use
/// `..Default::default()` for anything not set — it means exactly "absent on
/// the wire") and serialize it with [`to_wire`]; the plugin gets it back from
/// [`parse`], which sanitizes and caps every text field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusPayload {
    /// The Zellij terminal pane the status is about (the wire's
    /// `pane: {"type": "terminal", "id": N}` — `parse` rejects non-terminal
    /// panes). Required on the wire.
    pub pane_id: u32,
    /// The pane's agent status. Required on the wire (a missing field is a
    /// parse error), but lenient in value: an unknown or empty token parses
    /// as `Status::Idle`.
    pub status: Status,
    /// Repository name shown on the pane's row. Optional; sanitized and
    /// capped at [`MAX_REPO_CHARS`] on parse.
    pub repo: String,
    /// VCS branch, rendered next to `repo`. Optional; sanitized and capped at
    /// [`MAX_BRANCH_CHARS`] on parse.
    pub branch: String,
    /// One-line activity message ("running tests", the agent's question, …).
    /// Optional; sanitized and capped at [`MAX_MSG_CHARS`] on parse.
    pub msg: String,
    /// Sticky task label (first line of the user's prompt). Wire semantics:
    /// empty/absent = leave the pane's stored task unchanged; non-empty =
    /// replace. Clearing is a plugin lifecycle rule, never a wire signal.
    /// Sanitized and capped at [`MAX_TASK_CHARS`] on parse.
    pub task: String,
    /// Producer identity token (`"claude"`, `"codex"`, …) — the plugin maps it
    /// through `Kind::from_source`, so unknown sources still render (as
    /// `Other`). Optional; sanitized and capped at [`MAX_SOURCE_CHARS`].
    pub source: String,
}

/// Mirrors what `parse` produces for a payload with every optional field
/// absent: every optional text field defaults to `""` (an absent field's
/// serde default on `Raw`, sanitized), and the two *required* fields take
/// their degenerate values — `pane_id` is `0` (the primitive default
/// `RawPane.id` would take) and `status` is the fallback (`Status::Idle`)
/// that `Status::from_wire` gives an unknown or empty token. (`status` cannot
/// actually be absent — a missing field is a hard parse error — which is why
/// the paired test drives this with `"status":""`.) So
/// `..Default::default()` in a struct literal means exactly "absent on the
/// wire" — this is the forward-compatible construction path for external
/// producers: a future field addition to `StatusPayload` defaults the same
/// way an absent wire field would, so existing `..Default::default()`
/// literals keep compiling and keep meaning the same thing.
impl Default for StatusPayload {
    fn default() -> Self {
        StatusPayload {
            pane_id: 0,
            status: Status::default(),
            repo: String::new(),
            branch: String::new(),
            msg: String::new(),
            task: String::new(),
            source: String::new(),
        }
    }
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
    // `Status`'s Deserialize is the lenient wire_serde path (unknown → Idle),
    // so typing the field here IS the "unknown status maps to Idle" policy —
    // no hand-written `from_wire` step. A *missing* status stays a parse error.
    status: Status,
    #[serde(default)]
    repo: String,
    #[serde(default)]
    branch: String,
    #[serde(default)]
    msg: String,
    #[serde(default)]
    task: String,
    #[serde(default)]
    source: String,
}
// Note: the retired clear-on-focus hint key is silently ignored (serde drops
// unknown fields) — no longer consumed, kept tolerated on the wire for back-compat
// with older producers, exactly like the legacy `seq`. `v` is the same story
// but by design, not retirement: `Raw` deliberately has no `v` field, so serde
// drops it unread. The pipe NAME (`STATUS_PIPE_NAME`) is the version authority;
// `to_wire` stamps `v` for human/debug legibility, but any value there — old,
// new, or garbage — parses identically. See `parses_ignores_v_field_value`.

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
        status: r.status,
        repo: sanitize(&r.repo, MAX_REPO_CHARS),
        branch: sanitize(&r.branch, MAX_BRANCH_CHARS),
        msg: sanitize(&r.msg, MAX_MSG_CHARS),
        task: sanitize(&r.task, MAX_TASK_CHARS),
        source: sanitize(&r.source, MAX_SOURCE_CHARS),
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
    task: &'a str,
}

#[derive(serde::Serialize)]
struct WirePane {
    #[serde(rename = "type")]
    kind: &'static str,
    id: u32,
}

/// Build a `zj_radar.status.v1` JSON payload (inverse of `parse`). Shared by the
/// CLI producer and tested against `parse` so the two can never drift.
///
/// Takes `&StatusPayload` rather than seven positional args (four of them
/// adjacent `&str`s) — the struct's named fields make a `msg`/`task` or
/// `repo`/`branch` swap a compile-visible field-name mismatch instead of a
/// silent argument-order bug.
pub fn to_wire(p: &StatusPayload) -> String {
    serde_json::to_string(&Wire {
        v: STATUS_VERSION,
        source: &p.source,
        pane: WirePane {
            kind: "terminal",
            id: p.pane_id,
        },
        status: p.status,
        repo: &p.repo,
        branch: &p.branch,
        msg: &p.msg,
        task: &p.task,
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
    fn default_matches_parse_of_an_all_absent_payload() {
        // `StatusPayload::default()` must mean the same thing as "every optional
        // field absent on the wire". `pane.id` and `status` are the only two
        // fields `Raw` requires outright (no `#[serde(default)]`); pin `id: 0`
        // (the primitive default) and an unknown `status` token, which
        // `from_wire` maps to `Status::Idle` — the same fallback `Default`
        // uses. Every other field is genuinely absent.
        let got = p(r#"{"pane":{"type":"terminal","id":0},"status":""}"#).unwrap();
        assert_eq!(got, StatusPayload::default());
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
        // Each text field has a distinct cap (repo/branch MAX_REPO_CHARS/
        // MAX_BRANCH_CHARS, msg MAX_MSG_CHARS, source MAX_SOURCE_CHARS). Pin all
        // four so nudging any cap in `parse` is caught here — the wire boundary
        // is the only thing standing between a hostile producer and an
        // unbounded row. The literal VALUES of every cap const are pinned here
        // too, so an accidental edit to a const still fails a test, not just
        // whichever behavior test happens to exercise it.
        assert_eq!(MAX_REPO_CHARS, 40);
        assert_eq!(MAX_BRANCH_CHARS, 40);
        assert_eq!(MAX_SOURCE_CHARS, 16);
        assert_eq!(MAX_MSG_CHARS, 60);
        assert_eq!(MAX_TASK_CHARS, 60);
        // Not a `parse` cap, but the same class of intake bound (tab names and
        // pane titles sanitize to it at the plugin's host intake).
        assert_eq!(MAX_TAB_NAME_CHARS, 40);
        let json = format!(
            r#"{{"pane":{{"type":"terminal","id":1}},"status":"running","repo":"{r}","branch":"{b}","msg":"{m}","source":"{s}"}}"#,
            r = "r".repeat(100),
            b = "b".repeat(100),
            m = "m".repeat(200),
            s = "s".repeat(100),
        );
        let got = p(&json).unwrap();
        assert_eq!(got.repo.chars().count(), MAX_REPO_CHARS);
        assert_eq!(got.branch.chars().count(), MAX_BRANCH_CHARS);
        assert_eq!(got.msg.chars().count(), MAX_MSG_CHARS);
        assert_eq!(got.source.chars().count(), MAX_SOURCE_CHARS);
    }

    #[test]
    fn parses_ignores_v_field_value() {
        // The pipe NAME is the version authority (see the `Raw` comment above);
        // an otherwise-valid payload with a bogus `v` still parses as if it were
        // v1 — `Raw` has no `v` field, so serde silently drops it.
        let got = p(r#"{"v":999,"pane":{"type":"terminal","id":1},"status":"running","repo":"pinky"}"#).unwrap();
        assert_eq!(got.pane_id, 1);
        assert_eq!(got.status, Status::Running);
        assert_eq!(got.repo, "pinky");
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
    fn task_field_round_trips_and_defaults_empty() {
        // Absent task defaults to "" (old producers).
        let got = p(r#"{"pane":{"type":"terminal","id":3},"status":"running"}"#).unwrap();
        assert_eq!(got.task, "");
        // Present task survives to_wire → parse.
        let json = to_wire(&StatusPayload {
            pane_id: 9,
            status: Status::Running,
            repo: "r".into(),
            branch: "b".into(),
            msg: "editing x.rs".into(),
            task: "fix flaky e2e".into(),
            source: "claude".into(),
        });
        let got = parse(&json).expect("to_wire output must parse");
        assert_eq!(got.task, "fix flaky e2e");
        assert_eq!(got.msg, "editing x.rs");
    }

    #[test]
    fn task_is_sanitized_and_capped() {
        let json = format!(
            r#"{{"pane":{{"type":"terminal","id":1}},"status":"running","task":"\u001b[31m{}"}}"#,
            "t".repeat(100),
        );
        let got = p(&json).unwrap();
        assert_eq!(got.task.chars().count(), MAX_TASK_CHARS);
        assert!(!got.task.contains('\u{1b}'));
    }

    #[test]
    fn to_wire_round_trips_through_parse() {
        use crate::status::Status;
        let json = to_wire(&StatusPayload {
            pane_id: 12,
            status: Status::Running,
            repo: "pinky".into(),
            branch: "fix/x".into(),
            msg: "running tests".into(),
            task: "".into(),
            source: "claude".into(),
        });
        let got = parse(&json).expect("to_wire output must parse");
        assert_eq!(got.pane_id, 12);
        assert_eq!(got.status, Status::Running);
        assert_eq!(got.repo, "pinky");
        assert_eq!(got.branch, "fix/x");
        assert_eq!(got.msg, "running tests");
        assert_eq!(got.source, "claude");
    }

    #[test]
    fn to_wire_emits_the_exact_pinned_wire_bytes() {
        // The wire bytes are the public contract (`zj_radar.status.v1`), not an
        // implementation detail transitively pinned by the parse round-trip —
        // pin the exact JSON here, including field names, "v":1, and the
        // {"type":"terminal","id":N} pane shape, so a serde field rename or
        // reorder is caught directly instead of only failing downstream.
        let json = to_wire(&StatusPayload {
            pane_id: 12,
            status: Status::Running,
            repo: "pinky".into(),
            branch: "fix/x".into(),
            msg: "running tests".into(),
            task: "fix flaky e2e".into(),
            source: "claude".into(),
        });
        assert_eq!(
            json,
            r#"{"v":1,"source":"claude","pane":{"type":"terminal","id":12},"status":"running","repo":"pinky","branch":"fix/x","msg":"running tests","task":"fix flaky e2e"}"#
        );
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
            task in "[a-zA-Z0-9 ]{0,40}",
            source in "[a-z]{0,12}",
        ) {
            // to_wire and parse must be inverses: a round-trip through the wire
            // format must preserve EVERY field parse surfaces — across all statuses,
            // the full pane-id range, and msg/source (the fields the old version
            // silently dropped). Only printable ASCII within each field's cap is
            // generated, so sanitize does not alter any field and whole-struct
            // equality is the exact inverse law.
            let p = StatusPayload { pane_id: pane, status, repo, branch, msg, task, source };
            let got = parse(&to_wire(&p)).expect("our own wire output must parse");
            prop_assert_eq!(got, p);
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
