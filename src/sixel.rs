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

/// What size/shape of image a panel wants — everything the encoder needs, and
/// the key the cache is matched on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanKey {
    /// longest edge of the picture itself, px
    pub side: u32,
    /// the cell-aligned canvas the picture is centred on, in cells
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, Copy)]
pub struct Plan {
    pub key: PlanKey,
    /// where the canvas goes on screen
    pub rect: Rect,
}

/// Fit a square picture into `area`, and size a canvas of whole cells around
/// it so that the picture is *exactly* centred.
///
/// Sixel can only be placed at a cell boundary, so a picture that isn't an
/// exact number of cells wide would otherwise sit up to a cell off-centre.
/// Instead the canvas is a whole number of cells whose parity matches the
/// area's (so the offset to centre it is a whole number of cells), and the
/// encoder centres the picture inside the canvas at pixel precision.
pub fn plan(area: Rect, cell: (u16, u16)) -> Option<Plan> {
    let (cw, ch) = (cell.0.max(1) as u32, cell.1.max(1) as u32);
    let side = (area.width as u32 * cw).min(area.height as u32 * ch).min(MAX_SIDE);
    if side < 12 {
        return None;
    }
    let mut cols = (side.div_ceil(cw) as u16).min(area.width);
    let mut rows = (side.div_ceil(ch) as u16).min(area.height);
    if (area.width - cols) % 2 == 1 && cols < area.width {
        cols += 1;
    }
    if (area.height - rows) % 2 == 1 && rows < area.height {
        rows += 1;
    }
    let rect = Rect::new(area.x + (area.width - cols) / 2, area.y + (area.height - rows) / 2, cols, rows);
    Some(Plan { key: PlanKey { side, cols, rows }, rect })
}

// ── encoding ──────────────────────────────────────────────────────────

#[allow(dead_code)] // width/height document the canvas and are asserted in tests
pub struct Encoded {
    pub data: Vec<u8>,
    /// the canvas, in pixels (a whole number of cells)
    pub width: u32,
    pub height: u32,
    /// where the picture itself sits on the canvas, in pixels: `(x, y, w, h)`.
    /// Everything outside it is transparent, everything inside is opaque —
    /// which is what lets a new image be drawn straight over an old one
    /// without erasing the cells it will cover anyway (see [`covered_cells`])
    pub pic: (u32, u32, u32, u32),
}

/// Decode `path`, fit it inside `plan.side`, quantise, and encode it centred on
/// the plan's canvas. Blocking — run it off the UI thread.
pub fn encode_file(path: &std::path::Path, plan: &Plan, cell: (u16, u16)) -> Option<Encoded> {
    Some(encode_image(&crate::art::decode(path)?, plan, cell))
}

/// Encode an already-decoded cover for `plan` (so a caller that has the pixels
/// doesn't decode the file again).
pub fn encode_image(img: &RgbImage, plan: &Plan, cell: (u16, u16)) -> Encoded {
    let (cw, ch) = (plan.key.cols as u32 * cell.0.max(1) as u32, plan.key.rows as u32 * cell.1.max(1) as u32);
    encode_canvas(&fit(img, plan.key.side), cw, ch, PALETTE)
}

// ── live preview ──────────────────────────────────────────────────────
//
// Sixel is a bitmap: the terminal cannot scale it, so a picture that follows a
// panel while it is dragged has to be *re-encoded* at every size. The quality
// encoder (NeuQuant + Floyd–Steinberg, ≈40 ms at 640 px) is too slow for that,
// and withdrawing the picture until the drag ends leaves a hole. So while the
// layout is moving we send a cheap approximation instead — a bilinear resample
// of the resident copy onto a fixed colour cube with ordered dithering, a few
// milliseconds — and swap in the real encode once things settle.

/// Levels per channel of the preview's fixed colour cube: 6 × 7 × 6 = 252
/// registers (green gets the extra level; the eye is most sensitive there).
const PV: (u32, u32, u32) = (6, 7, 6);

/// 4×4 Bayer thresholds.
const BAYER: [u32; 16] = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];

fn preview_palette() -> Vec<[u8; 3]> {
    let lv = |i: u32, n: u32| (i * 255 / (n - 1)) as u8;
    let mut p = Vec::with_capacity((PV.0 * PV.1 * PV.2) as usize);
    for r in 0..PV.0 {
        for g in 0..PV.1 {
            for b in 0..PV.2 {
                p.push([lv(r, PV.0), lv(g, PV.1), lv(b, PV.2)]);
            }
        }
    }
    p
}

