//! The `zj_radar.cmd.v1` imperative command vocabulary.
//!
//! Verbs arrive as bare strings on the command pipe (typically from a Zellij
//! `MessagePlugin` keybind). Unknown verbs are `None` — the caller treats that
//! as a silent no-op, matching the radar's "parsing never fails" stance.

/// A parsed command verb.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    AttentionNext,
    AttentionPrev,
}

/// Parse a bare verb string. Trims surrounding whitespace; case-sensitive
/// lowercase verbs. Returns `None` for unknown/empty input.
pub(crate) fn parse(s: &str) -> Option<Command> {
    match s.trim() {
        "attention-next" => Some(Command::AttentionNext),
        "attention-prev" => Some(Command::AttentionPrev),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_verbs_trimmed() {
        assert_eq!(parse("attention-next"), Some(Command::AttentionNext));
        assert_eq!(parse("  attention-prev\n"), Some(Command::AttentionPrev));
    }

    #[test]
    fn rejects_unknown_and_empty() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("attention-top"), None);
        assert_eq!(parse("ATTENTION-NEXT"), None);
    }
}
