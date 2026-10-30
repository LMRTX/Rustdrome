//! Drawing. `draw` is the only entry point; it repaints the whole frame from
//! `App` state every tick and records hit-boxes for the mouse.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::anim;
use crate::app::*;
use crate::dock::PanelId;
use crate::modal::*;
use crate::queue::Repeat;
use crate::stats;
use crate::theme::Theme;

/// Rows taken by everything that isn't a panel: the top bar, the 4-row
/// now-playing box and the footer.
pub const CHROME_ROWS: u16 = 1 + NOW_ROWS + 1;
/// Height of the now-playing box: its border and two lines.
const NOW_ROWS: u16 = 4;
/// Zen mode has no panels, so it needs far less width — but the cover plus its
/// nine rows of text, plus the bars above and below.
pub const MIN_SIZE_ZEN: (u16, u16) = (30, 20);

/// Smallest terminal the *current* layout works in: whatever the panels'
/// minimum sizes add up to in the arrangement the user has chosen, plus the
/// bars. So "needed for current config" is literally true.
pub fn min_size(app: &App) -> (u16, u16) {
    if app.zen {
        MIN_SIZE_ZEN
    } else {
        let (w, h) = app.dock.min_size();
        (w, h + CHROME_ROWS)
    }
}

/// `v` limited to `lo..=hi`, never panicking: unlike `Ord::clamp`, an inverted
/// range (which a small terminal makes easy to produce) just means `hi` wins.
fn fit(v: u16, lo: u16, hi: u16) -> u16 {
    v.max(lo).min(hi)
}

// nerd-font glyphs (same set as the Python version)
const PLAY: &str = "\u{f04b}";
const PAUSE: &str = "\u{f04c}";
const SHUFFLE: &str = "\u{f074}";
const REPEAT: &str = "\u{f01e}";
const SPEED: &str = "\u{f0e4}";
const MOON: &str = "\u{f186}";
const STAR: &str = "\u{f005}";
// speaker glyphs (Nerd Font Material Design set): muted, low, medium, high
const VOL_MUTE: &str = "\u{f075f}";
const VOL_LOW: &str = "\u{f057f}";
const VOL_MED: &str = "\u{f0580}";
const VOL_HIGH: &str = "\u{f057e}";

/// Which of the four speaker icons shows for a volume: muted (or 0), then
/// thirds of the 0–100 range (anything louder than 100 counts as max).
fn vol_icon(vol: i64, muted: bool) -> &'static str {
    match (muted, vol) {
        (true, _) | (_, ..=0) => VOL_MUTE,
        (_, 1..=33) => VOL_LOW,
        (_, 34..=66) => VOL_MED,
        _ => VOL_HIGH,
    }
}
const DOWNLOAD: &str = "\u{f019}";
const CHECK_CIRCLE: &str = "\u{f058}";
const ALL_TRACKS: &str = "\u{f188e}";
const PLAYLIST: &str = "\u{f0cb8}";
// library categories: recently added / recently played / most played
const NEW_ICON: &str = "\u{f271}";
const HISTORY: &str = "\u{f1da}";
const MOST_PLAYED: &str = "\u{f080}";
const QUALITY: &str = "\u{f012}";
const PLUS: &str = "\u{f067}";
const PLUG: &str = "\u{f1e6}";
const FILTER: &str = "\u{f0b0}";
const PODCAST: &str = "\u{f2ce}";
const RADIO: &str = "\u{f519}";
const USER: &str = "\u{f007}";

const HELP_SECTIONS: &[(&str, &[(&str, &str)])] = &[
    (
        "playback",
        &[
            ("space", "play / pause"),
            ("enter", "play track / view / playlist"),
            ("n / b", "next / previous track"),
            ("← / →", "seek 5s  (shift: 30s)"),
            ("- / +", "volume down / up"),
            ("m", "mute"),
            (">", "cycle playback speed"),
            ("<", "sleep timer  (off→15→…→end)"),
            ("s", "toggle shuffle"),
            ("r", "cycle repeat  (off→all→one)"),
            ("i", "start radio from track / artist"),
            ("I", "toggle endless autoplay"),
        ],
    ),
    (
        "queue",
        &[
            ("a", "add to queue"),
            ("A", "play next"),
            ("x", "remove (in queue panel)"),
            ("ctrl+↑/↓", "move track up / down"),
            ("X", "clear queue"),
            ("ctrl+s", "save queue as a playlist"),
        ],
    ),
    (
        "playlists",
        &[
            ("p", "add track to a playlist"),
            ("P", "remove track from playlist"),
            ("shift+↑/↓", "reorder in playlist"),
            ("ctrl+r", "rename playlist"),
            ("ctrl+x", "delete playlist"),
        ],
    ),
    (
        "library",
        &[
            ("j/k/g/G", "move in lists"),
            ("[count] j/k", "repeat motion (3j = down 3)"),
            ("h / l", "previous / next panel"),
            ("/", "search"),
            ("\\", "filter pane (type to narrow)"),
            ("v", "select mode (space toggles)"),
            ("f", "star / unstar"),
            ("d / D", "download track / view"),
            ("ctrl+d", "download whole library"),
            ("O", "offline mode"),
            ("Q", "cycle stream quality"),
            ("1-5", "rate track (repeat clears)"),
            ("e / E", "go to album / artist"),
            ("y", "browse by genre"),
            ("w / W", "bookmark pos / jump to it"),
            ("L", "lyrics"),
            ("S", "copy share link"),
            ("R", "refresh from server"),
        ],
    ),
    (
        "app",
        &[
            ("t", "cycle themes"),
            ("B", "square / rounded boxes"),
            ("ctrl+l", "layouts: presets, save your own"),
            ("ctrl+e", "arrange panels with the keyboard"),
            ("ctrl/alt+drag", "move a panel; edges dock, middle swaps"),
            ("drag borders", "resize panels (double-click resets)"),
            ("T", "theme picker (live preview)"),
            ("N", "toggle notifications"),
            ("ctrl+y", "private listening"),
            ("ctrl+w", "listening stats"),
            ("z", "zen / now-playing splash"),
            ("ctrl+p", "command palette"),
            ("?", "this help"),
            ("q", "quit"),
        ],
    ),
];

fn panel<'a>(title: &str, subtitle: Option<String>, focused: bool, th: &Theme) -> Block<'a> {
    let color = if focused { th.border_focus } else { th.border };
    let mut b = Block::default()
        .borders(Borders::ALL)
        .border_type(th.bt)
        .border_style(Style::default().fg(color))
        .title(Span::styled(format!(" {title} "), Style::default().fg(if focused { th.lav } else { th.dim })));
    if let Some(s) = subtitle {
        let mut spans = vec![Span::styled(" ", Style::default().fg(th.dim))];
        let mut rest = s.as_str();
        if let Some(g) = rest.chars().next().filter(|c| [FILTER, CHECK_CIRCLE].iter().any(|i| i.starts_with(*c))) {
            spans.push(Span::styled(g.to_string(), Style::default().fg(th.blue)));
            rest = &rest[g.len_utf8()..];
        }
        spans.push(Span::styled(format!("{rest} "), Style::default().fg(th.dim)));
        b = b.title_bottom(Line::from(spans).right_aligned());
    }
    b
}

/// Render only the rows in view. Building a `ListItem` per song every frame
/// would be wasteful for a 5000-track library (and each row stats the audio
/// cache), so rows are built lazily for the visible window.
fn windowed_list<'a>(
    f: &mut Frame,
    area: Rect,
    len: usize,
    state: &mut ListState,
    th: &Theme,
    focused: bool,
    row: impl Fn(usize) -> ListItem<'a>,
) {
    let h = area.height as usize;
    if h == 0 {
        return;
    }
    let sel = state.selected();
    let mut off = state.offset().min(len.saturating_sub(1));
    if let Some(s) = sel {
        if s < off {
            off = s;
        } else if s >= off + h {
            off = s + 1 - h;
        }
    }
    *state.offset_mut() = off;
    let end = (off + h).min(len);
    let items: Vec<ListItem> = (off..end).map(&row).collect();
    let mut local = ListState::default();
    local.select(sel.filter(|s| (off..end).contains(s)).map(|s| s - off));
    f.render_stateful_widget(List::new(items), area, &mut local);
    // The selected row is marked by its text only — bold and in the theme colour —
    // never by a background, so the panel looks the same under the cursor.
    if let Some(r) = local.selected() {
        let row = Rect::new(area.x, area.y + r as u16, area.width, 1).intersection(area);
        let bold = ratatui::style::Modifier::BOLD;
        if focused {
            f.buffer_mut().set_style(row, Style::default().fg(th.blue).add_modifier(bold));
        } else {
            // unfocused: bold light text, but accent-coloured cells (the icons) stay as they are
            let buf = f.buffer_mut();
            for x in row.x..row.x + row.width {
                if let Some(c) = buf.cell_mut((x, row.y)) {
                    if c.fg != th.blue {
                        c.set_fg(th.sub);
                    }
                    c.modifier.insert(bold);
                }
            }
        }
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let th = app.theme();
    let area = f.area();
    app.sixel.want = None;
    app.sixel.show = None;
    app.sixel.area_now = None;
    app.overlay_rects.clear();
    f.render_widget(Block::default().style(th.base()), area);

    let (min_w, min_h) = min_size(app);
    if area.width < min_w || area.height < min_h {
        // Nothing fits, so draw nothing else — not even a modal. Forget the
        // hit-boxes too, so a click can't land on a panel that isn't there.
        app.rects = Rects::default();
        let custom = !app.zen && app.dock != crate::dock::Dock::classic();
        draw_too_small(f, area, (min_w, min_h), custom, &th);
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(3), Constraint::Length(NOW_ROWS), Constraint::Length(1)])
        .split(area);

    draw_topbar(f, app, rows[0], &th);
    if app.zen {
        draw_zen(f, app, rows[1], &th);
    } else {
        draw_main(f, app, rows[1], &th);
    }
    draw_now(f, app, rows[2], &th);
    draw_footer(f, app, rows[3], &th);
    // toasts sit over the tracks column, not the cover (a bitmap can't be
    // painted over cheaply); in zen mode there are no columns, so use it all
    let toast_area = if app.zen { rows[1] } else { app.rects.tracks };
    let toasts = draw_toasts(f, app, toast_area, &th);
    app.overlay_rects.extend(toasts);
    if app.modal.is_some() {
        draw_modal(f, app, area, &th);
    }
}

/// "Terminal size too small" — the current size with each dimension green
/// when it's enough and red when it isn't, and what's needed.
fn draw_too_small(f: &mut Frame, area: Rect, need: (u16, u16), custom: bool, th: &Theme) {
    if area.width >= 3 && area.height >= 3 {
        f.render_widget(
            Block::default().borders(Borders::ALL).border_type(th.bt).border_style(Style::default().fg(th.border_focus)),
            area,
        );
    }
    let ok = |have: u16, need: u16| th.fg(if have >= need { th.green } else { th.red });
    let dim = th.fg(th.sub);
    let size = |w: u16, h: u16, wst: Style, hst: Style| {
        Line::from(vec![
            Span::styled("Width = ", dim),
            Span::styled(w.to_string(), wst),
            Span::styled(" Height = ", dim),
            Span::styled(h.to_string(), hst),
        ])
        .centered()
    };
    let lines = vec![
        Line::from(Span::styled("Terminal size too small:", th.bold(th.text))).centered(),
        size(area.width, area.height, ok(area.width, need.0), ok(area.height, need.1)),
        Line::from(""),
        Line::from(Span::styled("Needed for current config:", th.bold(th.text))).centered(),
        size(need.0, need.1, th.fg(th.text), th.fg(th.text)),
    ];
    let mut lines = lines;
    if custom {
        // you may have just made the layout bigger than the window: say how to undo it
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("custom layout — ctrl+e, then r, resets it", th.fg(th.dim))).centered());
    }
    let h = lines.len() as u16;
    let y = area.y + area.height.saturating_sub(h) / 2;
    let inner = Rect::new(area.x, y, area.width, h.min(area.height.saturating_sub(y - area.y)));
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_topbar(f: &mut Frame, app: &App, area: Rect, th: &Theme) {
    let mut left = vec![Span::raw(" ")];
    left.extend(anim::shimmer("Rustdrome", app.logo_phase, th.mauve, th.text, th));
    f.render_widget(Paragraph::new(Line::from(left)), area);

    let mut right: Vec<Span> = vec![];
    if app.private_mode {
        right.push(Span::styled("private  ", th.bold(th.mauve)));
    }
    if app.offline {
        right.push(Span::styled(format!("{PLUG} offline  "), th.fg(th.blue)));
    }
    if app.quality_now.0 > 0 {
        right.push(Span::styled(format!("{QUALITY} {}k  ", app.quality_now.0), th.fg(th.blue)));
    }
    if app.radio {
        right.push(Span::styled(format!("{RADIO} radio  "), th.fg(th.blue)));
    }
    if let Some(c) = &app.client {
        let host = c.server.split("://").last().unwrap_or("");
        right.push(Span::styled(format!("{}@{host}", c.username), th.fg(th.dim)));
    }
    right.push(Span::styled(format!("  · v{} ", env!("CARGO_PKG_VERSION")), th.fg(th.vfaint)));
    f.render_widget(Paragraph::new(Line::from(right)).alignment(Alignment::Right), area);
}

