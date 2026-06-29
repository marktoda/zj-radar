//! Source-agnostic identity kind — agents and tasks as peers.
//! No zellij-tile dependency.

use crate::status::GlyphSet;

/// Define `Kind` and everything that varies per variant from one table.
///
/// Each row is `Variant => "wire-source", plain_mark, nerd_mark`. The macro
/// expands the table into the `Kind` enum plus `from_source` / `as_source` /
/// `mark` / `ALL`, so the variant list, the wire vocabulary, and the two glyph
/// sets are a *single source of truth* — they cannot drift, and adding an agent
/// or task type is one new row. The generated `as_source` / `mark` use
/// exhaustive `match self`, so a row that omits a variant fails to compile
/// rather than silently falling back.
macro_rules! kinds {
    ( $( $variant:ident => $source:literal, $plain:literal, $nerd:literal );+ $(;)? ) => {
        /// What kind of process owns a pane. Agents (Claude, Codex, Gemini) and
        /// task types (Test, Build, Deploy, Server) are peers — only the mark
        /// glyph differs; the renderer and sorter treat all variants identically.
        ///
        /// Generated from the `kinds!` table below — see it for each variant's
        /// wire token and marks.
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum Kind {
            $( $variant ),+
        }

        impl Kind {
            /// Every `Kind`, in table order. Lets callers and exhaustiveness
            /// tests iterate the variants without re-typing the list.
            pub const ALL: &'static [Kind] = &[ $( Kind::$variant ),+ ];

            /// Derive a `Kind` from the payload `source` field (lowercased wire
            /// value); unknown or absent sources fall back to `Other`.
            pub fn from_source(s: &str) -> Kind {
                match s {
                    $( $source => Kind::$variant, )+
                    _ => Kind::Other,
                }
            }

            /// The wire `source` token for this kind — the exact inverse of
            /// `from_source` for every known variant
            /// (`Kind::from_source(k.as_source()) == k`).
            pub fn as_source(self) -> &'static str {
                match self {
                    $( Kind::$variant => $source, )+
                }
            }

            /// Single-character identity mark shown before the activity. Glyph-set
            /// aware: the Nerd set upgrades the three *agent* marks (thin
            /// asterisk-family glyphs in Plain) to heavier font-native MDI icons;
            /// task marks already read well, so most rows repeat one glyph.
            pub fn mark(self, set: GlyphSet) -> char {
                match self {
                    $( Kind::$variant => match set {
                        GlyphSet::Plain => $plain,
                        GlyphSet::Nerd => $nerd,
                    }, )+
                }
            }
        }
    };
}

kinds! {
    Claude  => "claude",  '✳', '\u{f06a9}'; // nf-md-robot
    Codex   => "codex",   '❉', '\u{f167a}'; // nf-md-robot-outline
    Gemini  => "gemini",  '✦', '\u{f0eb9}'; // nf-md-star-four-points (sparkle)
    Command => "command", '$', '$';
    Other   => "other",   '⦿', '⦿';
    Test    => "test",    '⚗', '⚗';
    Build   => "build",   '⚙', '⚙';
    Deploy  => "deploy",  '⇡', '⇡';
    Server  => "server",  '❯', '❯';
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
    fn source_round_trips_for_every_kind() {
        // The table generates `from_source`/`as_source` from one row each, so the
        // inverse holds for every variant by construction — this guards it.
        for &k in Kind::ALL {
            assert_eq!(
                Kind::from_source(k.as_source()),
                k,
                "{k:?} must survive a source round-trip",
            );
        }
    }

    #[test]
    fn all_enumerates_every_variant() {
        // `ALL` drives the exhaustiveness of the other tests; pin its size so a
        // dropped table row is caught here instead of silently shrinking coverage.
        assert_eq!(Kind::ALL.len(), 9);
        assert_eq!(Kind::Other.as_source(), "other");
    }

    #[test]
    fn all_marks_distinct() {
        for set in [GlyphSet::Plain, GlyphSet::Nerd] {
            for (i, a) in Kind::ALL.iter().enumerate() {
                for b in &Kind::ALL[i + 1..] {
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
