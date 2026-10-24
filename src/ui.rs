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
use crate::modal::*;
use crate::queue::Repeat;
use crate::stats;
use crate::theme::Theme;

// nerd-font glyphs (same set as the Python version)
const PLAY: &str = "\u{f04b}";
const PAUSE: &str = "\u{f04c}";
const SHUFFLE: &str = "\u{f074}";
const REPEAT: &str = "\u{f01e}";
const SPEED: &str = "\u{f0e4}";
const MOON: &str = "\u{f186}";
const STAR: &str = "\u{f005}";
const CHECK: &str = "\u{f00c}";
const CHECK_CIRCLE: &str = "\u{f058}";
const LIST_ICON: &str = "\u{f03a}";
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
        b = b.title_bottom(Line::from(Span::styled(format!(" {s} "), Style::default().fg(th.dim))).right_aligned());
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
    let hl = if focused { Style::default().bg(th.cursor) } else { Style::default().bg(th.vfaint) };
    f.render_stateful_widget(List::new(items).highlight_style(hl), area, &mut local);
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let th = app.theme();
    let area = f.area();
    app.sixel.want = None;
    app.sixel.show = None;
    app.sixel.area_now = None;
    app.overlay_rects.clear();
    f.render_widget(Block::default().style(th.base()), area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(3), Constraint::Length(4), Constraint::Length(1)])
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

