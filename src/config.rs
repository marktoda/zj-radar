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

/// Vertical density of the sidebar rail.
///
/// - `Compact`: flush rail — no blank lines between tabs (original behaviour).
/// - `Comfortable`: one blank separator line after each tab's content block.
/// - `Cards` (default): every tab is a card — a 256-color surface band keyed to
///   its class (idle dim / agent mid / focused-active bright), one blank rail
///   row between cards. The active tab keeps the mauve spine + bold name.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Density {
    /// No blank lines between tabs.
    Compact,
    /// One blank separator line after each tab's content block.
    Comfortable,
    /// Default. Every tab is a card: a 256-color surface band keyed to its
    /// class (idle dim / agent mid / focused-active bright), one blank rail row
    /// between cards. The active tab keeps the mauve spine + bold name.
    #[default]
    Cards,
}

impl Density {
    pub fn from_config(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "compact" => Density::Compact,
            "comfortable" => Density::Comfortable,
            // Unknown/invalid values fall back to the field default (Cards).
            _ => Density::Cards,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Config {
    pub naming: NamingMode,
    pub header: bool,
    pub glyphs: crate::status::GlyphSet,
    pub density: Density,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            naming: NamingMode::default(),
            header: true,
            glyphs: crate::status::GlyphSet::default(),
            density: Density::default(),
        }
    }
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

impl NamingMode {
    pub fn from_config(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" => NamingMode::Off,
            "managed" => NamingMode::Managed,
            "force" => NamingMode::Force,
            _ => NamingMode::default(),
        }
    }
}

/// Generate `Config::from_map` and `Config::apply_overrides` from one field
/// table, so the KDL-load path and the live-override path parse the same keys
/// with the same parsers *by construction* — never by hand-kept agreement.
/// `from_map` is exactly "start from defaults, then apply the map", so an absent
/// key keeps the field default and the two paths can't interpret a value
/// differently. Adding a configurable field is one new row.
///
/// Two parser contracts, one row group each:
/// - `total` — `fn(&str) -> T`: the parser resolves unknown input itself (the
///   enum `from_config`s fold to their default), so a *present* key always sets
///   the field.
/// - `opt` — `fn(&str) -> Option<T>`: a present-but-unparseable value is left
///   untouched (keeps the default / current value); used by `parse_bool`.
macro_rules! config_fields {
    (
        total { $( $tfield:ident : $tkey:literal => $tparser:path ),* $(,)? }
        opt   { $( $ofield:ident : $okey:literal => $oparser:path ),* $(,)? }
    ) => {
        impl Config {
            /// Build from the flat key→value map of the KDL `plugin { ... }`
            /// block. Unlisted keys take the field default; unknown keys are
            /// ignored (forward-compatible).
            pub fn from_map(cfg: &BTreeMap<String, String>) -> Config {
                let mut config = Config::default();
                config.apply_overrides(cfg);
                config
            }

            /// Apply runtime overrides from a flat key→value map (e.g. parsed
            /// from a JSON pipe payload). Unknown keys are silently ignored.
            pub fn apply_overrides(&mut self, kv: &BTreeMap<String, String>) {
                $( if let Some(v) = kv.get($tkey) {
                    self.$tfield = $tparser(v);
                } )*
                $( if let Some(v) = kv.get($okey) {
                    if let Some(parsed) = $oparser(v) {
                        self.$ofield = parsed;
                    }
                } )*
            }
        }
    };
}