/// Quick, lower-quality encode of `src` for `plan` — same canvas, same
/// picture size and the same pixel-exact centring as [`encode_image`], so the
/// swap to the real encode changes the picture's quality, never its place.
pub fn encode_preview(src: &RgbImage, plan: &Plan, cell: (u16, u16)) -> Encoded {
    let (cw, ch) = (plan.key.cols as u32 * cell.0.max(1) as u32, plan.key.rows as u32 * cell.1.max(1) as u32);
    let (sw, sh) = src.dimensions();
    let side = plan.key.side.max(1);
    let scale = side as f64 / sw.max(sh).max(1) as f64;
    let nw = ((sw as f64 * scale).round() as u32).clamp(1, side);
    let nh = ((sh as f64 * scale).round() as u32).clamp(1, side);
    if sw == 0 || sh == 0 {
        return emit(&preview_palette(), &vec![0; (nw * nh) as usize], nw, nh, cw, ch);
    }

    // sample positions in 16.16 fixed point, computed once per row/column
    let axis = |n: u32, s: u32| -> Vec<(usize, usize, u32)> {
        (0..n)
            .map(|i| {
                let pos = ((i as u64 * 2 + 1) * s as u64 * 65536 / (n as u64 * 2)) as i64 - 32768;
                let pos = pos.max(0);
                let a = ((pos >> 16) as u32).min(s - 1);
                let b = (a + 1).min(s - 1);
                (a as usize, b as usize, ((pos & 0xFFFF) >> 8) as u32)
            })
            .collect()
    };
    let (xs, ys) = (axis(nw, sw), axis(nh, sh));
    let raw = src.as_raw();
    let stride = sw as usize * 3;
    let level = |c: u32, n: u32, t: u32| {
        let v = c * (n - 1) * 16 / 255;
        (v >> 4) + ((v & 15) > t) as u32
    };

    let mut idx = vec![0u8; (nw * nh) as usize];
    for (y, &(ya, yb, wy)) in ys.iter().enumerate() {
        let (ra, rb) = (&raw[ya * stride..], &raw[yb * stride..]);
        for (x, &(xa, xb, wx)) in xs.iter().enumerate() {
            let t = BAYER[(y & 3) * 4 + (x & 3)];
            let mut q = [0u32; 3];
            for (k, qk) in q.iter_mut().enumerate() {
                let top = ra[xa * 3 + k] as u32 * (256 - wx) + ra[xb * 3 + k] as u32 * wx;
                let bot = rb[xa * 3 + k] as u32 * (256 - wx) + rb[xb * 3 + k] as u32 * wx;
                let c = (top * (256 - wy) + bot * wy) >> 16;
                *qk = c.min(255);
            }
            let (r, g, b) = (level(q[0], PV.0, t), level(q[1], PV.1, t), level(q[2], PV.2, t));
            idx[y * nw as usize + x] = ((r * PV.1 + g) * PV.2 + b) as u8;
        }
    }
    emit(&preview_palette(), &idx, nw, nh, cw, ch)
}

/// The cells that `enc`, placed with its canvas at `rect`, covers *completely*
/// with opaque pixels. Drawing the image overwrites those cells' old contents,
/// so a repaint need not blank them first — which is what keeps a picture
/// from flickering when it is replaced by one of nearly the same size.
pub fn covered_cells(enc: &Encoded, rect: Rect, cell: (u16, u16)) -> Rect {
    let (cw, ch) = (cell.0.max(1) as u32, cell.1.max(1) as u32);
    let (px, py, pw, ph) = enc.pic;
    let (x0, y0) = (px.div_ceil(cw), py.div_ceil(ch));
    let (x1, y1) = ((px + pw) / cw, (py + ph) / ch);
    if x1 <= x0 || y1 <= y0 {
        return Rect::default();
    }
    let r = Rect::new(rect.x + x0 as u16, rect.y + y0 as u16, (x1 - x0) as u16, (y1 - y0) as u16);
    r.intersection(rect)
}

/// Does the window's pixel size still add up to `cell` — i.e. is the cell size
/// the one we measured? A resize that only adds or removes whole cells leaves
/// `W ≈ cols × cell_w` (plus less than one cell of slack and a little padding);
/// a font zoom or a DPI change does not. `None` when the terminal doesn't
/// report pixel sizes, so nothing can be said.
pub fn cell_consistent(win: (u16, u16, u16, u16), cell: (u16, u16)) -> Option<bool> {
    let (cols, rows, w, h) = (win.0 as u32, win.1 as u32, win.2 as u32, win.3 as u32);
    if cols == 0 || rows == 0 || w == 0 || h == 0 {
        return None;
    }
    const PAD: u32 = 24; // window padding / borders some terminals add
    let (cw, ch) = (cell.0.max(1) as u32, cell.1.max(1) as u32);
    let ok = |px: u32, n: u32, c: u32| px + 1 >= n * c && px < (n + 1) * c + PAD;
    Some(ok(w, cols, cw) && ok(h, rows, ch))
}

fn fit(img: &RgbImage, side: u32) -> RgbImage {
    let (w, h) = img.dimensions();
    let scale = side as f64 / w.max(h) as f64;
    let nw = ((w as f64 * scale).round() as u32).clamp(1, side);
    let nh = ((h as f64 * scale).round() as u32).clamp(1, side);
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
    // The palette only needs the image's colour *distribution*, which a
    // thumbnail has just as well as the full picture. Training NeuQuant on every
    // pixel was most of the encode time (≈90 ms of ≈100 at 360 px); training on
    // ≤160 px costs a few milliseconds and picks a palette of the same quality.
    let train = if img.width().max(img.height()) > TRAIN_EDGE {
        let s = TRAIN_EDGE as f64 / img.width().max(img.height()) as f64;
        image::imageops::thumbnail(img, ((img.width() as f64 * s).round() as u32).max(1), ((img.height() as f64 * s).round() as u32).max(1))
    } else {
        img.clone()
    };
    let rgba: Vec<u8> = train.pixels().flat_map(|p| [p.0[0], p.0[1], p.0[2], 255]).collect();
    let nq = NeuQuant::new(1, max, &rgba);
    let map = nq.color_map_rgba();
    let palette: Vec<[u8; 3]> = map.chunks_exact(4).map(|c| [c[0], c[1], c[2]]).collect();
    let idx = dither(img, &nq, &palette);
    (palette, idx)
}

/// Longest edge of the thumbnail the palette is trained on.
const TRAIN_EDGE: u32 = 160;

/// Floyd–Steinberg error diffusion (serpentine scan) against the palette.
/// A palette of 256 across a smooth gradient shows hard contour bands;
/// pushing each pixel's rounding error onto its unvisited neighbours trades
/// the bands for fine grain the eye averages away.
fn dither(img: &RgbImage, nq: &NeuQuant, palette: &[[u8; 3]]) -> Vec<u8> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let mut out = vec![0u8; w * h];
    // Nearest-palette lookups are the hot loop (one per pixel). Colours repeat
    // endlessly, so memoise them on a 6-bit-per-channel grid (256 K entries,
    // 512 KB, freed when the encode ends): tens of thousands of real searches
    // instead of hundreds of thousands. (5 bits was faster still but visibly
    // grainier.) The error diffusion below corrects for the grid's coarseness.
    let mut lut = vec![u16::MAX; 1 << 18];
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
            let cell = ((px[0] as usize >> 2) << 12) | ((px[1] as usize >> 2) << 6) | (px[2] as usize >> 2);
            let i = match lut[cell] {
                u16::MAX => {
                    // search with the middle of the grid cell, not the pixel itself
                    let probe = [(px[0] & 0xFC) | 2, (px[1] & 0xFC) | 2, (px[2] & 0xFC) | 2, 255];
                    let i = nq.index_of(&probe);
                    lut[cell] = i as u16;
                    i
                }
                known => known as usize,
            };
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

