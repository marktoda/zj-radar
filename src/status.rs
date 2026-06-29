//! Pure agent-status vocabulary. No zellij-tile dependency.

/// Variants are declared in ascending-severity order (`Idle` … `Error`) so the
/// derived `Ord` *is* the aggregation order used by the Tab Roll-Up — there is
/// no separate severity table to keep in sync. Reordering these variants
/// reorders severity.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Status {
    Idle,
    Done,
    Running,
    Pending,
    Error,
}

// Serde delegates to the existing wire vocabulary so the snapshot uses the same
// tokens as the pipe payload — one source of truth (`as_wire`/`from_wire`), no
// second representation to keep in sync. Deserialization is deliberately lenient
// (unknown/absent → Idle), matching how the pipe payload parses status.
impl serde::Serialize for Status {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(self.as_wire())
    }
}

impl<'de> serde::Deserialize<'de> for Status {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        Ok(Status::from_wire(&<String as serde::Deserialize>::deserialize(de)?))
    }
}

impl Status {
    /// Parse a wire value; anything unknown/absent is Idle.
    pub fn from_wire(s: &str) -> Status {
        match s {
            "running" => Status::Running,
            "pending" => Status::Pending,
            "done" => Status::Done,
            "error" => Status::Error,
            _ => Status::Idle,
        }
    }

    /// Serialize to the wire vocabulary (inverse of `from_wire`).
    pub fn as_wire(self) -> &'static str {
        match self {
            Status::Running => "running",
            Status::Pending => "pending",
            Status::Done => "done",
            Status::Error => "error",
            Status::Idle => "idle",
        }
    }

    pub fn is_active(self) -> bool {
        self != Status::Idle
    }

    pub fn role(self) -> Role {
        match self {
            Status::Error => Role::Error,
            Status::Pending => Role::Attention,
            Status::Running => Role::Working,
            Status::Done => Role::Success,
            Status::Idle => Role::Muted,
        }
    }

    pub fn glyph_for(self, set: GlyphSet) -> char {
        match set {
            GlyphSet::Plain => match self {
                Status::Idle => '○',
                Status::Running => '◐',
                Status::Pending => '◆',
                Status::Done => '●',
                Status::Error => '✗',
            },
            GlyphSet::Nerd => match self {
                Status::Idle => '\u{eb83}',
                Status::Running => '\u{f110}',
                Status::Pending => '\u{f0f3}',
                Status::Done => '\u{f058}',
                Status::Error => '\u{f057}',
            },
        }
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

/// Working status glyph animation (both glyph sets): ◐ ◓ ◑ ◒.
pub fn working_spin(frame: usize) -> char {
    const FRAMES: [char; 4] = ['◐', '◓', '◑', '◒'];
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
    fn glyphs_and_roles_distinct_per_variant() {
        use GlyphSet::Plain;
        use Status::*;
        let all = [Idle, Done, Running, Pending, Error];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a.glyph_for(Plain), b.glyph_for(Plain));
            }
        }
        assert_eq!(Done.glyph_for(Plain), '●');
        assert_eq!(Error.role().ansi(), "\x1b[31m");
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
        assert_eq!(Status::Running.glyph_for(Plain), '◐');
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
    fn working_spinner_cycles_quarter_circles() {
        assert_eq!(working_spin(0), '◐');
        assert_eq!(working_spin(1), '◓');
        assert_eq!(working_spin(2), '◑');
        assert_eq!(working_spin(3), '◒');
        assert_eq!(working_spin(4), '◐'); // wraps
    }

}
