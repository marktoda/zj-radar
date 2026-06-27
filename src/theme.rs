// `PaletteColor` lives in the zellij_tile dependency, which drags in the full
// zellij-utils stack (including curl/openssl). Gate the import + the converter
// to wasm32 so that host `cargo test` builds remain curl-free.
#[cfg(target_arch = "wasm32")]
use zellij_tile::prelude::PaletteColor;

/// Convert a PaletteColor to an (r, g, b) triple.
/// Only available in the wasm plugin build (zellij_tile is curl-heavy).
#[cfg(target_arch = "wasm32")]
pub fn palette_color_to_rgb(c: PaletteColor) -> (u8, u8, u8) {
    match c {
        PaletteColor::Rgb((r, g, b)) => (r, g, b),
        PaletteColor::EightBit(i) => eight_bit_to_rgb(i),
    }
}

#[cfg(any(target_arch = "wasm32", test))]
fn eight_bit_to_rgb(i: u8) -> (u8, u8, u8) {
    match i {
        0 => (0, 0, 0),
        1 => (128, 0, 0),
        2 => (0, 128, 0),
        3 => (128, 128, 0),
        4 => (0, 0, 128),
        5 => (128, 0, 128),
        6 => (0, 128, 128),
        7 => (192, 192, 192),
        8 => (128, 128, 128),
        9 => (255, 0, 0),
        10 => (0, 255, 0),
        11 => (255, 255, 0),
        12 => (0, 0, 255),
        13 => (255, 0, 255),
        14 => (0, 255, 255),
        15 => (255, 255, 255),
        16..=231 => {
            let idx = i - 16;
            let b = idx % 6;
            let g = (idx / 6) % 6;
            let r = idx / 36;
            let to_val = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            (to_val(r), to_val(g), to_val(b))
        }
        232..=255 => {
            let v = 8 + (i - 232) * 10;
            (v, v, v)
        }
    }
}

/// Linear per-channel blend: t=0 → a, t=1 → b.
pub fn blend(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let ch = |ac: u8, bc: u8| (ac as f32 + (bc as f32 - ac as f32) * t).round().clamp(0.0, 255.0) as u8;
    (ch(a.0, b.0), ch(a.1, b.1), ch(a.2, b.2))
}

/// Colors derived from the current theme's palette.
///
/// The sidebar is a cohesive DARK PANEL: `rail_bg` is the panel base (a "crust"
/// one step darker than the terminal bg), and the three card surfaces form a
/// subtle ladder UP from it — so cards read as *barely-there* steps within the
/// panel rather than light-grey bars on a dark rail. Only `surface_active` ever
/// climbs above the terminal bg, so the focused card gently pops.
///
/// The role hues (`attention`/`error`/`working`/`success`/`accent`) are vivid
/// truecolor values pulled straight from the theme palette so labels read in the
/// theme's own colors — crucially, waiting is `attention` (peach/orange), clearly
/// distinct from the red `error`.
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
    /// Weak dim: quoted message line
    pub dim_weak: (u8, u8, u8),
    /// Idle text dim: row name when idle
    pub idle_text: (u8, u8, u8),
    /// Waiting / "needs you" — peach/orange (NOT red).
    pub attention: (u8, u8, u8),
    /// Error — red.
    pub error: (u8, u8, u8),
    /// Working — yellow.
    pub working: (u8, u8, u8),
    /// Done — green.
    pub success: (u8, u8, u8),
    /// Active spine / accent — mauve.
    pub accent: (u8, u8, u8),
}

impl DerivedColors {
    /// Derive every surface + dim from bg/fg and the five role hues.
    ///
    /// `bg`/`fg` are the terminal background/foreground; the five hues are the
    /// theme's peach (attention), red (error), yellow (working), green (success),
    /// and mauve (accent).
    fn derive(
        bg: (u8, u8, u8),
        fg: (u8, u8, u8),
        attention: (u8, u8, u8),
        error: (u8, u8, u8),
        working: (u8, u8, u8),
        success: (u8, u8, u8),
        accent: (u8, u8, u8),
    ) -> Self {
        // The panel base: one step darker than the terminal bg.
        let rail_bg = blend(bg, (0, 0, 0), 0.30);
        DerivedColors {
            rail_bg,
            // A ladder UP from the dark panel toward the terminal bg, so cards
            // are dark/subtle steps. Only `surface_active` rises above bg.
            surface_idle: blend(rail_bg, bg, 0.12),
            surface_agent: blend(rail_bg, bg, 0.50),
            surface_active: blend(bg, fg, 0.10),
            dim_strong: blend(fg, bg, 0.28),
            dim_weak: blend(fg, bg, 0.55),
            idle_text: blend(fg, bg, 0.45),
            attention,
            error,
            working,
            success,
            accent,
        }
    }

    /// Build a palette derived only from bg/fg, using the Catppuccin Mocha role
    /// hues. Used by `Default` (the pre-ModeUpdate fallback) and host tests.
    pub fn from_bg_fg(bg: (u8, u8, u8), fg: (u8, u8, u8)) -> Self {
        DerivedColors::derive(
            bg,
            fg,
            MOCHA_PEACH,
            MOCHA_RED,
            MOCHA_YELLOW,
            MOCHA_GREEN,
            MOCHA_MAUVE,
        )
    }

