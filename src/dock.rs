//! The panel layout as data: which panels exist, where they sit, how big.
//!
//! The screen is a row of **columns**; each column is a vertical **stack** of
//! panels. That is enough to express the classic three-column layout and any
//! rearrangement of it (panels beside, above or below each other), while
//! staying simple enough that every operation can keep the layout valid:
//!
//! * every panel appears exactly once, no column is empty;
//! * the column holding **Tracks** is the elastic one (it takes whatever width
//!   the others leave); every other column has a fixed width in cells;
//! * heights inside a stack are proportional weights.
//!
//! This module is pure geometry and bookkeeping — no terminal, no app state —
//! so it can be tested exhaustively. Moving a panel is "drop it on a zone of
//! another panel": its edges dock beside / above / below that panel, its middle
//! swaps the two (like a docking window manager, or tmux/zellij pane moves).

use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PanelId {
    Library,
    Tracks,
    Cover,
    Queue,
}

impl PanelId {
    pub const ALL: [PanelId; 4] = [PanelId::Library, PanelId::Tracks, PanelId::Cover, PanelId::Queue];

    pub fn label(self) -> &'static str {
        match self {
            PanelId::Library => "library",
            PanelId::Tracks => "tracks",
            PanelId::Cover => "cover",
            PanelId::Queue => "queue",
        }
    }

    /// Smallest `(width, height)`, borders included, the panel is usable at.
    pub fn min_size(self) -> (u16, u16) {
        match self {
            PanelId::Library => (14, 5),
            PanelId::Tracks => (24, 5),
            PanelId::Cover => (22, 4),
            PanelId::Queue => (24, 4),
        }
    }

    /// The width a column holding only this panel starts with when it's created.
    pub fn default_width(self) -> u16 {
        match self {
            PanelId::Library => 26,
            PanelId::Tracks => 40,
            PanelId::Cover | PanelId::Queue => 36,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Column {
    pub panels: Vec<PanelId>,
    /// relative heights, one per panel
    pub weights: Vec<u16>,
    /// width in cells. Ignored while this column holds Tracks (it is elastic),
    /// but remembered, so the width is sensible if Tracks later moves away.
    pub width: u16,
}

impl Column {
    fn single(p: PanelId) -> Self {
        Self { panels: vec![p], weights: vec![100], width: p.default_width() }
    }

    pub fn holds_tracks(&self) -> bool {
        self.panels.contains(&PanelId::Tracks)
    }

    fn min_w(&self) -> u16 {
        self.panels.iter().map(|p| p.min_size().0).max().unwrap_or(1)
    }

    fn min_h(&self) -> u16 {
        self.panels.iter().map(|p| p.min_size().1).sum()
    }
}

/// A boundary the user can drag.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Divider {
    /// between column `i` and column `i + 1`
    Col(usize),
    /// between panel `j` and `j + 1` of column `c`
    Row(usize, usize),
}

/// Where on a target panel a dragged panel is dropped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Zone {
    Left,
    Right,
    Top,
    Bottom,
    /// the middle: exchange places
    Swap,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// The result of laying a [`Dock`] out in an area.
#[derive(Clone, Debug, Default)]
pub struct Arranged {
    pub panels: Vec<(PanelId, Rect)>,
    pub cols: Vec<Rect>,
    /// grab zones — the two border rows/columns either side of a boundary
    pub dividers: Vec<(Divider, Rect)>,
}

impl Arranged {
    pub fn rect(&self, p: PanelId) -> Option<Rect> {
        self.panels.iter().find(|(id, _)| *id == p).map(|(_, r)| *r)
    }

    pub fn panel_at(&self, x: u16, y: u16) -> Option<PanelId> {
        self.panels
            .iter()
            .find(|(_, r)| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height)
            .map(|(id, _)| *id)
    }

    /// The nearest panel in direction `d` from `from` that overlaps it in the
    /// other axis — what the arrow keys mean in keyboard arrange mode.
    pub fn neighbor(&self, from: PanelId, d: Dir) -> Option<PanelId> {
        let r = self.rect(from)?;
        let overlap = |a0: u16, a1: u16, b0: u16, b1: u16| a0 < b1 && b0 < a1;
        self.panels
            .iter()
            .filter(|(id, _)| *id != from)
            .filter_map(|(id, o)| {
                let gap = match d {
                    Dir::Left if o.x + o.width <= r.x && overlap(r.y, r.y + r.height, o.y, o.y + o.height) => r.x - (o.x + o.width),
                    Dir::Right if o.x >= r.x + r.width && overlap(r.y, r.y + r.height, o.y, o.y + o.height) => o.x - (r.x + r.width),
                    Dir::Up if o.y + o.height <= r.y && overlap(r.x, r.x + r.width, o.x, o.x + o.width) => r.y - (o.y + o.height),
                    Dir::Down if o.y >= r.y + r.height && overlap(r.x, r.x + r.width, o.x, o.x + o.width) => o.y - (r.y + r.height),
                    _ => return None,
                };
                Some((gap, *id))
            })
            .min_by_key(|(gap, _)| *gap)
            .map(|(_, id)| id)
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Dock {
    pub columns: Vec<Column>,
}

impl Default for Dock {
    fn default() -> Self {
        Self::classic()
    }
}

// ── layout arithmetic ─────────────────────────────────────────────────

/// Split `total` proportionally to `weights` (largest-remainder, so it sums
/// exactly), never giving an item less than its `min` — the shortfall is taken
/// from whoever has the most to spare. If even the minimums don't fit, the
/// minimums win (callers show a "too small" screen before that matters).
fn split_proportional(total: u16, mins: &[u16], weights: &[u16]) -> Vec<u16> {
    let n = mins.len();
    if n == 0 {
        return vec![];
    }
    let sum_w: u32 = weights.iter().map(|w| (*w).max(1) as u32).sum();
    let mut out: Vec<u16> = Vec::with_capacity(n);
    let mut frac: Vec<(u32, usize)> = Vec::with_capacity(n);
    for (i, w) in weights.iter().enumerate() {
        let num = total as u32 * (*w).max(1) as u32;
        out.push((num / sum_w) as u16);
        frac.push((num % sum_w, i));
    }
    let mut left = total.saturating_sub(out.iter().sum());
    frac.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    for (_, i) in frac {
        if left == 0 {
            break;
        }
        out[i] += 1;
        left -= 1;
    }
    // raise anything under its minimum, paying from the largest surplus
    for i in 0..n {
        while out[i] < mins[i] {
            let donor = (0..n).filter(|&j| j != i && out[j] > mins[j]).max_by_key(|&j| out[j] - mins[j]);
            match donor {
                Some(j) => {
                    out[j] -= 1;
                    out[i] += 1;
                }
                None => {
                    out[i] = mins[i];
                    break;
                }
            }
        }
    }
    out
}

/// Column widths: fixed columns keep their width (but never below their
/// minimum), the elastic column absorbs what's left. If the fixed columns plus
/// the elastic minimum overflow, fixed columns shrink toward their minimums,
/// widest-slack first.
fn split_widths(total: u16, mins: &[u16], fixed: &[Option<u16>]) -> Vec<u16> {
    let n = mins.len();
    let mut w: Vec<u16> = (0..n).map(|i| fixed[i].map_or(mins[i], |f| f.max(mins[i]))).collect();
    let used: u32 = w.iter().map(|x| *x as u32).sum();
    if total as u32 >= used {
        let extra = (total as u32 - used) as u16;
        let sink = (0..n).find(|&i| fixed[i].is_none()).unwrap_or(n - 1);
        w[sink] += extra;
    } else {
        let mut deficit = (used - total as u32) as u16;
        while deficit > 0 {
            let Some(i) = (0..n).filter(|&i| w[i] > mins[i]).max_by_key(|&i| w[i] - mins[i]) else { break };
            let take = (w[i] - mins[i]).min(deficit);
            w[i] -= take;
            deficit -= take;
        }
    }
    w
}

impl Dock {
    // ── presets ───────────────────────────────────────────────────────
    /// library | tracks | cover over queue
    pub fn classic() -> Self {
        Self {
            columns: vec![
                Column::single(PanelId::Library),
                Column::single(PanelId::Tracks),
                Column { panels: vec![PanelId::Cover, PanelId::Queue], weights: vec![40, 60], width: 36 },
            ],
        }
    }

    pub fn presets() -> Vec<(&'static str, Dock)> {
        let col = |panels: &[PanelId], weights: &[u16], width: u16| Column { panels: panels.to_vec(), weights: weights.to_vec(), width };
        vec![
            ("classic", Self::classic()),
            (
                "mirrored",
                Dock {
                    columns: vec![
                        col(&[PanelId::Cover, PanelId::Queue], &[40, 60], 36),
                        Column::single(PanelId::Tracks),
                        Column::single(PanelId::Library),
                    ],
                },
            ),
            (
                "stacked left",
                Dock {
                    columns: vec![
                        col(&[PanelId::Library, PanelId::Cover, PanelId::Queue], &[30, 35, 35], 30),
                        Column::single(PanelId::Tracks),
                    ],
                },
            ),
            (
                "queue under tracks",
                Dock {
                    columns: vec![
                        Column::single(PanelId::Library),
                        col(&[PanelId::Tracks, PanelId::Queue], &[70, 30], 40),
                        Column::single(PanelId::Cover),
                    ],
                },
            ),
        ]
    }

    // ── queries ───────────────────────────────────────────────────────
    pub fn find(&self, p: PanelId) -> Option<(usize, usize)> {
        self.columns.iter().enumerate().find_map(|(c, col)| col.panels.iter().position(|x| *x == p).map(|i| (c, i)))
    }

    /// Smallest area the layout works in: the columns' minimum widths side by
    /// side, and the tallest stack's minimum heights stacked.
    pub fn min_size(&self) -> (u16, u16) {
        (self.columns.iter().map(Column::min_w).sum(), self.columns.iter().map(Column::min_h).max().unwrap_or(1))
    }

    /// Lay the panels out in `area`: tiles it exactly, no gaps or overlaps.
    pub fn arrange(&self, area: Rect) -> Arranged {
        let mins: Vec<u16> = self.columns.iter().map(Column::min_w).collect();
        let fixed: Vec<Option<u16>> = self.columns.iter().map(|c| (!c.holds_tracks()).then_some(c.width)).collect();
        let widths = split_widths(area.width, &mins, &fixed);
        let mut out = Arranged::default();
        let mut x = area.x;
        for (ci, (col, w)) in self.columns.iter().zip(widths).enumerate() {
            let crect = Rect::new(x, area.y, w, area.height);
            let pmins: Vec<u16> = col.panels.iter().map(|p| p.min_size().1).collect();
            let heights = split_proportional(area.height, &pmins, &col.weights);
            let mut y = area.y;
            for (pi, (p, h)) in col.panels.iter().zip(heights).enumerate() {
                let r = Rect::new(x, y, w, h);
                out.panels.push((*p, r));
                if pi + 1 < col.panels.len() {
                    out.dividers.push((Divider::Row(ci, pi), Rect::new(x, (y + h).saturating_sub(1), w, 2)));
                }
                y += h;
            }
            if ci + 1 < self.columns.len() {
                out.dividers.push((Divider::Col(ci), Rect::new((x + w).saturating_sub(1), area.y, 2, area.height)));
            }
            out.cols.push(crect);
            x += w;
        }
        out
    }

    // ── resizing ──────────────────────────────────────────────────────
    /// Drag the boundary between column `i` and `i + 1` so the left column's
    /// last cell is at screen column `x`. The pair keeps its combined width, so
    /// no other column moves; the elastic (Tracks) column is never stored.
    pub fn resize_col(&mut self, i: usize, x: u16, area: Rect) {
        if i + 1 >= self.columns.len() {
            return;
        }
        let arr = self.arrange(area);
        let (l, r) = (arr.cols[i], arr.cols[i + 1]);
        let total = l.width + r.width;
        let (lmin, rmin) = (self.columns[i].min_w(), self.columns[i + 1].min_w());
        if lmin + rmin > total {
            return;
        }
        let new_left = (x + 1).saturating_sub(l.x).clamp(lmin, total - rmin);
        let new_right = total - new_left;
        if self.columns[i].holds_tracks() {
            self.columns[i + 1].width = new_right;
        } else if self.columns[i + 1].holds_tracks() {
            self.columns[i].width = new_left;
        } else {
            self.columns[i].width = new_left;
            self.columns[i + 1].width = new_right;
        }
    }

    /// Drag the boundary between panel `j` and `j + 1` of column `c` so the
    /// upper panel's last row is `y`. Afterwards the column's weights are its
    /// current heights, so the split reproduces exactly at this size.
    pub fn resize_row(&mut self, c: usize, j: usize, y: u16, area: Rect) {
        let Some(col) = self.columns.get(c) else { return };
        if j + 1 >= col.panels.len() {
            return;
        }
        let arr = self.arrange(area);
        let rects: Vec<Rect> = col.panels.iter().filter_map(|p| arr.rect(*p)).collect();
        let (a, b) = (rects[j], rects[j + 1]);
        let total = a.height + b.height;
        let (amin, bmin) = (col.panels[j].min_size().1, col.panels[j + 1].min_size().1);
        if amin + bmin > total {
            return;
        }
        let new_a = (y + 1).saturating_sub(a.y).clamp(amin, total - bmin);
        let mut h: Vec<u16> = rects.iter().map(|r| r.height).collect();
        h[j] = new_a;
        h[j + 1] = total - new_a;
        self.columns[c].weights = h;
    }

    /// Put one boundary back to its default.
    pub fn reset_divider(&mut self, d: Divider) {
        match d {
            Divider::Col(i) => {
                for k in [i, i + 1] {
                    if let Some(col) = self.columns.get_mut(k) {
                        col.width = col.panels.iter().map(|p| p.default_width()).max().unwrap_or(36);
                    }
                }
            }
            Divider::Row(c, _) => {
                if let Some(col) = self.columns.get_mut(c) {
                    col.weights = col
                        .panels
                        .iter()
                        .map(|p| match p {
                            PanelId::Cover => 40,
                            PanelId::Queue => 60,
                            PanelId::Library => 40,
                            PanelId::Tracks => 60,
                        })
                        .collect();
                }
            }
        }
    }

    // ── moving panels ─────────────────────────────────────────────────
    /// Drop `src` on `zone` of `target`. Returns whether anything changed.
    pub fn move_panel(&mut self, src: PanelId, target: PanelId, zone: Zone) -> bool {
        if src == target {
            return false;
        }
        let (Some((sc, sp)), Some((tc, tp))) = (self.find(src), self.find(target)) else { return false };
        if zone == Zone::Swap {
            let both_alone = self.columns[sc].panels.len() == 1 && self.columns[tc].panels.len() == 1;
            if sc == tc {
                self.columns[sc].panels.swap(sp, tp);
            } else {
                let (a, b) = (self.columns[sc].panels[sp], self.columns[tc].panels[tp]);
                self.columns[sc].panels[sp] = b;
                self.columns[tc].panels[tp] = a;
                if both_alone {
                    // a column of one panel is that panel's "size": it travels with it
                    let w = self.columns[sc].width;
                    self.columns[sc].width = self.columns[tc].width;
                    self.columns[tc].width = w;
                }
            }
            self.normalize();
            return true;
        }
        // take it out of where it is…
        self.columns[sc].panels.remove(sp);
        self.columns[sc].weights.remove(sp);
        if self.columns[sc].panels.is_empty() {
            self.columns.remove(sc);
        }
        // …and put it down relative to the target (indices may have shifted)
        let (tc, tp) = self.find(target).expect("target still present");
        match zone {
            Zone::Top | Zone::Bottom => {
                let col = &mut self.columns[tc];
                let at = if zone == Zone::Top { tp } else { tp + 1 };
                // the newcomer takes half of the target's share
                let share = col.weights[tp].max(2);
                let half = share / 2;
                col.weights[tp] = share - half;
                col.panels.insert(at, src);
                col.weights.insert(at, half);
            }
            Zone::Left | Zone::Right => {
                let at = if zone == Zone::Left { tc } else { tc + 1 };
                self.columns.insert(at, Column::single(src));
            }
            Zone::Swap => unreachable!(),
        }
        self.normalize();
        true
    }

    /// Repair anything that violates the invariants. Cheap; called after edits
    /// and on load, so a hand-edited or stale file can never produce a layout
    /// that loses a panel or draws one twice.
    pub fn normalize(&mut self) {
        let mut seen: Vec<PanelId> = Vec::new();
        for col in &mut self.columns {
            let mut keep = Vec::new();
            for (i, p) in col.panels.iter().enumerate() {
                if !seen.contains(p) {
                    seen.push(*p);
                    keep.push((*p, col.weights.get(i).copied().unwrap_or(50).max(1)));
                }
            }
            col.panels = keep.iter().map(|k| k.0).collect();
            col.weights = keep.iter().map(|k| k.1).collect();
        }
        self.columns.retain(|c| !c.panels.is_empty());
        for p in PanelId::ALL {
            if !seen.contains(&p) {
                self.columns.push(Column::single(p)); // a panel that went missing comes back
            }
        }
        for col in &mut self.columns {
            col.width = col.width.clamp(col.min_w(), 160);
        }
    }

    // ── persistence ───────────────────────────────────────────────────
    pub fn to_json(&self) -> Value {
        serde_json::json!({ "columns": self.columns })
    }

    /// Parse and repair; `None` if it isn't a layout at all.
    pub fn from_json(v: &Value) -> Option<Self> {
        let cols: Vec<Column> = serde_json::from_value(v.get("columns")?.clone()).ok()?;
        let mut d = Dock { columns: cols };
        d.normalize();
        Some(d)
    }

    /// Migrate the earlier fixed three-slot layout (`ui_layout`).
    pub fn from_legacy(sidebar: u16, side: u16, cover_pct: u16) -> Self {
        let mut d = Self::classic();
        d.columns[0].width = sidebar;
        d.columns[2].width = side;
        let c = cover_pct.clamp(1, 99);
        d.columns[2].weights = vec![c, 100 - c];
        d.normalize();
        d
    }
}

// ── drop zones ────────────────────────────────────────────────────────

/// Which zone of `rect` the pointer at `(x, y)` is over: within the outer
/// quarter of an edge it docks to that edge, in the middle it swaps.
pub fn zone_at(rect: Rect, x: u16, y: u16) -> Zone {
    if rect.width < 4 || rect.height < 3 {
        return Zone::Swap;
    }
    let fx = (x.saturating_sub(rect.x)) as f32 / (rect.width - 1) as f32;
    let fy = (y.saturating_sub(rect.y)) as f32 / (rect.height - 1) as f32;
    let edges = [(fx, Zone::Left), (1.0 - fx, Zone::Right), (fy, Zone::Top), (1.0 - fy, Zone::Bottom)];
    let (d, z) = edges.into_iter().fold((f32::MAX, Zone::Swap), |best, e| if e.0 < best.0 { e } else { best });
    if d < 0.25 {
        z
    } else {
        Zone::Swap
    }
}

/// The part of `rect` a drop on `zone` would occupy — for the on-screen preview.
pub fn preview_rect(rect: Rect, zone: Zone) -> Rect {
    let (hw, hh) = ((rect.width / 2).max(1), (rect.height / 2).max(1));
    match zone {
        Zone::Swap => rect,
        Zone::Left => Rect::new(rect.x, rect.y, hw, rect.height),
        Zone::Right => Rect::new(rect.x + rect.width - hw, rect.y, hw, rect.height),
        Zone::Top => Rect::new(rect.x, rect.y, rect.width, hh),
        Zone::Bottom => Rect::new(rect.x, rect.y + rect.height - hh, rect.width, hh),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_ZONES: [Zone; 5] = [Zone::Left, Zone::Right, Zone::Top, Zone::Bottom, Zone::Swap];

    fn area(w: u16, h: u16) -> Rect {
        Rect::new(0, 1, w, h)
    }

    /// The invariants every reachable layout must satisfy.
    fn assert_valid(d: &Dock, ctx: &str) {
        let mut all: Vec<PanelId> = d.columns.iter().flat_map(|c| c.panels.clone()).collect();
        all.sort_by_key(|p| p.label());
        let mut want = PanelId::ALL.to_vec();
        want.sort_by_key(|p| p.label());
        assert_eq!(all, want, "{ctx}: every panel exactly once");
        for c in &d.columns {
            assert!(!c.panels.is_empty(), "{ctx}: empty column");
            assert_eq!(c.panels.len(), c.weights.len(), "{ctx}: weights/panels mismatch");
            assert!(c.weights.iter().all(|w| *w >= 1), "{ctx}: zero weight");
        }
        assert_eq!(d.columns.iter().filter(|c| c.holds_tracks()).count(), 1, "{ctx}");
    }

    /// The panels tile the area exactly: inside it, disjoint, and covering every cell.
    fn assert_tiles(d: &Dock, a: Rect, ctx: &str) {
        let arr = d.arrange(a);
        let mut covered = vec![0u8; a.width as usize * a.height as usize];
        for (id, r) in &arr.panels {
            assert!(r.x >= a.x && r.y >= a.y && r.x + r.width <= a.x + a.width && r.y + r.height <= a.y + a.height, "{ctx}: {id:?} {r:?} outside {a:?}");
            let (mw, mh) = id.min_size();
            assert!(r.width >= mw && r.height >= mh, "{ctx}: {id:?} {r:?} below its minimum {mw}x{mh}");
            for y in r.y..r.y + r.height {
                for x in r.x..r.x + r.width {
                    covered[(y - a.y) as usize * a.width as usize + (x - a.x) as usize] += 1;
                }
            }
        }
        assert!(covered.iter().all(|c| *c == 1), "{ctx}: gaps or overlaps");
    }

    #[test]
    fn presets_are_valid_and_tile_the_screen_at_their_minimum_and_beyond() {
        for (name, d) in Dock::presets() {
            assert_valid(&d, name);
            let (mw, mh) = d.min_size();
            for (w, h) in [(mw, mh), (mw + 1, mh + 1), (mw + 17, mh + 9), (200, 60)] {
                assert_tiles(&d, area(w, h), &format!("{name} at {w}x{h}"));
            }
        }
    }

    #[test]
    fn classic_matches_the_original_geometry() {
        let arr = Dock::classic().arrange(area(150, 36));
        let w = |p| arr.rect(p).unwrap().width;
        assert_eq!((w(PanelId::Library), w(PanelId::Cover), w(PanelId::Tracks)), (26, 36, 88));
        assert_eq!(arr.rect(PanelId::Cover).unwrap().height + arr.rect(PanelId::Queue).unwrap().height, 36);
    }

    #[test]
    fn every_single_move_from_every_preset_keeps_the_layout_valid() {
        for (name, base) in Dock::presets() {
            for src in PanelId::ALL {
                for target in PanelId::ALL {
                    for zone in ALL_ZONES {
                        let mut d = base.clone();
                        let changed = d.move_panel(src, target, zone);
                        let ctx = format!("{name}: {src:?} -> {target:?} {zone:?}");
                        assert_eq!(changed, src != target, "{ctx}");
                        assert_valid(&d, &ctx);
                        let (mw, mh) = d.min_size();
                        assert_tiles(&d, area(mw.max(100), mh.max(30)), &ctx);
                    }
                }
            }
        }
    }

    #[test]
    fn long_random_sequences_of_moves_and_resizes_never_corrupt_the_layout() {
        // a tiny deterministic generator, so a failure is reproducible
        let mut seed = 0x2545F491u32;
        let mut next = move |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as usize % n
        };
        let a = area(160, 40);
        for run in 0..40 {
            let mut d = Dock::presets()[run % 4].1.clone();
            for step in 0..60 {
                match next(4) {
                    0 | 1 => {
                        let (s, t) = (PanelId::ALL[next(4)], PanelId::ALL[next(4)]);
                        d.move_panel(s, t, ALL_ZONES[next(5)]);
                    }
                    2 if d.columns.len() > 1 => d.resize_col(next(d.columns.len() - 1), next(160) as u16, a),
                    _ => {
                        let c = next(d.columns.len());
                        if d.columns[c].panels.len() > 1 {
                            d.resize_row(c, next(d.columns[c].panels.len() - 1), 1 + next(40) as u16, a);
                        }
                    }
                }
                let ctx = format!("run {run} step {step}");
                assert_valid(&d, &ctx);
                assert_tiles(&d, a, &ctx);
            }
        }
    }

    #[test]
    fn swap_exchanges_panels_and_their_sizes_when_each_is_alone_in_its_column() {
        let mut d = Dock::classic();
        d.move_panel(PanelId::Library, PanelId::Tracks, Zone::Swap);
        let arr = d.arrange(area(150, 36));
        // tracks is now on the left and elastic; library took the elastic slot but kept its 26 columns
        assert!(arr.rect(PanelId::Tracks).unwrap().x < arr.rect(PanelId::Library).unwrap().x);
        assert_eq!(arr.rect(PanelId::Library).unwrap().width, 26, "the library kept its size");
        assert_eq!(arr.rect(PanelId::Cover).unwrap().width, 36);
    }

    #[test]
    fn docking_beside_above_and_below() {
        // library dropped on the queue's top edge → joins the right-hand stack above it
        let mut d = Dock::classic();
        d.move_panel(PanelId::Library, PanelId::Queue, Zone::Top);
        assert_eq!(d.columns.len(), 2, "the library's old column is gone");
        assert_eq!(d.columns[1].panels, vec![PanelId::Cover, PanelId::Library, PanelId::Queue]);
        // cover dropped on tracks' right edge → a new column right after tracks
        let mut d = Dock::classic();
        d.move_panel(PanelId::Cover, PanelId::Tracks, Zone::Right);
        assert_eq!(d.columns.iter().map(|c| c.panels.clone()).collect::<Vec<_>>(), vec![vec![PanelId::Library], vec![PanelId::Tracks], vec![PanelId::Cover], vec![PanelId::Queue]]);
        // queue dropped on the library's left → leftmost
        let mut d = Dock::classic();
        d.move_panel(PanelId::Queue, PanelId::Library, Zone::Left);
        assert_eq!(d.columns[0].panels, vec![PanelId::Queue]);
        // reordering inside a stack
        let mut d = Dock::classic();
        d.move_panel(PanelId::Queue, PanelId::Cover, Zone::Top);
        assert_eq!(d.columns[2].panels, vec![PanelId::Queue, PanelId::Cover]);
    }

    #[test]
    fn resizing_a_column_boundary_moves_only_that_pair() {
        let a = area(150, 36);
        let mut d = Dock::classic();
        d.resize_col(0, 39, a); // library's last column → 39: it becomes 40 wide
        let arr = d.arrange(a);
        assert_eq!(arr.rect(PanelId::Library).unwrap().width, 40);
        assert_eq!(arr.rect(PanelId::Cover).unwrap().width, 36, "the far column didn't move");
        // dragging far past the limit clamps instead of breaking
        d.resize_col(0, 500, a);
        assert!(arr_width(&d, a, PanelId::Tracks) >= PanelId::Tracks.min_size().0);
        d.resize_col(0, 0, a);
        assert_eq!(arr_width(&d, a, PanelId::Library), PanelId::Library.min_size().0);
        // the boundary between tracks and the right column adjusts the right column
        let mut d = Dock::classic();
        d.resize_col(1, 100, a);
        assert_eq!(arr_width(&d, a, PanelId::Cover), 150 - 101);
    }

    fn arr_width(d: &Dock, a: Rect, p: PanelId) -> u16 {
        d.arrange(a).rect(p).unwrap().width
    }

    #[test]
    fn resizing_a_row_boundary_conserves_the_stack_height() {
        let a = area(150, 36);
        let mut d = Dock::classic();
        d.resize_row(2, 0, 1 + 20, a); // cover's last row is 20 → cover is 21 tall
        let arr = d.arrange(a);
        assert_eq!(arr.rect(PanelId::Cover).unwrap().height, 21);
        assert_eq!(arr.rect(PanelId::Queue).unwrap().height, 15);
        // and it reproduces exactly when laid out again
        assert_eq!(d.arrange(a).rect(PanelId::Cover).unwrap().height, 21);
        d.resize_row(2, 0, 999, a);
        assert_eq!(d.arrange(a).rect(PanelId::Queue).unwrap().height, PanelId::Queue.min_size().1);
    }

    #[test]
    fn min_size_reflects_the_layout() {
        assert_eq!(Dock::classic().min_size(), (14 + 24 + 24, 8)); // 3 columns; cover+queue stacked
        let stacked = &Dock::presets()[2].1;
        assert_eq!(stacked.min_size().0, 24 + 24);
        assert_eq!(stacked.min_size().1, 5 + 4 + 4);
        // laying out at exactly the minimum works for every preset, and one cell less never panics
        for (_, d) in Dock::presets() {
            let (w, h) = d.min_size();
            let _ = d.arrange(area(w.saturating_sub(1), h.saturating_sub(1)));
            let _ = d.arrange(area(1, 1));
            let _ = d.arrange(area(0, 0));
        }
    }

    #[test]
    fn json_round_trip_repair_and_legacy_migration() {
        for (name, d) in Dock::presets() {
            assert_eq!(Dock::from_json(&d.to_json()), Some(d), "{name}");
        }
        // damaged layouts are repaired, not rejected: a duplicated panel and a missing one
        let broken = serde_json::json!({ "columns": [
            { "panels": ["tracks", "tracks", "library"], "weights": [1], "width": 10 },
            { "panels": [], "weights": [], "width": 30 },
            { "panels": ["cover"], "weights": [0], "width": 9999 },
        ]});
        let fixed = Dock::from_json(&broken).expect("repairable");
        assert_valid(&fixed, "repaired");
        assert!(fixed.find(PanelId::Queue).is_some(), "the missing queue came back");
        assert!(fixed.columns.iter().all(|c| c.width <= 160));
        // not a layout at all
        assert_eq!(Dock::from_json(&serde_json::json!({ "nope": 1 })), None);
        assert_eq!(Dock::from_json(&serde_json::json!({ "columns": "x" })), None);
        // the old three-slot layout migrates with its sizes
        let m = Dock::from_legacy(31, 44, 55);
        assert_valid(&m, "legacy");
        assert_eq!((m.columns[0].width, m.columns[2].width, m.columns[2].weights.clone()), (31, 44, vec![55, 45]));
    }

    #[test]
    fn reset_divider_restores_defaults() {
        let a = area(150, 36);
        let mut d = Dock::classic();
        d.resize_col(0, 45, a);
        d.resize_row(2, 0, 25, a);
        d.reset_divider(Divider::Col(0));
        d.reset_divider(Divider::Row(2, 0));
        assert_eq!(d.columns[0].width, 26);
        assert_eq!(d.columns[2].weights, vec![40, 60]);
    }

    #[test]
    fn drop_zones_edges_dock_and_the_middle_swaps() {
        let r = Rect::new(10, 5, 40, 20);
        assert_eq!(zone_at(r, 25, 15), Zone::Swap);
        assert_eq!(zone_at(r, 10, 15), Zone::Left);
        assert_eq!(zone_at(r, 12, 15), Zone::Left);
        assert_eq!(zone_at(r, 49, 15), Zone::Right);
        assert_eq!(zone_at(r, 25, 5), Zone::Top);
        assert_eq!(zone_at(r, 25, 24), Zone::Bottom);
        // corners resolve to the nearer edge, never to nothing
        assert!(matches!(zone_at(r, 10, 5), Zone::Left | Zone::Top));
        // panels too small to subdivide just swap
        assert_eq!(zone_at(Rect::new(0, 0, 3, 10), 0, 0), Zone::Swap);
        // the preview is the half that would be taken, and always inside the panel
        for z in ALL_ZONES {
            let p = preview_rect(r, z);
            assert!(p.x >= r.x && p.y >= r.y && p.x + p.width <= r.x + r.width && p.y + p.height <= r.y + r.height, "{z:?}");
        }
        assert_eq!(preview_rect(r, Zone::Swap), r);
        assert_eq!(preview_rect(r, Zone::Left).width, 20);
        assert_eq!(preview_rect(r, Zone::Bottom).y, 15);
    }

    #[test]
    fn neighbors_follow_the_geometry() {
        let arr = Dock::classic().arrange(area(150, 36));
        assert_eq!(arr.neighbor(PanelId::Library, Dir::Right), Some(PanelId::Tracks));
        assert_eq!(arr.neighbor(PanelId::Tracks, Dir::Right).map(|p| p == PanelId::Cover || p == PanelId::Queue), Some(true));
        assert_eq!(arr.neighbor(PanelId::Cover, Dir::Down), Some(PanelId::Queue));
        assert_eq!(arr.neighbor(PanelId::Queue, Dir::Up), Some(PanelId::Cover));
        assert_eq!(arr.neighbor(PanelId::Library, Dir::Left), None);
        assert_eq!(arr.neighbor(PanelId::Cover, Dir::Right), None);
        assert_eq!(arr.panel_at(5, 5), Some(PanelId::Library));
        assert_eq!(arr.panel_at(149, 2), Some(PanelId::Cover));
        assert_eq!(arr.panel_at(500, 500), None);
    }

    #[test]
    fn proportional_split_is_exact_idempotent_and_respects_minimums() {
        for total in 8..120u16 {
            let r = split_proportional(total, &[4, 4], &[40, 60]);
            assert_eq!(r.iter().sum::<u16>(), total);
            assert!(r.iter().all(|h| *h >= 4));
            // feeding the result back in as weights reproduces it (what resize_row relies on)
            assert_eq!(split_proportional(total, &[4, 4], &r), r, "total {total}");
        }
        // minimums win when they don't fit
        assert_eq!(split_proportional(5, &[4, 4], &[1, 1]), vec![4, 4]);
        assert!(split_proportional(10, &[], &[]).is_empty());
    }
}