fn draw_main(f: &mut Frame, app: &mut App, area: Rect, th: &Theme) {
    let arr = app.dock.arrange(area);
    for (id, rect) in &arr.panels {
        match id {
            PanelId::Library => draw_sidebar(f, app, *rect, th),
            PanelId::Tracks => draw_tracks(f, app, *rect, th),
            PanelId::Cover => draw_art(f, app, *rect, th, true),
            PanelId::Queue => draw_queue(f, app, *rect, th),
        }
    }
    // remember where everything went, for the mouse
    app.rects.main = area;
    app.rects.arranged = arr.clone();

    if let Some(d) = app.drag {
        // light up the boundary being dragged
        if let Some((_, zone)) = arr.dividers.iter().find(|(x, _)| *x == d) {
            for y in zone.y..zone.y + zone.height {
                for x in zone.x..zone.x + zone.width {
                    if let Some(c) = f.buffer_mut().cell_mut((x, y)) {
                        c.set_fg(th.mauve);
                    }
                }
            }
        }
    }
    if let Some(mv) = app.moving {
        draw_move_overlay(f, &arr, mv, th);
    }
    if let Some(sel) = app.arrange {
        if let Some(r) = arr.rect(sel) {
            recolor_border(f, r, th.mauve);
        }
    }
}

/// Recolour a panel's existing border (keeping its glyphs and title) — drawing
/// a new border over it would erase the title.
fn recolor_border(f: &mut Frame, r: Rect, color: ratatui::style::Color) {
    if r.width == 0 || r.height == 0 {
        return;
    }
    let (x1, y1) = (r.x + r.width - 1, r.y + r.height - 1);
    let mut paint = |x: u16, y: u16| {
        if let Some(c) = f.buffer_mut().cell_mut((x, y)) {
            c.set_style(Style::default().fg(color).add_modifier(ratatui::style::Modifier::BOLD));
        }
    };
    for x in r.x..=x1 {
        paint(x, r.y);
        paint(x, y1);
    }
    for y in r.y..=y1 {
        paint(r.x, y);
        paint(x1, y);
    }
}

/// While a panel is being carried: dim where it came from, preview where it
/// would land (the half of the target it would take, tinted and outlined), and
/// tag the pointer with its name.
fn draw_move_overlay(f: &mut Frame, arr: &crate::dock::Arranged, mv: Moving, th: &Theme) {
    let tint = |f: &mut Frame, r: Rect, strong: bool| {
        for y in r.y..r.y + r.height {
            for x in r.x..r.x + r.width {
                if let Some(c) = f.buffer_mut().cell_mut((x, y)) {
                    if strong {
                        if th.can_blend() {
                            c.set_bg(crate::theme::blend(th.modal_bg, th.mauve, 0.38));
                        } else {
                            c.set_style(Style::default().add_modifier(ratatui::style::Modifier::REVERSED));
                        }
                    } else {
                        c.set_style(Style::default().add_modifier(ratatui::style::Modifier::DIM));
                    }
                }
            }
        }
    };
    if let Some(src) = arr.rect(mv.panel) {
        tint(f, src, false);
    }
    if let Some(target) = arr.panel_at(mv.x, mv.y).filter(|t| *t != mv.panel) {
        if let Some(r) = arr.rect(target) {
            let zone = crate::dock::zone_at(r, mv.x, mv.y);
            let pr = crate::dock::preview_rect(r, zone);
            tint(f, pr, true);
            f.render_widget(Block::default().borders(Borders::ALL).border_type(th.bt).border_style(th.bold(th.mauve)), pr);
        }
    }
    let label = format!(" {} ", mv.panel.label());
    let fr = f.area();
    let w = label.chars().count() as u16;
    let (x, y) = ((mv.x + 2).min(fr.width.saturating_sub(w)), (mv.y + 1).min(fr.height.saturating_sub(1)));
    f.render_widget(
        Paragraph::new(Span::styled(label, Style::default().fg(th.modal_bg).bg(th.mauve).add_modifier(ratatui::style::Modifier::BOLD))),
        Rect::new(x, y, w.min(fr.width), 1),
    );
}

fn draw_sidebar(f: &mut Frame, app: &mut App, area: Rect, th: &Theme) {
    let focused = app.focus == Focus::Sidebar;
    let block = panel("library", None, focused, th);
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.rects.sidebar = area;
    let rows = &app.side_rows;
    let cur_view = app.view.as_str();
    let row = |i: usize| -> ListItem<'static> {
        let line = match &rows[i] {
            SideRow::Header(h) => Line::from(Span::styled(format!(" {h}"), th.bold(th.dim))),
            SideRow::Spacer => Line::from(" "),
            SideRow::View { id, label } => {
                let glyph = match *id {
                    "all-songs" => ALL_TRACKS,
                    "shuffle-all" => SHUFFLE,
                    "starred" => STAR,
                    "newest" => NEW_ICON,
                    "recent" => HISTORY,
                    "frequent" => MOST_PLAYED,
                    _ => "◍",
                };
                let color = th.blue;
                let name = if *id == cur_view { th.bold(th.text) } else { th.fg(th.text) };
                Line::from(vec![Span::styled(format!(" {glyph} "), th.fg(color)), Span::styled(label.to_string(), name)])
            }
            SideRow::Playlist(p) => Line::from(vec![
                Span::styled(format!(" {PLAYLIST} "), th.fg(th.blue)),
                Span::styled(p.name.clone(), th.fg(th.text)),
            ]),
            SideRow::NewPlaylist => {
                Line::from(vec![Span::styled(format!(" {PLUS} "), th.fg(th.blue)), Span::styled("new playlist", th.fg(th.sub))])
            }
            SideRow::Podcast(c, n) => Line::from(vec![
                Span::styled(format!(" {PODCAST} "), th.fg(th.blue)),
                Span::styled(c.title.clone(), th.fg(th.text)),
                Span::styled(format!(" {n}"), th.fg(th.vfaint)),
            ]),
            SideRow::Radio(n) => Line::from(vec![
                Span::styled(format!(" {RADIO} "), th.fg(th.blue)),
                Span::styled("stations", th.fg(th.text)),
                Span::styled(format!(" {n}"), th.fg(th.vfaint)),
            ]),
        };
        ListItem::new(line)
    };
    windowed_list(f, inner, rows.len(), &mut app.sidebar, th, focused, row);
}

fn draw_tracks(f: &mut Frame, app: &mut App, area: Rect, th: &Theme) {
    let focused = app.focus == Focus::Tracks;
    let n = app.view_len();
    let subtitle = if app.filtering {
        Some(format!("{FILTER} {}▏ {}/{}", app.filter_query, n, app.songs.len()))
    } else if app.select_mode {
        Some(format!("{CHECK_CIRCLE} {} selected", app.selected.len()))
    } else if app.dl_total > 0 {
        Some(format!("{} downloading {}/{}", anim::spinner(app.tick), app.dl_done, app.dl_total))
    } else if app.loading > 0 {
        Some(format!("{} refreshing", anim::spinner(app.tick)))
    } else if !app.count.is_empty() {
        Some(format!("{}…", app.count))
    } else if n > 0 {
        Some(n.to_string())
    } else {
        None
    };
    let block = panel(&app.tracks_title.clone(), subtitle, focused, th);
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.rects.tracks = area;

    if n == 0 {
        let msg = if app.client.is_none() { "" } else if app.loading > 0 { "loading…" } else { "nothing here" };
        f.render_widget(Paragraph::new(msg).style(th.fg(th.dim)).alignment(Alignment::Center), inner);
        return;
    }
    // disjoint field borrows: no per-frame clone of a 5000-song list
    let (songs, filtered, filtering) = (&app.songs, &app.filtered, app.filtering);
    let selected = &app.selected;
    let current = app.queue.current().map(|s| s.id.clone());
    let client = app.client.clone();
    let row = |i: usize| -> ListItem<'static> {
        let s = &songs[if filtering { filtered[i] } else { i }];
        let is_cur = current.as_deref() == Some(s.id.as_str());
        let mut spans = vec![];
        if selected.contains(&s.id) {
            spans.push(Span::styled(format!(" {CHECK_CIRCLE} "), th.fg(th.blue)));
        } else {
            spans.push(Span::styled(
                format!(" {} ", if is_cur { "♪" } else { "·" }),
                th.fg(if is_cur { th.blue } else { th.vfaint }),
            ));
        }
        spans.push(Span::styled(s.title.clone(), if is_cur { th.bold(th.blue) } else { th.fg(th.text) }));
        if client.as_ref().is_some_and(|c| c.is_pinned(&s.id)) {
            spans.push(Span::styled(format!(" {DOWNLOAD}"), th.fg(th.blue)));
        }
        if s.starred {
            spans.push(Span::styled(format!(" {STAR}"), th.fg(th.blue)));
        }
        spans.push(Span::styled(format!("  {}", s.artist), th.fg(th.dim)));
        if s.duration > 0 {
            spans.push(Span::styled(format!(" · {}", anim::fmt_time(s.duration as f64)), th.fg(th.vfaint)));
        }
        ListItem::new(Line::from(spans))
    };
    windowed_list(f, inner, n, &mut app.tracks, th, focused, row);
}

fn draw_art(f: &mut Frame, app: &mut App, area: Rect, th: &Theme, bordered: bool) {
    let inner = if bordered {
        let block = panel("cover", None, false, th);
        let i = block.inner(area);
        f.render_widget(block, area);
        i
    } else {
        area
    };
    app.sixel.area_now = Some(inner);
    if app.art_mode == crate::art::ArtMode::Sixel {
        // The bitmap is written to the terminal after the frame (see
        // `sixel::sync`); here we only say what we want and leave the cells
        // blank — drawing glyphs under a bitmap would fight with it.
        let cover = app.now_song.as_ref().and_then(|s| s.cover_art.clone());
        let ready = matches!((&app.art, &cover), (Some((id, _)), Some(c)) if id == c);
        if !app.settling() {
            app.sixel.plan_hint = crate::sixel::plan(inner, app.cell_px).map(|p| (p, app.cell_px));
        }
        // A picture is never withdrawn just because its panel is changing size:
        //  * while a divider is dragged or the window resized, a quick
        //    approximation is encoded for each new size (a few ms) so the cover
        //    follows the panel; no quality encode is started meanwhile;
        //  * once things settle the quality encode is requested, and the
        //    approximation stays up until it lands — then it is painted over.
        // Only a panel being carried around (`moving`) still hides it.
        if let (true, Some(cover)) = (ready && app.moving.is_none(), cover) {
            if let Some(plan) = crate::sixel::plan(inner, app.cell_px) {
                let theme = app.theme_name.clone();
                let shown = |preview| crate::sixel::Shown { rect: plan.rect, cover: cover.clone(), key: plan.key, theme: theme.clone(), area: inner, preview };
                let have_preview = matches!(&app.sixel.preview, Some((c, k, _)) if *c == cover && *k == plan.key);
                let have_real = matches!(&app.sixel.cache, Some((c, k, _)) if *c == cover && *k == plan.key);
                if app.live_resize() && have_real {
                    // the panel's size hasn't changed (a drag just started, or this step
                    // landed on the same size): keep the good picture, don't downgrade it
                    app.sixel.show = Some(shown(false));
                } else if app.live_resize() {
                    if !have_preview {
                        if let Some((_, art)) = &app.art {
                            let enc = crate::sixel::encode_preview(art.source(), &plan, app.cell_px);
                            app.sixel.preview = Some((cover.clone(), plan.key, enc));
                        }
                    }
                    app.sixel.show = Some(shown(true));
                } else {
                    app.sixel.want = Some((cover.clone(), plan));
                    if have_real {
                        app.sixel.show = Some(shown(false));
                    } else if have_preview {
                        app.sixel.show = Some(shown(true)); // keep the approximation until the real one lands
                    }
                }
            }
        }
        return;
    }
    match &app.art {
        Some((id, art)) if app.now_song.as_ref().and_then(|s| s.cover_art.as_deref()) == Some(id.as_str()) => {
            art.render(inner, f.buffer_mut());
        }
        _ => {
            let note = vec![
                Line::from(""),
                Line::from(Span::styled("♪", th.bold(th.faint))),
                Line::from(Span::styled(if app.now_song.is_some() { "no cover" } else { "" }, th.fg(th.vfaint))),
            ];
            let mid = inner.y + inner.height.saturating_sub(3) / 2;
            let r = Rect::new(inner.x, mid, inner.width, 3.min(inner.height));
            f.render_widget(Paragraph::new(note).alignment(Alignment::Center), r);
        }
    }
}

fn draw_queue(f: &mut Frame, app: &mut App, area: Rect, th: &Theme) {
    let focused = app.focus == Focus::Queue;
    let upcoming = app.queue.upcoming();
    let remaining: i64 = upcoming.iter().map(|s| s.duration).sum();
    let subtitle = (!app.queue.songs.is_empty())
        .then(|| format!("{}♪ up next · {}", upcoming.len(), anim::fmt_time(remaining as f64)));
    let block = panel("queue", subtitle, focused, th);
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.rects.queue = area;
    let songs = &app.queue.songs;
    let idx = app.queue.index;
    // same state as the icon on the now-playing line
    let playing = app.player.active() && !app.player.paused();
    let row = |i: usize| -> ListItem<'static> {
        let s = &songs[i];
        let ii = i as i64;
        let line = if ii < idx {
            Line::from(vec![
                Span::styled(format!("{:>2} ", i + 1), th.fg(th.vfaint)),
                Span::styled(s.title.clone(), th.fg(th.faint)),
                Span::styled(format!("  {}", s.artist), th.fg(th.vfaint)),
            ])
        } else if ii == idx {
            Line::from(vec![
                Span::styled(format!(" {} ", if playing { PLAY } else { PAUSE }), th.fg(th.blue)),
                Span::styled(s.title.clone(), th.bold(th.blue)),
                Span::styled(format!("  {}", s.artist), th.fg(th.dim)),
            ])
        } else {
            Line::from(vec![
                Span::styled(format!("{:>2} ", i + 1), th.fg(th.vfaint)),
                Span::styled(s.title.clone(), th.fg(th.text)),
                Span::styled(format!("  {}", s.artist), th.fg(th.dim)),
            ])
        };
        ListItem::new(line)
    };
    windowed_list(f, inner, songs.len(), &mut app.queue_state, th, focused, row);
}