fn draw_topbar(f: &mut Frame, app: &App, area: Rect, th: &Theme) {
    let mut left = vec![
        Span::raw(" "),
        Span::styled(format!("{} ", anim::note(app.logo_phase as usize)), Style::default().fg(th.mauve)),
    ];
    left.extend(anim::shimmer("Rustdrome", app.logo_phase, th.mauve, th.text, th));
    f.render_widget(Paragraph::new(Line::from(left)), area);

    let mut right: Vec<Span> = vec![];
    if app.private_mode {
        right.push(Span::styled("private  ", th.bold(th.mauve)));
    }
    if app.offline {
        right.push(Span::styled(format!("{PLUG} offline  "), th.fg(th.yellow)));
    }
    if app.quality_now.0 > 0 {
        right.push(Span::styled(format!("\u{f012} {}k  ", app.quality_now.0), th.fg(th.yellow)));
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
    let l = app.layout.clamped(area.width);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(l.sidebar), Constraint::Min(10), Constraint::Length(l.side)])
        .split(area);
    draw_sidebar(f, app, cols[0], th);
    draw_tracks(f, app, cols[1], th);
    // cover gets a share of the right column; the queue keeps at least 4 rows
    let cover_h = ((area.height as u32 * l.cover_pct as u32 / 100) as u16).clamp(4.min(area.height), area.height.saturating_sub(4));
    let side = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(cover_h), Constraint::Min(4)])
        .split(cols[2]);
    draw_art(f, app, side[0], th, true);
    draw_queue(f, app, side[1], th);

    // grab zones: the two border columns (or rows) either side of a boundary
    app.rects.main = area;
    app.rects.side_col = cols[2];
    app.rects.div_sidebar = Rect::new(cols[0].x + cols[0].width.saturating_sub(1), area.y, 2, area.height);
    app.rects.div_side = Rect::new(cols[1].x + cols[1].width.saturating_sub(1), area.y, 2, area.height);
    app.rects.div_cover = Rect::new(cols[2].x, side[0].y + side[0].height.saturating_sub(1), cols[2].width, 2);
    if let Some(d) = app.drag {
        let zone = match d {
            Divider::Sidebar => app.rects.div_sidebar,
            Divider::Side => app.rects.div_side,
            Divider::Cover => app.rects.div_cover,
        };
        for y in zone.y..zone.y + zone.height {
            for x in zone.x..zone.x + zone.width {
                if let Some(c) = f.buffer_mut().cell_mut((x, y)) {
                    c.set_fg(th.mauve);
                }
            }
        }
    }
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
                let (glyph, color) = match *id {
                    "shuffle-all" => (SHUFFLE, th.peach),
                    "starred" => (STAR, th.yellow),
                    _ => ("◍", th.mauve),
                };
                let name = if *id == cur_view { th.bold(th.text) } else { th.fg(th.text) };
                Line::from(vec![Span::styled(format!(" {glyph} "), th.fg(color)), Span::styled(label.to_string(), name)])
            }
            SideRow::Playlist(p) => Line::from(vec![
                Span::styled(format!(" {LIST_ICON} "), th.fg(th.lav)),
                Span::styled(p.name.clone(), th.fg(th.text)),
                Span::styled(format!(" {}♪", p.song_count), th.fg(th.vfaint)),
            ]),
            SideRow::NewPlaylist => {
                Line::from(vec![Span::styled(format!(" {PLUS} "), th.fg(th.green)), Span::styled("new playlist", th.fg(th.sub))])
            }
            SideRow::Podcast(c, n) => Line::from(vec![
                Span::styled(format!(" {PODCAST} "), th.fg(th.green)),
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
            spans.push(Span::styled(format!(" {CHECK_CIRCLE} "), th.fg(th.green)));
        } else {
            spans.push(Span::styled(
                format!(" {} ", if is_cur { "♪" } else { "·" }),
                th.fg(if is_cur { th.blue } else { th.vfaint }),
            ));
        }
        spans.push(Span::styled(s.title.clone(), if is_cur { th.bold(th.blue) } else { th.fg(th.text) }));
        if client.as_ref().is_some_and(|c| c.cached_stream(&s.id).is_some()) {
            spans.push(Span::styled(format!(" {CHECK}"), th.fg(th.green)));
        }
        if s.starred {
            spans.push(Span::styled(format!(" {STAR}"), th.fg(th.yellow)));
        }
        if (1..=5).contains(&s.user_rating) {
            spans.push(Span::styled(
                format!(" {}", char::from_u32(0x2460 + s.user_rating as u32 - 1).unwrap_or('•')),
                th.fg(th.peach),
            ));
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
        // while the layout is still changing (divider drag, zoom, window resize)
        // show nothing until it settles, then encode once at the right size
        if let (true, Some(cover), false) = (ready, cover, app.settling()) {
            if let Some(plan) = crate::sixel::plan(inner, app.cell_px) {
                app.sixel.want = Some((cover.clone(), plan));
                if let Some((c, k, _)) = &app.sixel.cache {
                    if *c == cover && *k == plan.key {
                        app.sixel.show = Some(crate::sixel::Shown {
                            rect: plan.rect,
                            cover,
                            key: plan.key,
                            theme: app.theme_name.clone(),
                            area: inner,
                        });
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
    let running = app.player.active() && !app.player.paused();
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
                Span::styled(format!("{} ", if running { PLAY } else { PAUSE }), th.fg(th.green)),
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
    let playing = app.player.active() && !app.player.paused();
    let border = if playing { th.border_alt } else { th.border };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(th.bt)
        .border_style(Style::default().fg(border))
        .title(Span::styled(" now playing ", th.fg(th.dim)));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 2 {
        return;
    }
    let width = inner.width as usize;

    // ── top line ──
    let mut top: Vec<Span> = anim::VizModel::render(&app.viz, th);
    top.push(Span::raw("  "));
    match &app.now_song {
        None => {
            top.push(Span::styled("nothing playing", th.fg(th.dim)));
            top.push(Span::styled("  ·  press ", th.fg(th.vfaint)));
            top.push(Span::styled("enter", th.fg(th.dim)));
            top.push(Span::styled(" on a track", th.fg(th.vfaint)));
        }
        Some(song) => {
            top.push(Span::styled(format!("{} ", if playing { PLAY } else { PAUSE }), th.fg(if playing { th.green } else { th.peach })));
            let mut badges: Vec<Span> = vec![];
            if (app.player.speed() - 1.0).abs() > 1e-3 {
                let mut c = th.mauve;
                if app.speed_flash > 0 && th.can_blend() {
                    c = crate::theme::blend(c, ratatui::style::Color::Rgb(255, 255, 255), app.speed_flash as f64 / 12.0);
                }
                badges.push(Span::styled(format!(" {SPEED}{}x", app.player.speed()), th.fg(c)));
            }
            if !app.sleep_label.is_empty() {
                badges.push(Span::styled(format!(" {MOON}{}", app.sleep_label), th.fg(th.lav)));
            }
            let badge_w: usize = badges.iter().map(|s| s.content.width()).sum();
            let star = if song.starred { format!(" {STAR}") } else { String::new() };
            let used: usize = top.iter().map(|s| s.content.width()).sum();
            let room = width.saturating_sub(used + star.width() + badge_w + 1).max(8);
            let body = format!("{}  —  {} · {}", song.title, song.artist, song.album);
            let flash = app.title_flash as f64 / 12.0;
            let tc = crate::theme::blend(th.text, ratatui::style::Color::Rgb(255, 255, 255), 0.7 * flash);
            top.push(Span::styled(anim::marquee(&body, room, app.tick / 2), th.bold(tc)));
            if !star.is_empty() {
                top.push(Span::styled(star, th.fg(th.yellow)));
            }
            top.extend(badges);
        }
    }
    f.render_widget(Paragraph::new(Line::from(top)), Rect::new(inner.x, inner.y, inner.width, 1));

    // ── bottom line ──
    let pos = app.now_position;
    let dur = if app.now_duration > 0.0 { app.now_duration } else { app.now_song.as_ref().map_or(0.0, |s| s.duration as f64) };
    let times = format!(" {} / {} ", anim::fmt_time(pos), anim::fmt_time(dur));
    let muted = app.player.muted();
    let vol = app.player.volume();
    let vol_frac = if muted { 0.0 } else { vol as f64 / 100.0 };

    let mut right: Vec<Span> = vec![Span::raw("  "), Span::styled("vol ", th.fg(th.vfaint))];
    let gauge_start = 2 + 4;
    let mut gauge = anim::mini_gauge(vol_frac, 6, th);
    if app.vol_flash > 0 && th.can_blend() {
        let c = crate::theme::blend(th.lav, ratatui::style::Color::Rgb(255, 255, 255), app.vol_flash as f64 / 14.0);
        for s in &mut gauge {
            s.style = s.style.fg(c);
        }
    }
    right.extend(gauge);
    let gauge_end = gauge_start + 6;
    right.push(Span::styled(
        format!(" {}", if muted { "mut".to_string() } else { format!("{vol:>3}") }),
        th.fg(if muted { th.red } else { th.dim }),
    ));
    right.push(Span::raw("  "));
    let shuf_start: usize = right.iter().map(|s| s.content.width()).sum();
    right.push(Span::styled(format!("{SHUFFLE} "), th.fg(if app.queue.shuffle { th.peach } else { th.vfaint })));
    let rep_start: usize = right.iter().map(|s| s.content.width()).sum();
    right.push(Span::styled(REPEAT, th.fg(if app.queue.repeat != Repeat::Off { th.peach } else { th.vfaint })));
    if app.queue.repeat == Repeat::One {
        right.push(Span::styled("¹", th.fg(th.peach)));
    }
    let rep_end: usize = right.iter().map(|s| s.content.width()).sum();
    // Nerd-font glyphs are width 1 to us but often draw wider; without a gap
    // after the last one it bleeds into the box's right border.
    right.push(Span::raw("  "));
    let right_w = rep_end + 2;

    let bar_w = width.saturating_sub(times.width() + right_w).max(4);
    let frac = if dur > 0.0 { pos / dur } else { 0.0 };
    let pulse = if playing { ((app.tick as f64 * 0.55).sin() + 1.0) / 2.0 } else { 0.0 };
    let mut line = anim::smooth_bar(frac, bar_w, pulse, th);
    line.push(Span::styled(times.clone(), th.fg(th.dim)));
    let base = bar_w + times.width();
    line.extend(right);
    let y = inner.y + 1;
    f.render_widget(Paragraph::new(Line::from(line)), Rect::new(inner.x, y, inner.width, 1));

    // hit-boxes for the mouse
    let x0 = inner.x;
    app.rects.bar = Rect::new(x0, y, bar_w as u16, 1);
    app.rects.gauge = Rect::new(x0 + (base + gauge_start) as u16, y, 6, 1);
    app.rects.shuffle = Rect::new(x0 + (base + shuf_start) as u16, y, 2, 1);
    app.rects.repeat = Rect::new(x0 + (base + rep_start) as u16, y, (rep_end - rep_start) as u16, 1);
    let _ = gauge_end;
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
            let mut head = vec![Span::raw(" "), Span::styled(format!("{} ", anim::note(logo_phase as usize)), th.fg(th.mauve))];
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
            let h = (s.rows.len() as u16 + 6).clamp(8, area.height.saturating_sub(4));
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
                        Line::from(vec![Span::styled(format!("  {USER} "), th.fg(th.peach)), Span::styled(x.name.clone(), th.fg(th.text)), Span::styled(format!("  {} albums", x.album_count), th.fg(th.dim))])
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
                    Span::styled(format!("\u{f06d} {} day streak", s.streak), th.fg(th.peach)),
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
            let h = (vis.len() as u16 + if p.query.is_some() { 5 } else { 4 }).clamp(6, area.height.saturating_sub(4));
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
    use crate::art::{Art, ArtMode};
    use crate::config::{AppDirs, Settings};
    use crate::models::Song;
    use image::{Rgb, RgbImage};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn test_app(mode: ArtMode) -> App {
        let tmp = std::env::temp_dir().join(format!("rustdrome-ui-{}", std::process::id()));
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
                    let (area_top, area_h) = (1u16, h - 6);
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
    fn repeat_glyph_keeps_a_gap_before_the_right_border() {
        let mut app = test_app(ArtMode::HalfCell);
        app.zen = false;
        app.queue.repeat = crate::queue::Repeat::One;
        for w in [100u16, 150] {
            let t = render(&mut app, w, 42);
            let y = find_row(&t, "vol").unwrap();
            let row: Vec<char> = row_text(&t, y).chars().collect();
            let border = row.iter().rposition(|c| *c == '│').unwrap();
            let last_glyph = row[..border].iter().rposition(|c| *c != ' ').unwrap();
            assert!(border - last_glyph >= 3, "{w} cols: only {} blank cells before the border", border - last_glyph - 1);
        }
    }

    #[test]
    fn dividers_expose_grab_zones_and_dragging_resizes_panels() {
        let mut app = test_app(ArtMode::HalfCell);
        app.zen = false;
        let _ = render(&mut app, 150, 42);
        let r = app.rects;
        assert!(r.div_sidebar.width > 0 && r.div_side.width > 0 && r.div_cover.height > 0);
        // the zones sit on the panel borders
        assert_eq!(r.div_sidebar.x + 1, r.rects_tracks_x());
        let before = app.layout;
        app.layout = before.dragged(Divider::Sidebar, 40, 10, r.main, r.side_col);
        let t = render(&mut app, 150, 42);
        // the library panel is now 41 columns wide: its top-right corner is column 40
        assert!(row_text(&t, 1).chars().nth(40).map_or(false, |c| "╮┐".contains(c)), "{}", row_text(&t, 1));
    }
}

impl Rects {
    #[cfg(test)]
    fn rects_tracks_x(&self) -> u16 {
        self.tracks.x
    }
}