    /// Build a palette from the full Zellij theme palette: extract bg/fg + the
    /// five role hues and derive everything. Only available in the wasm plugin
    /// build (`Palette`/`palette_color_to_rgb` drag in the curl-heavy
    /// zellij-utils stack; host tests use `from_bg_fg`/`Default`).
    #[cfg(target_arch = "wasm32")]
    pub fn from_palette(palette: &zellij_tile::prelude::Palette) -> Self {
        DerivedColors::derive(
            palette_color_to_rgb(palette.bg),
            palette_color_to_rgb(palette.fg),
            palette_color_to_rgb(palette.orange), // attention = peach
            palette_color_to_rgb(palette.red),    // error
            palette_color_to_rgb(palette.yellow), // working
            palette_color_to_rgb(palette.green),  // success
            palette_color_to_rgb(palette.magenta), // accent = mauve
        )
    }
}

/// Catppuccin Mocha defaults used until the first ModeUpdate arrives.
pub const MOCHA_BG: (u8, u8, u8) = (30, 30, 46);
pub const MOCHA_FG: (u8, u8, u8) = (205, 214, 244);
pub const MOCHA_PEACH: (u8, u8, u8) = (250, 179, 135);
pub const MOCHA_RED: (u8, u8, u8) = (243, 139, 168);
pub const MOCHA_YELLOW: (u8, u8, u8) = (249, 226, 175);
pub const MOCHA_GREEN: (u8, u8, u8) = (166, 227, 161);
pub const MOCHA_MAUVE: (u8, u8, u8) = (203, 166, 247);

impl Default for DerivedColors {
    fn default() -> Self {
        DerivedColors::from_bg_fg(MOCHA_BG, MOCHA_FG)
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

    fn lum(c: (u8, u8, u8)) -> u32 {
        c.0 as u32 + c.1 as u32 + c.2 as u32
    }

    #[test]
    fn rail_bg_is_darker_than_terminal_bg() {
        // The panel base is a "crust" one step darker than the terminal bg.
        let d = DerivedColors::from_bg_fg(MOCHA_BG, MOCHA_FG);
        assert!(lum(d.rail_bg) < lum(MOCHA_BG),
            "rail_bg {:?} must be darker than bg {:?}", d.rail_bg, MOCHA_BG);
    }

    #[test]
    fn surface_ladder_is_ordered_up_from_rail() {
        // Cards are subtle steps UP from the dark panel: rail_bg ≤ idle < agent,
        // and the active card is the only one brighter than the terminal bg.
        let d = DerivedColors::from_bg_fg(MOCHA_BG, MOCHA_FG);
        assert!(lum(d.rail_bg) <= lum(d.surface_idle),
            "idle {:?} must sit at or above rail_bg {:?}", d.surface_idle, d.rail_bg);
        assert!(lum(d.surface_idle) < lum(d.surface_agent),
            "agent {:?} must be brighter than idle {:?}", d.surface_agent, d.surface_idle);
        assert!(lum(d.surface_active) > lum(MOCHA_BG),
            "active {:?} must be brighter than terminal bg {:?}", d.surface_active, MOCHA_BG);
    }

    #[test]
    fn default_exposes_mocha_role_hues() {
        // The pre-ModeUpdate fallback carries the Catppuccin Mocha role hues.
        let d = DerivedColors::default();
        assert_eq!(d.attention, MOCHA_PEACH, "attention should be Mocha peach");
        assert_eq!(d.error, MOCHA_RED, "error should be Mocha red");
        assert_eq!(d.working, MOCHA_YELLOW, "working should be Mocha yellow");
        assert_eq!(d.success, MOCHA_GREEN, "success should be Mocha green");
        assert_eq!(d.accent, MOCHA_MAUVE, "accent should be Mocha mauve");
        // Waiting (attention/peach) must be distinct from error (red).
        assert_ne!(d.attention, d.error, "attention must differ from error");
    }

    #[test]
    fn dims_are_between_fg_and_bg() {
        let bg = MOCHA_BG;
        let fg = MOCHA_FG;
        let d = DerivedColors::from_bg_fg(bg, fg);
        // dims blend fg toward bg, so they should be dimmer than fg but brighter than bg
        let lum = |c: (u8, u8, u8)| c.0 as u32 + c.1 as u32 + c.2 as u32;
        let l_bg = lum(bg);
        let l_fg = lum(fg);
        for &dim in &[d.dim_strong, d.dim_weak, d.idle_text] {
            let l = lum(dim);
            assert!(l > l_bg, "dim {:?} not brighter than bg", dim);
            assert!(l < l_fg, "dim {:?} not dimmer than fg", dim);
        }
    }

    #[test]
    fn eight_bit_grayscale_ramp() {
        // 232 should be near black, 255 near white
        let (r, g, b) = eight_bit_to_rgb(232);
        assert_eq!((r, g, b), (8, 8, 8));
        let (r, g, b) = eight_bit_to_rgb(255);
        assert_eq!((r, g, b), (238, 238, 238));
    }

    #[test]
    fn eight_bit_cube_white() {
        // index 231 = max of cube = (255, 255, 255)
        let (r, g, b) = eight_bit_to_rgb(231);
        assert_eq!((r, g, b), (255, 255, 255));
    }
}
