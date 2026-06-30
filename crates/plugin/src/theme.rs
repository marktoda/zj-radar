//! Surface + dim colors derived from the terminal's own background/foreground.
//!
//! The sidebar's *status hues* (waiting/error/working/done/accent) are emitted as
//! ANSI-16 codes elsewhere, so the terminal renders them in its own theme and they
//! always match. This module owns only the *dark-panel* part: the subtle card
//! surfaces and dim greys, which are truecolor and so MUST be derived from the
//! terminal's real `default_bg`/`default_fg` (reported per-pane in `PaneInfo`) to
//! sit correctly against whatever theme the terminal is using.

/// An (r, g, b) color triple. Only consumed by the wasm glue (the `PaneUpdate`
/// handler), so it looks dead to host test/non-test builds.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub type Rgb = (u8, u8, u8);

/// Parse a hex color string (`"#rrggbb"` or `"rrggbb"`) into an (r, g, b) triple.
/// Returns `None` for anything that isn't exactly six hex digits (optionally
/// prefixed with `#`).
pub fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    let h = s.strip_prefix('#').unwrap_or(s);
    if h.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&h[0..2], 16).ok()?;
    let g = u8::from_str_radix(&h[2..4], 16).ok()?;
    let b = u8::from_str_radix(&h[4..6], 16).ok()?;
    Some((r, g, b))
}

