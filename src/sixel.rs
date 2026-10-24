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
}

/// Decode `path`, fit it inside `plan.side`, quantise, and encode it centred on
/// the plan's canvas. Blocking — run it off the UI thread.
pub fn encode_file(path: &std::path::Path, plan: &Plan, cell: (u16, u16)) -> Option<Encoded> {
    // cached covers have no file extension, so sniff the format from content
    let img = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?.to_rgb8();
    let (cw, ch) = (plan.key.cols as u32 * cell.0.max(1) as u32, plan.key.rows as u32 * cell.1.max(1) as u32);
    Some(encode_canvas(&fit(&img, plan.key.side), cw, ch, PALETTE))
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

/// Encode `img` as-is (canvas = the image). Used by tests.
#[cfg(test)]
pub fn encode(img: &RgbImage, max_colors: usize) -> Encoded {
    encode_canvas(img, img.width(), img.height(), max_colors)
}

/// Encode `img` centred on a `cw × ch` px canvas. Pixels outside the picture
/// are left unset (`P2 = 1`: transparent), so the cells underneath show
/// through and the padding costs nothing.
pub fn encode_canvas(img: &RgbImage, cw: u32, ch: u32, max_colors: usize) -> Encoded {
    let (w, h) = img.dimensions();
    let (cw, ch) = (cw.max(w), ch.max(h));
    let (ox, oy) = ((cw - w) / 2, (ch - h) / 2);
    let (palette, idx) = quantize(img, max_colors.min(PALETTE));
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
    Encoded { data: out, width: cw, height: ch }
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
    /// set by `draw` each frame: what should be visible right now
    pub show: Option<Shown>,
    /// what we last wrote to the terminal
    pub shown: Option<Shown>,
}

use crate::app::{App, Msg};

impl App {
    /// The layout is still changing — a divider is being dragged, or the
    /// window / font was just resized and the cell size hasn't been re-read
    /// yet. No image is planned or shown meanwhile: a plan made with a stale
    /// cell size is the wrong size by definition, and encoding it only gets in
    /// the way of the right one.
    pub fn settling(&self) -> bool {
        self.drag.is_some() || self.resize_at.is_some()
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
    /// Called every tick; cheap unless something actually changed:
    ///
    /// * the window's size in cells *or pixels* changing is the trigger — the
    ///   pixel size catches a DPI change that leaves rows/columns alone;
    /// * after it settles for 150ms, re-ask the terminal (`CSI 16 t`, exact);
    /// * terminals that never answer fall back to window pixels ÷ cells.
    pub fn maintain_cell_size(&mut self, input: &crate::input::Input) {
        use std::time::Instant;
        if self.art_mode != crate::art::ArtMode::Sixel {
            return;
        }
        let ws = crossterm::terminal::window_size().ok().map(|w| (w.columns, w.rows, w.width, w.height));
        if ws != self.win_sig {
            self.win_sig = ws;
            self.resize_at = Some(Instant::now());
        }
        let Some(t) = self.resize_at else { return };
        if t.elapsed() < Duration::from_millis(150) {
            return; // still moving (a drag-resize fires dozens of events)
        }
        self.resize_at = None;
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

/// Blank the picture's cells (outside any overlay) with the theme background.
/// Only safe while those cells are still blank in ratatui's buffer — i.e. the
/// panel hasn't moved — which is why the callers check.
fn erase(term: &mut ratatui::DefaultTerminal, rect: Rect, overlays: &[Rect], bg: ratatui::style::Color) -> std::io::Result<()> {
    use std::io::Write;
    // theme backgrounds are either true color or "the terminal's own"
    let bg = match bg {
        ratatui::style::Color::Rgb(r, g, b) => crossterm::style::Color::Rgb { r, g, b },
        _ => crossterm::style::Color::Reset,
    };
    let out = term.backend_mut();
    for (y, x0, x1) in free_spans(rect, overlays) {
        crossterm::queue!(
            out,
            crossterm::cursor::MoveTo(x0, y),
            crossterm::style::SetBackgroundColor(bg),
            crossterm::style::Print(" ".repeat((x1 - x0) as usize)),
            crossterm::style::ResetColor
        )?;
    }
    out.flush()
}

/// Reconcile the terminal with what `draw` says should be visible. Runs after
/// every frame; does nothing unless the picture must change.
///
/// Rules, and why:
/// * the image is blank cells as far as ratatui knows, so its diff never
///   touches it — it stays put on its own until something changes it;
/// * to take it down, blank *just its cells* (minus whatever a modal or toast
///   already painted over). A full-screen clear would leave the whole terminal
///   black until the next frame, which is what a drag looked like;
/// * that is only safe while the cells are *still blank* in ratatui's buffer —
///   i.e. the art panel hasn't moved. If it has, the screen is cleared and
///   the function returns `true`: the caller must redraw **immediately**, in
///   the same step, so nothing is ever seen blank.
pub fn sync(app: &mut App, term: &mut ratatui::DefaultTerminal) -> std::io::Result<bool> {
    use std::io::Write;

    if app.art_mode != crate::art::ArtMode::Sixel {
        return Ok(false);
    }
    let bg = app.theme().bg;
    if app.settling() {
        // the layout is changing (a divider drag, a zoom, a window resize): drop
        // the picture once and wait — redrawing it every frame would crawl, and
        // a size computed from a stale cell size would be wrong anyway
        if let Some(old) = app.sixel.shown.take() {
            if app.sixel.area_now == Some(old.area) {
                erase(term, old.rect, &app.overlay_rects, bg)?;
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
    if let Some(old) = app.sixel.shown.take() {
        if app.sixel.area_now == Some(old.area) {
            erase(term, old.rect, &app.overlay_rects, bg)?;
        } else {
            // the panel moved under it: stale pixels may sit in cells that are
            // blank both before and after, so only a full repaint is certain
            return clear_screen(term);
        }
    }
    if let Some(w) = want {
        if let Some((c, s, enc)) = &app.sixel.cache {
            if *c == w.cover && *s == w.key {
                let out = term.backend_mut();
                crossterm::queue!(out, crossterm::cursor::MoveTo(w.rect.x, w.rect.y))?;
                out.write_all(&enc.data)?;
                out.flush()?;
                app.sixel.shown = Some(w);
            }
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
}