/// Encode `img` as-is (canvas = the image). Used by tests.
#[cfg(test)]
pub fn encode(img: &RgbImage, max_colors: usize) -> Encoded {
    encode_canvas(img, img.width(), img.height(), max_colors)
}

/// Encode `img` centred on a `cw × ch` px canvas. Pixels outside the picture
/// are left unset (`P2 = 1`: transparent), so the cells underneath show
/// through and the padding costs nothing.
pub fn encode_canvas(img: &RgbImage, cw: u32, ch: u32, max_colors: usize) -> Encoded {
    let (palette, idx) = quantize(img, max_colors.min(PALETTE));
    emit(&palette, &idx, img.width(), img.height(), cw, ch)
}

/// Write palette-indexed pixels (`w × h`, row-major) as a sixel, centred on a
/// `cw × ch` canvas. The shared back half of the quality encoder and the
/// live-resize preview.
fn emit(palette: &[[u8; 3]], idx: &[u8], w: u32, h: u32, cw: u32, ch: u32) -> Encoded {
    let (cw, ch) = (cw.max(w), ch.max(h));
    let (ox, oy) = ((cw - w) / 2, (ch - h) / 2);
    let (wu, hu) = (w as usize, h as usize);
    let (cwu, chu, oxu, oyu) = (cw as usize, ch as usize, ox as usize, oy as usize);

    let mut out: Vec<u8> = Vec::with_capacity(wu * hu / 3 + 4096);
    out.extend_from_slice(format!("\x1bP0;1;0q\"1;1;{cw};{ch}").as_bytes());
    for (i, c) in palette.iter().enumerate() {
        let pct = |v: u8| (v as u32 * 100 + 127) / 255;
        out.extend_from_slice(format!("#{i};2;{};{};{}", pct(c[0]), pct(c[1]), pct(c[2])).as_bytes());
    }

    // one 6-pixel-tall band of the *canvas* at a time; per colour, a row of
    // 6-bit column masks
    let mut masks: Vec<Vec<u8>> = vec![Vec::new(); palette.len()];
    let mut used: Vec<usize> = Vec::new();
    for y0 in (0..chu).step_by(6) {
        used.clear();
        for dy in 0..6 {
            let cy = y0 + dy;
            if cy >= chu {
                break;
            }
            if cy < oyu || cy >= oyu + hu {
                continue; // padding row
            }
            let row = &idx[(cy - oyu) * wu..(cy - oyu + 1) * wu];
            for (x, &c) in row.iter().enumerate() {
                let m = &mut masks[c as usize];
                if m.is_empty() {
                    m.resize(cwu, 0);
                    used.push(c as usize);
                }
                m[x + oxu] |= 1 << dy;
            }
        }
        for (n, &c) in used.iter().enumerate() {
            out.extend_from_slice(format!("#{c}").as_bytes());
            rle(&masks[c], &mut out);
            out.push(if n + 1 == used.len() { b'-' } else { b'$' });
            masks[c].clear();
        }
        if used.is_empty() {
            out.push(b'-'); // a band of pure padding must still advance the row
        }
    }
    if out.last() == Some(&b'-') {
        out.pop(); // no new line after the last band
    }
    out.extend_from_slice(b"\x1b\\");
    Encoded { data: out, width: cw, height: ch, pic: (ox, oy, w, h) }
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
    pub key: PlanKey,
    /// part of the identity: a theme change repaints every cell's background,
    /// which wipes the image, so it has to be sent again
    pub theme: String,
    pub rect: Rect,
    /// the art panel's area when this was drawn
    pub area: Rect,
    /// the quick live-resize approximation rather than the quality encode.
    /// Part of the identity, so the swap to the real one is a repaint
    pub preview: bool,
}

#[derive(Default)]
pub struct SixelState {
    /// the one encoded image we keep: `(cover id, side px, data)`
    pub cache: Option<(String, PlanKey, Encoded)>,
    pub pending: Option<(String, PlanKey)>,
    /// an encode that failed — don't retry it every tick
    pub failed: Option<(String, PlanKey)>,
    /// set by `draw` each frame: the cover + size wanted for the art panel
    pub want: Option<(String, Plan)>,
    /// set by `draw` each frame: the art panel's area (even with no image)
    pub area_now: Option<Rect>,
    /// the plan for the panel as last laid out (kept across frames), so covers
    /// prepared in the background can pre-encode for it
    pub plan_hint: Option<(Plan, (u16, u16))>,
    /// set by `draw` each frame: what should be visible right now
    pub show: Option<Shown>,
    /// what we last wrote to the terminal
    pub shown: Option<Shown>,
    /// the live-resize approximation for the cover being resized: `(cover id, size, data)`.
    /// Cheap to rebuild and dropped as soon as the real encode has replaced it
    pub preview: Option<(String, PlanKey, Encoded)>,
    /// cells of the picture's old rectangle that the new frame left blank, with
    /// their background — the pixels under them are stale and `sync` must
    /// blank them (see [`note_frame`])
    pub blanks: Vec<(u16, u16, ratatui::style::Color)>,
}

use crate::app::{App, Msg};

impl App {
    /// The layout is still changing — a divider is being dragged, a panel is being moved, or the
    /// window / font was just resized and the cell size hasn't been re-read
    /// yet. No image is planned or shown meanwhile: a plan made with a stale
    /// cell size is the wrong size by definition, and encoding it only gets in
    /// the way of the right one.
    pub fn settling(&self) -> bool {
        self.drag.is_some() || self.moving.is_some() || self.resize_at.is_some()
    }

