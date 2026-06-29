//! Pure agent-status vocabulary. No zellij-tile dependency.
//!
//! `Status` and everything that varies per variant — wire token, color role,
//! and both glyph sets — are generated from the `statuses!` table below, the
//! single source of truth. Callers should reach for the generated `as_wire` /
//! `from_wire` / `role` / `glyph_for` / `ALL` rather than re-deriving any of
//! these by hand, so the vocabulary can never drift across the codebase.

/// Define `Status` and everything that varies per variant from one table.
///
/// Each row is `Variant => "wire", Role, plain_glyph, nerd_glyph`. The macro
/// expands the table into the `Status` enum plus `from_wire` / `as_wire` /
/// `role` / `glyph_for` / `ALL` and the `serde` pair (via `wire_serde!`), so
/// the variant list, the wire vocabulary, the role mapping, the two glyph sets,
/// and the snapshot encoding are a *single source of truth* — they cannot
/// drift, and the generated `as_wire` / `role` / `glyph_for` use exhaustive
/// `match self`, so a row that omits a variant fails to compile.
///
/// Two invariants ride on the table layout, both preserved by construction:
/// - **Severity order.** Rows are listed in ascending-severity order so the
///   derived `Ord` *is* the aggregation order used by the Tab Roll-Up
///   (`.max()` picks the most-urgent member). Reordering rows reorders severity.
/// - **Lenient parse.** `from_wire` falls back to `$fallback` for any unknown or
///   absent token, matching how the pipe payload parses status.
macro_rules! statuses {
    (
        fallback = $fallback:ident;
        $( $variant:ident => $wire:literal, $role:expr, $plain:literal, $nerd:literal );+ $(;)?
    ) => {
        /// A pure agent-status value. See the `statuses!` table below for each
        /// variant's wire token, role color, and glyph in both glyph sets.
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
        pub enum Status {
            $( $variant ),+
        }

        impl Status {
            /// Every `Status`, in table (ascending-severity) order. Lets callers
            /// and exhaustiveness tests iterate the variants without re-typing.
            pub const ALL: &'static [Status] = &[ $( Status::$variant ),+ ];

            /// Parse a wire value; anything unknown/absent is the fallback
            /// (`Status::Idle`).
            pub fn from_wire(s: &str) -> Status {
                match s {
                    $( $wire => Status::$variant, )+
                    _ => Status::$fallback,
                }
            }

            /// Serialize to the wire vocabulary — the inverse of `from_wire` for
            /// every variant (`Status::from_wire(s.as_wire()) == s`).
            pub fn as_wire(self) -> &'static str {
                match self {
                    $( Status::$variant => $wire, )+
                }
            }

            /// The semantic color role for this status.
            pub fn role(self) -> Role {
                match self {
                    $( Status::$variant => $role, )+
                }
            }

            /// The status glyph for the active glyph set.
            pub fn glyph_for(self, set: GlyphSet) -> char {
                match self {
                    $( Status::$variant => match set {
                        GlyphSet::Plain => $plain,
                        GlyphSet::Nerd => $nerd,
                    }, )+
                }
            }
        }

        // Snapshot/pipe encoding, generated from the same table. Lenient: an
        // unknown/absent token deserializes to the `from_wire` fallback (Idle),
        // matching how the pipe payload parses status.
        $crate::wire::wire_serde!(lenient, Status);
    };
}

statuses! {
    fallback = Idle;
    //  variant    wire         role             plain  nerd
    Idle    => "idle",    Role::Muted,     '○', '\u{eb83}';
    Done    => "done",    Role::Success,   '●', '\u{f058}';
    Running => "running", Role::Working,   '⠋', '\u{f110}';
    Pending => "pending", Role::Attention, '◆', '\u{f0f3}';
    Error   => "error",   Role::Error,     '✗', '\u{f057}';
}

impl Status {
    pub fn is_active(self) -> bool {
        self != Status::Idle
    }

