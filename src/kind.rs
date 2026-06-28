//! Source-agnostic identity kind — agents and tasks as peers.
//! No zellij-tile dependency.

/// What kind of process owns a pane. Agents (Claude, Codex, Gemini) and task
/// types (Test, Build, Deploy, Server) are peers — only the mark glyph differs.
/// The renderer and sorter treat all variants identically.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Claude,
    Codex,
    Gemini,
    Command,
    Other,
    Test,
    Build,
    Deploy,
    Server,
}

impl Kind {
    /// Derive a Kind from the payload `source` field (lowercased wire value).
    pub fn from_source(s: &str) -> Kind {
        match s {
            "claude" => Kind::Claude,
            "codex" => Kind::Codex,
            "gemini" => Kind::Gemini,
            "command" => Kind::Command,
            "test" => Kind::Test,
            "build" => Kind::Build,
            "deploy" => Kind::Deploy,
            "server" => Kind::Server,
            _ => Kind::Other,
        }
    }

    /// Single-character identity mark shown on line 2 before the activity.
    pub fn mark(self) -> char {
        match self {
            Kind::Claude => '✳',
            Kind::Codex => '❉',
            Kind::Gemini => '✦',
            Kind::Command => '$',
            Kind::Other => '⦿',
            Kind::Test => '⚗',
            Kind::Build => '⚙',
            Kind::Deploy => '⇡',
            Kind::Server => '❯',
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_source_agent_variants() {
        assert_eq!(Kind::from_source("claude"), Kind::Claude);
        assert_eq!(Kind::from_source("codex"), Kind::Codex);
        assert_eq!(Kind::from_source("gemini"), Kind::Gemini);
        assert_eq!(Kind::from_source("command"), Kind::Command);
    }

    #[test]
    fn from_source_task_variants() {
        assert_eq!(Kind::from_source("test"), Kind::Test);
        assert_eq!(Kind::from_source("build"), Kind::Build);
        assert_eq!(Kind::from_source("deploy"), Kind::Deploy);
        assert_eq!(Kind::from_source("server"), Kind::Server);
    }

    #[test]
    fn from_source_unknown_is_other() {
        assert_eq!(Kind::from_source(""), Kind::Other);
        assert_eq!(Kind::from_source("unknown"), Kind::Other);
        assert_eq!(Kind::from_source("Claude"), Kind::Other); // case-sensitive
        assert_eq!(Kind::from_source("anything-else"), Kind::Other);
    }

    #[test]
    fn mark_per_variant() {
        assert_eq!(Kind::Claude.mark(), '✳');
        assert_eq!(Kind::Codex.mark(), '❉');
        assert_eq!(Kind::Gemini.mark(), '✦');
        assert_eq!(Kind::Command.mark(), '$');
        assert_eq!(Kind::Other.mark(), '⦿');
        assert_eq!(Kind::Test.mark(), '⚗');
        assert_eq!(Kind::Build.mark(), '⚙');
        assert_eq!(Kind::Deploy.mark(), '⇡');
        assert_eq!(Kind::Server.mark(), '❯');
    }

    #[test]
    fn all_marks_distinct() {
        use Kind::*;
        let all = [
            Claude, Codex, Gemini, Command, Other, Test, Build, Deploy, Server,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(
                    a.mark(),
                    b.mark(),
                    "{:?} and {:?} share the same mark '{}'",
                    a,
                    b,
                    a.mark()
                );
            }
        }
    }
}
