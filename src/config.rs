//! Plugin configuration parsed from the KDL `plugin { ... }` block. Pure — no
//! zellij-tile dependency. Parsing never fails: invalid values fall back to the
//! field default and unknown keys are ignored (forward-compatible).

use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum NamingMode {
    /// Never rename tabs.
    Off,
    /// Rename only default ("Tab #N") or our own prior names (clobber guard).
    #[default]
    Managed,
    /// Rename any tab, overriding user-chosen names.
    Force,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Config {
    pub naming: NamingMode,
    pub stuck_secs: u64,
    pub header: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config { naming: NamingMode::default(), stuck_secs: 600, header: true }
    }
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

impl Config {
    pub fn from_map(cfg: &BTreeMap<String, String>) -> Config {
        let d = Config::default();
        let naming = match cfg.get("naming").map(|s| s.trim().to_ascii_lowercase()) {
            Some(s) if s == "off" => NamingMode::Off,
            Some(s) if s == "managed" => NamingMode::Managed,
            Some(s) if s == "force" => NamingMode::Force,
            _ => d.naming,
        };
        let stuck_secs = cfg
            .get("stuck_secs")
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(d.stuck_secs);
        let header = cfg.get("header").and_then(|s| parse_bool(s)).unwrap_or(d.header);
        Config { naming, stuck_secs, header }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn empty_map_is_defaults() {
        let c = Config::from_map(&map(&[]));
        assert_eq!(c, Config::default());
        assert_eq!(c.naming, NamingMode::Managed);
        assert_eq!(c.stuck_secs, 600);
        assert!(c.header);
    }

    #[test]
    fn parses_all_keys() {
        let c = Config::from_map(&map(&[("naming", "force"), ("stuck_secs", "120"), ("header", "false")]));
        assert_eq!(c.naming, NamingMode::Force);
        assert_eq!(c.stuck_secs, 120);
        assert!(!c.header);
    }

    #[test]
    fn naming_is_case_insensitive_and_falls_back() {
        assert_eq!(Config::from_map(&map(&[("naming", "OFF")])).naming, NamingMode::Off);
        assert_eq!(Config::from_map(&map(&[("naming", "Force")])).naming, NamingMode::Force);
        // unknown value → default
        assert_eq!(Config::from_map(&map(&[("naming", "wat")])).naming, NamingMode::Managed);
    }

    #[test]
    fn bool_accepts_several_spellings() {
        for t in ["true", "1", "yes", "on", "ON", "Yes"] {
            assert!(Config::from_map(&map(&[("header", t)])).header);
        }
        for f in ["false", "0", "no", "off"] {
            assert!(!Config::from_map(&map(&[("header", f)])).header);
        }
        // garbage → default (true)
        assert!(Config::from_map(&map(&[("header", "maybe")])).header);
    }

    #[test]
    fn stuck_secs_invalid_falls_back() {
        assert_eq!(Config::from_map(&map(&[("stuck_secs", "")])).stuck_secs, 600);
        assert_eq!(Config::from_map(&map(&[("stuck_secs", "abc")])).stuck_secs, 600);
        assert_eq!(Config::from_map(&map(&[("stuck_secs", "0")])).stuck_secs, 0);
    }

    #[test]
    fn unknown_keys_ignored() {
        let c = Config::from_map(&map(&[("totally_unknown", "x"), ("naming", "off")]));
        assert_eq!(c.naming, NamingMode::Off);
        assert_eq!(c.stuck_secs, 600);
    }
}