fn draw_now(f: &mut Frame, app: &mut App, area: Rect, th: &Theme) {
    app.rects.bar = Rect::default();
    app.rects.seek = Rect::default();
    app.rects.gauge = Rect::default();
    app.rects.vol = Rect::default();
    app.rects.shuffle = Rect::default();
    app.rects.repeat = Rect::default();

    let playing = app.player.active() && !app.player.paused();
    let border = if playing { th.border_alt } else { th.border };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(th.bt)
        .border_style(Style::default().fg(border))
        .title(Span::styled(" now playing ", th.fg(th.dim)));
    let below = block.inner(area);
    f.render_widget(block, area);
    // one column of padding inside the border on each side
    let inner = Rect::new(below.x + 1, below.y, below.width.saturating_sub(2), below.height);
    if inner.height < 2 || inner.width < 8 {
        return;
    }
    let width = inner.width as usize;
    let (y1, y2) = (inner.y, inner.y + 1);

    let dur = app.track_duration();
    // while the seek line is held the dot follows the pointer; otherwise it is
    // extrapolated between mpv's quarter-second reports so it moves evenly
    let pos = if app.scrub == Some(crate::app::Scrub::Seek) {
        app.scrub_frac * dur
    } else if let Some((reported, at)) = app.pos_report {
        anim::playhead(reported, at.elapsed().as_secs_f64(), app.player.speed(), playing, dur, app.shown_pos)
    } else {
        app.now_position
    };
    app.shown_pos = pos;
    let muted = app.player.muted();
    let vol = app.player.volume();
    let vol_frac = if muted { 0.0 } else { vol as f64 / 100.0 };
    let pulse = if playing { ((app.tick as f64 * 0.55).sin() + 1.0) / 2.0 } else { 0.0 };
    let w_of = |v: &[Span]| -> usize { v.iter().map(|s| s.content.width()).sum() };

    // ── top line, right side: shuffle  repeat  🔊 ───── 100% ──
    // fixed part: "⇆ ⟳¹" + 2 gap + icon + gap + " " + "100%" = 15 cells; the slider takes what is left
    const SLIDER_MAX: usize = 12;
    const LEFT_MIN: usize = 10;
    let slider_w = width.saturating_sub(15 + 1 + LEFT_MIN).min(SLIDER_MAX);
    let slider_w = if slider_w < 3 { 0 } else { slider_w };

    let mut right: Vec<Span> = vec![Span::styled(format!("{SHUFFLE} "), th.fg(if app.queue.shuffle { th.blue } else { th.vfaint }))];
    let rep_start = w_of(&right);
    let rep_on = if app.queue.repeat != Repeat::Off { th.blue } else { th.vfaint };
    right.push(Span::styled(REPEAT, th.fg(rep_on)));
    // always one cell after the glyph: the ¹ of repeat-one, else a gap (Nerd-font
    // glyphs often draw wider than their width of 1 and would touch what follows)
    right.push(Span::styled(if app.queue.repeat == Repeat::One { "¹" } else { " " }, th.fg(th.blue)));
    let rep_end = w_of(&right);
    right.push(Span::raw("  "));

    let flash = if app.vol_flash > 0 && th.can_blend() { app.vol_flash as f64 / 14.0 } else { 0.0 };
    let lit = crate::theme::blend(th.lav, ratatui::style::Color::Rgb(255, 255, 255), flash);
    let icon_lit = crate::theme::blend(th.blue, ratatui::style::Color::Rgb(255, 255, 255), flash);
    let vol_start = w_of(&right);
    right.push(Span::styled(format!("{} ", vol_icon(vol, muted)), th.fg(if muted { th.red } else { icon_lit })));
    let gauge_start = w_of(&right);
    if slider_w > 0 {
        right.extend(anim::line_bar(vol_frac, slider_w, lit, flash, th));
        right.push(Span::raw(" "));
    }
    let pct = if muted { "mute".to_string() } else { format!("{vol}%") };
    right.push(Span::styled(format!("{pct:>4}"), th.fg(if muted { th.red } else { th.dim })));
    let vol_end = w_of(&right);
    let right_w = vol_end.min(width);
    let rx = inner.x + (width - right_w) as u16;
    f.render_widget(Paragraph::new(Line::from(right)), Rect::new(rx, y1, right_w as u16, 1));

    // ── top line, left side: what is playing ──
    let left_w = width.saturating_sub(right_w + 1);
    let mut top: Vec<Span> = vec![];
    match &app.now_song {
        None => {
            top.push(Span::styled("nothing playing", th.fg(th.dim)));
            top.push(Span::styled("  ·  press ", th.fg(th.vfaint)));
            top.push(Span::styled("enter", th.fg(th.dim)));
            top.push(Span::styled(" on a track", th.fg(th.vfaint)));
        }
        Some(song) => {
            top.push(Span::styled(format!("{} ", if playing { PLAY } else { PAUSE }), th.fg(th.blue)));
            let mut badges: Vec<Span> = vec![];
            if (app.player.speed() - 1.0).abs() > 1e-3 {
                let mut c = th.blue;
                if app.speed_flash > 0 && th.can_blend() {
                    c = crate::theme::blend(c, ratatui::style::Color::Rgb(255, 255, 255), app.speed_flash as f64 / 12.0);
                }
                badges.push(Span::styled(format!(" {SPEED}{}x", app.player.speed()), th.fg(c)));
            }
            if !app.sleep_label.is_empty() {
                badges.push(Span::styled(format!(" {MOON}{}", app.sleep_label), th.fg(th.blue)));
            }
            let badge_w: usize = badges.iter().map(|s| s.content.width()).sum();
            let star = if song.starred { format!(" {STAR}") } else { String::new() };
            let used: usize = top.iter().map(|s| s.content.width()).sum();
            let room = left_w.saturating_sub(used + star.width() + badge_w).max(8);
            let body = format!("{}  —  {} · {}", song.title, song.artist, song.album);
            let flash = app.title_flash as f64 / 12.0;
            let tc = crate::theme::blend(th.text, ratatui::style::Color::Rgb(255, 255, 255), 0.7 * flash);
            top.push(Span::styled(anim::marquee(&body, room, app.tick / 2), th.bold(tc)));
            if !star.is_empty() {
                top.push(Span::styled(star, th.fg(th.blue)));
            }
            top.extend(badges);
        }
    }
    f.render_widget(Paragraph::new(Line::from(top)), Rect::new(inner.x, y1, left_w as u16, 1));

    // ── bottom line: elapsed ───── total ──
    let (pos_s, dur_s) = (anim::fmt_time(pos), anim::fmt_time(dur));
    let tw = pos_s.width().max(dur_s.width()).max(4);
    let bar_w = width.saturating_sub(2 * tw + 2);
    let frac = if dur > 0.0 { pos / dur } else { 0.0 };
    let fill = crate::theme::blend(th.blue, th.lav, 0.3);
    let mut line = vec![Span::styled(format!("{pos_s:<tw$} "), th.fg(th.dim))];
    line.extend(anim::line_bar(frac, bar_w, fill, pulse, th));
    line.push(Span::styled(format!(" {dur_s:>tw$}"), th.fg(th.dim)));
    f.render_widget(Paragraph::new(Line::from(line)), Rect::new(inner.x, y2, inner.width, 1));

    // hit-boxes for the mouse
    app.rects.bar = Rect::new(inner.x + tw as u16 + 1, y2, bar_w as u16, 1);
    app.rects.seek = Rect::new(inner.x, y2, inner.width, 1);
    app.rects.gauge = Rect::new(rx + gauge_start as u16, y1, slider_w as u16, 1);
    app.rects.vol = Rect::new(rx + vol_start as u16, y1, (vol_end - vol_start) as u16, 1);
    app.rects.shuffle = Rect::new(rx, y1, 2, 1);
    app.rects.repeat = Rect::new(rx + rep_start as u16, y1, (rep_end - rep_start) as u16, 1);
}

/// Rows of text under the cover in zen mode: a blank line, the title, the
/// album line, a blank line, and a five-line lyrics window.
const ZEN_TEXT_ROWS: u16 = 9;

/// How many rows the zen cover occupies: the biggest square that fits the
/// width and what's left after the text, in rows. Knowing this up front lets
/// the whole composition (cover + text) be centred as one group.
pub fn zen_art_rows(width: u16, avail: u16, cell: (u16, u16), mode: crate::art::ArtMode) -> u16 {
    if avail == 0 || width == 0 {
        return 0;
    }
    match mode {
        crate::art::ArtMode::Sixel => {
            let (cw, ch) = (cell.0.max(1) as u32, cell.1.max(1) as u32);
            let side = (width as u32 * cw).min(avail as u32 * ch).min(crate::sixel::MAX_SIDE);
            (side.div_ceil(ch) as u16).clamp(1, avail)
        }
        // half-cells: one pixel per column, two per row
        _ => (width.min(avail * 2) / 2).clamp(1, avail),
    }
}

fn draw_zen(f: &mut Frame, app: &mut App, area: Rect, th: &Theme) {
    let avail = area.height.saturating_sub(ZEN_TEXT_ROWS);
    let art_rows = zen_art_rows(area.width, avail, app.cell_px, app.art_mode);
    // centre the cover and its text together, not the cover with text stuck to the bottom
    let group = art_rows + ZEN_TEXT_ROWS.min(area.height);
    let top = area.y + area.height.saturating_sub(group) / 2;

    let art_box = Rect::new(area.x, top, area.width, art_rows);
    draw_art(f, app, art_box, th, false);

    let info_y = top + art_rows + 1; // one empty line under the cover
    let lyrics_y = info_y + 2 + 1; //   and one between the album line and the lyrics
    let bottom = area.y + area.height;
    let line_w = area.width.saturating_sub(2) as usize;
    if let Some(s) = &app.now_song {
        if info_y + 2 <= bottom {
            let lines = vec![
                Line::from(Span::styled(anim::ellipsize(&s.title, line_w), th.bold(th.text))).centered(),
                Line::from(Span::styled(anim::ellipsize(&format!("{} · {}", s.artist, s.album), line_w), th.fg(th.dim))).centered(),
            ];
            f.render_widget(Paragraph::new(lines), Rect::new(area.x, info_y, area.width, 2));
        }
    }
    if let Some((_, lines)) = &app.zen_lyrics {
        if lyrics_y < bottom {
            let pos = app.now_position;
            let cur = lines.iter().rposition(|(t, _)| *t <= pos);
            let mut out = vec![];
            for off in -2i64..=2 {
                let idx = cur.map(|c| c as i64 + off);
                let text = idx.and_then(|i| usize::try_from(i).ok()).and_then(|i| lines.get(i)).map_or("", |(_, t)| t.as_str());
                let style = if off == 0 { th.bold(th.text) } else { th.fg(if off.abs() == 1 { th.dim } else { th.vfaint }) };
                out.push(Line::from(Span::styled(anim::ellipsize(text, line_w), style)).centered());
            }
            let h = 5.min(bottom - lyrics_y);
            f.render_widget(Paragraph::new(out), Rect::new(area.x, lyrics_y, area.width, h));
        }
    }
}

