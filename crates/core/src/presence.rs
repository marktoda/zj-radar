//! Cross-session presence: the bounded JSON each session's rail publishes
//! to `zj-radar.presence.<zellij_pid>.json` in the plugin's shared cache.
//! Read by peer rails (badge, session tree) and by `zj-radar state`. The
//! JSON is the contract (`docs/producers.md`); these Rust types are
//! workspace-internal. Pure data + lenient parse; file IO lives with the
//! callers.

use serde::{Deserialize, Serialize};

/// Ceiling on an inbound session name — presence files are peer input, and
/// a corrupt or hostile file must not bloat the rail. Enforced through the
/// sanitize round-trip in `parse` (`payload::sanitize` truncates past the
/// ceiling, so an oversized name fails the equality check like any other
/// unclean one).
const MAX_SESSION_NAME_CHARS: usize = 128;
pub const MAX_TABS: usize = 64;
pub const MAX_PANES_PER_TAB: usize = 16;
pub const MAX_LABEL_CHARS: usize = 96;
pub const MAX_TOKEN_CHARS: usize = 24;

/// Current file schema version, written as `v`.
pub const PRESENCE_VERSION: u32 = 1;

/// How long a peer's presence file may sit unrefreshed before its badge row
/// dims to stale. The rail's timer heartbeats an idle-but-alive session's
/// own file at least once per Slow (60s) tick, so 90s gives 50% margin
/// against a single missed beat before flagging it — generous enough that
/// ordinary scheduler jitter never flickers an entry, but a session that's
/// genuinely gone quiet reads as such promptly. A missed beat marks stale;
/// only [`DEAD_AFTER_SECS`] reaps.
pub const STALE_AFTER_SECS: u64 = 90;

/// How old a peer's presence file must be before the entry is judged dead:
/// reaped from the badge and unlinked by the runtime. Five missed 60s
/// heartbeats past the write guarantee. The gap over [`STALE_AFTER_SECS`] is
/// deliberate: stale must stay twitchy — a dim at 90s is cheap,
/// self-correcting cosmetics — while dead must be conservative, because a
/// reap also unlinks the on-disk file. Machine-sleep caveat: right after a
/// wake every file looks old for up to one heartbeat, so a false reap of a
/// live peer is possible — and harmless, because dismissal is
/// non-destructive by construction: the live session's next heartbeat
/// republishes its file and the entry returns, fresh.
pub const DEAD_AFTER_SECS: u64 = 300;

