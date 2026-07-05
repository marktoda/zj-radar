//! The `zj_radar.cmd.v1` imperative command vocabulary.
//!
//! Verbs arrive as bare strings on the command pipe (typically from a Zellij
//! `MessagePlugin` keybind). Unknown verbs are `None` — the caller treats that
//! as a silent no-op, matching the radar's "parsing never fails" stance.

/// The imperative-command pipe. Payload is a single bare verb string;
/// breaking the verb vocabulary means a new name (same versioning scheme as
/// `payload::STATUS_PIPE_NAME`).
pub(crate) const CMD_PIPE: &str = "zj_radar.cmd.v1";

/// A parsed command verb.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verb {
    AttentionNext,
    AttentionPrev,
}

/// Parse a bare verb string. Trims surrounding whitespace; case-sensitive
/// lowercase verbs. Returns `None` for unknown/empty input.
pub(crate) fn parse(s: &str) -> Option<Verb> {
    match s.trim() {
        "attention-next" => Some(Verb::AttentionNext),
        "attention-prev" => Some(Verb::AttentionPrev),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_verbs_trimmed() {
        assert_eq!(parse("attention-next"), Some(Verb::AttentionNext));
        assert_eq!(parse("  attention-prev\n"), Some(Verb::AttentionPrev));
    }

    #[test]
    fn rejects_unknown_and_empty() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("attention-top"), None);
        assert_eq!(parse("ATTENTION-NEXT"), None);
    }

    /// docs/configuration.md is where users copy their `MessagePlugin`
    /// keybinds from; a drifted verb or pipe name there is a silently dead
    /// keybind — the same failure class the bash-producer pipe-name pin in
    /// lib.rs guards against. Pin every documented cmd-pipe payload to the
    /// parser. (This crate isn't published to crates.io, so `include_str!`
    /// outside the crate dir is safe — same pattern as `reference_tests.rs`.)
    #[test]
    fn documented_cmd_pipe_verbs_parse() {
        let doc = include_str!("../../../docs/configuration.md");
        assert!(
            doc.contains(CMD_PIPE),
            "configuration.md must document the {CMD_PIPE} pipe by name"
        );
        let bind = format!("name \"{CMD_PIPE}\"; payload \"");
        let mut checked = 0;
        for chunk in doc.split(bind.as_str()).skip(1) {
            let verb = chunk.split('"').next().expect("split yields at least one piece");
            assert!(
                parse(verb).is_some(),
                "configuration.md documents cmd verb {verb:?}, which control::parse rejects"
            );
            checked += 1;
        }
        assert!(
            checked >= 2,
            "expected configuration.md to document at least the two attention verbs \
             via `{bind}…\"` (found {checked}); if the doc changed shape, update this pin"
        );
    }
}
