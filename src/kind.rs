//! Source-agnostic identity kind — agents and tasks as peers.
//! No zellij-tile dependency.

use crate::status::GlyphSet;

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

    /// Single-character identity mark shown before the activity. Glyph-set
    /// aware: the Nerd set upgrades the three *agent* marks (which are thin
    /// asterisk-family glyphs in Plain) to heavier font-native MDI icons; the
    /// task marks (`⚙ ⚗ ⇡ ❯ $ ⦿`) already read well and are shared across sets.
    pub fn mark(self, set: GlyphSet) -> char {
        match set {
            GlyphSet::Nerd => match self {
                Kind::Claude => '\u{f06a9}', // nf-md-robot 󰚩
                Kind::Codex => '\u{f167a}',  // nf-md-robot_outline
                Kind::Gemini => '\u{f0eb9}', // nf-md-star_four_points (sparkle)
                Kind::Command => '$',
                Kind::Other => '⦿',
                Kind::Test => '⚗',
                Kind::Build => '⚙',
                Kind::Deploy => '⇡',
                Kind::Server => '❯',
            },
            GlyphSet::Plain => match self {
                Kind::Claude => '✳',
                Kind::Codex => '❉',
                Kind::Gemini => '✦',
                Kind::Command => '$',
                Kind::Other => '⦿',
                Kind::Test => '⚗',
                Kind::Build => '⚙',
                Kind::Deploy => '⇡',
                Kind::Server => '❯',
            },
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
        use GlyphSet::Plain;
        assert_eq!(Kind::Claude.mark(Plain), '✳');
        assert_eq!(Kind::Codex.mark(Plain), '❉');
        assert_eq!(Kind::Gemini.mark(Plain), '✦');
        assert_eq!(Kind::Command.mark(Plain), '$');
        assert_eq!(Kind::Other.mark(Plain), '⦿');
        assert_eq!(Kind::Test.mark(Plain), '⚗');
        assert_eq!(Kind::Build.mark(Plain), '⚙');
        assert_eq!(Kind::Deploy.mark(Plain), '⇡');
        assert_eq!(Kind::Server.mark(Plain), '❯');
    }

    #[test]
    fn nerd_set_upgrades_agent_marks() {
        use GlyphSet::Nerd;
        // Agent marks become heavier font-native MDI glyphs in the Nerd set.
        assert_eq!(Kind::Claude.mark(Nerd), '\u{f06a9}');
        assert_eq!(Kind::Codex.mark(Nerd), '\u{f167a}');
        assert_eq!(Kind::Gemini.mark(Nerd), '\u{f0eb9}');
        // Task marks are shared across sets.
        assert_eq!(Kind::Build.mark(Nerd), '⚙');
        assert_eq!(Kind::Command.mark(Nerd), '$');
    }

    #[test]
    fn all_marks_distinct() {
        use Kind::*;
        let all = [
            Claude, Codex, Gemini, Command, Other, Test, Build, Deploy, Server,
        ];
        for set in [GlyphSet::Plain, GlyphSet::Nerd] {
            for (i, a) in all.iter().enumerate() {
                for b in &all[i + 1..] {
                    assert_ne!(
                        a.mark(set),
                        b.mark(set),
                        "{:?} and {:?} share the same mark '{}' in {:?}",
                        a,
                        b,
                        a.mark(set),
                        set
                    );
                }
            }
        }
    }
}
