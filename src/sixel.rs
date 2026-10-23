//! Sixel graphics: detect support, encode a cover, work out where it goes.
//!
//! Sixel is a bitmap format: the terminal receives the image as an escape
//! sequence (`DCS q … ST`) and paints pixels at the cursor. Three things make
//! it different from drawing text, and shape this module:
//!
//! 1. **Support and cell size are unknown.** Terminals advertise sixel in
//!    their Device Attributes reply (attribute `4`), and report their cell size
//!    in pixels via `CSI 16 t`. [`probe`] asks for both in one round trip.
//! 2. **It is a palette format** (≤256 registers), so true-color covers are
//!    quantised — exactly when they already fit, with NeuQuant otherwise.
//! 3. **It lives outside ratatui's cell buffer.** A sixel is ~100s of KB, and
//!    ratatui measures a cell's symbol width to decide what to redraw, so
//!    stuffing one into a cell would make it re-send the rest of the screen
//!    every frame. Instead the image is written straight to the terminal
//!    *after* each frame, and only when it actually needs to (see
//!    [`crate::app::App::sixel_sync`]).

use std::collections::HashMap;
use std::time::Duration;

use color_quant::NeuQuant;
use image::imageops::FilterType;
use image::RgbImage;
use ratatui::layout::Rect;

/// Largest edge we ever encode, in pixels. Bounds encode time and output size.
pub const MAX_SIDE: u32 = 640;
const PALETTE: usize = 256;

// ── terminal probing ──────────────────────────────────────────────────

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Probe {
    /// DA1 listed attribute 4 (sixel graphics)
    pub sixel: bool,
    /// cell size in pixels `(width, height)`, if the terminal reported it
    pub cell_px: Option<(u16, u16)>,
}

/// Parse the reply to `CSI 16 t` + `CSI c`. Pure, so it's unit-testable.
///
/// * cell size:  `ESC [ 6 ; <height> ; <width> t`
/// * attributes: `ESC [ ? 62 ; 4 ; 22 c`  (4 = sixel)
pub fn parse_probe(buf: &[u8]) -> Probe {
    let text = String::from_utf8_lossy(buf);
    let mut probe = Probe::default();
    if let Some(i) = text.find("\x1b[6;") {
        let rest = &text[i + 4..];
        if let Some(end) = rest.find('t') {
            let mut it = rest[..end].split(';').filter_map(|n| n.parse::<u16>().ok());
            if let (Some(h), Some(w)) = (it.next(), it.next()) {
                if w > 0 && h > 0 {
                    probe.cell_px = Some((w, h));
                }
            }
        }
    }
    if let Some(i) = text.find("\x1b[?") {
        let rest = &text[i + 3..];
        if let Some(end) = rest.find('c') {
            probe.sixel = rest[..end].split(';').any(|a| a == "4");
        }
    }
    probe
}

/// Is the DA1 reply (always the *last* thing a terminal answers) complete?
fn da1_complete(buf: &[u8]) -> bool {
    let t = String::from_utf8_lossy(buf);
    t.find("\x1b[?").is_some_and(|i| t[i..].contains('c'))
}

/// Ask the terminal what it supports. Must run in raw mode, *before* the
/// event reader starts (it reads stdin directly). Anything the user types in
/// the ~300ms this can take is swallowed.
#[cfg(unix)]
pub fn probe(timeout: Duration) -> Probe {
    use std::io::Write;
    use std::time::Instant;

    let mut out = std::io::stdout();
    if out.write_all(b"\x1b[16t\x1b[c").and_then(|_| out.flush()).is_err() {
        return Probe::default();
    }
    let mut buf = Vec::new();
    let start = Instant::now();
    while start.elapsed() < timeout && !da1_complete(&buf) {
        let left = timeout.saturating_sub(start.elapsed()).as_millis().clamp(1, 1000) as i32;
        let mut pfd = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
        // SAFETY: pfd is a valid pollfd and we pass nfds = 1.
        if unsafe { libc::poll(&mut pfd, 1, left) } <= 0 {
            break;
        }
        let mut tmp = [0u8; 256];
        // SAFETY: tmp is a valid writable buffer of the stated length.
        let n = unsafe { libc::read(0, tmp.as_mut_ptr().cast(), tmp.len()) };
        if n <= 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n as usize]);
    }
    parse_probe(&buf)
}

