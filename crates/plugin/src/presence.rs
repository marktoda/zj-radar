//! Cross-session presence: the tiny JSON one session's rail publishes so
//! peer sessions can render its badge. Pure data + lenient parse — file IO
//! lives in `session_files`, state in `sessions`.

use serde::{Deserialize, Serialize};

/// Ceiling on an inbound session name — presence files are peer input, and
/// a corrupt or hostile file must not bloat the rail. Enforced through the
/// sanitize round-trip in `parse` (`payload::sanitize` truncates past the
/// ceiling, so an oversized name fails the equality check like any other
/// unclean one).
const MAX_SESSION_NAME_CHARS: usize = 128;
pub(crate) const MAX_TABS: usize = 64;
pub(crate) const MAX_PANES_PER_TAB: usize = 16;
pub(crate) const MAX_LABEL_CHARS: usize = 96;

/// A bounded, display-only view of another session's tab tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PresenceTab {
    pub position: usize,
    pub name: String,
    #[serde(default)]
    pub panes: Vec<PresencePane>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PresencePane {
    pub kind: String,
    pub status: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Presence {
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
    pub(crate) fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Lenient: any malformation (bad JSON, missing name, oversized name)
    /// yields `None` — a corrupt peer file skips its badge, never crashes.
    /// The name must also survive `payload::sanitize` unchanged: the badge
    /// writes it into emitted ANSI, so control/escape/bidi content is the
    /// same hostile-input class every other rail-bound string is scrubbed
    /// for. Rejected rather than cleaned in place, because the name is also
    /// the `SwitchSession` identity — a display-cleaned variant would
    /// silently diverge from the session it claims to switch to.
    pub(crate) fn parse(s: &str) -> Option<Presence> {
        let p: Presence = serde_json::from_str(s).ok()?;
        if p.session_name.is_empty()
            || p.session_name != crate::payload::sanitize(&p.session_name, MAX_SESSION_NAME_CHARS)
            || p.tabs.len() > MAX_TABS
            || p.tabs.iter().any(|tab| {
                tab.name != crate::payload::sanitize(&tab.name, MAX_LABEL_CHARS)
                    || tab.panes.len() > MAX_PANES_PER_TAB
                    || tab.panes.iter().any(|pane| {
                        pane.label != crate::payload::sanitize(&pane.label, MAX_LABEL_CHARS)
                            || pane.kind != crate::payload::sanitize(&pane.kind, 24)
                            || pane.status != crate::payload::sanitize(&pane.status, 24)
                    })
            })
        {
            return None;
        }
        Some(p)
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
        };
        assert_eq!(Presence::parse(&p.to_json()), Some(p));
    }

    #[test]
    fn parse_is_lenient_on_garbage_and_missing_fields() {
        assert_eq!(Presence::parse("not json"), None);
        assert_eq!(Presence::parse("{}"), None); // session_name is required
        // Unknown fields are ignored; absent optionals default.
        let p = Presence::parse(
            r#"{"session_name":"a","running":1,"attention":0,"future_field":true}"#,
        )
        .unwrap();
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
    fn peer_tree_rejects_control_text_and_oversized_rosters() {
        let unsafe_name = r#"{"session_name":"work","running":0,"attention":0,"tabs":[{"position":0,"name":"\u001b[31mevil","panes":[]}]}"#;
        assert!(Presence::parse(unsafe_name).is_none());
        let too_many = serde_json::json!({
            "session_name": "work", "running": 0, "attention": 0,
            "tabs": (0..65).map(|i| serde_json::json!({"position": i, "name": "tab"})).collect::<Vec<_>>()
        });
        assert!(Presence::parse(&too_many.to_string()).is_none());
    }

    #[test]
    fn oversized_session_name_is_rejected() {
        let long = "x".repeat(10_000);
        let json = format!(r#"{{"session_name":"{long}","running":0,"attention":0}}"#);
        assert_eq!(Presence::parse(&json), None);
    }
}
