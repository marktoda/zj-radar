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

/// Colors derived from the current theme's bg/fg palette pair.
#[derive(Clone, Debug)]
pub struct DerivedColors {
    /// Card surface when idle
    pub surface_idle: (u8, u8, u8),
    /// Card surface when an agent is running
    pub surface_agent: (u8, u8, u8),
    /// Card surface when the row is active/focused
    pub surface_active: (u8, u8, u8),
    /// Strong dim: detail location / spinner line
    pub dim_strong: (u8, u8, u8),
    /// Weak dim: quoted message line
    pub dim_weak: (u8, u8, u8),
    /// Idle text dim: row name when idle
    pub idle_text: (u8, u8, u8),
}

impl DerivedColors {
    pub fn from_bg_fg(bg: (u8, u8, u8), fg: (u8, u8, u8)) -> Self {
        DerivedColors {
            surface_idle: blend(bg, fg, 0.05),
            surface_agent: blend(bg, fg, 0.09),
            surface_active: blend(bg, fg, 0.16),
            dim_strong: blend(fg, bg, 0.28),
            dim_weak: blend(fg, bg, 0.55),
            idle_text: blend(fg, bg, 0.45),
        }
    }
}

/// Catppuccin Mocha defaults used until the first ModeUpdate arrives.
pub const MOCHA_BG: (u8, u8, u8) = (30, 30, 46);
pub const MOCHA_FG: (u8, u8, u8) = (205, 214, 244);

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

    #[test]
    fn derived_surfaces_are_lighter_than_bg_on_dark_theme() {
        let bg = MOCHA_BG;
        let fg = MOCHA_FG;
        let d = DerivedColors::from_bg_fg(bg, fg);
        // In a dark theme (bg < fg), surfaces should be brighter than bg
        let lum = |c: (u8, u8, u8)| c.0 as u32 + c.1 as u32 + c.2 as u32;
        assert!(lum(d.surface_idle) > lum(bg));
        assert!(lum(d.surface_agent) > lum(d.surface_idle));
        assert!(lum(d.surface_active) > lum(d.surface_agent));
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