    /// The panel is being resized right now (divider drag or window/font
    /// resize) and the picture should *follow* it with the quick preview
    /// encode rather than disappear. A panel being carried around (`moving`)
    /// still withdraws it: the move overlay owns that gesture.
    pub fn live_resize(&self) -> bool {
        self.moving.is_none() && (self.drag.is_some() || self.resize_at.is_some())
    }

    /// Kick off a background encode if the panel wants an image we don't have.
    /// One at a time: a second request waits for the first to land, so results
    /// can't arrive out of order and a stale one can never overwrite a newer.
    pub fn ensure_sixel(&mut self) {
        if self.settling() {
            return;
        }
        let Some((cover, plan)) = self.sixel.want.clone() else { return };
        let key = (cover.clone(), plan.key);
        let have = matches!(&self.sixel.cache, Some((c, k, _)) if *c == cover && *k == plan.key);
        if have || self.sixel.pending.is_some() || self.sixel.failed.as_ref() == Some(&key) {
            return;
        }
        let Some(path) = self.art_path.clone() else { return };
        self.sixel.pending = Some(key);
        let (tx, cell) = (self.tx.clone(), self.cell_px);
        self.spawn(async move {
            // decode + quantise + encode is CPU work: keep it off the UI task
            let enc = tokio::task::spawn_blocking(move || encode_file(&path, &plan, cell)).await.ok().flatten();
            let _ = tx.send(Msg::Sixel { cover, key: plan.key, enc });
        });
    }

    /// Keep `cell_px` true as the font size or monitor changes.
    ///
    /// The cell size in pixels is what turns "this many cells" into "this many
    /// pixels", so a stale value makes the picture the wrong size for its
    /// panel (too small after zooming the font in, too big after zooming out).
    ///
    /// * [`App::note_window`] runs on every resize event *and* every tick (a DPI
    ///   change can leave the cell counts alone). It tells a plain resize —
    ///   whole cells added or removed, cell size unchanged — from a font or DPI
    ///   change, and for the latter switches to a pixels ÷ cells estimate at
    ///   once, so the live preview is about the right size straight away;
    /// * only a font/DPI change needs the exact answer: after it has settled for
    ///   150ms, re-ask the terminal (`CSI 16 t`). A plain resize skips that;
    /// * terminals that never answer fall back to window pixels ÷ cells.
    pub fn maintain_cell_size(&mut self, input: &crate::input::Input) {
        if self.art_mode != crate::art::ArtMode::Sixel {
            return;
        }
        self.note_window();
        let Some(t) = self.resize_at else { return };
        if t.elapsed() < Duration::from_millis(150) {
            return; // still moving (a drag-resize fires dozens of events)
        }
        self.resize_at = None;
        if !std::mem::take(&mut self.cell_stale) {
            return; // the cell size is still the one we measured
        }
        let ws = crossterm::terminal::window_size().ok().map(|w| (w.columns, w.rows, w.width, w.height));
        let mut cell = None;
        if self.cell_probe_ok {
            match input.paused(|| probe(Duration::from_millis(250))).cell_px {
                Some(c) => cell = Some(c),
                None => self.cell_probe_ok = false, // doesn't answer: stop asking
            }
        }
        if cell.is_none() {
            if let Some((c, r, w, h)) = ws {
                if c > 0 && r > 0 && w >= c && h >= r {
                    cell = Some((w / c, h / r));
                }
            }
        }
        if let Some(c) = cell {
            self.cell_px = c;
        }
    }

    /// The window's size (cells or pixels) changed: start (or extend) the live
    /// resize, and decide whether the cell size can still be trusted.
    pub fn note_window(&mut self) {
        let ws = crossterm::terminal::window_size().ok().map(|w| (w.columns, w.rows, w.width, w.height));
        if ws == self.win_sig {
            return;
        }
        self.win_sig = ws;
        self.resize_at = Some(std::time::Instant::now());
        match ws.and_then(|w| cell_consistent(w, self.cell_px)) {
            Some(true) => {} // only the number of cells changed
            Some(false) => {
                // font zoom / DPI change: pixels ÷ cells is close enough for the
                // preview; the exact value is read once the window settles
                if let Some((c, r, w, h)) = ws {
                    self.cell_px = ((w / c).max(1), (h / r).max(1));
                }
                self.cell_stale = true;
            }
            None => self.cell_stale = true, // no pixel size to judge by
        }
    }

    /// A `Resize` event arrived: start the live resize. (If the size really
    /// changed, ratatui clears the screen and `draw_step` forgets the picture;
    /// the next frame then draws the preview at the new size inside the same
    /// synchronized update.)
    pub fn on_resize(&mut self) {
        if self.art_mode == crate::art::ArtMode::Sixel {
            self.resize_at = Some(std::time::Instant::now());
            self.note_window();
        }
    }

    pub fn on_sixel(&mut self, cover: String, key: PlanKey, enc: Option<Encoded>) {
        self.sixel.pending = None;
        // keep it only if it is still what the panel wants (the layout may have
        // moved on while this was encoding); otherwise `ensure_sixel` asks again
        let wanted = matches!(&self.sixel.want, Some((c, p)) if *c == cover && p.key == key);
        match enc {
            Some(e) if wanted => self.sixel.cache = Some((cover, key, e)),
            None if wanted => self.sixel.failed = Some((cover, key)),
            _ => {}
        }
        crate::mem::trim(); // the decode left a few MB of scratch behind
    }
}

/// For each row of `rect`, the runs of cells `(y, x_start, x_end)` that no
/// overlay (modal, toast) covers. Those are the only cells that can still hold
/// stale picture pixels: anything an overlay drew over was already repainted.
pub fn free_spans(rect: Rect, overlays: &[Rect]) -> Vec<(u16, u16, u16)> {
    let covered = |x: u16, y: u16| overlays.iter().any(|o| o.x <= x && x < o.x + o.width && o.y <= y && y < o.y + o.height);
    let mut spans = Vec::new();
    for y in rect.y..rect.y + rect.height {
        let mut x = rect.x;
        let end = rect.x + rect.width;
        while x < end {
            if covered(x, y) {
                x += 1;
                continue;
            }
            let start = x;
            while x < end && !covered(x, y) {
                x += 1;
            }
            spans.push((y, start, x));
        }
    }
    spans
}

