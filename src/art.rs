//! Cover art: truecolor half-cells (the universal fallback) plus the
//! album-art tint color. Sixel output lives in `sixel.rs`.
//!
//! Each terminal cell shows two vertically stacked pixels via `▀` (foreground
//! = top pixel, background = bottom), which works in nearly every truecolor
//! terminal. (The Python version can also speak kitty/sixel; this port sticks
//! to half-cells. `RUSTDROME_ART=off` shows a placeholder instead.)

use std::cell::RefCell;
use std::path::Path;

use image::imageops::FilterType;
use image::RgbImage;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

/// Longest edge kept in memory. Half-cell rendering needs one pixel per
/// column and two per row, so even a very wide terminal never wants more
/// than ~200 — and the sixel path re-decodes from disk at exactly the size it
/// needs, so nothing larger has to stay resident (a 320px copy was 300 KB).
const MAX_EDGE: u32 = 192;

/// What size to ask the server for. Enough for a big sixel/zen cover; the
/// old 1200px request cost a 4 MB decode and a multi-MB cache file per album.
pub const SERVER_SIZE: u32 = 640;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtMode {
    Off,
    HalfCell,
    Sixel,
}

/// `RUSTDROME_ART` = `auto` (default) | `sixel` | `halfcell` | `off`.
/// `auto` uses sixel when the terminal says it supports it.
pub fn resolve_mode(env: Option<&str>, sixel_supported: bool) -> ArtMode {
    match env.map(str::to_ascii_lowercase).as_deref() {
        Some("off") | Some("none") => ArtMode::Off,
        Some("halfcell") | Some("half") | Some("blocks") => ArtMode::HalfCell,
        Some("sixel") => ArtMode::Sixel,
        _ if sixel_supported => ArtMode::Sixel,
        _ => ArtMode::HalfCell,
    }
}

pub fn load(path: &Path) -> Option<RgbImage> {
    // cached covers have no file extension, so sniff the format from content
    let img = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?.to_rgb8();
    let (w, h) = img.dimensions();
    if w.max(h) > MAX_EDGE {
        let s = MAX_EDGE as f64 / w.max(h) as f64;
        return Some(image::imageops::resize(
            &img,
            ((w as f64 * s).round() as u32).max(1),
            ((h as f64 * s).round() as u32).max(1),
            FilterType::Triangle,
        ));
    }
    Some(img)
}

/// Pull one vibrant dominant color out of the cover.
///
/// Downscales hard (a ~32px thumbnail is plenty for a single color) and
/// scores pixels by saturation × mid-weighted lightness, so a lively hue wins
/// over the near-black/near-white that dominates most covers.
pub fn extract_vibrant(img: &RgbImage) -> Option<Color> {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let s = 32.0 / w.max(h) as f64;
    let thumb = if s < 1.0 {
        image::imageops::thumbnail(
            img,
            ((w as f64 * s).round() as u32).max(1),
            ((h as f64 * s).round() as u32).max(1),
        )
    } else {
        img.clone()
    };

    let mut best: Option<((u8, u8, u8), f64)> = None;
    let mut fallback: ((u8, u8, u8), u32) = ((0, 0, 0), 0);
    for px in thumb.pixels() {
        let [r, g, b] = px.0;
        let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
        let lum = mx as u32 + mn as u32; // 0..510, cheap "lightness"
        let sat = if mx == 0 { 0.0 } else { (mx - mn) as f64 / mx as f64 };
        // mid-lightness weight: peaks around 50% grey, falls off at the ends
        let light = 1.0 - ((lum as f64 / 510.0) - 0.5).abs() * 2.0;
        let score = sat * (0.35 + 0.65 * light);
        if best.map_or(true, |(_, s)| score > s) {
            best = Some(((r, g, b), score));
        }
        if lum > fallback.1 {
            fallback = ((r, g, b), lum);
        }
    }
    let (r, g, b) = match best {
        Some((rgb, score)) if score > 0.12 => rgb,
        _ => fallback.0,
    };
    Some(Color::Rgb(r, g, b))
}

/// A loaded cover plus a one-entry cache of its last resampled size, so the
/// 8fps heartbeat doesn't resample every frame.
pub struct Art {
    src: RgbImage,
    scaled: RefCell<Option<(u32, RgbImage)>>,
}

impl Art {
    pub fn new(src: RgbImage) -> Self {
        Self { src, scaled: RefCell::new(None) }
    }

    pub fn tint(&self) -> Option<Color> {
        extract_vibrant(&self.src)
    }

