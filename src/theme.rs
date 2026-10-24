//! The five kit themes and their palette roles.
//!
//! Every theme defines the same roles (text/sub/dim/…/yellow plus chrome
//! colors). `clear` and `system` use the terminal's own background
//! (`Color::Reset`); `system` additionally uses the terminal's ANSI palette,
//! so gradients and shimmers degrade to flat colors there (`ansi == true`).

use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::BorderType;

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub name: &'static str,
    /// terminal-native palette: nothing honest to blend
    pub ansi: bool,
    pub bg: Color,
    pub text: Color,
    pub sub: Color,
    pub dim: Color,
    pub faint: Color,
    pub vfaint: Color,
    pub blue: Color,
    pub lav: Color,
    pub peach: Color,
    pub green: Color,
    pub red: Color,
    pub mauve: Color,
    pub yellow: Color,
    pub border: Color,
    pub border_focus: Color,
    pub border_alt: Color,
    pub modal_bg: Color,
    pub cursor: Color,
    /// `Rounded` (default) or `Plain` (square corners) — every box uses this
    pub bt: BorderType,
}

/// `(r, g, b)` if the color is a truecolor value.
pub fn rgb_of(c: Color) -> Option<(u8, u8, u8)> {
    match c {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    }
}

/// Mix two colors. Anything that isn't truecolor can't be blended honestly
/// (the terminal owns the real values), so `a` is returned untouched.
pub fn blend(a: Color, b: Color, t: f64) -> Color {
    let (Some(x), Some(y)) = (rgb_of(a), rgb_of(b)) else {
        return a;
    };
    let t = t.clamp(0.0, 1.0);
    let mix = |p: u8, q: u8| (p as f64 + (q as f64 - p as f64) * t).round() as u8;
    Color::Rgb(mix(x.0, y.0), mix(x.1, y.1), mix(x.2, y.2))
}

const MOCHA: Theme = Theme {
    name: "mocha",
    ansi: false,
    bg: Color::Rgb(0x1e, 0x1e, 0x2e),
    text: Color::Rgb(0xcd, 0xd6, 0xf4),
    sub: Color::Rgb(0xa6, 0xad, 0xc8),
    dim: Color::Rgb(0x6c, 0x70, 0x86),
    faint: Color::Rgb(0x45, 0x47, 0x5a),
    vfaint: Color::Rgb(0x31, 0x32, 0x44),
    blue: Color::Rgb(0x89, 0xb4, 0xfa),
    lav: Color::Rgb(0xb4, 0xbe, 0xfe),
    peach: Color::Rgb(0xfa, 0xb3, 0x87),
    green: Color::Rgb(0xa6, 0xe3, 0xa1),
    red: Color::Rgb(0xf3, 0x8b, 0xa8),
    mauve: Color::Rgb(0xcb, 0xa6, 0xf7),
    yellow: Color::Rgb(0xf9, 0xe2, 0xaf),
    border: Color::Rgb(0x45, 0x47, 0x5a),
    border_focus: Color::Rgb(0x89, 0xb4, 0xfa),
    border_alt: Color::Rgb(0xb4, 0xbe, 0xfe),
    modal_bg: Color::Rgb(0x18, 0x18, 0x25),
    cursor: Color::Rgb(0x3e, 0x48, 0x69),
    bt: BorderType::Rounded,
};

const VOID: Theme = Theme {
    name: "void",
    bg: Color::Rgb(0, 0, 0),
    border: Color::Rgb(0x26, 0x26, 0x2e),
    modal_bg: Color::Rgb(0x0a, 0x0a, 0x10),
    cursor: Color::Rgb(0x1e, 0x2a, 0x4a),
    vfaint: Color::Rgb(0x31, 0x32, 0x44),
    ..MOCHA
};

const ONYX: Theme = Theme {
    name: "onyx",
    bg: Color::Rgb(0x0e, 0x0e, 0x11),
    text: Color::Rgb(0xd4, 0xd6, 0xdd),
    blue: Color::Rgb(0x9a, 0xa5, 0xb5),
    lav: Color::Rgb(0xb8, 0xc0, 0xcc),
    mauve: Color::Rgb(0x7d, 0x84, 0x94),
    border: Color::Rgb(0x33, 0x33, 0x3c),
    border_focus: Color::Rgb(0x9a, 0xa5, 0xb5),
    border_alt: Color::Rgb(0xb8, 0xc0, 0xcc),
    modal_bg: Color::Rgb(0x14, 0x14, 0x19),
    cursor: Color::Rgb(0x2b, 0x30, 0x3b),
    ..MOCHA
};