fn term_color(bg: ratatui::style::Color) -> crossterm::style::Color {
    // theme backgrounds are either true color or "the terminal's own"
    match bg {
        ratatui::style::Color::Rgb(r, g, b) => crossterm::style::Color::Rgb { r, g, b },
        _ => crossterm::style::Color::Reset,
    }
}

/// Queue blanks over runs `(y, x_start, x_end, background)`.
fn queue_blanks(
    out: &mut impl std::io::Write,
    runs: impl IntoIterator<Item = (u16, u16, u16, ratatui::style::Color)>,
) -> std::io::Result<()> {
    for (y, x0, x1, bg) in runs {
        crossterm::queue!(
            out,
            crossterm::cursor::MoveTo(x0, y),
            crossterm::style::SetBackgroundColor(term_color(bg)),
            crossterm::style::Print(" ".repeat((x1 - x0) as usize)),
            crossterm::style::ResetColor
        )?;
    }
    Ok(())
}

/// Runs of the cells in `cells` (row-major, as `note_frame` records them) that
/// no `skip` rectangle covers, merged while the background stays the same.
fn blank_runs(cells: &[(u16, u16, ratatui::style::Color)], skip: &[Rect]) -> Vec<(u16, u16, u16, ratatui::style::Color)> {
    let covered = |x: u16, y: u16| skip.iter().any(|o| o.x <= x && x < o.x + o.width && o.y <= y && y < o.y + o.height);
    let mut runs: Vec<(u16, u16, u16, ratatui::style::Color)> = Vec::new();
    for &(x, y, bg) in cells.iter().filter(|&&(x, y, _)| !covered(x, y)) {
        match runs.last_mut() {
            Some((ly, _, lx1, lbg)) if *ly == y && *lx1 == x && *lbg == bg => *lx1 = x + 1,
            _ => runs.push((y, x, x + 1, bg)),
        }
    }
    runs
}

/// The encoded image `w` refers to, if it is still the one we hold.
fn image_for<'a>(st: &'a SixelState, w: &Shown) -> Option<&'a Encoded> {
    let slot = if w.preview { &st.preview } else { &st.cache };
    slot.as_ref().filter(|(c, k, _)| *c == w.cover && *k == w.key).map(|(_, _, e)| e)
}

/// Called right after a frame is drawn, with the buffer ratatui just painted.
///
/// When the art panel's *area* has changed under a picture that is on screen
/// (a divider is being dragged), ratatui has repainted every cell whose content
/// differs — but the cells that were blank under the old picture and are still
/// blank are, to ratatui, unchanged, so it never touched them and the old
/// pixels sit there. Those are exactly the cells in the old rectangle that are
/// blank in the new buffer; record them so [`sync`] can blank just those.
/// (Blanking the whole old rectangle would wipe the new neighbours' borders.)
pub fn note_frame(app: &mut App, buf: &ratatui::buffer::Buffer) {
    app.sixel.blanks.clear();
    let Some(rect) = app.sixel.shown.as_ref().map(|s| s.rect) else { return };
    let (Some(now), Some(old_area)) = (app.sixel.area_now, app.sixel.shown.as_ref().map(|s| s.area)) else { return };
    if now == old_area || app.moving.is_some() {
        return;
    }
    let r = rect.intersection(buf.area);
    for y in r.y..r.y + r.height {
        for x in r.x..r.x + r.width {
            let c = &buf[(x, y)];
            if c.symbol() == " " {
                app.sixel.blanks.push((x, y, c.bg));
            }
        }
    }
}

/// Reconcile the terminal with what `draw` says should be visible. Runs after
/// every frame; does nothing unless the picture must change.
///
/// Rules, and why:
/// * the image is blank cells as far as ratatui knows, so its diff never
///   touches it — it stays put on its own until something changes it;
/// * **a picture that is replaced is not withdrawn first.** Its new version is
///   drawn over it, and only the cells the new one will *not* fully cover are
///   blanked (`covered_cells`) — all in one step inside the synchronized update.
///   So a drag or a resize shows a picture that follows the panel, never a hole;
/// * to take it down for good, blank *just its cells* (minus whatever a modal or
///   toast already painted over). A full-screen clear would leave the whole
///   terminal black until the next frame;
/// * if the panel's area changed under it, the cells to blank are the ones the
///   new frame left blank (`note_frame`), because ratatui has already painted
///   the rest. Outside a live resize (a panel being moved, a layout switch) the
///   screen is cleared instead and the function returns `true`: the caller must
///   redraw **immediately**, in the same step, so nothing is ever seen blank.
pub fn sync(app: &mut App, term: &mut ratatui::DefaultTerminal) -> std::io::Result<bool> {
    use std::io::Write;

    if app.art_mode != crate::art::ArtMode::Sixel {
        return Ok(false);
    }
    let bg = app.theme().bg;
    if app.moving.is_some() {
        // a panel is being carried: drop the picture once and wait — the move
        // overlay is drawn over where it was, and the layout under it is changing
        if let Some(old) = app.sixel.shown.take() {
            if app.sixel.area_now == Some(old.area) {
                let skip = app.overlay_rects.clone();
                let out = term.backend_mut();
                queue_blanks(out, free_spans(old.rect, &skip).into_iter().map(|(y, a, b)| (y, a, b, bg)))?;
                out.flush()?;
            } else {
                return clear_screen(term);
            }
        }
        return Ok(false);
    }
    let hit = |overlays: &[Rect], r: Rect| overlays.iter().any(|o| o.intersects(r));
    let want = app.sixel.show.clone().filter(|s| !hit(&app.overlay_rects, s.rect));
    if want == app.sixel.shown {
        return Ok(false);
    }
    // what is about to be drawn, and the cells it is certain to overwrite
    let cell = app.cell_px;
    let hole = want
        .as_ref()
        .and_then(|w| image_for(&app.sixel, w).map(|e| covered_cells(e, w.rect, cell)))
        .unwrap_or_default();
    if let Some(old) = app.sixel.shown.take() {
        let mut skip = app.overlay_rects.clone();
        skip.push(hole);
        if app.sixel.area_now == Some(old.area) {
            let out = term.backend_mut();
            queue_blanks(out, free_spans(old.rect, &skip).into_iter().map(|(y, a, b)| (y, a, b, bg)))?;
        } else if app.live_resize() && app.sixel.area_now.is_some() {
            let runs = blank_runs(&app.sixel.blanks, &skip);
            queue_blanks(term.backend_mut(), runs)?;
        } else {
            // the panel moved under it: stale pixels may sit in cells that are
            // blank both before and after, so only a full repaint is certain
            return clear_screen(term);
        }
    }
    let mut drawn = false;
    if let Some(w) = &want {
        if let Some(enc) = image_for(&app.sixel, w) {
            let out = term.backend_mut();
            crossterm::queue!(out, crossterm::cursor::MoveTo(w.rect.x, w.rect.y))?;
            out.write_all(&enc.data)?;
            drawn = true;
        }
    }
    term.backend_mut().flush()?;
    if drawn {
        let preview = want.as_ref().is_some_and(|w| w.preview);
        app.sixel.shown = want;
        if !preview && app.sixel.preview.take().is_some() {
            crate::mem::trim(); // the quick version has done its job
        }
    }
    Ok(false)
}