    /// Draw the cover centered in `area`, as large as fits while staying
    /// square (a cell is ~1:2, so a square image is `w` cells × `w/2` rows).
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        // pixel budget: one px per column, two per row
        let side = (area.width as u32).min(area.height as u32 * 2).max(2);
        let side = side & !1; // even, so rows split cleanly
        let mut cache = self.scaled.borrow_mut();
        if cache.as_ref().map(|(s, _)| *s) != Some(side) {
            *cache = Some((side, image::imageops::resize(&self.src, side, side, FilterType::Triangle)));
        }
        let img = &cache.as_ref().expect("just filled").1;
        let cols = side as u16;
        let rows = (side / 2) as u16;
        let x0 = area.x + (area.width - cols) / 2;
        let y0 = area.y + (area.height - rows) / 2;
        for cy in 0..rows {
            for cx in 0..cols {
                let top = img.get_pixel(cx as u32, cy as u32 * 2).0;
                let bot = img.get_pixel(cx as u32, cy as u32 * 2 + 1).0;
                if let Some(cell) = buf.cell_mut((x0 + cx, y0 + cy)) {
                    cell.set_symbol("▀").set_style(
                        Style::default()
                            .fg(Color::Rgb(top[0], top[1], top[2]))
                            .bg(Color::Rgb(bot[0], bot[1], bot[2])),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    #[test]
    fn vibrant_prefers_saturated_hue_over_grey_majority() {
        let mut img = RgbImage::from_pixel(64, 64, Rgb([40, 40, 40]));
        for y in 0..10 {
            for x in 0..10 {
                img.put_pixel(x, y, Rgb([220, 30, 60]));
            }
        }
        let Some(Color::Rgb(r, g, b)) = extract_vibrant(&img) else { panic!("no color") };
        assert!(r > 150 && g < 100 && b < 120, "got {r},{g},{b}");
    }

    #[test]
    fn mode_resolution() {
        assert_eq!(resolve_mode(None, true), ArtMode::Sixel, "auto picks sixel when supported");
        assert_eq!(resolve_mode(Some("auto"), false), ArtMode::HalfCell);
        assert_eq!(resolve_mode(Some("off"), true), ArtMode::Off);
        assert_eq!(resolve_mode(Some("HalfCell"), true), ArtMode::HalfCell, "explicit choice wins, case-insensitive");
        assert_eq!(resolve_mode(Some("sixel"), false), ArtMode::Sixel, "forcing sixel skips the capability check");
        assert_eq!(resolve_mode(Some(""), false), ArtMode::HalfCell);
    }

    #[test]
    fn plain_grey_falls_back_to_brightest() {
        let mut img = RgbImage::from_pixel(8, 8, Rgb([30, 30, 30]));
        img.put_pixel(3, 3, Rgb([200, 200, 200]));
        assert_eq!(extract_vibrant(&img), Some(Color::Rgb(200, 200, 200)));
    }

    #[test]
    fn loads_extensionless_cache_files_by_sniffing_content() {
        let dir = std::env::temp_dir().join(format!("rustdrome-art-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("al1-1200"); // exactly how the cache names covers
        RgbImage::from_pixel(8, 8, Rgb([10, 200, 30])).save_with_format(&path, image::ImageFormat::Png).unwrap();
        let img = load(&path).expect("decodes without an extension");
        assert_eq!(img.dimensions(), (8, 8));
    }

    #[test]
    fn halfcell_paints_top_in_fg_and_bottom_in_bg() {
        // 2x2 image: top row red, bottom row blue
        let mut img = RgbImage::new(2, 2);
        for x in 0..2 {
            img.put_pixel(x, 0, Rgb([255, 0, 0]));
            img.put_pixel(x, 1, Rgb([0, 0, 255]));
        }
        let art = Art::new(img);
        let area = Rect::new(0, 0, 2, 1);
        let mut buf = Buffer::empty(area);
        art.render(area, &mut buf);
        let c = buf.cell((0, 0)).unwrap();
        assert_eq!(c.symbol(), "▀");
        assert_eq!(c.fg, Color::Rgb(255, 0, 0));
        assert_eq!(c.bg, Color::Rgb(0, 0, 255));
    }

    #[test]
    fn render_is_centered_and_stays_inside_the_area() {
        let art = Art::new(RgbImage::from_pixel(100, 100, Rgb([9, 9, 9])));
        let area = Rect::new(5, 2, 40, 10);
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 20));
        art.render(area, &mut buf);
        // 10 rows → 20 px square → 20 cols, centered in 40 cols
        assert_eq!(buf.cell((5 + 10, 2)).unwrap().symbol(), "▀");
        assert_eq!(buf.cell((5 + 9, 2)).unwrap().symbol(), " ");
        assert_eq!(buf.cell((5 + 30, 2)).unwrap().symbol(), " ");
    }
}
