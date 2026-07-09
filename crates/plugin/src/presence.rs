//! Cross-session presence: the tiny JSON one session's rail publishes so
//! peer sessions can render its badge. Pure data + lenient parse — file IO
//! lives in `session_files`, state in `sessions`.

use serde::{Deserialize, Serialize};

/// Ceiling on an inbound session name — presence files are peer input, and
/// a corrupt or hostile file must not bloat the rail (mirrors payload's
/// sanitize discipline).
const MAX_SESSION_NAME_CHARS: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Presence {
    pub session_name: String,
    pub running: usize,
    pub attention: usize,
    #[serde(default)]
    pub attention_tab_position: Option<usize>,
    #[serde(default)]
    pub updated_epoch_s: u64,
}

impl Presence {
    pub(crate) fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Lenient: any malformation (bad JSON, missing name, oversized name)
    /// yields `None` — a corrupt peer file skips its badge, never crashes.
    pub(crate) fn parse(s: &str) -> Option<Presence> {
        let p: Presence = serde_json::from_str(s).ok()?;
        if p.session_name.is_empty() || p.session_name.chars().count() > MAX_SESSION_NAME_CHARS {
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
    fn oversized_session_name_is_rejected() {
        let long = "x".repeat(10_000);
        let json = format!(r#"{{"session_name":"{long}","running":0,"attention":0}}"#);
        assert_eq!(Presence::parse(&json), None);
    }
}