/// Clear and ask the caller to repaint immediately (the caller has the whole
/// step inside a synchronized update, so the blank is never presented).
fn clear_screen(term: &mut ratatui::DefaultTerminal) -> std::io::Result<bool> {
    term.clear()?;
    Ok(true)
}

/// Start a frame inside a *synchronized update* (DEC private mode 2026).
///
/// When the terminal's size changes, ratatui clears the screen and then draws
/// the new frame — but it flushes the clear on its own, so a terminal that
/// paints as bytes arrive shows a blank screen until the frame follows. Telling
/// the terminal to hold its display until [`end_frame`] makes the clear and the
/// repaint appear as one. Terminals without the mode ignore the sequence.
pub fn begin_frame(term: &mut ratatui::DefaultTerminal) -> std::io::Result<()> {
    use std::io::Write;
    term.backend_mut().write_all(b"\x1b[?2026h")
}

pub fn end_frame(term: &mut ratatui::DefaultTerminal) -> std::io::Result<()> {
    use std::io::Write;
    let out = term.backend_mut();
    out.write_all(b"\x1b[?2026l")?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    /// A deliberately independent decoder, so the test doesn't just check the
    /// encoder against itself.
    fn decode(data: &[u8]) -> (u32, u32, Vec<Option<[u8; 3]>>) {
        let s = std::str::from_utf8(data).unwrap();
        let body = s.strip_prefix("\x1bP0;1;0q").expect("dcs header").strip_suffix("\x1b\\").expect("st");
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

    /// `cargo test --release encode_speed -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn encode_speed() {
        for side in [240u32, 360, 640] {
            let mut img = RgbImage::new(side, side);
            for (x, y, p) in img.enumerate_pixels_mut() {
                let (fx, fy) = (x as f64 / side as f64, y as f64 / side as f64);
                let blob = ((fx - 0.3).hypot(fy - 0.4) * 6.0).sin() * 40.0 + ((fx + fy) * 9.0).cos() * 25.0;
                let tex = ((x * 7 + y * 13) % 17) as f64;
                p.0 = [
                    (fx * 200.0 + blob + tex).clamp(0.0, 255.0) as u8,
                    (fy * 180.0 + 40.0 - blob * 0.5 + tex).clamp(0.0, 255.0) as u8,
                    ((fx + fy) * 110.0 + blob * 0.8).clamp(0.0, 255.0) as u8,
                ];
            }
            let t = std::time::Instant::now();
            let enc = encode_canvas(&img, side + 20, side + 20, 256);
            eprintln!("{side}x{side}: {:?}  ({} KB)", t.elapsed(), enc.data.len() / 1024);
        }
    }

    #[test]
    fn free_spans_skip_exactly_the_cells_an_overlay_covers() {
        let rect = Rect::new(10, 2, 8, 3); // x 10..18, y 2..5
        // no overlay: every row is one full span
        assert_eq!(free_spans(rect, &[]), vec![(2, 10, 18), (3, 10, 18), (4, 10, 18)]);
        // a modal covering the middle of the middle row, and the whole top-left corner of row 2
        let overlays = [Rect::new(12, 3, 3, 1), Rect::new(0, 0, 12, 3)];
        let spans = free_spans(rect, &overlays);
        assert_eq!(spans, vec![(2, 12, 18), (3, 10, 12), (3, 15, 18), (4, 10, 18)]);
        // fully covered → nothing to erase
        assert!(free_spans(rect, &[Rect::new(0, 0, 80, 40)]).is_empty());
        // overlays elsewhere don't matter
        assert_eq!(free_spans(rect, &[Rect::new(30, 30, 5, 5)]).len(), 3);
        // every free cell is genuinely uncovered and every uncovered cell is listed
        for (y, x0, x1) in free_spans(rect, &overlays) {
            for x in x0..x1 {
                assert!(!overlays.iter().any(|o| o.x <= x && x < o.x + o.width && o.y <= y && y < o.y + o.height));
            }
        }
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
    fn plan_centers_the_canvas_exactly_for_any_size_and_cell() {
        // every combination of odd/even panel sizes and a spread of real-world cell sizes
        let cells = [(10, 20), (9, 18), (14, 28), (7, 15), (8, 16), (13, 31), (11, 22)];
        for &cell in &cells {
            for w in 8..=60u16 {
                for h in 4..=30u16 {
                    let area = Rect::new(3, 2, w, h);
                    let Some(p) = plan(area, cell) else { continue };
                    let r = p.rect;
                    // exact centring: left gap == right gap, top gap == bottom gap
                    assert_eq!(r.x - area.x, (area.x + area.width) - (r.x + r.width), "x gap {area:?} {cell:?} {r:?}");
                    assert_eq!(r.y - area.y, (area.y + area.height) - (r.y + r.height), "y gap {area:?} {cell:?} {r:?}");
                    assert!(r.width <= area.width && r.height <= area.height);
                    // the canvas must be big enough to hold the picture
                    assert!(r.width as u32 * cell.0 as u32 >= p.key.side.min(area.width as u32 * cell.0 as u32));
                    assert!(p.key.side <= MAX_SIDE);
                    // …and the picture must fit the panel in pixels
                    assert!(p.key.side <= area.width as u32 * cell.0 as u32);
                    assert!(p.key.side <= area.height as u32 * cell.1 as u32);
                }
            }
        }
        assert!(plan(Rect::new(0, 0, 1, 1), (10, 20)).is_none());
    }

    #[test]
    fn a_bigger_font_gives_a_smaller_cell_count_not_a_wrong_pixel_size() {
        // Same panel in *pixels* (400×300), two font sizes. The picture must come out
        // the same physical size either way — that is what was broken when the cell
        // size was stale.
        let small = plan(Rect::new(0, 0, 40, 15), (10, 20)).unwrap();
        let big = plan(Rect::new(0, 0, 20, 7), (20, 42)).unwrap(); // ≈ same pixels, bigger cells
        assert!(small.key.side.abs_diff(big.key.side) <= 20, "{} vs {}", small.key.side, big.key.side);
        // …whereas using the *stale* cell size on the bigger-font panel would have under-sized it:
        let stale = plan(Rect::new(0, 0, 20, 7), (10, 20)).unwrap();
        assert!(stale.key.side < big.key.side / 2 + 20, "stale cell size under-sizes the image");
    }

    #[test]
    fn picture_is_centered_on_the_canvas_at_pixel_precision() {
        // 10×8 picture on a 30×20 canvas → offset (10, 6)
        let img = RgbImage::from_pixel(10, 8, Rgb([200, 30, 30]));
        let enc = encode_canvas(&img, 30, 20, 256);
        assert_eq!((enc.width, enc.height), (30, 20));
        let (w, h, px) = decode(&enc.data);
        assert_eq!((w, h), (30, 20));
        for y in 0..20u32 {
            for x in 0..30u32 {
                let inside = (10..20).contains(&x) && (6..14).contains(&y);
                assert_eq!(px[(y * 30 + x) as usize].is_some(), inside, "pixel ({x},{y})");
            }
        }
    }

    #[test]
    fn padding_only_bands_still_advance_the_row() {
        // a picture low in a tall canvas: the empty bands above it must not shift it up
        let img = RgbImage::from_pixel(6, 6, Rgb([9, 200, 9]));
        let mut canvas_top = encode_canvas(&img, 6, 60, 256); // oy = 27
        let (_, _, px) = decode(&canvas_top.data);
        let first = px.iter().position(Option::is_some).unwrap() as u32;
        assert_eq!(first / 6, 27, "picture must start on canvas row 27");
        canvas_top.data.clear();
    }

    #[test]
    fn fit_keeps_aspect_and_never_exceeds_the_side() {
        let img = RgbImage::from_pixel(400, 300, Rgb([9, 9, 9]));
        let f = fit(&img, 120);
        assert_eq!((f.width(), f.height()), (120, 90));
        let tall = fit(&RgbImage::from_pixel(100, 400, Rgb([1, 1, 1])), 200);
        assert_eq!((tall.width(), tall.height()), (50, 200));
    }

    fn ramp(w: u32, h: u32) -> RgbImage {
        let mut img = RgbImage::new(w, h);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = Rgb([(x * 255 / w.max(1)) as u8, (y * 255 / h.max(1)) as u8, ((x + y) * 120 / (w + h)) as u8 + 40]);
        }
        img
    }

    #[test]
    fn preview_lands_exactly_where_the_quality_encode_does() {
        // the swap from preview to real encode must change quality, never position
        for (area, cell, src) in [
            (Rect::new(0, 0, 34, 12), (10u16, 20u16), (200u32, 200u32)),
            (Rect::new(0, 0, 41, 17), (9, 18), (256, 256)),
            (Rect::new(0, 0, 20, 30), (14, 28), (256, 190)), // non-square cover
            (Rect::new(0, 0, 9, 4), (7, 15), (64, 64)),
        ] {
            let img = ramp(src.0, src.1);
            let p = plan(area, cell).expect("plan");
            let (pv, q) = (encode_preview(&img, &p, cell), encode_image(&img, &p, cell));
            let ctx = format!("{area:?} cell {cell:?} src {src:?}");
            assert_eq!((pv.width, pv.height), (q.width, q.height), "{ctx}: canvas");
            assert_eq!(pv.pic, q.pic, "{ctx}: picture box");
            let (dw, dh, dp) = decode(&pv.data);
            let (_, _, dq) = decode(&q.data);
            assert_eq!((dw, dh), (p.key.cols as u32 * cell.0 as u32, p.key.rows as u32 * cell.1 as u32), "{ctx}");
            // the same pixels are opaque / transparent in both
            let mism = dp.iter().zip(&dq).filter(|(a, b)| a.is_some() != b.is_some()).count();
            assert_eq!(mism, 0, "{ctx}: {mism} pixels differ in transparency");
        }
    }

    #[test]
    fn preview_looks_like_the_picture() {
        let img = ramp(240, 240);
        let p = plan(Rect::new(0, 0, 30, 12), (10, 20)).unwrap();
        let enc = encode_preview(&img, &p, (10, 20));
        let (w, _, px) = decode(&enc.data);
        let (ox, oy, pw, ph) = enc.pic;
        // compare block averages of the preview with the same blocks of the source,
        // resampled to the picture's size (the eye averages ordered-dither grain away)
        let src = image::imageops::resize(&img, pw, ph, FilterType::Triangle);
        let (mut err, mut n) = (0u64, 0u64);
        for by in (0..ph - 8).step_by(8) {
            for bx in (0..pw - 8).step_by(8) {
                for k in 0..3 {
                    let (mut a, mut b) = (0i64, 0i64);
                    for dy in 0..8 {
                        for dx in 0..8 {
                            a += px[((oy + by + dy) * w + ox + bx + dx) as usize].expect("opaque inside the picture")[k] as i64;
                            b += src.get_pixel(bx + dx, by + dy).0[k] as i64;
                        }
                    }
                    err += (a - b).unsigned_abs() / 64;
                    n += 1;
                }
            }
        }
        let mean = err as f64 / n as f64;
        assert!(mean < 9.0, "block-averaged error {mean:.2}/255 is too coarse for a preview");
    }

    #[test]
    fn preview_survives_degenerate_sources_and_sizes() {
        let p = plan(Rect::new(0, 0, 30, 12), (10, 20)).unwrap();
        for src in [(1u32, 1u32), (1, 40), (40, 1), (3, 3)] {
            let enc = encode_preview(&RgbImage::from_pixel(src.0, src.1, Rgb([9, 99, 199])), &p, (10, 20));
            let (w, h, px) = decode(&enc.data);
            assert_eq!((w, h), (enc.width, enc.height), "{src:?}");
            assert!(px.iter().any(|p| p.is_some()), "{src:?}: something is drawn");
        }
        // a flat colour maps to one palette entry and the sixel is tiny (RLE)
        let flat = encode_preview(&RgbImage::from_pixel(64, 64, Rgb([255, 255, 255])), &p, (10, 20));
        assert!(flat.data.len() < 8 * 1024, "{} bytes", flat.data.len());
        // pure black / white hit the ends of the cube exactly, with no dither noise
        let (_, _, px) = decode(&flat.data);
        assert!(px.iter().flatten().all(|c| *c == [255, 255, 255]));
    }

    #[test]
    fn covered_cells_are_only_the_cells_the_picture_fills_completely() {
        let enc = |pic| Encoded { data: vec![], width: 100, height: 100, pic };
        let rect = Rect::new(4, 2, 10, 5);
        // picture 95×100 at (5,3) on 10×20 cells: columns 1..=9 are full, rows 1..=4
        assert_eq!(covered_cells(&enc((5, 3, 95, 100)), rect, (10, 20)), Rect::new(5, 3, 9, 4));
        // a picture that fills its canvas exactly covers all of it
        assert_eq!(covered_cells(&enc((0, 0, 100, 100)), rect, (10, 20)), Rect::new(4, 2, 10, 5));
        // smaller than a cell: nothing is certain
        assert_eq!(covered_cells(&enc((5, 5, 8, 15)), rect, (10, 20)), Rect::default());
        // never spills outside the canvas
        let c = covered_cells(&enc((0, 0, 300, 300)), rect, (10, 20));
        assert!(c.x >= rect.x && c.y >= rect.y && c.right() <= rect.right() && c.bottom() <= rect.bottom());
    }

    #[test]
    fn cell_size_consistency_tells_a_resize_from_a_font_change() {
        let cell = (10u16, 20u16);
        // plain resizes: whole cells added/removed, window padding of a few px
        assert_eq!(cell_consistent((100, 40, 1000, 800), cell), Some(true));
        assert_eq!(cell_consistent((100, 40, 1007, 803), cell), Some(true));
        assert_eq!(cell_consistent((101, 40, 1012, 810), cell), Some(true));
        assert_eq!(cell_consistent((57, 22, 579, 441), cell), Some(true));
        // font zoomed in (same window, fewer cells) or out (more cells)
        assert_eq!(cell_consistent((83, 33, 1000, 800), cell), Some(false));
        assert_eq!(cell_consistent((125, 50, 1000, 800), cell), Some(false));
        // a DPI change: same cell counts, different pixel size
        assert_eq!(cell_consistent((100, 40, 1500, 1200), cell), Some(false));
        // no pixel size reported: can't say
        assert_eq!(cell_consistent((100, 40, 0, 0), cell), None);
        assert_eq!(cell_consistent((0, 0, 1000, 800), cell), None);
    }

    #[test]
    fn blank_runs_merge_neighbours_and_respect_the_skip_rects() {
        use ratatui::style::Color;
        let (a, b) = (Color::Rgb(1, 2, 3), Color::Rgb(9, 9, 9));
        let cells = vec![(0, 0, a), (1, 0, a), (2, 0, a), (3, 0, b), (0, 1, a), (2, 1, a)];
        assert_eq!(blank_runs(&cells, &[]), vec![(0, 0, 3, a), (0, 3, 4, b), (1, 0, 1, a), (1, 2, 3, a)]);
        // a skip rectangle splits a run
        assert_eq!(blank_runs(&cells, &[Rect::new(1, 0, 1, 1)]), vec![(0, 0, 1, a), (0, 2, 3, a), (0, 3, 4, b), (1, 0, 1, a), (1, 2, 3, a)]);
    }

    /// `cargo test --release -- --ignored --nocapture preview_speed`
    #[test]
    #[ignore]
    fn preview_speed() {
        let img = ramp(256, 256);
        for (cols, rows, cell) in [(26u16, 13u16, (10u16, 20u16)), (40, 20, (10, 20)), (60, 30, (12, 24))] {
            let p = plan(Rect::new(0, 0, cols, rows), cell).unwrap();
            let t = std::time::Instant::now();
            let n = 20;
            let mut bytes = 0;
            for _ in 0..n {
                bytes = encode_preview(&img, &p, cell).data.len();
            }
            let pv = t.elapsed() / n;
            let big = ramp(640, 640);
            let t = std::time::Instant::now();
            let q = encode_image(&big, &p, cell).data.len();
            println!("side {:>3}px: preview {:>8.2?} ({:>6} B)   quality {:>8.2?} ({:>6} B)", p.key.side, pv, bytes, t.elapsed(), q);
        }
    }
}