    /// Tabs in these states want the user's eyes (the radar's "attention set").
    /// `Running`/`Idle` are excluded — they need no action.
    pub fn needs_attention(self) -> bool {
        matches!(self, Status::Pending | Status::Error | Status::Done)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Error,
    Attention,
    Working,
    Success,
    Muted,
    Accent,
}

impl Role {
    pub fn ansi(self) -> &'static str {
        match self {
            Role::Error => "\x1b[31m",
            Role::Attention => "\x1b[91m",
            Role::Working => "\x1b[33m",
            Role::Success => "\x1b[32m",
            Role::Muted => "\x1b[90m",
            Role::Accent => "\x1b[35m",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum GlyphSet {
    Nerd,
    #[default]
    Plain,
}

impl GlyphSet {
    pub fn from_config(s: &str) -> GlyphSet {
        match s {
            "nerd" => GlyphSet::Nerd,
            _ => GlyphSet::Plain,
        }
    }
}

/// Working status glyph animation (both glyph sets): braille dots
/// ⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏.
pub fn working_spin(frame: usize) -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    FRAMES[frame % FRAMES.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_and_unknown() {
        assert_eq!(Status::from_wire("running"), Status::Running);
        assert_eq!(Status::from_wire("done"), Status::Done);
        assert_eq!(Status::from_wire("nonsense"), Status::Idle);
        assert_eq!(Status::from_wire(""), Status::Idle);
    }

    #[test]
    fn ord_ranks_idle_lowest_error_highest() {
        // `Status` derives `Ord` in ascending-severity declaration order; the
        // Tab Roll-Up relies on `max` picking the most-urgent member.
        assert!(Status::Idle < Status::Done);
        assert!(Status::Done < Status::Running);
        assert!(Status::Running < Status::Pending);
        assert!(Status::Pending < Status::Error);
        assert_eq!(
            [Status::Done, Status::Error, Status::Running].into_iter().max(),
            Some(Status::Error),
            "max yields the most-urgent status",
        );
    }

    #[test]
    fn is_active_excludes_idle_only() {
        assert!(!Status::Idle.is_active());
        assert!(Status::Done.is_active());
        assert!(Status::Running.is_active());
    }

    #[test]
    fn all_enumerates_every_variant() {
        // `ALL` drives the exhaustiveness of the other table tests; pin its size
        // so a dropped table row is caught here instead of silently shrinking
        // coverage.
        assert_eq!(Status::ALL.len(), 5);
        assert_eq!(Status::ALL.first(), Some(&Status::Idle)); // ascending severity
        assert_eq!(Status::ALL.last(), Some(&Status::Error));
    }

    #[test]
    fn wire_round_trips_for_every_status() {
        // The table generates `from_wire`/`as_wire` from one row each, so the
        // inverse holds for every variant by construction — this guards it.
        for &s in Status::ALL {
            assert_eq!(
                Status::from_wire(s.as_wire()),
                s,
                "{s:?} must survive a wire round-trip",
            );
        }
    }

    #[test]
    fn glyphs_and_roles_distinct_per_variant() {
        use GlyphSet::Plain;
        for (i, a) in Status::ALL.iter().enumerate() {
            for b in &Status::ALL[i + 1..] {
                assert_ne!(a.glyph_for(Plain), b.glyph_for(Plain));
            }
        }
        assert_eq!(Status::Done.glyph_for(Plain), '●');
        assert_eq!(Status::Error.role().ansi(), "\x1b[31m");
    }

    #[test]
    fn role_colors_match_spec() {
        assert_eq!(Role::Error.ansi(), "\x1b[31m");
        assert_eq!(Role::Attention.ansi(), "\x1b[91m");
        assert_eq!(Role::Working.ansi(), "\x1b[33m");
        assert_eq!(Role::Success.ansi(), "\x1b[32m");
        assert_eq!(Role::Muted.ansi(), "\x1b[90m");
        assert_eq!(Role::Accent.ansi(), "\x1b[35m");
    }

    #[test]
    fn status_maps_to_role() {
        assert_eq!(Status::Error.role(), Role::Error);
        assert_eq!(Status::Pending.role(), Role::Attention); // waiting is the loud one
        assert_eq!(Status::Running.role(), Role::Working);
        assert_eq!(Status::Done.role(), Role::Success);
        assert_eq!(Status::Idle.role(), Role::Muted);
    }

    #[test]
    fn plain_glyphs_use_geometric_shapes() {
        use GlyphSet::Plain;
        assert_eq!(Status::Idle.glyph_for(Plain), '○');
        assert_eq!(Status::Running.glyph_for(Plain), '⠋');
        assert_eq!(Status::Pending.glyph_for(Plain), '◆'); // moved from ◑ to ◆
        assert_eq!(Status::Done.glyph_for(Plain), '●');
        assert_eq!(Status::Error.glyph_for(Plain), '✗');
    }

    #[test]
    fn nerd_glyphs_use_private_use_codepoints() {
        use GlyphSet::Nerd;
        assert_eq!(Status::Pending.glyph_for(Nerd), '\u{f0f3}');
        assert_eq!(Status::Done.glyph_for(Nerd), '\u{f058}');
        assert_eq!(Status::Error.glyph_for(Nerd), '\u{f057}');
    }

    #[test]
    fn glyph_set_from_config_defaults_to_plain() {
        assert_eq!(GlyphSet::from_config("nerd"), GlyphSet::Nerd);
        assert_eq!(GlyphSet::from_config("plain"), GlyphSet::Plain);
        assert_eq!(GlyphSet::from_config("anything-else"), GlyphSet::Plain);
    }

    #[test]
    fn working_spinner_cycles_braille_dots() {
        assert_eq!(working_spin(0), '⠋');
        assert_eq!(working_spin(1), '⠙');
        assert_eq!(working_spin(9), '⠏');
        assert_eq!(working_spin(10), '⠋'); // wraps
    }

    #[test]
    fn needs_attention_covers_pending_error_done_only() {
        assert!(Status::Pending.needs_attention());
        assert!(Status::Error.needs_attention());
        assert!(Status::Done.needs_attention());
        assert!(!Status::Running.needs_attention());
        assert!(!Status::Idle.needs_attention());
    }

}
