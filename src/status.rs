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
}