#[cfg(not(unix))]
pub fn probe(_timeout: Duration) -> Probe {
    Probe::default()
}

/// Cell size in pixels: the terminal's own report, else the pixel size of
/// the window divided by its cells, else a typical 10×20.
pub fn cell_size(probe: &Probe) -> (u16, u16) {
    if let Some(c) = probe.cell_px {
        return c;
    }
    if let Ok(ws) = crossterm::terminal::window_size() {
        if ws.width > 0 && ws.height > 0 && ws.columns > 0 && ws.rows > 0 {
            return (ws.width / ws.columns, ws.height / ws.rows);
        }
    }
    (10, 20)
}

// ── placement ─────────────────────────────────────────────────────────

/// Largest square (in pixels, a multiple of 6 — sixel is 6 pixels tall per
/// band) that fits `area`, capped at [`MAX_SIDE`].
pub fn side_for(area: Rect, cell: (u16, u16)) -> Option<u32> {
    let (cw, ch) = (cell.0.max(1) as u32, cell.1.max(1) as u32);
    let side = (area.width as u32 * cw).min(area.height as u32 * ch).min(MAX_SIDE) / 6 * 6;
    (side >= 12).then_some(side)
}

/// The cell rectangle an encoded `w × h` px image covers, centred in `area`.
pub fn cells_for(area: Rect, cell: (u16, u16), w: u32, h: u32) -> Rect {
    let (cw, ch) = (cell.0.max(1) as u32, cell.1.max(1) as u32);
    let cols = (w.div_ceil(cw) as u16).min(area.width);
    let rows = (h.div_ceil(ch) as u16).min(area.height);
    Rect::new(area.x + (area.width - cols) / 2, area.y + (area.height - rows) / 2, cols, rows)
}

// ── encoding ──────────────────────────────────────────────────────────