fn key_label(spec: &str) -> String {
    let first = spec.split(',').next().unwrap_or(spec);
    first
        .replace("space", "space")
        .replace("slash", "/")
        .replace("backslash", "\\")
        .replace("greater_than_sign", ">")
        .replace("less_than_sign", "<")
        .replace("question_mark", "?")
        .replace("equals_sign", "=")
        .replace("minus", "-")
        .replace("plus", "+")
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect, th: &Theme) {
    if let Some(sel) = app.arrange {
        let mut spans = vec![Span::styled(format!(" arranging {} ", sel.label()), th.bold(th.mauve)), Span::styled("  ", th.fg(th.vfaint))];
        for (i, (k, d)) in [("←↑↓→", "swap"), ("shift+←↑↓→", "dock beside"), ("tab", "next panel"), ("r", "reset"), ("esc", "done")].iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled("  ", th.fg(th.vfaint)));
            }
            spans.push(Span::styled(k.to_string(), th.fg(th.blue)));
            spans.push(Span::styled(format!(" {d}"), th.fg(th.dim)));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }
    let kb = |id: &str| key_label(app.settings.keybinds.get(id).map_or("", |s| s.as_str()));
    let hints = [
        ("play_pause", "play/pause"),
        ("next_track", "next"),
        ("search", "search"),
        ("shuffle", "shuffle"),
        ("repeat", "repeat"),
        ("theme_cycle", "theme"),
        ("zen", "zen"),
        ("help", "help"),
        ("quit", "quit"),
    ];
    let mut spans = vec![Span::raw(" ")];
    for (i, (id, desc)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", th.fg(th.vfaint)));
        }
        spans.push(Span::styled(kb(id), th.fg(th.blue)));
        spans.push(Span::styled(format!(" {desc}"), th.fg(th.dim)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_toasts(f: &mut Frame, app: &App, area: Rect, th: &Theme) -> Vec<Rect> {
    let mut y = area.y;
    let mut rects = Vec::new();
    for t in app.toasts.iter().rev() {
        let w = (t.text.width() as u16 + 4).min(area.width.saturating_sub(2)).max(10);
        let r = Rect::new((area.x + area.width).saturating_sub(w + 1).max(area.x), y, w, 3);
        if r.y + r.height > area.y + area.height {
            break;
        }
        let color = match t.level {
            Level::Info => th.border_alt,
            Level::Warn => th.yellow,
            Level::Error => th.red,
        };
        rects.push(r);
        f.render_widget(Clear, r);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(th.bt)
            .border_style(Style::default().fg(color))
            .style(Style::default().bg(th.modal_bg));
        let inner = block.inner(r);
        f.render_widget(block, r);
        f.render_widget(
            Paragraph::new(anim::ellipsize(&t.text, inner.width as usize)).style(Style::default().fg(th.text).bg(th.modal_bg)),
            inner,
        );
        y += 3;
    }
    rects
}

// ── modals ────────────────────────────────────────────────────────────
fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

fn modal_block<'a>(title: &str, th: &Theme) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(th.bt)
        .border_style(Style::default().fg(th.border_focus))
        .style(Style::default().bg(th.modal_bg).fg(th.text))
        .title(Span::styled(format!(" {title} "), th.bold(th.sub)))
}

fn field_line<'a>(label: &str, value: &str, focused: bool, secret: bool, th: &Theme) -> Line<'a> {
    let shown = if secret { "•".repeat(value.chars().count()) } else { value.to_string() };
    Line::from(vec![
        Span::styled(format!("{label:<11}"), th.fg(if focused { th.blue } else { th.dim })),
        Span::styled(shown, th.fg(th.text)),
        Span::styled(if focused { "▏" } else { "" }, th.fg(th.blue)),
    ])
}

fn hint_line<'a>(pairs: &[(&str, &str)], th: &Theme) -> Line<'a> {
    let mut v = vec![Span::raw(" ")];
    for (i, (k, d)) in pairs.iter().enumerate() {
        if i > 0 {
            v.push(Span::styled("  ·  ", th.fg(th.vfaint)));
        }
        v.push(Span::styled(k.to_string(), th.fg(th.blue)));
        v.push(Span::styled(format!(" {d}"), th.fg(th.dim)));
    }
    Line::from(v)
}

fn draw_modal(f: &mut Frame, app: &mut App, area: Rect, th: &Theme) {
    let mut overlays: Vec<Rect> = Vec::new();
    let tick = app.tick;
    let position = app.now_position;
    let logo_phase = app.logo_phase;
    let Some(modal) = &app.modal else { return };
    match modal {
        Modal::Login(l) => {
            let r = centered(area, 60, 11);
            f.render_widget(Clear, r);
            overlays.push(r);
            let block = modal_block("connect to your navidrome", th);
            let inner = block.inner(r);
            f.render_widget(block, r);
            let mut head = vec![Span::raw(" ")];
            head.extend(anim::shimmer("Rustdrome", logo_phase, th.mauve, th.text, th));
            let mut lines = vec![
                Line::from(head),
                Line::from(""),
                field_line(" server", &l.fields[0].value, l.focus == 0, false, th),
                field_line(" user", &l.fields[1].value, l.focus == 1, false, th),
                field_line(" password", &l.fields[2].value, l.focus == 2, true, th),
                Line::from(""),
            ];
            lines.push(match &l.status {
                Some((t, true)) => Line::from(Span::styled(format!(" ✗ {t}"), th.fg(th.red))),
                Some((t, false)) => Line::from(Span::styled(format!(" {} {t}", anim::spinner(tick)), th.fg(th.sub))),
                None => Line::from(Span::styled(" e.g. https://music.example.com", th.fg(th.vfaint))),
            });
            lines.push(hint_line(&[("enter", "connect"), ("tab", "next field"), ("esc", "quit")], th));
            f.render_widget(Paragraph::new(lines), inner);
        }
        Modal::Input(m) => {
            let r = centered(area, 52, 6);
            f.render_widget(Clear, r);
            overlays.push(r);
            let block = modal_block(&m.title, th);
            let inner = block.inner(r);
            f.render_widget(block, r);
            let shown = if m.field.value.is_empty() { Span::styled(m.placeholder.clone(), th.fg(th.vfaint)) } else { Span::styled(m.field.value.clone(), th.fg(th.text)) };
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(vec![shown, Span::styled("▏", th.fg(th.blue))]),
                    Line::from(""),
                    hint_line(&[("enter", "ok"), ("esc", "cancel")], th),
                ]),
                inner,
            );
        }
        Modal::Search(s) => {
            let h = fit(s.rows.len() as u16 + 6, 8, area.height.saturating_sub(4));
            let r = centered(area, 72, h);
            f.render_widget(Clear, r);
            overlays.push(r);
            let block = modal_block("search", th);
            let inner = block.inner(r);
            f.render_widget(block, r);
            let parts = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Length(1), Constraint::Min(1), Constraint::Length(1)])
                .split(inner);
            let q = if s.field.value.is_empty() { Span::styled("search the library", th.fg(th.vfaint)) } else { Span::styled(s.field.value.clone(), th.fg(th.text)) };
            f.render_widget(Paragraph::new(Line::from(vec![Span::styled(" ", th.fg(th.blue)), q, Span::styled(if s.in_list { "" } else { "▏" }, th.fg(th.blue))])), parts[0]);
            let rows = &s.rows;
            let res = &s.results;
            let row = |i: usize| -> ListItem<'static> {
                ListItem::new(match rows[i] {
                    SearchRow::Header(h) => Line::from(Span::styled(format!(" {h}"), th.bold(th.dim))),
                    SearchRow::Song(k) => {
                        let x = &res.songs[k];
                        Line::from(vec![Span::styled("  ♪ ", th.fg(th.blue)), Span::styled(x.title.clone(), th.fg(th.text)), Span::styled(format!("  {}", x.artist), th.fg(th.dim))])
                    }
                    SearchRow::Album(k) => {
                        let x = &res.albums[k];
                        let mut v = vec![Span::styled("  ◉ ", th.fg(th.mauve)), Span::styled(x.name.clone(), th.fg(th.text)), Span::styled(format!("  {}", x.artist), th.fg(th.dim))];
                        if let Some(y) = x.year {
                            v.push(Span::styled(format!(" · {y}"), th.fg(th.faint)));
                        }
                        Line::from(v)
                    }
                    SearchRow::Artist(k) => {
                        let x = &res.artists[k];
                        Line::from(vec![Span::styled(format!("  {USER} "), th.fg(th.blue)), Span::styled(x.name.clone(), th.fg(th.text)), Span::styled(format!("  {} albums", x.album_count), th.fg(th.dim))])
                    }
                })
            };
            if s.rows.is_empty() && s.searched {
                f.render_widget(Paragraph::new("  no matches").style(th.fg(th.dim)), parts[2]);
            } else {
                let mut st = ListState::default();
                st.select(s.in_list.then_some(s.cursor));
                windowed_list(f, parts[2], s.rows.len(), &mut st, th, true, row);
            }
            f.render_widget(Paragraph::new(hint_line(&[("↓", "results"), ("enter", "play"), ("a", "queue"), ("A", "play next"), ("esc", "close")], th)), parts[3]);
        }
        Modal::Help { scroll } => {
            let r = centered(area, 64, area.height.saturating_sub(2));
            f.render_widget(Clear, r);
            overlays.push(r);
            let block = modal_block("keys", th);
            let inner = block.inner(r);
            f.render_widget(block, r);
            let mut lines: Vec<Line> = vec![];
            for (title, items) in HELP_SECTIONS {
                lines.push(Line::from(Span::styled(format!(" {title}"), th.bold(th.mauve))));
                for (k, d) in *items {
                    lines.push(Line::from(vec![Span::styled(format!("  {k:<13}"), th.fg(th.blue)), Span::styled(d.to_string(), th.fg(th.sub))]));
                }
                lines.push(Line::from(""));
            }
            let max = lines.len().saturating_sub(inner.height as usize);
            f.render_widget(Paragraph::new(lines).scroll(((*scroll).min(max) as u16, 0)), inner);
        }
        Modal::Lyrics(m) => {
            let r = centered(area, 70, area.height.saturating_sub(2));
            f.render_widget(Clear, r);
            overlays.push(r);
            let block = modal_block(&m.title, th);
            let inner = block.inner(r);
            f.render_widget(block, r);
            let mut lines: Vec<Line> = vec![];
            let mut scroll = m.scroll as u16;
            if let Some(synced) = &m.synced {
                let cur = m.current_line(position);
                for (i, (_, t)) in synced.iter().enumerate() {
                    let style = if Some(i) == cur { th.bold(th.text) } else if cur.is_some_and(|c| i < c) { th.fg(th.faint) } else { th.fg(th.dim) };
                    lines.push(Line::from(Span::styled(format!(" {t}"), style)));
                }
                // keep the current line about a third of the way down
                scroll = cur.map_or(0, |c| c.saturating_sub(inner.height as usize / 3)) as u16;
            } else {
                lines.extend(m.text.lines().map(|l| Line::from(Span::styled(format!(" {l}"), th.fg(th.sub)))));
            }
            f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), inner);
        }
        Modal::Stats { summary: s, scroll } => {
            let r = centered(area, 64, area.height.saturating_sub(2).min(30));
            f.render_widget(Clear, r);
            overlays.push(r);
            let block = modal_block("listening stats", th);
            let inner = block.inner(r);
            f.render_widget(block, r);
            let mut lines: Vec<Line> = vec![];
            if s.is_empty() {
                lines.push(Line::from(Span::styled(" no plays logged yet — listen to something ♪", th.fg(th.dim))));
            } else {
                lines.push(Line::from(vec![
                    Span::styled(format!(" {} ", s.total), th.bold(th.blue)),
                    Span::styled("plays total  ·  ", th.fg(th.dim)),
                    Span::styled(format!("{} ", s.week_total), th.bold(th.blue)),
                    Span::styled("this week  ·  ", th.fg(th.dim)),
                    Span::styled(format!("\u{f06d} {} day streak", s.streak), th.fg(th.blue)),
                ]));
                lines.push(Line::from(vec![Span::styled(format!(" last {} days  ", s.days_window), th.fg(th.dim)), Span::styled(stats::sparkline(&s.per_day), th.fg(th.lav))]));
                lines.push(Line::from(""));
                let mut section = |title: &str, rows: Vec<String>| {
                    lines.push(Line::from(Span::styled(format!(" {title}"), th.bold(th.mauve))));
                    if rows.is_empty() {
                        lines.push(Line::from(Span::styled("   —", th.fg(th.vfaint))));
                    }
                    for (i, r) in rows.into_iter().enumerate() {
                        lines.push(Line::from(vec![Span::styled(format!("  {}. ", i + 1), th.fg(th.vfaint)), Span::styled(r, th.fg(th.text))]));
                    }
                    lines.push(Line::from(""));
                };
                let t = |v: &[(String, String, usize)]| v.iter().map(|(t, a, n)| format!("{t} — {a}  ×{n}")).collect::<Vec<_>>();
                let a = |v: &[(String, usize)]| v.iter().map(|(a, n)| format!("{a}  ×{n}")).collect::<Vec<_>>();
                section("top tracks · this week", t(&s.top_tracks_week));
                section("top tracks · all time", t(&s.top_tracks_all));
                section("top artists · this week", a(&s.top_artists_week));
                section("top artists · all time", a(&s.top_artists_all));
            }
            let max = lines.len().saturating_sub(inner.height as usize);
            f.render_widget(Paragraph::new(lines).scroll(((*scroll).min(max) as u16, 0)), inner);
        }
        Modal::Picker(p) => {
            let vis = p.visible();
            let h = fit(vis.len() as u16 + if p.query.is_some() { 5 } else { 4 }, 6, area.height.saturating_sub(4));
            let r = centered(area, 54, h);
            f.render_widget(Clear, r);
            overlays.push(r);
            let block = modal_block(&p.title, th);
            let inner = block.inner(r);
            f.render_widget(block, r);
            let mut top = 0;
            if let Some(q) = &p.query {
                f.render_widget(Paragraph::new(Line::from(vec![Span::styled(" ", th.fg(th.blue)), Span::styled(q.clone(), th.fg(th.text)), Span::styled("▏", th.fg(th.blue))])), Rect::new(inner.x, inner.y, inner.width, 1));
                top = 2;
            }
            let list_area = Rect::new(inner.x, inner.y + top, inner.width, inner.height.saturating_sub(top + 1));
            let (items, vis2) = (&p.items, &vis);
            let row = |i: usize| ListItem::new(Line::from(Span::styled(format!(" {}", items[vis2[i]]), th.fg(th.text))));
            let mut st = ListState::default();
            st.select(Some(p.cursor));
            windowed_list(f, list_area, vis.len(), &mut st, th, true, row);
            let hint = if p.query.is_some() { hint_line(&[("enter", "run"), ("esc", "close")], th) } else { hint_line(&[("↑↓", "move"), ("enter", "choose"), ("esc", "cancel")], th) };
            f.render_widget(Paragraph::new(hint), Rect::new(inner.x, inner.y + inner.height.saturating_sub(1), inner.width, 1));
        }
    }
    app.overlay_rects.extend(overlays);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Scrub;
    use crate::art::{Art, ArtMode};
    use crate::config::{AppDirs, Settings};
    use crate::models::Song;
    use image::{Rgb, RgbImage};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn test_app(mode: ArtMode) -> App {
        // one empty directory per app: tests persist layouts via `save_dock`, so a directory
        // shared by pid would leak one test's saved dock into another's starting state
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let tmp = std::env::temp_dir().join(format!(
            "rustdrome-ui-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(AppDirs::under(&tmp), Settings::default(), tx);
        app.zen = true;
        app.art_mode = mode;
        app.now_song = Some(Song {
            id: "s".into(),
            title: "Zen Title".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            cover_art: Some("c1".into()),
            ..Default::default()
        });
        app.art = Some(("c1".into(), Art::new(RgbImage::from_pixel(64, 64, Rgb([200, 80, 40])))));
        // six lyric lines, positioned on line 2 so the whole five-line window is populated
        app.zen_lyrics = Some(("s".into(), (0..6).map(|i| (i as f64 * 5.0, format!("L{i}"))).collect()));
        app.now_position = 10.5;
        app
    }

    fn render(app: &mut App, w: u16, h: u16) -> Terminal<TestBackend> {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        t
    }

    fn row_text(t: &Terminal<TestBackend>, y: u16) -> String {
        let b = t.backend().buffer();
        (0..b.area.width).map(|x| b.cell((x, y)).unwrap().symbol().to_string()).collect()
    }

    fn find_row(t: &Terminal<TestBackend>, needle: &str) -> Option<u16> {
        (0..t.backend().buffer().area.height).find(|&y| row_text(t, y).contains(needle))
    }

    /// leading/trailing blank cells of a row
    fn gaps(row: &str) -> (usize, usize) {
        let chars: Vec<char> = row.chars().collect();
        let first = chars.iter().position(|c| *c != ' ').unwrap_or(chars.len());
        let last = chars.iter().rposition(|c| *c != ' ').unwrap_or(0);
        (first, chars.len() - 1 - last)
    }

    #[test]
    fn zen_text_has_the_blank_lines_and_the_cover_is_symmetric_at_every_size() {
        for w in [60u16, 61, 80, 99, 100, 101, 120, 121, 150, 151, 200] {
            for h in [24u16, 25, 30, 31, 36, 42, 43] {
                let mut app = test_app(ArtMode::HalfCell);
                let t = render(&mut app, w, h);
                let ctx = format!("{w}x{h}");

                let art_rows: Vec<u16> = (0..h).filter(|&y| row_text(&t, y).contains('▀')).collect();
                assert!(!art_rows.is_empty(), "{ctx}: no cover drawn");
                let (first_art, last_art) = (art_rows[0], *art_rows.last().unwrap());
                let title = find_row(&t, "Zen Title").unwrap_or_else(|| panic!("{ctx}: no title"));
                let album = find_row(&t, "Artist · Album").unwrap_or_else(|| panic!("{ctx}: no album line"));

                // one empty line under the cover, album directly under the title
                assert_eq!(title, last_art + 2, "{ctx}: exactly one blank row between cover and title");
                assert!(row_text(&t, last_art + 1).trim().is_empty(), "{ctx}: that row is blank");
                assert_eq!(album, title + 1, "{ctx}");
                // one empty line between the album line and the lyrics
                assert!(row_text(&t, album + 1).trim().is_empty(), "{ctx}: blank row under album");
                assert!(row_text(&t, album + 2).contains("L0"), "{ctx}: lyrics start right after the blank row");

                // the cover is centred to the cell: equal gaps left and right
                for &y in &art_rows {
                    let (l, r) = gaps(&row_text(&t, y));
                    assert_eq!(l, r, "{ctx}: cover not centred on row {y} (left {l}, right {r})");
                }
                // text is centred too (a centred string of odd/even width can be half a cell off)
                for y in [title, album] {
                    let (l, r) = gaps(&row_text(&t, y));
                    assert!(l.abs_diff(r) <= 1, "{ctx}: row {y} gaps {l}/{r}");
                }
                // the whole group (cover + 9 text rows) is centred in the zen area
                let area_top = 1u16;
                let area_bottom = h - 4 - 1;
                let group_end = album + 2 + 5; // lyrics window is five rows
                let (top_gap, bottom_gap) = (first_art - area_top, area_bottom.saturating_sub(group_end));
                assert!(top_gap.abs_diff(bottom_gap) <= 1, "{ctx}: group off-centre vertically ({top_gap} above, {bottom_gap} below)");
            }
        }
    }

    #[test]
    fn zen_sixel_canvas_is_exactly_centred_for_many_cell_sizes() {
        for cell in [(10u16, 20u16), (9, 18), (14, 28), (7, 15), (8, 16), (13, 31)] {
            for w in [80u16, 81, 120, 121, 160, 161] {
                for h in [26u16, 27, 36, 37, 46] {
                    let mut app = test_app(ArtMode::Sixel);
                    app.cell_px = cell;
                    let _ = render(&mut app, w, h);
                    let (_, plan) = app.sixel.want.clone().unwrap_or_else(|| panic!("{cell:?} {w}x{h}: no plan"));
                    let r = plan.rect;
                    let ctx = format!("cell {cell:?} {w}x{h} rect {r:?}");
                    assert_eq!(r.x, w - (r.x + r.width), "{ctx}: left gap != right gap");
                    // the picture must fit its box in pixels (the bug: it didn't once the font size changed)
                    assert!(plan.key.side <= w as u32 * cell.0 as u32 && plan.key.side <= r.height as u32 * cell.1 as u32 + cell.1 as u32, "{ctx}");
                    let (area_top, area_h) = (1u16, h - CHROME_ROWS);
                    let group = r.height + ZEN_TEXT_ROWS;
                    let top_gap = r.y - area_top;
                    let bottom_gap = area_h.saturating_sub(group + top_gap);
                    assert!(top_gap.abs_diff(bottom_gap) <= 1, "{ctx}: vertical {top_gap}/{bottom_gap}");
                }
            }
        }
    }

    #[test]
    fn zen_art_rows_never_exceed_what_is_available() {
        for mode in [ArtMode::Sixel, ArtMode::HalfCell, ArtMode::Off] {
            for avail in 0..40u16 {
                for width in [0u16, 1, 20, 80, 200] {
                    let rows = zen_art_rows(width, avail, (10, 20), mode);
                    assert!(rows <= avail, "{mode:?} avail {avail} width {width} -> {rows}");
                    assert_eq!(rows == 0, avail == 0 || width == 0);
                }
            }
        }
    }

    #[test]
    fn square_mode_uses_plain_corners_everywhere_and_rounded_otherwise() {
        for (square, corner, other) in [(false, "╭", "┌"), (true, "┌", "╭")] {
            let mut app = test_app(ArtMode::HalfCell);
            app.zen = false;
            app.square = square;
            app.modal = Some(Modal::Help { scroll: 0 });
            app.notify("a toast", Level::Info);
            let t = render(&mut app, 150, 42);
            let all: String = (0..42).map(|y| row_text(&t, y)).collect();
            assert!(all.contains(corner), "square={square}: expected {corner} corners");
            assert!(!all.contains(other), "square={square}: found stray {other} corners (panels, now-playing, modal or toast)");
        }
    }

    #[test]
    fn repeat_glyph_keeps_a_gap_before_the_speaker() {
        // Nerd-font glyphs are width 1 to us but often draw wider: whatever follows
        // one needs a blank cell, with or without the repeat-one ¹ marker.
        let mut app = test_app(ArtMode::HalfCell);
        app.zen = false;
        for (repeat, mark) in [(crate::queue::Repeat::One, true), (crate::queue::Repeat::All, false), (crate::queue::Repeat::Off, false)] {
            app.queue.repeat = repeat;
            for w in [100u16, 150] {
                let t = render(&mut app, w, 42);
                let y = find_row(&t, "now playing").unwrap() + 1;
                let row: Vec<char> = row_text(&t, y).chars().collect();
                let rep = row.iter().position(|c| *c == REPEAT.chars().next().unwrap()).unwrap();
                let spk = row.iter().position(|c| *c == VOL_HIGH.chars().next().unwrap() || *c == VOL_MED.chars().next().unwrap()).unwrap();
                assert_eq!(row[rep + 1] == '¹', mark, "{w} cols");
                assert!(spk - rep >= 4, "{w} cols: repeat and speaker only {} cells apart", spk - rep);
                // the speaker and the shuffle glyph each have a blank cell after them too
                assert_eq!(row[spk + 1], ' ');
                let shuf = row.iter().position(|c| *c == SHUFFLE.chars().next().unwrap()).unwrap();
                assert_eq!(row[shuf + 1], ' ');
                assert_eq!(row[row.len() - 2], ' ', "{w} cols: padding column before the border is blank");
            }
        }
    }

    /// The classic layout's minimum, as `(width, height)` of the whole screen.
    const MIN_SIZE: (u16, u16) = (14 + 24 + 24, 8 + CHROME_ROWS);

    /// Every screen the app can show, ready to be rendered at any size.
    fn scenarios() -> Vec<(&'static str, App)> {
        use crate::modal::{InputModal, InputPurpose, LyricsModal, Login, PickKind, Picker, SearchModal, TextField};
        let mut v: Vec<(&'static str, App)> = Vec::new();
        let mut add = |name, mode, f: &dyn Fn(&mut App)| {
            let mut a = test_app(mode);
            a.zen = false;
            f(&mut a);
            v.push((name, a));
        };
        add("normal", ArtMode::HalfCell, &|_| {});
        add("normal+sixel", ArtMode::Sixel, &|_| {});
        add("normal+square+toasts", ArtMode::HalfCell, &|a| {
            a.square = true;
            for i in 0..5 {
                a.notify(format!("a fairly long toast message number {i}"), Level::Warn);
            }
        });
        add("zen", ArtMode::HalfCell, &|a| a.zen = true);
        add("zen+sixel", ArtMode::Sixel, &|a| a.zen = true);
        add("help", ArtMode::HalfCell, &|a| a.modal = Some(Modal::Help { scroll: 3 }));
        add("search", ArtMode::HalfCell, &|a| {
            let mut s = SearchModal::default();
            s.set_results(crate::models::SearchResults {
                songs: (0..30).map(|i| Song { id: i.to_string(), title: format!("t{i}"), ..Default::default() }).collect(),
                ..Default::default()
            });
            a.modal = Some(Modal::Search(s));
        });
        add("picker", ArtMode::HalfCell, &|a| {
            let items = (0..60).map(|i| format!("item {i}")).collect();
            a.modal = Some(Modal::Picker(Picker::new("pick", items, PickKind::Genre, true)));
        });
        add("input", ArtMode::HalfCell, &|a| {
            a.modal = Some(Modal::Input(InputModal {
                title: "new playlist".into(),
                placeholder: "name".into(),
                field: TextField::new("x"),
                purpose: InputPurpose::SaveQueue,
            }));
        });
        add("lyrics", ArtMode::HalfCell, &|a| {
            a.modal = Some(Modal::Lyrics(LyricsModal { title: "t".into(), synced: Some(vec![(0.0, "a".into()); 50]), text: String::new(), scroll: 0 }));
        });
        add("stats", ArtMode::HalfCell, &|a| {
            a.modal = Some(Modal::Stats { summary: crate::stats::summarize(&[], 0.0, 14), scroll: 0 });
        });
        add("login", ArtMode::HalfCell, &|a| a.modal = Some(Modal::Login(Login::new("", ""))));
        v
    }

    #[test]
    fn no_screen_panics_at_any_terminal_size() {
        // Every screen × every size from 1×1 up past a big monitor. A terminal being dragged
        // smaller walks through all of these, so a panic at *any* of them is a crash for a user.
        // Exhaustive through and just past the minimum (64×16), where layouts are tightest and
        // bugs live; sparse beyond it, where a full render is ~100× dearer than a "too small" one.
        let widths: Vec<u16> = (1..=72).chain([80, 100, 120, 150, 200]).collect();
        let heights: Vec<u16> = (1..=24).chain([30, 36, 42]).collect();
        let mut failures = Vec::new();
        for (name, mut app) in scenarios() {
            for &w in &widths {
                for &h in &heights {
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
                        t.draw(|f| draw(f, &mut app)).unwrap();
                    }));
                    if r.is_err() {
                        failures.push(format!("{name} at {w}x{h}"));
                    }
                }
            }
        }
        assert!(failures.is_empty(), "panicked at: {:?} (+{} more)", &failures[..failures.len().min(8)], failures.len().saturating_sub(8));
    }

    #[test]
    fn layout_code_is_safe_below_the_minimum_size_even_without_the_guard() {
        // The too-small screen *hides* these functions' behaviour at small sizes, so test
        // them directly: the reported crash was `clamp(4, 2)` in draw_main on a 12-row terminal.
        for h in 0..=24u16 {
            for w in [0u16, 1, 10, 30, 64, 100] {
                let mut app = test_app(ArtMode::HalfCell);
                app.zen = false;
                let th = app.theme();
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut t = Terminal::new(TestBackend::new(w.max(1), h.max(1))).unwrap();
                    t.draw(|f| {
                        let a = Rect::new(0, 0, w, h);
                        draw_main(f, &mut app, a, &th);
                        draw_zen(f, &mut app, a, &th);
                        draw_modal(f, &mut app, a, &th);
                    })
                    .unwrap();
                }));
                assert!(r.is_ok(), "draw_main/draw_zen/draw_modal panicked in a {w}x{h} area");
            }
        }
        assert_eq!(fit(4, 4, 2), 2, "an inverted range must not panic");
        assert_eq!(fit(0, 4, 100), 4);
        assert_eq!(fit(500, 4, 100), 100);
    }

    #[test]
    fn too_small_screen_reports_sizes_with_red_and_green() {
        use ratatui::style::Color;
        let colour_of = |t: &Terminal<TestBackend>, row: u16, needle: &str| -> Color {
            let text = row_text(t, row);
            let x = text.find(needle).unwrap_or_else(|| panic!("{needle:?} not on row {row}: {text}"));
            let col = text[..x].chars().count() as u16;
            t.backend().buffer().cell((col, row)).unwrap().fg
        };
        let mut app = test_app(ArtMode::HalfCell);
        app.zen = false;
        let th = app.theme();

        // both dimensions short: both red
        let t = render(&mut app, 50, 12);
        let row = find_row(&t, "Terminal size too small").expect("message");
        assert!(row_text(&t, row + 1).contains("Width = 50 Height = 12"), "{}", row_text(&t, row + 1));
        assert!(row_text(&t, row + 3).contains("Needed for current config"));
        assert!(row_text(&t, row + 4).contains(&format!("Width = {} Height = {}", MIN_SIZE.0, MIN_SIZE.1)));
        assert_eq!(colour_of(&t, row + 1, "50"), th.red);
        assert_eq!(colour_of(&t, row + 1, "12"), th.red);

        // wide enough, too short: width green, height red
        let t = render(&mut app, 80, 12);
        let row = find_row(&t, "Terminal size too small").unwrap();
        assert_eq!(colour_of(&t, row + 1, "80"), th.green);
        assert_eq!(colour_of(&t, row + 1, "12"), th.red);

        // tall enough, too narrow: the other way round
        let t = render(&mut app, 40, 30);
        let row = find_row(&t, "Terminal size too small").unwrap();
        assert_eq!(colour_of(&t, row + 1, "40"), th.red);
        assert_eq!(colour_of(&t, row + 1, "30"), th.green);

        // exactly the minimum is fine: the real UI, no message
        let t = render(&mut app, MIN_SIZE.0, MIN_SIZE.1);
        assert!(find_row(&t, "Terminal size too small").is_none());
        assert!(find_row(&t, "library").is_some());
        // one short in either direction is not
        assert!(find_row(&render(&mut app, MIN_SIZE.0 - 1, MIN_SIZE.1), "too small").is_some());
        assert!(find_row(&render(&mut app, MIN_SIZE.0, MIN_SIZE.1 - 1), "too small").is_some());
    }

    #[test]
    fn zen_has_its_own_smaller_minimum() {
        let mut app = test_app(ArtMode::HalfCell); // zen = true
        let t = render(&mut app, 40, MIN_SIZE_ZEN.1 - 1);
        let row = find_row(&t, "Terminal size too small").expect("zen message");
        assert!(row_text(&t, row + 4).contains(&format!("Width = {} Height = {}", MIN_SIZE_ZEN.0, MIN_SIZE_ZEN.1)));
        // a size that is too small for the normal layout but fine for zen shows zen
        let t = render(&mut app, 40, MIN_SIZE_ZEN.1);
        assert!(find_row(&t, "Terminal size too small").is_none());
        assert!(find_row(&t, "Zen Title").is_some());
    }

    #[test]
    fn too_small_screen_covers_modals_and_forgets_stale_hit_boxes() {
        let mut app = test_app(ArtMode::HalfCell);
        app.zen = false;
        app.modal = Some(Modal::Help { scroll: 0 });
        app.notify("a toast", Level::Info);
        let _ = render(&mut app, 150, 42);
        assert!(app.rects.tracks.width > 0 && !app.overlay_rects.is_empty());
        let t = render(&mut app, 40, 10);
        assert!(find_row(&t, "Terminal size too small").is_some());
        assert!(find_row(&t, "a toast").is_none() && find_row(&t, "keys").is_none(), "nothing else is drawn");
        assert_eq!(app.rects.tracks.width, 0, "a click can't land on a panel that isn't there");
        assert!(app.overlay_rects.is_empty() && app.sixel.show.is_none() && app.sixel.want.is_none());
        // …and the app is intact when the window grows back
        let t = render(&mut app, 150, 42);
        assert!(find_row(&t, "Terminal size too small").is_none());
        assert!(matches!(app.modal, Some(Modal::Help { .. })), "the open modal survived");
    }

    #[test]
    fn dividers_expose_grab_zones_and_dragging_resizes_panels() {
        use crate::dock::Divider;
        let mut app = test_app(ArtMode::HalfCell);
        app.zen = false;
        let _ = render(&mut app, 150, 42);
        let zones = app.rects.arranged.dividers.clone();
        assert_eq!(zones.len(), 3, "two column boundaries and one row boundary in the classic layout");
        let col0 = zones.iter().find(|(d, _)| *d == Divider::Col(0)).unwrap().1;
        assert_eq!(col0.x + 1, app.rects.tracks.x, "the zone sits on the library/tracks border");
        // dragging that boundary resizes the library panel
        let main = app.rects.main;
        app.dock.resize_col(0, 40, main);
        let t = render(&mut app, 150, 42);
        // the library panel is now 41 columns wide: its top-right corner is column 40
        assert!(row_text(&t, 1).chars().nth(40).map_or(false, |c| "╮┐".contains(c)), "{}", row_text(&t, 1));
    }

    /// Every on-screen panel's title, to see where things ended up.
    fn titles(t: &Terminal<TestBackend>) -> Vec<(String, u16)> {
        let row = row_text(t, 1);
        ["library", "tracks", "all tracks", "cover", "queue"]
            .iter()
            .filter_map(|n| row.find(&format!(" {n} ")).map(|x| (n.to_string(), row[..x].chars().count() as u16)))
            .collect()
    }

    #[test]
    fn moving_a_panel_rearranges_what_is_drawn_and_adjusts_the_too_small_limit() {
        let mut app = test_app(ArtMode::HalfCell);
        app.zen = false;
        let t = render(&mut app, 150, 42);
        let x = |t: &Terminal<TestBackend>, name: &str| titles(t).iter().find(|(n, _)| n == name).map(|(_, x)| *x).unwrap_or_else(|| panic!("no {name} in {:?}", titles(t)));
        assert!(x(&t, "library") < x(&t, "cover"), "classic: library left of cover");

        // pick the library up and drop it on the cover's right edge
        let cover = app.rects.arranged.rect(PanelId::Cover).unwrap();
        app.finish_move(Moving { panel: PanelId::Library, x: cover.x + cover.width - 1, y: cover.y + 3 });
        let t = render(&mut app, 150, 42);
        assert!(x(&t, "library") > x(&t, "cover"), "library now right of the cover: {:?}", titles(&t));
        assert_eq!(app.dock.columns.len(), 3);

        // swap two panels by dropping on the middle of one
        let tr = app.rects.arranged.rect(PanelId::Tracks).unwrap();
        app.finish_move(Moving { panel: PanelId::Library, x: tr.x + tr.width / 2, y: tr.y + tr.height / 2 });
        let t = render(&mut app, 150, 42);
        // before the swap the order was tracks | cover | library; the library and tracks exchange
        // places, so it becomes library | cover | tracks
        assert!(x(&t, "library") < x(&t, "cover") && x(&t, "cover") < x(&t, "tracks"), "{:?}", titles(&t));
        // dropping a panel on itself, or on nothing, changes nothing
        let before = app.dock.clone();
        app.finish_move(Moving { panel: PanelId::Cover, x: 0, y: 0 });
        app.finish_move(Moving { panel: PanelId::Cover, x: 999, y: 999 });
        assert_eq!(app.dock, before);
    }

    #[test]
    fn a_layout_wider_than_the_terminal_is_refused_not_applied() {
        // the "too small" screen hides every menu, so a layout that doesn't fit must never be applied
        let mut app = test_app(ArtMode::HalfCell);
        app.zen = false;
        let _ = render(&mut app, 70, 30); // 70 cols: the 4-column preset needs 84
        let four_wide = crate::dock::Dock {
            columns: PanelId::ALL
                .iter()
                .map(|p| crate::dock::Column { panels: vec![*p], weights: vec![100], width: p.default_width() })
                .collect(),
        };
        let before = app.dock.clone();
        assert!(!app.try_dock(four_wide.clone(), "that layout"));
        assert_eq!(app.dock, before);
        assert!(app.toasts.iter().any(|t| t.text.contains("needs at least 84")), "{:?}", app.toasts.iter().map(|t| &t.text).collect::<Vec<_>>());
        // …but it is fine on a terminal that is wide enough
        let _ = render(&mut app, 120, 30);
        assert!(app.try_dock(four_wide, "that layout"));
        assert_eq!(app.dock.columns.len(), 4);
    }

    #[test]
    fn the_too_small_limit_follows_the_layout() {
        let mut app = test_app(ArtMode::HalfCell);
        app.zen = false;
        assert_eq!(min_size(&app), MIN_SIZE);
        let tall = crate::dock::Dock::presets()[2].1.clone(); // stacked left: 2 columns, 13 rows of panels
        app.dock = tall.clone();
        assert_eq!(min_size(&app), (24 + 24, 13 + CHROME_ROWS));
        // a custom layout that doesn't fit says how to undo it
        let t = render(&mut app, 40, 12);
        assert!(find_row(&t, "custom layout").is_some() && find_row(&t, "ctrl+e").is_some());
        app.dock = crate::dock::Dock::classic();
        let t = render(&mut app, 40, 12);
        assert!(find_row(&t, "custom layout").is_none(), "the hint is only for custom layouts");
    }

    #[test]
    fn arrange_mode_swaps_docks_cycles_resets_and_never_traps_the_quit_key() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let key = |c: KeyCode, m: KeyModifiers| KeyEvent::new(c, m);
        let mut app = test_app(ArtMode::HalfCell);
        app.zen = false;
        let _ = render(&mut app, 150, 42);
        app.focus = Focus::Sidebar;
        app.enter_arrange();
        assert_eq!(app.arrange, Some(PanelId::Library));
        // →: swap the library with its right-hand neighbour (the tracks)
        app.on_key(key(KeyCode::Right, KeyModifiers::NONE));
        let _ = render(&mut app, 150, 42);
        let lib = app.rects.arranged.rect(PanelId::Library).unwrap();
        let trk = app.rects.arranged.rect(PanelId::Tracks).unwrap();
        assert!(trk.x < lib.x, "tracks is now left of the library");
        // tab moves the selection; keys that mean nothing here are swallowed, not acted on
        app.on_key(key(KeyCode::Tab, KeyModifiers::NONE));
        assert_ne!(app.arrange, Some(PanelId::Library));
        let playing_before = app.player.paused();
        app.on_key(key(KeyCode::Char(' '), KeyModifiers::NONE));
        assert_eq!(app.player.paused(), playing_before, "space is consumed in arrange mode");
        // shift+arrow docks beside the neighbour rather than swapping
        app.arrange = Some(PanelId::Queue);
        let _ = render(&mut app, 150, 42);
        app.on_key(key(KeyCode::Up, KeyModifiers::SHIFT));
        let _ = render(&mut app, 150, 42);
        assert_eq!(app.dock.find(PanelId::Queue).map(|(_, i)| i), Some(0), "queue docked above the cover");
        // r resets
        app.on_key(key(KeyCode::Char('r'), KeyModifiers::NONE));
        assert_eq!(app.dock, crate::dock::Dock::classic());
        // quit always works, even here
        app.on_key(key(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(app.quit, "the quit key must not be trapped by arrange mode");
        // esc leaves the mode
        let mut app = test_app(ArtMode::HalfCell);
        app.zen = false;
        let _ = render(&mut app, 150, 42);
        app.enter_arrange();
        app.on_key(key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.arrange, None);
    }

    #[test]
    fn arrange_highlight_recolours_the_border_and_keeps_the_title() {
        let mut app = test_app(ArtMode::HalfCell);
        app.zen = false;
        let th = app.theme();
        let t = render(&mut app, 150, 42);
        let tr = app.rects.arranged.rect(PanelId::Tracks).unwrap();
        let title_before = row_text(&t, tr.y);
        let plain = t.backend().buffer().cell((tr.x, tr.y)).unwrap().fg;
        app.arrange = Some(PanelId::Tracks);
        let t = render(&mut app, 150, 42);
        assert_eq!(row_text(&t, tr.y), title_before, "the panel keeps its title while selected");
        let cell = t.backend().buffer().cell((tr.x, tr.y)).unwrap();
        assert_eq!(cell.fg, th.mauve);
        assert_ne!(cell.fg, plain, "the border colour changed");
        // the far corner is highlighted too, so the whole outline reads as selected
        assert_eq!(t.backend().buffer().cell((tr.x + tr.width - 1, tr.y + tr.height - 1)).unwrap().fg, th.mauve);
    }

    #[test]
    fn move_preview_is_drawn_while_carrying_and_cancelled_by_escape() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app(ArtMode::HalfCell);
        app.zen = false;
        let _ = render(&mut app, 150, 42);
        let q = app.rects.arranged.rect(PanelId::Queue).unwrap();
        app.moving = Some(Moving { panel: PanelId::Library, x: q.x + 4, y: q.y + 2 });
        let t = render(&mut app, 150, 42);
        // the pointer is tagged with the carried panel's name
        assert!(row_text(&t, q.y + 3).contains(" library "), "{}", row_text(&t, q.y + 3));
        assert!(app.settling(), "the sixel image stays out of the way while carrying");
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.moving.is_none());
        let before = app.dock.clone();
        let _ = render(&mut app, 150, 42);
        assert_eq!(app.dock, before, "cancelling moved nothing");
    }

    #[test]
    fn saved_layouts_round_trip_through_state_and_appear_in_the_menu() {
        use crate::modal::{InputPurpose, LayoutChoice, PickKind};
        let mut app = test_app(ArtMode::HalfCell);
        app.zen = false;
        app.dock = crate::dock::Dock::presets()[1].1.clone();
        app.on_input(InputPurpose::SaveLayout, "my setup".into());
        app.dock = crate::dock::Dock::classic();
        app.layout_menu();
        let Some(Modal::Picker(p)) = &app.modal else { panic!("no picker") };
        let PickKind::Layout(choices) = &p.kind else { panic!("wrong picker") };
        // save-current + the 4 presets + the saved one
        assert_eq!(choices.len(), 1 + 4 + 1, "{:?}", p.items);
        assert!(p.items.iter().any(|i| i == "★ my setup"));
        let LayoutChoice::Apply(saved) = choices.last().unwrap() else { panic!() };
        assert_eq!(*saved, crate::dock::Dock::presets()[1].1);
    }

    /// the Sixel test app in the classic (non-zen) layout, with a cover that is ready to draw
    fn classic_sixel_app() -> App {
        let mut app = test_app(ArtMode::Sixel);
        app.zen = false;
        app
    }

    #[test]
    fn a_resize_shows_a_preview_where_it_used_to_show_nothing() {
        let mut app = classic_sixel_app();
        let _ = render(&mut app, 140, 40);
        assert!(app.sixel.want.is_some() && app.sixel.show.is_none(), "settled, nothing encoded yet: wants the real one, shows nothing");
        let settled = app.sixel.want.clone().unwrap().1;

        // the window is being resized: no quality encode is wanted, a preview is shown.
        // (the cover is width-limited in tall windows, so look for a height that changes it)
        app.resize_at = Some(std::time::Instant::now());
        let mut changed = None;
        for h in [28u16, 24, 20, 18, 16, 15, 14] {
            let _ = render(&mut app, 140, h);
            let plan = crate::sixel::plan(app.sixel.area_now.unwrap(), app.cell_px).unwrap();
            let shown = app.sixel.show.clone().expect("a picture follows the resize at every size");
            assert!(shown.preview && app.sixel.want.is_none(), "{h}: preview only, no quality encode requested");
            assert_eq!((shown.key, shown.rect), (plan.key, plan.rect), "{h}: sized for the panel as it is *now*");
            assert!(matches!(&app.sixel.preview, Some((c, k, e)) if c == "c1" && *k == plan.key && !e.data.is_empty()), "{h}");
            if plan.key != settled.key {
                changed = Some(h);
            }
        }
        assert!(changed.is_some(), "the panel never changed size, so this test proved nothing");

        // the same for a divider drag
        app.resize_at = None;
        app.drag = Some(crate::dock::Divider::Col(0));
        let _ = render(&mut app, 100, 30);
        assert!(app.sixel.show.as_ref().is_some_and(|s| s.preview), "dragging a divider keeps the cover up");
    }

    #[test]
    fn the_preview_stays_up_until_the_real_encode_arrives() {
        let mut app = classic_sixel_app();
        app.resize_at = Some(std::time::Instant::now());
        let _ = render(&mut app, 130, 38);
        let key = app.sixel.show.as_ref().unwrap().key;

        // settled: the quality encode is asked for, and nothing is withdrawn meanwhile
        app.resize_at = None;
        let _ = render(&mut app, 130, 38);
        assert!(matches!(&app.sixel.want, Some((_, p)) if p.key == key), "the real encode is requested");
        let shown = app.sixel.show.clone().expect("still showing the approximation");
        assert!(shown.preview && shown.key == key);

        // the encode lands: now the real picture is shown, in the same place
        let plan = app.sixel.want.as_ref().unwrap().1;
        let (_, art) = app.art.as_ref().unwrap();
        let enc = crate::sixel::encode_image(art.source(), &plan, app.cell_px);
        app.sixel.cache = Some(("c1".into(), plan.key, enc));
        let _ = render(&mut app, 130, 38);
        let real = app.sixel.show.clone().unwrap();
        assert!(!real.preview, "quality encode replaces the preview");
        assert_eq!((real.rect, real.key), (shown.rect, shown.key), "…without moving");
    }

    #[test]
    fn carrying_a_panel_still_hides_the_picture() {
        let mut app = classic_sixel_app();
        app.resize_at = Some(std::time::Instant::now());
        app.moving = Some(crate::app::Moving { panel: crate::dock::PanelId::Cover, x: 5, y: 5 });
        let _ = render(&mut app, 140, 40);
        assert!(app.sixel.show.is_none() && app.sixel.want.is_none(), "the move overlay owns the gesture");
        assert!(!app.live_resize() && app.settling());
    }

    #[test]
    fn live_preview_is_safe_at_every_size() {
        // the preview is encoded inside `draw`, so it must never panic or overrun for any panel size
        let mut app = classic_sixel_app();
        app.drag = Some(crate::dock::Divider::Col(0));
        for w in (1..=90u16).step_by(3).chain([140, 200]) {
            for h in (1..=30u16).step_by(2).chain([50]) {
                let _ = render(&mut app, w, h);
                if let (Some(s), Some((_, _, e))) = (&app.sixel.show, &app.sixel.preview) {
                    assert!(e.width <= s.rect.width as u32 * app.cell_px.0 as u32 && e.height <= s.rect.height as u32 * app.cell_px.1 as u32, "{w}x{h}");
                }
            }
        }
    }

    #[test]
    fn a_drag_that_leaves_the_size_alone_keeps_the_quality_picture() {
        let mut app = classic_sixel_app();
        let _ = render(&mut app, 140, 40);
        let plan = app.sixel.want.as_ref().unwrap().1;
        let enc = crate::sixel::encode_image(app.art.as_ref().unwrap().1.source(), &plan, app.cell_px);
        app.sixel.cache = Some(("c1".into(), plan.key, enc));
        let _ = render(&mut app, 140, 40);
        assert!(app.sixel.show.as_ref().is_some_and(|s| !s.preview));
        app.drag = Some(crate::dock::Divider::Col(0)); // mouse pressed on a border, nothing moved yet
        let _ = render(&mut app, 140, 40);
        assert!(app.sixel.show.as_ref().is_some_and(|s| !s.preview), "no downgrade to the preview when the size is unchanged");
        assert!(app.sixel.preview.is_none(), "and no preview was even encoded");
    }

    // ── now-playing strip ─────────────────────────────────────────────

    fn mouse(kind: crossterm::event::MouseEventKind, x: u16, y: u16) -> crossterm::event::MouseEvent {
        crossterm::event::MouseEvent { kind, column: x, row: y, modifiers: crossterm::event::KeyModifiers::NONE }
    }
    fn classic(w: u16, h: u16) -> (App, Terminal<TestBackend>) {
        let mut app = test_app(ArtMode::HalfCell);
        app.zen = false;
        app.player.set_volume(100);
        let t = render(&mut app, w, h);
        (app, t)
    }

    #[test]
    fn speaker_icon_has_four_states() {
        assert_eq!(vol_icon(0, false), VOL_MUTE);
        assert_eq!(vol_icon(80, true), VOL_MUTE, "muted wins over the level");
        assert_eq!(vol_icon(1, false), VOL_LOW);
        assert_eq!(vol_icon(33, false), VOL_LOW);
        assert_eq!(vol_icon(34, false), VOL_MED);
        assert_eq!(vol_icon(66, false), VOL_MED);
        assert_eq!(vol_icon(67, false), VOL_HIGH);
        assert_eq!(vol_icon(130, false), VOL_HIGH);
        let all = [VOL_MUTE, VOL_LOW, VOL_MED, VOL_HIGH];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn now_playing_keeps_its_four_row_box_with_times_either_side_of_the_line() {
        let (h, w) = (30u16, 100u16);
        let (app, t) = classic(w, h);
        assert_eq!((NOW_ROWS, CHROME_ROWS), (4, 6), "the box keeps its original height");
        let top = find_row(&t, "now playing").expect("label");
        assert_eq!(top, h - 1 - NOW_ROWS, "box is the four rows above the footer");
        assert!(row_text(&t, top + 3).contains('╰') || row_text(&t, top + 3).contains('└'), "bottom border is still there");
        // what is inside the side borders
        let inside = |y: u16| -> String {
            let r: Vec<char> = row_text(&t, y).chars().collect();
            r[1..r.len() - 1].iter().collect()
        };
        let (title, line) = (inside(top + 1), inside(top + 2));
        // elapsed on the left, total on the right, one dot on the line between them
        assert!(line.trim_start().starts_with("0:10"), "{line:?}");
        assert!(line.trim_end().ends_with("0:00"), "{line:?}");
        assert_eq!(line.matches('●').count(), 1, "{line:?}");
        assert!(line.contains('━') || line.contains('─'));
        assert!(!line.contains('█'), "no blocky bar any more");
        // title row: the controls are right-aligned, in the order shuffle, repeat, speaker, slider, percent
        let (sh, rp, sp, pc) = (
            title.find(SHUFFLE).unwrap(),
            title.find(REPEAT).unwrap(),
            title.find(VOL_HIGH).unwrap(),
            title.find("100%").unwrap(),
        );
        assert!(sh < rp && rp < sp && sp < pc, "{title:?}");
        assert!(title.contains("Zen Title"));
        assert_eq!(title.trim_end().chars().count() as u16, w - 3, "percent ends one padding column in from the border");
        // hit-boxes agree with what was drawn
        let r = &app.rects;
        assert_eq!((r.bar.y, r.seek.y), (top + 2, top + 2));
        assert_eq!((r.gauge.y, r.vol.y), (top + 1, top + 1));
        assert!(r.bar.width > 20 && r.gauge.width >= 3);
        assert!(r.vol.x <= r.gauge.x && r.gauge.x + r.gauge.width <= r.vol.x + r.vol.width);
        assert!(r.shuffle.x + r.shuffle.width <= r.repeat.x && r.repeat.x + r.repeat.width <= r.vol.x);
    }

    #[test]
    fn dot_follows_the_position() {
        let mut app = test_app(ArtMode::HalfCell);
        app.zen = false;
        app.now_duration = 100.0;
        let mut last = None;
        for pos in [0.0, 25.0, 50.0, 100.0] {
            app.now_position = pos;
            let t = render(&mut app, 100, 30);
            let row = find_row(&t, "now playing").unwrap() + 2;
            let text: Vec<char> = row_text(&t, row).chars().collect();
            let bar = app.rects.bar;
            let col = text.iter().position(|c| *c == '●').unwrap() as u16;
            assert!(col >= bar.x && col < bar.x + bar.width);
            assert_eq!(col - bar.x, crate::anim::dot_index(pos / 100.0, bar.width as usize) as u16);
            if let Some(prev) = last {
                assert!(col > prev, "dot moves right as the track plays");
            }
            last = Some(col);
        }
    }

    #[test]
    fn volume_icon_and_percent_follow_the_level_and_mute() {
        let mut app = test_app(ArtMode::HalfCell);
        app.zen = false;
        for (v, icon) in [(100, VOL_HIGH), (50, VOL_MED), (10, VOL_LOW), (0, VOL_MUTE)] {
            app.player.set_volume(v);
            let t = render(&mut app, 100, 30);
            let row = row_text(&t, find_row(&t, "now playing").unwrap() + 1);
            assert!(row.contains(icon), "{v}: {row:?}");
            assert!(row.contains(&format!("{v}%")), "{v}: {row:?}");
        }
        app.player.set_volume(80);
        app.player.toggle_mute();
        let t = render(&mut app, 100, 30);
        let row = row_text(&t, find_row(&t, "now playing").unwrap() + 1);
        assert!(row.contains(VOL_MUTE) && row.contains("mute"), "{row:?}");
        let slider: String = row_text(&t, app.rects.gauge.y).chars().skip(app.rects.gauge.x as usize).take(app.rects.gauge.width as usize).collect();
        assert!(slider.starts_with('●'), "muted slider rests at zero: {slider:?}");
    }

    #[test]
    fn wheel_changes_volume_by_five_and_clicks_reach_both_ends() {
        use crossterm::event::{MouseButton, MouseEventKind as K};
        let (mut app, _t) = classic(100, 30);
        let v = app.rects.vol;
        app.player.set_volume(50);
        app.on_mouse(mouse(K::ScrollUp, v.x, v.y));
        assert_eq!(app.player.volume(), 55);
        app.on_mouse(mouse(K::ScrollUp, v.x + v.width - 1, v.y));
        assert_eq!(app.player.volume(), 60);
        app.on_mouse(mouse(K::ScrollDown, v.x + 1, v.y));
        app.on_mouse(mouse(K::ScrollDown, v.x + 1, v.y));
        assert_eq!(app.player.volume(), 50);
        // the percentage and the icon count as the control, not just the slider
        let g = app.rects.gauge;
        app.on_mouse(mouse(K::ScrollDown, g.x + g.width + 2, g.y));
        assert_eq!(app.player.volume(), 45);
        // clicking the slider's first/last cell gives 0 / 100, the middle gives about half
        app.on_mouse(mouse(K::Down(MouseButton::Left), g.x, g.y));
        assert_eq!(app.player.volume(), 0);
        app.on_mouse(mouse(K::Down(MouseButton::Left), g.x + g.width - 1, g.y));
        assert_eq!(app.player.volume(), 100);
        app.on_mouse(mouse(K::Down(MouseButton::Left), g.x + (g.width - 1) / 2, g.y));
        assert!((app.player.volume() - 50).abs() <= 6, "{}", app.player.volume());
        // clamped like the keyboard
        app.player.set_volume(98);
        app.on_mouse(mouse(K::ScrollUp, v.x, v.y));
        app.on_mouse(mouse(K::ScrollUp, v.x, v.y));
        assert_eq!(app.player.volume(), 108.min(130));
    }

    #[test]
    fn wheel_over_the_strip_does_not_scroll_the_lists() {
        use crossterm::event::MouseEventKind as K;
        let (mut app, _t) = classic(100, 30);
        let before = (app.sidebar.selected(), app.queue_state.selected());
        let (seek, vol) = (app.rects.seek, app.rects.vol);
        for _ in 0..5 {
            app.on_mouse(mouse(K::ScrollDown, seek.x + 3, seek.y));
            app.on_mouse(mouse(K::ScrollUp, vol.x, vol.y));
        }
        assert_eq!((app.sidebar.selected(), app.queue_state.selected()), before);
    }

    #[test]
    fn strip_is_sane_at_the_narrowest_sizes() {
        for (zen, w) in [(true, 30u16), (true, 31), (true, 40), (false, 62), (false, 63), (false, 200)] {
            let mut app = test_app(ArtMode::HalfCell);
            app.zen = zen;
            let h = min_size(&app).1 + 2;
            let t = render(&mut app, w, h);
            let top = find_row(&t, "now playing").unwrap_or_else(|| panic!("{w}: no strip"));
            let title = row_text(&t, top + 1);
            assert!(title.contains('%'), "{w}: {title:?}");
            let r = &app.rects;
            for z in [r.bar, r.seek, r.gauge, r.vol, r.shuffle, r.repeat] {
                assert!(z.x + z.width <= w - 1 && z.y < h, "{w}: {z:?} leaves the strip");
            }
            // the controls never overlap each other
            assert!(r.shuffle.x + r.shuffle.width <= r.repeat.x && r.repeat.x + r.repeat.width <= r.vol.x, "{w}");
        }
    }


    /// a classic (non-zen) screen with one playlist, a few tracks and a queue
    fn library_app() -> App {
        use crate::models::Playlist;
        let mut app = test_app(ArtMode::HalfCell);
        app.zen = false;
        app.playlists = vec![Playlist { id: "p".into(), name: "Jazz Mix".into(), song_count: 12, ..Default::default() }];
        app.rebuild_sidebar();
        let song = |i: usize| Song { id: format!("id{i}"), title: format!("Track{i}"), artist: "Band".into(), duration: 100, ..Default::default() };
        app.songs = (0..4).map(song).collect();
        app.tracks.select(Some(1));
        app.queue.set_songs((0..4).map(song).collect(), 1);
        app.queue_state.select(Some(2));
        app.focus = Focus::Tracks;
        app
    }

    /// (x, y) of the first cell of `needle` on screen
    fn find_cell(t: &Terminal<TestBackend>, needle: &str) -> (u16, u16) {
        let y = find_row(t, needle).unwrap_or_else(|| panic!("{needle:?} not on screen"));
        let row = row_text(t, y);
        let x = row.find(needle).map(|b| row[..b].chars().count()).unwrap();
        (x as u16, y)
    }

    #[test]
    fn selected_row_is_bold_and_accent_coloured_with_no_background_change() {
        use ratatui::style::Modifier;
        let mut app = library_app();
        let th = app.theme();
        let t = render(&mut app, 150, 42);
        let buf = t.backend().buffer();
        // the tracks cursor is on Track1; Track0/Track2 are plain rows right next to it
        let (x, y) = find_cell(&t, "Track1  Band");
        let sel = buf.cell((x, y)).unwrap();
        assert!(sel.modifier.contains(Modifier::BOLD), "selected text is bold");
        assert_eq!(sel.fg, th.blue, "selected text takes the theme colour");
        let (x0, y0) = find_cell(&t, "Track0  Band");
        let plain = buf.cell((x0, y0)).unwrap();
        assert!(!plain.modifier.contains(Modifier::BOLD) && plain.fg != th.blue);
        // the whole row, including its blank tail, keeps the panel background
        let panel = app.rects.tracks;
        for xx in panel.x + 1..panel.x + panel.width - 1 {
            assert_eq!(buf.cell((xx, y)).unwrap().bg, buf.cell((xx, y0)).unwrap().bg, "background differs under the cursor at x={xx}");
        }
        // an unfocused panel still marks its cursor, in bold, and still without a background
        app.focus = Focus::Sidebar;
        let t = render(&mut app, 150, 42);
        let buf = t.backend().buffer();
        let (x, y) = find_cell(&t, "Track1  Band");
        assert!(buf.cell((x, y)).unwrap().modifier.contains(Modifier::BOLD));
        assert_eq!(buf.cell((x, y)).unwrap().bg, buf.cell((x0, y0)).unwrap().bg);
    }

    #[test]
    fn queue_current_row_shows_the_same_play_pause_icon_as_the_now_playing_line() {
        {
            let mut app = library_app();
            app.focus = Focus::Queue;
            let t = render(&mut app, 150, 42);
            let q = app.rects.queue;
            let slice = |y: u16| -> String { row_text(&t, y).chars().skip(q.x as usize).take(q.width as usize).collect() };
            let row = (q.y..q.y + q.height).map(slice).find(|r| r.contains("Track1")).expect("current queue row is drawn");
            // nothing is playing in the test app, so both places show the paused icon
            let now = find_row(&t, "Zen Title").map(|y| row_text(&t, y)).expect("now-playing line");
            assert!(now.contains(PAUSE) && !now.contains(PLAY), "{now:?}");
            assert!(row.contains(PAUSE) && !row.contains(PLAY), "{row:?}");
            assert!(!row.contains(" 2 "), "the number is replaced by the icon: {row:?}");
        }
    }

    #[test]
    fn sidebar_icons_describe_their_category_and_follow_the_accent_colour() {
        let mut app = library_app();
        let th = app.theme();
        let t = render(&mut app, 150, 42);
        let buf = t.backend().buffer();
        for (label, glyph) in [
            ("all tracks", ALL_TRACKS),
            ("recently added", NEW_ICON),
            ("recently played", HISTORY),
            ("most played", MOST_PLAYED),
            ("Jazz Mix", PLAYLIST),
            ("starred", STAR),
        ] {
            // look only inside the library panel ("all tracks" is also the tracks panel's title)
            let lib = app.rects.sidebar;
            let (x, y) = (lib.y..lib.y + lib.height)
                .find_map(|y| {
                    let row: String = row_text(&t, y).chars().skip(lib.x as usize).take(lib.width as usize).collect();
                    row.find(label).map(|b| (lib.x + row[..b].chars().count() as u16, y))
                })
                .unwrap_or_else(|| panic!("{label} not in the library panel"));
            let row = row_text(&t, y);
            let g = row.chars().nth(x as usize - 2).unwrap();
            assert_eq!(g.to_string(), glyph, "{label}: wrong icon");
            assert_eq!(buf.cell((x - 2, y)).unwrap().fg, th.blue, "{label}: icon is not the accent colour");
        }
        // no stray count badge next to a playlist (it is a track count, not a play count)
        let (_, y) = find_cell(&t, "Jazz Mix");
        let lib = app.rects.sidebar;
        let row: String = row_text(&t, y).chars().skip(lib.x as usize).take(lib.width as usize).collect();
        assert!(!row.contains("12") && !row.contains('♪'), "{row:?}");
    }

    #[test]
    fn glyph_constants_are_the_requested_ones() {
        assert_eq!(PLAYLIST, "\u{f0cb8}");
        assert_eq!(DOWNLOAD, "\u{f019}");
        assert_eq!(ALL_TRACKS, "\u{f188e}");
        let all = [ALL_TRACKS, NEW_ICON, HISTORY, MOST_PLAYED, STAR, SHUFFLE, PLAYLIST];
        for (i, a) in all.iter().enumerate() {
            assert!(all.iter().skip(i + 1).all(|b| a != b), "two sidebar icons are the same: {a:?}");
        }
    }

    #[test]
    fn the_volume_slider_slides_under_a_held_button() {
        use crossterm::event::{MouseButton::Left, MouseEventKind as K};
        let (mut app, _t) = classic(100, 30);
        let g = app.rects.gauge;
        assert!(g.width >= 8);
        app.on_mouse(mouse(K::Down(Left), g.x, g.y));
        assert_eq!((app.player.volume(), app.scrub), (0, Some(Scrub::Volume)));
        let mut last = 0;
        for c in 0..g.width {
            app.on_mouse(mouse(K::Drag(Left), g.x + c, g.y));
            let want = (crate::anim::fraction_at(c as usize, g.width as usize) * 100.0).round() as i64;
            assert_eq!(app.player.volume(), want, "column {c}");
            assert!(app.player.volume() >= last);
            last = app.player.volume();
        }
        // the pointer may wander off the slider — up/down the screen and past either end
        app.on_mouse(mouse(K::Drag(Left), g.x + g.width + 20, 0));
        assert_eq!(app.player.volume(), 100);
        app.on_mouse(mouse(K::Drag(Left), 0, g.y + 3));
        assert_eq!(app.player.volume(), 0);
        app.on_mouse(mouse(K::Drag(Left), g.x + g.width / 2, g.y + 5));
        let mid = app.player.volume();
        assert!((40..=60).contains(&mid), "{mid}");
        // releasing ends it: more motion changes nothing
        app.on_mouse(mouse(K::Up(Left), g.x + g.width / 2, g.y));
        assert_eq!(app.scrub, None);
        app.on_mouse(mouse(K::Drag(Left), g.x, g.y));
        assert_eq!(app.player.volume(), mid);
    }

    #[test]
    fn a_drag_that_starts_elsewhere_never_touches_the_controls() {
        use crossterm::event::{MouseButton::Left, MouseEventKind as K};
        let (mut app, _t) = classic(100, 30);
        let (g, b) = (app.rects.gauge, app.rects.bar);
        app.player.set_volume(42);
        app.on_mouse(mouse(K::Down(Left), 3, 3)); // on a panel
        for x in [g.x, g.x + g.width - 1, b.x + 5] {
            app.on_mouse(mouse(K::Drag(Left), x, g.y));
        }
        assert_eq!((app.player.volume(), app.scrub), (42, None));
    }

    #[test]
    fn the_seek_line_follows_the_pointer_while_held_and_lets_go_on_release() {
        use crossterm::event::{MouseButton::Left, MouseEventKind as K};
        let mut app = test_app(ArtMode::HalfCell);
        app.zen = false;
        app.now_duration = 100.0;
        let mut t = render(&mut app, 100, 30);
        let b = app.rects.bar;
        let line_y = b.y;
        let read = |t: &Terminal<TestBackend>| {
            let row: Vec<char> = row_text(t, line_y).chars().collect();
            let dot = row.iter().position(|c| *c == '●').unwrap() as u16;
            let label: String = row.iter().skip(2).take(4).collect::<String>().trim().to_string();
            (dot, label)
        };
        assert_eq!(read(&t).1, "0:10");
        // press in the middle: the dot and the elapsed label jump there at once
        let mid = b.x + (b.width - 1) / 2;
        app.on_mouse(mouse(K::Down(Left), mid, line_y));
        assert_eq!(app.scrub, Some(Scrub::Seek));
        t = render(&mut app, 100, 30);
        let (dot, label) = read(&t);
        assert_eq!(dot, mid);
        assert!(label.starts_with("0:4") || label.starts_with("0:5"), "{label}");
        // drag to the far end — even with the pointer well below the line — then back past the start
        app.on_mouse(mouse(K::Drag(Left), b.x + b.width + 9, line_y + 8));
        t = render(&mut app, 100, 30);
        assert_eq!(read(&t), (b.x + b.width - 1, "1:40".to_string()));
        app.on_mouse(mouse(K::Drag(Left), 0, line_y));
        t = render(&mut app, 100, 30);
        assert_eq!(read(&t), (b.x, "0:00".to_string()));
        // let go: back to what the player says (the null player can't have moved)
        app.on_mouse(mouse(K::Up(Left), 0, line_y));
        assert_eq!(app.scrub, None);
        t = render(&mut app, 100, 30);
        assert_eq!(read(&t).1, "0:10");
    }

    #[test]
    fn reports_that_predate_our_own_seek_are_ignored_briefly() {
        let mut app = test_app(ArtMode::HalfCell);
        app.set_position(50.0);
        app.seek_hold = Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
        app.on_position(10.0, 100.0); // from before the seek, still in flight
        assert_eq!(app.now_position, 50.0);
        app.on_position(50.4, 100.0); // after it
        assert_eq!(app.now_position, 50.4);
        assert!(app.seek_hold.is_none(), "the first report that fits ends the hold");
        app.on_position(10.0, 100.0); // later jumps are real (a seek made elsewhere)
        assert_eq!(app.now_position, 10.0);
        // an expired hold lets everything through
        app.seek_hold = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        app.on_position(80.0, 100.0);
        assert_eq!(app.now_position, 80.0);
    }

    #[test]
    fn resetting_the_position_stops_extrapolating_until_a_real_report() {
        let mut app = test_app(ArtMode::HalfCell);
        app.on_position(30.0, 100.0);
        assert!(app.pos_report.is_some());
        app.set_position(0.0);
        assert_eq!((app.now_position, app.shown_pos), (0.0, 0.0));
        assert!(app.pos_report.is_none());
    }
}