/// A bounded, display-only view of another session's tab tree.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceTab {
    pub position: usize,
    pub name: String,
    #[serde(default)]
    pub panes: Vec<PresencePane>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresencePane {
    /// Zellij terminal pane id (v1+; absent in pre-v1 files).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<u32>,
    /// `"status"` (agent hook/pipe) or `"command"` (observed command) — v1+.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    pub kind: String,
    pub status: String,
    pub label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presence {
    /// File schema version ([`PRESENCE_VERSION`]); 0 = pre-v1 writer.
    /// Additive fields never bump it; a meaning or cap change does.
    #[serde(default)]
    pub v: u32,
    pub session_name: String,
    pub running: usize,
    pub attention: usize,
    #[serde(default)]
    pub attention_tab_position: Option<usize>,
    #[serde(default)]
    pub updated_epoch_s: u64,
    #[serde(default)]
    pub tabs: Vec<PresenceTab>,
}

impl Presence {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Lenient: bad JSON, a missing/empty name, or any string that doesn't
    /// survive `payload::sanitize` unchanged yields `None` (hostile or
    /// corrupt — skip it, never crash, never display it). Structural caps
    /// are applied by TRUNCATION first, so a future writer with a larger cap
    /// still reads; validation then runs only over what's kept. The name
    /// is rejected rather than cleaned because it is also the
    /// `SwitchSession` identity.
    pub fn parse(s: &str) -> Option<Presence> {
        let mut p: Presence = serde_json::from_str(s).ok()?;
        p.tabs.truncate(MAX_TABS);
        for tab in &mut p.tabs {
            tab.panes.truncate(MAX_PANES_PER_TAB);
        }
        let clean = |s: &str, max: usize| s == crate::payload::sanitize(s, max);
        let ok = !p.session_name.is_empty()
            && clean(&p.session_name, MAX_SESSION_NAME_CHARS)
            && p.tabs.iter().all(|tab| {
                clean(&tab.name, MAX_LABEL_CHARS)
                    && tab.panes.iter().all(|pane| {
                        clean(&pane.label, MAX_LABEL_CHARS)
                            && clean(&pane.kind, MAX_TOKEN_CHARS)
                            && clean(&pane.status, MAX_TOKEN_CHARS)
                            && pane.origin.as_deref().is_none_or(|o| clean(o, MAX_TOKEN_CHARS))
                    })
            });
        ok.then_some(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let p = Presence {
            session_name: "work".into(),
            running: 3,
            attention: 1,
            attention_tab_position: Some(2),
            updated_epoch_s: 1000,
            tabs: vec![],
            ..Default::default()
        };
        assert_eq!(Presence::parse(&p.to_json()), Some(p));
    }

    #[test]
    fn parse_is_lenient_on_garbage_and_missing_fields() {
        assert_eq!(Presence::parse("not json"), None);
        assert_eq!(Presence::parse("{}"), None); // session_name is required
                                                 // Unknown fields are ignored; absent optionals default.
        let p = Presence::parse(r#"{"session_name":"a","running":1,"attention":0,"future_field":true}"#).unwrap();
        assert_eq!(p.attention_tab_position, None);
        assert_eq!(p.updated_epoch_s, 0);
    }

    #[test]
    fn hostile_session_name_with_control_or_bidi_content_is_rejected() {
        // The badge writes the name into emitted ANSI, so a name carrying an
        // OSC sequence (title/clipboard injection), a CSI color splice, or a
        // bidi override is hostile-or-corrupt — and per this module's
        // contract a corrupt peer file skips its badge entirely. Rejected,
        // never cleaned in place: the name is also the `SwitchSession`
        // identity, and a display-cleaned variant would diverge from it.
        for name in ["\\u001b]0;pwned\\u0007work", "a\\u001b[31mred", "safe\\u202Eevil", "two\\nlines"] {
            let json = format!(r#"{{"session_name":"{name}","running":0,"attention":0}}"#);
            assert_eq!(Presence::parse(&json), None, "must reject {name}");
        }
    }

    #[test]
    fn peer_tree_rejects_control_text() {
        let unsafe_name = r#"{"session_name":"work","running":0,"attention":0,"tabs":[{"position":0,"name":"\u001b[31mevil","panes":[]}]}"#;
        assert!(Presence::parse(unsafe_name).is_none());
    }

    #[test]
    fn oversized_rosters_truncate_instead_of_rejecting() {
        let pane = serde_json::json!({"kind": "claude", "status": "running", "label": "x"});
        let big = serde_json::json!({
            "session_name": "work", "running": 0, "attention": 0,
            "tabs": (0..80).map(|i| serde_json::json!({
                "position": i, "name": "tab", "panes": vec![pane.clone(); 20]
            })).collect::<Vec<_>>()
        });
        let p = Presence::parse(&big.to_string()).expect("a bigger future cap still reads");
        assert_eq!(p.tabs.len(), MAX_TABS);
        assert!(p.tabs.iter().all(|t| t.panes.len() == MAX_PANES_PER_TAB));
    }

    #[test]
    fn v1_fields_round_trip_and_pre_v1_files_parse() {
        let p = Presence {
            v: PRESENCE_VERSION,
            session_name: "work".into(),
            tabs: vec![PresenceTab {
                position: 2,
                name: "api".into(),
                panes: vec![PresencePane {
                    pane_id: Some(14),
                    origin: Some("status".into()),
                    kind: "claude".into(),
                    status: "pending".into(),
                    label: "fix login".into(),
                }],
            }],
            ..Default::default()
        };
        assert_eq!(Presence::parse(&p.to_json()), Some(p));

        let old = r#"{"session_name":"a","running":1,"attention":0,"tabs":[{"position":0,"name":"t","panes":[{"kind":"claude","status":"running","label":"x"}]}]}"#;
        let old = Presence::parse(old).unwrap();
        assert_eq!(old.v, 0);
        assert_eq!(old.tabs[0].panes[0].pane_id, None);
        assert!(
            !Presence { tabs: old.tabs.clone(), ..old.clone() }.to_json().contains("pane_id"),
            "absent stays absent"
        );
    }

    #[test]
    fn hostile_origin_is_rejected() {
        let json = r#"{"session_name":"a","running":0,"attention":0,"tabs":[{"position":0,"name":"t","panes":[{"origin":"\u001b]0;x\u0007","kind":"claude","status":"idle","label":"x"}]}]}"#;
        assert_eq!(Presence::parse(json), None);
    }

    #[test]
    fn oversized_session_name_is_rejected() {
        let long = "x".repeat(10_000);
        let json = format!(r#"{{"session_name":"{long}","running":0,"attention":0}}"#);
        assert_eq!(Presence::parse(&json), None);
    }
}