// no background at all — the terminal's own shows through
const CLEAR: Theme = Theme {
    name: "clear",
    bg: Color::Reset,
    blue: Color::Rgb(0x8a, 0x93, 0xa5),
    lav: Color::Rgb(0xa9, 0xb1, 0xc0),
    mauve: Color::Rgb(0x6f, 0x77, 0x87),
    border: Color::Rgb(0x3c, 0x3f, 0x4a),
    border_focus: Color::Rgb(0x8a, 0x93, 0xa5),
    border_alt: Color::Rgb(0xa9, 0xb1, 0xc0),
    modal_bg: Color::Rgb(0x16, 0x16, 0x1d),
    cursor: Color::Rgb(0x28, 0x2c, 0x38),
    ..MOCHA
};

// the terminal's own ANSI palette + no background
const SYSTEM: Theme = Theme {
    name: "system",
    ansi: true,
    bg: Color::Reset,
    text: Color::Reset,
    sub: Color::Gray,
    dim: Color::DarkGray,
    faint: Color::DarkGray,
    vfaint: Color::DarkGray,
    blue: Color::Blue,
    lav: Color::LightBlue,
    peach: Color::Yellow,
    green: Color::Green,
    red: Color::Red,
    mauve: Color::Magenta,
    yellow: Color::Yellow,
    border: Color::DarkGray,
    border_focus: Color::Blue,
    border_alt: Color::LightBlue,
    modal_bg: Color::Black,
    cursor: Color::DarkGray,
    bt: BorderType::Rounded,
};

pub const THEMES: [Theme; 5] = [MOCHA, VOID, ONYX, CLEAR, SYSTEM];

pub fn by_name(name: &str) -> Theme {
    THEMES.iter().copied().find(|t| t.name == name).unwrap_or(MOCHA)
}

pub fn next_theme(name: &str) -> Theme {
    let i = THEMES.iter().position(|t| t.name == name).unwrap_or(0);
    THEMES[(i + 1) % THEMES.len()]
}

impl Theme {
    /// Nudge `blue`/`lav`/`mauve` toward an album color so borders, the
    /// progress bar and the now-playing line quietly take on the cover's hue.
    /// A no-op under the ANSI theme (nothing to blend).
    pub fn tinted(mut self, tint: Option<Color>) -> Self {
        if let (Some(t), false) = (tint, self.ansi) {
            self.blue = blend(self.blue, t, 0.55);
            self.lav = blend(self.lav, t, 0.50);
            self.mauve = blend(self.mauve, t, 0.45);
        }
        self
    }

    /// Square corners instead of rounded ones.
    pub fn with_square(mut self, square: bool) -> Self {
        self.bt = if square { BorderType::Plain } else { BorderType::Rounded };
        self
    }

    pub fn can_blend(&self) -> bool {
        !self.ansi
    }

    // ── style helpers ─────────────────────────────────────────────────
    /// The base style every frame is painted with.
    pub fn base(&self) -> Style {
        Style::default().fg(self.text).bg(self.bg)
    }
    pub fn fg(&self, c: Color) -> Style {
        Style::default().fg(c)
    }
    pub fn bold(&self, c: Color) -> Style {
        Style::default().fg(c).add_modifier(Modifier::BOLD)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_midpoint_and_ansi_passthrough() {
        let m = blend(Color::Rgb(0, 0, 0), Color::Rgb(200, 100, 50), 0.5);
        assert_eq!(m, Color::Rgb(100, 50, 25));
        assert_eq!(blend(Color::Blue, Color::Rgb(1, 2, 3), 0.5), Color::Blue);
    }

    #[test]
    fn tint_leaves_ansi_theme_alone() {
        let sys = by_name("system");
        let t = sys.tinted(Some(Color::Rgb(255, 0, 0)));
        assert_eq!(t.blue, sys.blue);
        let mocha = by_name("mocha").tinted(Some(Color::Rgb(255, 0, 0)));
        assert_ne!(mocha.blue, by_name("mocha").blue);
    }

    #[test]
    fn square_mode_changes_only_the_border_type() {
        let r = by_name("mocha");
        let q = r.with_square(true);
        assert_eq!(q.bt, BorderType::Plain);
        assert_eq!(r.bt, BorderType::Rounded);
        assert_eq!((q.blue, q.bg, q.text), (r.blue, r.bg, r.text), "colours untouched");
        assert_eq!(q.with_square(false).bt, BorderType::Rounded);
        assert!(THEMES.iter().all(|t| t.bt == BorderType::Rounded), "rounded is the default everywhere");
    }

    #[test]
    fn theme_cycle_wraps() {
        assert_eq!(next_theme("system").name, "mocha");
        assert_eq!(next_theme("mocha").name, "void");
    }
}