/// Linear per-channel blend: t=0 → a, t=1 → b.
pub fn blend(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let ch = |ac: u8, bc: u8| {
        (ac as f32 + (bc as f32 - ac as f32) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    (ch(a.0, b.0), ch(a.1, b.1), ch(a.2, b.2))
}

/// Surface + dim colors derived from the terminal's background/foreground.
///
/// The sidebar is a cohesive DARK PANEL: `rail_bg` is the panel base (a "crust"
/// one step darker than the terminal bg), and the three card surfaces form a
/// subtle ladder UP from it — so cards read as *barely-there* steps within the
/// panel rather than light-grey bars on a dark rail. Only `surface_active` ever
/// climbs above the terminal bg, so the focused card gently pops.
///
/// These are the only truecolor values the renderer uses; the status hues are
/// ANSI-16 and rendered by the terminal in its own theme.
#[derive(Clone, Debug)]
pub struct DerivedColors {
    /// The dark panel base — the whole sidebar column sits on this.
    pub rail_bg: (u8, u8, u8),
    /// Card surface when idle (barely above the panel — idle recedes).
    pub surface_idle: (u8, u8, u8),
    /// Card surface when an agent is running
    pub surface_agent: (u8, u8, u8),
    /// Card surface when the row is active/focused (the only one brighter than bg).
    pub surface_active: (u8, u8, u8),
    /// Strong dim: detail location / spinner line
    pub dim_strong: (u8, u8, u8),
    /// Idle text dim: row name when idle
    pub idle_text: (u8, u8, u8),
}

impl DerivedColors {
    /// Derive the panel ladder + dims from the terminal's bg/fg.
    ///
    /// The panel base recedes by darkening the terminal bg. The *depth* is
    /// polarity-aware: a deep crust on a dark terminal, but only a gentle step
    /// on a light one — darkening a near-white bg by 30% slaps a muddy mid-grey
    /// slab on the terminal and collapses the dims' contrast (which blend fg→bg).
    /// A gentle step keeps a light terminal's panel light, so its dark dims stay
    /// legible. The design requires "legible on light"; the dark path is
    /// unchanged (`dim_text_keeps_contrast_against_its_surface_in_both_polarities`).
    pub fn from_bg_fg(bg: (u8, u8, u8), fg: (u8, u8, u8)) -> Self {
        let is_dark = luminance(bg) <= luminance(fg);
        let recede = if is_dark { 0.30 } else { 0.08 };
        let rail_bg = blend(bg, (0, 0, 0), recede);
        DerivedColors {
            rail_bg,
            // A ladder from the panel toward the terminal bg, so cards are subtle
            // steps. Only `surface_active` steps past bg (toward fg).
            surface_idle: blend(rail_bg, bg, 0.30),
            surface_agent: blend(rail_bg, bg, 0.72),
            surface_active: blend(bg, fg, 0.18),
            dim_strong: blend(fg, bg, 0.28),
            idle_text: blend(fg, bg, 0.45),
        }
    }
}

/// Channel-sum luminance — a cheap brightness proxy, enough to tell a dark
/// terminal theme (bg darker than fg) from a light one.
fn luminance((r, g, b): (u8, u8, u8)) -> u32 {
    r as u32 + g as u32 + b as u32
}

/// Neutral-dark fallback used until the terminal reports its own colors. A
/// generic dark — NOT branded — so an unthemed/unreported terminal still gets a
/// reasonable dark panel.
pub const FALLBACK_BG: (u8, u8, u8) = (26, 27, 38);
pub const FALLBACK_FG: (u8, u8, u8) = (192, 202, 220);

impl Default for DerivedColors {
    fn default() -> Self {
        DerivedColors::from_bg_fg(FALLBACK_BG, FALLBACK_FG)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_at_zero_returns_a() {
        let a = (10u8, 20u8, 30u8);
        let b = (100u8, 150u8, 200u8);
        assert_eq!(blend(a, b, 0.0), a);
    }

    #[test]
    fn blend_at_one_returns_b() {
        let a = (10u8, 20u8, 30u8);
        let b = (100u8, 150u8, 200u8);
        assert_eq!(blend(a, b, 1.0), b);
    }

    #[test]
    fn blend_at_half_returns_midpoint() {
        let a = (0u8, 0u8, 0u8);
        let b = (100u8, 200u8, 50u8);
        let mid = blend(a, b, 0.5);
        assert_eq!(mid.0, 50);
        assert_eq!(mid.1, 100);
        assert_eq!(mid.2, 25);
    }

    // ── parse_hex ──

    #[test]
    fn parse_hex_with_hash() {
        assert_eq!(parse_hex("#1a1b26"), Some((0x1a, 0x1b, 0x26)));
    }

    #[test]
    fn parse_hex_without_hash() {
        assert_eq!(parse_hex("c0cadc"), Some((0xc0, 0xca, 0xdc)));
    }

    #[test]
    fn parse_hex_uppercase() {
        assert_eq!(parse_hex("#FF00AA"), Some((255, 0, 170)));
    }

    #[test]
    fn parse_hex_bad_input_is_none() {
        assert_eq!(parse_hex(""), None);
        assert_eq!(parse_hex("#fff"), None); // too short
        assert_eq!(parse_hex("#1a1b2"), None); // 5 digits
        assert_eq!(parse_hex("#1a1b266"), None); // 7 digits
        assert_eq!(parse_hex("#gggggg"), None); // non-hex
        assert_eq!(parse_hex("rgb(1,2,3)"), None);
    }

    fn lum(c: (u8, u8, u8)) -> u32 {
        c.0 as u32 + c.1 as u32 + c.2 as u32
    }

    #[test]
    fn dim_text_keeps_contrast_against_its_surface_in_both_polarities() {
        // The truecolor dims (idle row name / detail line) must stay legible
        // against the card surface they sit on, whether the terminal is dark or
        // light. The design requires "legible on light"; a fixed dark-panel
        // derivation darkens a near-white terminal into a muddy mid-grey slab,
        // collapsing the contrast. Channel-sum luminance distance is a coarse but
        // stable proxy — the dark path clears this comfortably.
        const MIN_DELTA: u32 = 250;
        let check = |bg, fg, who: &str| {
            let d = DerivedColors::from_bg_fg(bg, fg);
            let delta = lum(d.surface_idle).abs_diff(lum(d.idle_text));
            assert!(
                delta >= MIN_DELTA,
                "{who}: idle-name contrast {delta} < {MIN_DELTA} \
                 (surface {:?}, text {:?})",
                d.surface_idle,
                d.idle_text
            );
        };
        check((26, 27, 38), (192, 202, 220), "dark");
        check((250, 250, 250), (40, 40, 40), "light");
    }

    #[test]
    fn rail_bg_is_darker_than_terminal_bg() {
        // The panel base is a "crust" one step darker than the terminal bg.
        let d = DerivedColors::from_bg_fg(FALLBACK_BG, FALLBACK_FG);
        assert!(
            lum(d.rail_bg) < lum(FALLBACK_BG),
            "rail_bg {:?} must be darker than bg {:?}",
            d.rail_bg,
            FALLBACK_BG
        );
    }

    #[test]
    fn surface_ladder_is_ordered_up_from_rail() {
        // Cards are subtle steps UP from the dark panel: rail_bg ≤ idle < agent,
        // and the active card is the only one brighter than the terminal bg.
        let d = DerivedColors::from_bg_fg(FALLBACK_BG, FALLBACK_FG);
        assert!(
            lum(d.rail_bg) <= lum(d.surface_idle),
            "idle {:?} must sit at or above rail_bg {:?}",
            d.surface_idle,
            d.rail_bg
        );
        assert!(
            lum(d.surface_idle) < lum(d.surface_agent),
            "agent {:?} must be brighter than idle {:?}",
            d.surface_agent,
            d.surface_idle
        );
        assert!(
            lum(d.surface_active) > lum(FALLBACK_BG),
            "active {:?} must be brighter than terminal bg {:?}",
            d.surface_active,
            FALLBACK_BG
        );
    }

    #[test]
    fn dims_are_between_fg_and_bg() {
        let bg = FALLBACK_BG;
        let fg = FALLBACK_FG;
        let d = DerivedColors::from_bg_fg(bg, fg);
        // dims blend fg toward bg, so they should be dimmer than fg but brighter than bg
        let l_bg = lum(bg);
        let l_fg = lum(fg);
        for &dim in &[d.dim_strong, d.idle_text] {
            let l = lum(dim);
            assert!(l > l_bg, "dim {:?} not brighter than bg", dim);
            assert!(l < l_fg, "dim {:?} not dimmer than fg", dim);
        }
    }

    #[test]
    fn default_derives_from_neutral_fallback() {
        // The fallback theme matches deriving from the neutral-dark bg/fg.
        let d = DerivedColors::default();
        let expected = DerivedColors::from_bg_fg(FALLBACK_BG, FALLBACK_FG);
        assert_eq!(d.rail_bg, expected.rail_bg);
        assert_eq!(d.surface_active, expected.surface_active);
    }
}