pub struct Encoded {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Decode `path`, fit it inside `side × side` (keeping aspect, height padded
/// to a multiple of 6), quantise and encode. Blocking — run it off the UI
/// thread.
pub fn encode_file(path: &std::path::Path, side: u32) -> Option<Encoded> {
    // cached covers have no file extension, so sniff the format from content
    let img = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?.to_rgb8();
    Some(encode(&fit(&img, side), PALETTE))
}

fn fit(img: &RgbImage, side: u32) -> RgbImage {
    let (w, h) = img.dimensions();
    let scale = side as f64 / w.max(h) as f64;
    let nw = ((w as f64 * scale).round() as u32).clamp(1, side);
    // sixel paints in 6-pixel bands: make the height a whole number of them
    let nh = (((h as f64 * scale).round() as u32).clamp(1, side) / 6 * 6).max(6);
    if (nw, nh) == (w, h) {
        return img.clone();
    }
    image::imageops::resize(img, nw, nh, FilterType::CatmullRom)
}

/// Map every pixel to a palette index. Images that already use ≤ `max`
/// distinct colors are encoded *exactly*; otherwise NeuQuant picks the palette.
fn quantize(img: &RgbImage, max: usize) -> (Vec<[u8; 3]>, Vec<u8>) {
    let mut seen: HashMap<[u8; 3], u8> = HashMap::new();
    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut idx: Vec<u8> = Vec::with_capacity((img.width() * img.height()) as usize);
    let mut exact = true;
    for p in img.pixels() {
        let c = p.0;
        let i = match seen.get(&c) {
            Some(&i) => i,
            None if palette.len() < max => {
                let i = palette.len() as u8;
                seen.insert(c, i);
                palette.push(c);
                i
            }
            None => {
                exact = false;
                break;
            }
        };
        idx.push(i);
    }
    if exact {
        return (palette, idx);
    }
    let rgba: Vec<u8> = img.pixels().flat_map(|p| [p.0[0], p.0[1], p.0[2], 255]).collect();
    // NeuQuant trains on 1 in `factor` pixels. Measured (release, 360×360
    // artwork): factor 1 = 86 ms / mean error 3.4, factor 10 = 18 ms / 3.8; on
    // small images the gap is wider (4.6 vs 6.8). It runs once per cover on a
    // background thread, so spend the milliseconds — but back off for very
    // large images, where the extra training buys little.
    let factor = if rgba.len() / 4 <= 200_000 { 1 } else { 3 };
    let nq = NeuQuant::new(factor, max, &rgba);
    let map = nq.color_map_rgba();
    let palette: Vec<[u8; 3]> = map.chunks_exact(4).map(|c| [c[0], c[1], c[2]]).collect();
    let idx = dither(img, &nq, &palette);
    (palette, idx)
}

/// Floyd–Steinberg error diffusion (serpentine scan) against the palette.
/// A palette of 256 across a smooth gradient shows hard contour bands;
/// pushing each pixel's rounding error onto its unvisited neighbours trades
/// the bands for fine grain the eye averages away.
fn dither(img: &RgbImage, nq: &NeuQuant, palette: &[[u8; 3]]) -> Vec<u8> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let mut out = vec![0u8; w * h];
    // error rows in 1/16ths, padded by one cell each side
    let mut cur = vec![[0i32; 3]; w + 2];
    let mut next = vec![[0i32; 3]; w + 2];
    for y in 0..h {
        let ltr = y % 2 == 0;
        for step in 0..w {
            let x = if ltr { step } else { w - 1 - step };
            let src = img.get_pixel(x as u32, y as u32).0;
            let mut px = [0u8; 4];
            px[3] = 255;
            for k in 0..3 {
                px[k] = (src[k] as i32 + cur[x + 1][k] / 16).clamp(0, 255) as u8;
            }
            let i = nq.index_of(&px);
            out[y * w + x] = i as u8;
            let chosen = palette[i];
            for k in 0..3 {
                // clamp so a wildly-off palette entry can't smear colour across the image
                let e = (px[k] as i32 - chosen[k] as i32).clamp(-48, 48);
                let (fwd, back) = if ltr { (x + 2, x) } else { (x, x + 2) };
                cur[fwd][k] += e * 7;
                next[back][k] += e * 3;
                next[x + 1][k] += e * 5;
                next[fwd][k] += e;
            }
        }
        std::mem::swap(&mut cur, &mut next);
        next.iter_mut().for_each(|e| *e = [0; 3]);
    }
    out
}

/// Encode as a sixel DCS string.
pub fn encode(img: &RgbImage, max_colors: usize) -> Encoded {
    let (w, h) = img.dimensions();
    let (palette, idx) = quantize(img, max_colors.min(PALETTE));
    let (wu, hu) = (w as usize, h as usize);

    let mut out: Vec<u8> = Vec::with_capacity(wu * hu / 3 + 4096);
    out.extend_from_slice(format!("\x1bP0;0;0q\"1;1;{w};{h}").as_bytes());
    for (i, c) in palette.iter().enumerate() {
        let pct = |v: u8| (v as u32 * 100 + 127) / 255;
        out.extend_from_slice(format!("#{i};2;{};{};{}", pct(c[0]), pct(c[1]), pct(c[2])).as_bytes());
    }

    // one 6-pixel-tall band at a time; per color, a row of 6-bit column masks
    let mut masks: Vec<Vec<u8>> = vec![Vec::new(); palette.len()];
    let mut used: Vec<usize> = Vec::new();
    for y0 in (0..hu).step_by(6) {
        used.clear();
        for dy in 0..6.min(hu - y0) {
            let row = &idx[(y0 + dy) * wu..(y0 + dy + 1) * wu];
            for (x, &c) in row.iter().enumerate() {
                let m = &mut masks[c as usize];
                if m.is_empty() {
                    m.resize(wu, 0);
                    used.push(c as usize);
                }
                m[x] |= 1 << dy;
            }
        }
        for (n, &c) in used.iter().enumerate() {
            out.extend_from_slice(format!("#{c}").as_bytes());
            rle(&masks[c], &mut out);
            out.push(if n + 1 == used.len() { b'-' } else { b'$' });
            masks[c].clear();
        }
    }
    if out.last() == Some(&b'-') {
        out.pop(); // no new line after the last band
    }
    out.extend_from_slice(b"\x1b\\");
    Encoded { data: out, width: w, height: h }
}

/// Run-length encode one color's column masks (`!<n><char>` for runs > 3).
fn rle(masks: &[u8], out: &mut Vec<u8>) {
    // trailing empty columns carry no information
    let end = masks.iter().rposition(|&m| m != 0).map_or(0, |p| p + 1);
    let mut i = 0;
    while i < end {
        let m = masks[i];
        let mut run = 1;
        while i + run < end && masks[i + run] == m {
            run += 1;
        }
        let ch = 63 + m;
        if run > 3 {
            out.extend_from_slice(format!("!{run}").as_bytes());
            out.push(ch);
        } else {
            out.extend(std::iter::repeat(ch).take(run));
        }
        i += run;
    }
}

// ── integration with the app ──────────────────────────────────────────

/// What is (or should be) painted on screen.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Shown {
    pub cover: String,
    pub side: u32,
    /// part of the identity: a theme change repaints every cell's background,
    /// which wipes the image, so it has to be sent again
    pub theme: String,
    pub rect: Rect,
    /// the art panel's area when this was drawn
    pub area: Rect,
}