config_fields! {
    total {
        naming:  "naming"  => NamingMode::from_config,
        density: "density" => Density::from_config,
        glyphs:  "glyphs"  => crate::status::GlyphSet::from_config,
    }
    opt {
        header: "header" => parse_bool,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn empty_map_is_defaults() {
        let c = Config::from_map(&map(&[]));
        assert_eq!(c, Config::default());
        assert_eq!(c.naming, NamingMode::Managed);
        assert!(c.header);
        assert_eq!(c.glyphs, crate::status::GlyphSet::Plain);
    }

    #[test]
    fn parses_all_keys() {
        let c = Config::from_map(&map(&[
            ("naming", "force"),
            ("header", "false"),
            ("glyphs", "nerd"),
        ]));
        assert_eq!(c.naming, NamingMode::Force);
        assert!(!c.header);
        assert_eq!(c.glyphs, crate::status::GlyphSet::Nerd);
    }

    #[test]
    fn glyphs_parses_and_defaults_to_plain() {
        assert_eq!(
            Config::from_map(&map(&[("glyphs", "nerd")])).glyphs,
            crate::status::GlyphSet::Nerd
        );
        // absent → default (Plain)
        assert_eq!(
            Config::from_map(&map(&[])).glyphs,
            crate::status::GlyphSet::Plain
        );
    }

    #[test]
    fn naming_is_case_insensitive_and_falls_back() {
        assert_eq!(
            Config::from_map(&map(&[("naming", "OFF")])).naming,
            NamingMode::Off
        );
        assert_eq!(
            Config::from_map(&map(&[("naming", "Force")])).naming,
            NamingMode::Force
        );
        // unknown value → default
        assert_eq!(
            Config::from_map(&map(&[("naming", "wat")])).naming,
            NamingMode::Managed
        );
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
    fn unknown_keys_ignored() {
        let c = Config::from_map(&map(&[("totally_unknown", "x"), ("naming", "off")]));
        assert_eq!(c.naming, NamingMode::Off);
        assert!(c.header);
    }

    #[test]
    fn density_default_is_cards() {
        assert_eq!(Config::default().density, Density::Cards);
        // absent → Cards
        assert_eq!(Config::from_map(&map(&[])).density, Density::Cards);
    }

    #[test]
    fn density_parses_all_variants() {
        assert_eq!(
            Config::from_map(&map(&[("density", "compact")])).density,
            Density::Compact
        );
        assert_eq!(
            Config::from_map(&map(&[("density", "comfortable")])).density,
            Density::Comfortable
        );
        assert_eq!(
            Config::from_map(&map(&[("density", "cards")])).density,
            Density::Cards
        );
    }

    #[test]
    fn density_unknown_value_falls_back_to_cards() {
        assert_eq!(
            Config::from_map(&map(&[("density", "super-dense")])).density,
            Density::Cards
        );
        assert_eq!(
            Config::from_map(&map(&[("density", "")])).density,
            Density::Cards
        );
    }

    #[test]
    fn density_is_case_insensitive() {
        assert_eq!(
            Config::from_map(&map(&[("density", "COMPACT")])).density,
            Density::Compact
        );
        assert_eq!(
            Config::from_map(&map(&[("density", "Cards")])).density,
            Density::Cards
        );
    }

    #[test]
    fn apply_overrides_flips_two_fields_leaves_others_unchanged() {
        // Start from defaults: naming=Managed, density=Comfortable, header=true, glyphs=Plain
        let mut c = Config::default();
        let kv = map(&[("density", "cards"), ("naming", "managed")]);
        c.apply_overrides(&kv);
        // density must flip to Cards
        assert_eq!(c.density, Density::Cards);
        // naming stays Managed (was already Managed — value explicitly set)
        assert_eq!(c.naming, NamingMode::Managed);
        // header and glyphs must be unchanged
        assert!(c.header);
        assert_eq!(c.glyphs, crate::status::GlyphSet::Plain);
    }

    #[test]
    fn apply_overrides_unknown_key_ignored() {
        let mut c = Config::default();
        let kv = map(&[("totally_unknown_key", "something"), ("density", "compact")]);
        c.apply_overrides(&kv);
        assert_eq!(c.density, Density::Compact);
        // everything else is default
        assert_eq!(c.naming, NamingMode::Managed);
        assert!(c.header);
        assert_eq!(c.glyphs, crate::status::GlyphSet::Plain);
    }

    #[test]
    fn apply_overrides_all_four_fields() {
        let mut c = Config::default();
        let kv = map(&[
            ("naming", "force"),
            ("density", "compact"),
            ("glyphs", "nerd"),
            ("header", "false"),
        ]);
        c.apply_overrides(&kv);
        assert_eq!(c.naming, NamingMode::Force);
        assert_eq!(c.density, Density::Compact);
        assert_eq!(c.glyphs, crate::status::GlyphSet::Nerd);
        assert!(!c.header);
    }

    #[test]
    fn naming_from_config_parses_all_variants() {
        assert_eq!(NamingMode::from_config("off"), NamingMode::Off);
        assert_eq!(NamingMode::from_config("managed"), NamingMode::Managed);
        assert_eq!(NamingMode::from_config("force"), NamingMode::Force);
        // unknown → default (Managed)
        assert_eq!(NamingMode::from_config("wat"), NamingMode::Managed);
        // case-insensitive
        assert_eq!(NamingMode::from_config("OFF"), NamingMode::Off);
        assert_eq!(NamingMode::from_config("Force"), NamingMode::Force);
    }

    #[test]
    fn from_map_agrees_with_apply_overrides_on_every_key() {
        // `from_map` is generated as "defaults, then apply_overrides", so both
        // config paths interpret every key identically by construction. Pin it
        // so a future hand-rewrite of either path can't silently let them drift
        // (the property the single `config_fields!` table exists to guarantee).
        let inputs = map(&[
            ("naming", "force"),
            ("density", "compact"),
            ("glyphs", "nerd"),
            ("header", "false"),
            ("unknown_key", "ignored"),
        ]);
        let via_from_map = Config::from_map(&inputs);
        let mut via_apply = Config::default();
        via_apply.apply_overrides(&inputs);
        assert_eq!(via_from_map, via_apply);
    }
}
