//! Pure agent-status vocabulary. No zellij-tile dependency.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Idle,
    Done,
    Running,
    Pending,
    Error,
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

    /// Higher = more urgent. Used for per-tab aggregation.
    pub fn severity(self) -> u8 {
        match self {
            Status::Error => 4,
            Status::Pending => 3,
            Status::Running => 2,
            Status::Done => 1,
            Status::Idle => 0,
        }
    }

    pub fn glyph(self) -> char {
        match self {
            Status::Error => '✗',
            Status::Pending => '◑',
            Status::Running => '◐',
            Status::Done => '●',
            Status::Idle => '○',
        }
    }

    /// ANSI SGR foreground color for the glyph.
    pub fn ansi(self) -> &'static str {
        match self {
            Status::Error => "\x1b[31m",   // red
            Status::Pending => "\x1b[33m", // yellow/orange
            Status::Running => "\x1b[93m", // bright yellow
            Status::Done => "\x1b[32m",    // green
            Status::Idle => "\x1b[90m",    // dim grey
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GlyphSet {
    Nerd,
    Plain,
}

impl GlyphSet {
    pub fn from_config(s: &str) -> GlyphSet {
        match s {
            "plain" => GlyphSet::Plain,
            _ => GlyphSet::Nerd,
        }
    }
}

/// Working status glyph animation (both glyph sets): ◐ ◓ ◑ ◒.
pub fn working_spin(frame: usize) -> char {
    const FRAMES: [char; 4] = ['◐', '◓', '◑', '◒'];
    FRAMES[frame % FRAMES.len()]
}

/// In-message braille spinner.
pub fn msg_spin(frame: usize) -> char {
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
    fn severity_orders_error_highest_idle_lowest() {
        assert!(Status::Error.severity() > Status::Pending.severity());
        assert!(Status::Pending.severity() > Status::Running.severity());
        assert!(Status::Running.severity() > Status::Done.severity());
        assert!(Status::Done.severity() > Status::Idle.severity());
    }

    #[test]
    fn is_active_excludes_idle_only() {
        assert!(!Status::Idle.is_active());
        assert!(Status::Done.is_active());
        assert!(Status::Running.is_active());
    }

    #[test]
    fn glyph_and_ansi_are_distinct_per_variant() {
        use Status::*;
        let all = [Idle, Done, Running, Pending, Error];
        // glyphs are all distinct
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a.glyph(), b.glyph());
                assert_ne!(a.ansi(), b.ansi());
            }
        }
        // sanity: known mappings
        assert_eq!(Done.glyph(), '●');
        assert_eq!(Error.ansi(), "\x1b[31m");
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
    fn glyph_set_from_config_defaults_to_nerd() {
        assert_eq!(GlyphSet::from_config("plain"), GlyphSet::Plain);
        assert_eq!(GlyphSet::from_config("nerd"), GlyphSet::Nerd);
        assert_eq!(GlyphSet::from_config("anything-else"), GlyphSet::Nerd);
    }

    #[test]
    fn working_spinner_cycles_quarter_circles() {
        assert_eq!(working_spin(0), '◐');
        assert_eq!(working_spin(1), '◓');
        assert_eq!(working_spin(2), '◑');
        assert_eq!(working_spin(3), '◒');
        assert_eq!(working_spin(4), '◐'); // wraps
    }

    #[test]
    fn msg_spinner_cycles_braille() {
        assert_eq!(msg_spin(0), '⠋');
        assert_eq!(msg_spin(10), '⠋'); // wraps at 10
    }
}