#[derive(Default)]
pub struct SixelState {
    /// the one encoded image we keep: `(cover id, side px, data)`
    pub cache: Option<(String, u32, Encoded)>,
    pub pending: Option<(String, u32)>,
    /// an encode that failed — don't retry it every tick
    pub failed: Option<(String, u32)>,
    /// set by `draw` each frame: the cover + size wanted for the art panel
    pub want: Option<(String, u32, Rect)>,
    /// set by `draw` each frame: the art panel's area (even with no image)
    pub area_now: Option<Rect>,
    /// set by `draw` each frame: what should be visible right now
    pub show: Option<Shown>,
    /// what we last wrote to the terminal
    pub shown: Option<Shown>,
}

use crate::app::{App, Msg};

impl App {
    /// Kick off a background encode if the panel wants an image we don't have.
    pub fn ensure_sixel(&mut self) {
        let Some((cover, side, _)) = self.sixel.want.clone() else { return };
        let key = (cover.clone(), side);
        let have = matches!(&self.sixel.cache, Some((c, s, _)) if *c == cover && *s == side);
        if have || self.sixel.pending.as_ref() == Some(&key) || self.sixel.failed.as_ref() == Some(&key) {
            return;
        }
        let Some(path) = self.art_path.clone() else { return };
        self.sixel.pending = Some(key);
        let tx = self.tx.clone();
        self.spawn(async move {
            // decode + quantise + encode is CPU work: keep it off the UI task
            let enc = tokio::task::spawn_blocking(move || encode_file(&path, side)).await.ok().flatten();
            let _ = tx.send(Msg::Sixel { cover, side, enc });
        });
    }

    pub fn on_sixel(&mut self, cover: String, side: u32, enc: Option<Encoded>) {
        self.sixel.pending = None;
        match enc {
            Some(e) => self.sixel.cache = Some((cover, side, e)),
            None => self.sixel.failed = Some((cover, side)),
        }
        crate::mem::trim(); // the decode left a few MB of scratch behind
    }
}

/// Reconcile the terminal with what `draw` says should be visible. Runs after
/// every frame; does nothing unless the picture must change.
///
/// Rules, and why:
/// * the image is blank cells as far as ratatui knows, so its diff never
///   touches it — it stays put on its own until something changes it;
/// * if a modal/toast overlaps it we drop it (they'd paint over a bitmap the
///   buffer doesn't know about) and put it back when they go away;
/// * erasing with spaces is only safe when the cells are *still blank* in
///   ratatui's buffer — i.e. the art panel hasn't moved. Otherwise (zen
///   toggled, terminal resized, a modal was on top) we repaint the screen.
pub fn sync(app: &mut App, term: &mut ratatui::DefaultTerminal) -> std::io::Result<()> {
    use std::io::Write;

    if app.art_mode != crate::art::ArtMode::Sixel {
        return Ok(());
    }
    let hit = |overlays: &[Rect], r: Rect| overlays.iter().any(|o| o.intersects(r));
    let want = app.sixel.show.clone().filter(|s| !hit(&app.overlay_rects, s.rect));
    if want == app.sixel.shown {
        return Ok(());
    }
    if let Some(old) = app.sixel.shown.take() {
        let covered = hit(&app.overlay_rects, old.rect);
        let moved = app.sixel.area_now != Some(old.area);
        if covered || moved {
            term.clear()?; // next frame repaints everything; the image goes with it
            if want.is_some() {
                // repaint first, then draw the image on the following frame
                return Ok(());
            }
        } else {
            // theme backgrounds are either true color or "the terminal's own"
            let bg = match app.theme().bg {
                ratatui::style::Color::Rgb(r, g, b) => crossterm::style::Color::Rgb { r, g, b },
                _ => crossterm::style::Color::Reset,
            };
            let out = term.backend_mut();
            for y in old.rect.y..old.rect.y + old.rect.height {
                crossterm::queue!(
                    out,
                    crossterm::cursor::MoveTo(old.rect.x, y),
                    crossterm::style::SetBackgroundColor(bg),
                    crossterm::style::Print(" ".repeat(old.rect.width as usize)),
                    crossterm::style::ResetColor
                )?;
            }
            out.flush()?;
        }
    }
    if let Some(w) = want {
        if let Some((c, s, enc)) = &app.sixel.cache {
            if *c == w.cover && *s == w.side {
                let out = term.backend_mut();
                crossterm::queue!(out, crossterm::cursor::MoveTo(w.rect.x, w.rect.y))?;
                out.write_all(&enc.data)?;
                out.flush()?;
                app.sixel.shown = Some(w);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    /// A deliberately independent decoder, so the test doesn't just check the
    /// encoder against itself.
    fn decode(data: &[u8]) -> (u32, u32, Vec<Option<[u8; 3]>>) {
        let s = std::str::from_utf8(data).unwrap();
        let body = s.strip_prefix("\x1bP0;0;0q").expect("dcs header").strip_suffix("\x1b\\").expect("st");
        let b = body.as_bytes();
        let mut i = 0;
        let num = |i: &mut usize| {
            let st = *i;
            while *i < b.len() && b[*i].is_ascii_digit() {
                *i += 1;
            }
            body[st..*i].parse::<u32>().unwrap_or(0)
        };
        assert_eq!(b[i], b'"');
        i += 1;
        let (_, _, w, h) = {
            let a = num(&mut i);
            i += 1;
            let bb = num(&mut i);
            i += 1;
            let w = num(&mut i);
            i += 1;
            let h = num(&mut i);
            (a, bb, w, h)
        };
        let mut pal: HashMap<u32, [u8; 3]> = HashMap::new();
        let mut px = vec![None; (w * h) as usize];
        let (mut x, mut band, mut cur) = (0u32, 0u32, 0u32);
        while i < b.len() {
            match b[i] {
                b'#' => {
                    i += 1;
                    let n = num(&mut i);
                    if i < b.len() && b[i] == b';' {
                        i += 1;
                        assert_eq!(num(&mut i), 2, "RGB colour space");
                        let mut v = [0u8; 3];
                        for c in &mut v {
                            i += 1;
                            *c = ((num(&mut i) * 255 + 50) / 100) as u8;
                        }
                        pal.insert(n, v);
                    } else {
                        cur = n;
                    }
                }
                b'$' => {
                    x = 0;
                    i += 1;
                }
                b'-' => {
                    x = 0;
                    band += 1;
                    i += 1;
                }
                b'!' => {
                    i += 1;
                    let n = num(&mut i);
                    let ch = b[i];
                    i += 1;
                    for _ in 0..n {
                        paint(&mut px, w, h, x, band, ch - 63, pal.get(&cur).copied());
                        x += 1;
                    }
                }
                ch @ 63..=126 => {
                    paint(&mut px, w, h, x, band, ch - 63, pal.get(&cur).copied());
                    x += 1;
                    i += 1;
                }
                other => panic!("unexpected byte {other}"),
            }
        }
        (w, h, px)
    }

    fn paint(px: &mut [Option<[u8; 3]>], w: u32, h: u32, x: u32, band: u32, mask: u8, c: Option<[u8; 3]>) {
        for dy in 0..6 {
            let y = band * 6 + dy;
            if mask & (1 << dy) != 0 && x < w && y < h {
                px[(y * w + x) as usize] = c;
            }
        }
    }

    #[test]
    fn exact_palette_round_trips_losslessly() {
        // a flat-colour image like a lot of album art: stripes, a box, a gradient column
        let mut img = RgbImage::from_pixel(37, 24, Rgb([10, 20, 30]));
        for y in 0..24 {
            for x in 0..37 {
                if (x / 5) % 2 == 0 {
                    img.put_pixel(x, y, Rgb([200, 40, 40]));
                }
                if (8..20).contains(&x) && (5..17).contains(&y) {
                    img.put_pixel(x, y, Rgb([250, 250, 250]));
                }
            }
            img.put_pixel(36, y, Rgb([y as u8 * 10, 0, 255 - y as u8 * 10]));
        }
        let enc = encode(&img, 256);
        let (w, h, px) = decode(&enc.data);
        assert_eq!((w, h), (37, 24));
        for y in 0..24 {
            for x in 0..37 {
                // sixel percent palette → allow the ±1 rounding of 0..100 scaling
                let got = px[(y * 37 + x) as usize].expect("every pixel painted");
                let want = img.get_pixel(x, y).0;
                for k in 0..3 {
                    assert!((got[k] as i32 - want[k] as i32).abs() <= 2, "({x},{y}) {got:?} vs {want:?}");
                }
            }
        }
    }

    #[test]
    fn height_not_a_multiple_of_six_still_decodes() {
        let img = RgbImage::from_pixel(10, 8, Rgb([0, 128, 255]));
        let (w, h, px) = decode(&encode(&img, 256).data);
        assert_eq!((w, h), (10, 8));
        assert!(px.iter().all(Option::is_some));
    }

    #[test]
    fn photographic_image_is_quantised_to_the_palette_limit() {
        let mut img = RgbImage::new(64, 60);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = Rgb([(x * 4) as u8, (y * 4) as u8, ((x + y) * 2) as u8]); // a smooth ramp: thousands of distinct colours
        }
        let enc = encode(&img, 256);
        let text = String::from_utf8_lossy(&enc.data);
        // count real palette definitions (`#<n>;2;r;g;b`), not any ";2;" substring
        let regs = text
            .split('#')
            .filter(|seg| seg.trim_start_matches(|c: char| c.is_ascii_digit()).starts_with(";2;"))
            .count();
        assert!(regs <= 256 && regs > 64, "{regs} palette registers");
        let (_, _, px) = decode(&enc.data);
        // mean error should be small for a smooth gradient
        let mut err = 0u64;
        for (i, p) in img.pixels().enumerate() {
            let g = px[i].unwrap();
            err += (0..3).map(|k| (g[k] as i64 - p.0[k] as i64).unsigned_abs()).sum::<u64>();
        }
        let _ = err; // per-pixel error is *higher* with dithering by design; judge what the eye sees:
        // average each 4×4 block (the eye's low-pass) and compare with the source's blocks
        let mut block_err = 0u64;
        let mut n = 0u64;
        for by in (0..60).step_by(4) {
            for bx in (0..64).step_by(4) {
                for k in 0..3 {
                    let (mut a, mut b) = (0i64, 0i64);
                    for dy in 0..4 {
                        for dx in 0..4 {
                            a += img.get_pixel(bx + dx, by + dy).0[k] as i64;
                            b += px[((by + dy) * 64 + bx + dx) as usize].unwrap()[k] as i64;
                        }
                    }
                    block_err += ((a - b) / 16).unsigned_abs();
                    n += 1;
                }
            }
        }
        let mean = block_err as f64 / n as f64;
        assert!(mean < 2.5, "perceived (block-averaged) error too high: {mean:.2}");
    }

    #[test]
    fn runs_are_compressed() {
        let img = RgbImage::from_pixel(300, 12, Rgb([1, 2, 3]));
        let enc = encode(&img, 256);
        assert!(enc.data.len() < 200, "flat image should RLE to almost nothing, got {}", enc.data.len());
        assert!(String::from_utf8_lossy(&enc.data).contains("!300~"));
    }

    #[test]
    fn probe_parsing() {
        let p = parse_probe(b"\x1b[6;20;10t\x1b[?62;4;22c");
        assert_eq!(p, Probe { sixel: true, cell_px: Some((10, 20)) });
        let no = parse_probe(b"\x1b[?1;2c");
        assert_eq!(no, Probe { sixel: false, cell_px: None });
        // attribute "40" must not be mistaken for "4"
        assert!(!parse_probe(b"\x1b[?62;40;22c").sixel);
        // garbage / empty
        assert_eq!(parse_probe(b""), Probe::default());
        assert!(da1_complete(b"\x1b[6;1;1t\x1b[?62;4c"));
        assert!(!da1_complete(b"\x1b[6;1;1t"));
    }

    #[test]
    fn side_is_a_multiple_of_six_and_fits() {
        let area = Rect::new(0, 0, 34, 14);
        let side = side_for(area, (10, 20)).unwrap();
        assert_eq!(side % 6, 0);
        assert!(side <= 34 * 10 && side <= 14 * 20);
        assert_eq!(side_for(Rect::new(0, 0, 1, 1), (10, 20)), None);
        assert_eq!(side_for(Rect::new(0, 0, 200, 200), (10, 20)).unwrap(), MAX_SIDE / 6 * 6);
    }

    #[test]
    fn cells_are_centered_and_never_exceed_the_area() {
        let area = Rect::new(5, 2, 40, 12);
        let r = cells_for(area, (10, 20), 240, 240); // 24 cols x 12 rows
        assert_eq!((r.width, r.height), (24, 12));
        assert_eq!(r.x, 5 + (40 - 24) / 2);
        let big = cells_for(area, (10, 20), 9999, 9999);
        assert!(big.width <= area.width && big.height <= area.height);
    }

    #[test]
    fn fit_keeps_aspect_and_pads_height_to_bands() {
        let img = RgbImage::from_pixel(400, 300, Rgb([9, 9, 9]));
        let f = fit(&img, 120);
        assert_eq!(f.width(), 120);
        assert_eq!(f.height() % 6, 0);
        assert!((f.height() as i32 - 90).abs() <= 6);
    }
}
