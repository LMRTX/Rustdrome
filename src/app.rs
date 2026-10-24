//! Application state and event dispatch.
//!
//! One `App` owns all UI state and is only ever touched from the main task.
//! Background work (network, downloads, art) runs in spawned tasks that report
//! back through `Msg`, so there is no shared mutable UI state and no locks on
//! the hot path.

use std::collections::HashSet;
use std::future::Future;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::widgets::ListState;
use tokio::sync::mpsc::UnboundedSender;

use crate::anim::VizModel;
use crate::api::SubsonicClient;
use crate::art::{Art, ArtMode};
use crate::sixel::SixelState;
use crate::config::{AppDirs, Credentials, Settings};
use crate::keys::{Action, Keymap};
use crate::modal::*;
use crate::models::*;
use crate::mutations::MutationQueue;
use crate::player::{Player, PlayerEvent};
use crate::queue::PlayQueue;
use crate::stats::StatsStore;
use crate::theme::{self, Theme};

// ── constants shared with the Python version ──────────────────────────
pub const VIEWS: [(&str, &str); 6] = [
    ("all-songs", "all tracks"),
    ("newest", "recently added"),
    ("recent", "recently played"),
    ("frequent", "most played"),
    ("starred", "starred"),
    ("shuffle-all", "shuffle everything"),
];
pub const SPEED_STEPS: [f64; 6] = [1.0, 1.25, 1.5, 1.75, 2.0, 0.75];
/// minutes; 0 = off; -1 = stop at the end of the current track
pub const SLEEP_PRESETS: [i64; 6] = [0, 15, 30, 45, 60, -1];
pub const QUALITY_PRESETS: [(&str, i64, &str); 4] =
    [("original", 0, ""), ("320 kbps", 320, "mp3"), ("192 kbps", 192, "mp3"), ("96 kbps", 96, "opus")];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Info,
    Warn,
    Error,
}

pub struct Toast {
    pub text: String,
    pub level: Level,
    pub until: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Sidebar,
    Tracks,
    Queue,
}

#[derive(Clone, Debug)]
pub enum SideRow {
    Header(&'static str),
    Spacer,
    View { id: &'static str, label: &'static str },
    Playlist(Playlist),
    NewPlaylist,
    Podcast(PodcastChannel, usize),
    Radio(usize),
}

impl SideRow {
    pub fn selectable(&self) -> bool {
        !matches!(self, SideRow::Header(_) | SideRow::Spacer)
    }
    /// The view id this row loads (`None` for non-loading rows).
    pub fn view_id(&self) -> Option<String> {
        match self {
            SideRow::View { id, .. } => Some((*id).to_string()),
            SideRow::Playlist(p) => Some(format!("pl:{}", p.id)),
            SideRow::Podcast(c, _) => Some(format!("podcast:{}", c.id)),
            SideRow::Radio(_) => Some("radio".to_string()),
            _ => None,
        }
    }
}

/// A queue edit to apply on track change (so a crossfade can run first).
pub type Pick = Box<dyn FnOnce(&mut PlayQueue) -> bool + Send>;

pub enum Msg {
    Toast(String, Level),
    Playlists(Vec<Playlist>),
    PodcastsRadio(Vec<(PodcastChannel, Vec<Song>)>, Vec<Song>),
    Songs { token: u64, view: String, title: String, songs: Vec<Song>, focus_tracks: bool },
    ConnTrouble(String),
    Search { seq: u64, res: SearchResults },
    Lyrics { title: String, synced: Option<Vec<(f64, String)>>, text: String },
    Genres(Vec<Genre>),
    Bookmarks(Vec<Bookmark>),
    Art { cover_id: String, path: std::path::PathBuf, small: image::RgbImage },
    Sixel { cover: String, key: crate::sixel::PlanKey, enc: Option<crate::sixel::Encoded> },
    Share(String),
    DlProgress { failed: bool },
    DlDone { label: String },
    RadioRefill { songs: Vec<Song>, autoplay: bool },
    Login(Result<Credentials, String>),
    AlbumSongs { songs: Vec<Song>, name: String },
    Reload,
    CrossfadeApply(Pick, i64),
    ZenLyrics { song_id: String, lines: Vec<(f64, String)> },
    /// a superseded load finished without producing rows
    Pinned,
}

/// A draggable boundary between panels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Divider {
    /// library | tracks
    Sidebar,
    /// tracks | cover+queue
    Side,
    /// cover / queue
    Cover,
}

/// User-adjustable panel sizes (remembered in `state.json`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PanelLayout {
    /// library column width, cells
    pub sidebar: u16,
    /// cover+queue column width, cells
    pub side: u16,
    /// share of the right column given to the cover, percent
    pub cover_pct: u16,
}

impl Default for PanelLayout {
    fn default() -> Self {
        Self { sidebar: 26, side: 36, cover_pct: 40 }
    }
}

impl PanelLayout {
    pub const MIN_SIDEBAR: u16 = 14;
    pub const MAX_SIDEBAR: u16 = 50;
    pub const MIN_SIDE: u16 = 24;
    pub const MAX_SIDE: u16 = 80;
    pub const MIN_TRACKS: u16 = 24;
    pub const MIN_COVER: u16 = 15;
    pub const MAX_COVER: u16 = 80;

    /// Keep every panel usable for a given total width: the tracks list always
    /// keeps `MIN_TRACKS`, taking from the right column first, then the left.
    pub fn clamped(self, total: u16) -> Self {
        let mut sidebar = self.sidebar.clamp(Self::MIN_SIDEBAR, Self::MAX_SIDEBAR);
        let mut side = self.side.clamp(Self::MIN_SIDE, Self::MAX_SIDE);
        let over = (sidebar + side + Self::MIN_TRACKS).saturating_sub(total);
        let take = over.min(side - Self::MIN_SIDE);
        side -= take;
        sidebar -= (over - take).min(sidebar - Self::MIN_SIDEBAR);
        Self { sidebar, side, cover_pct: self.cover_pct.clamp(Self::MIN_COVER, Self::MAX_COVER) }
    }

    pub fn to_json(self) -> serde_json::Value {
        serde_json::json!({ "sidebar": self.sidebar, "side": self.side, "cover_pct": self.cover_pct })
    }

    pub fn from_json(v: Option<&serde_json::Value>) -> Self {
        let d = Self::default();
        let get = |k: &str, def: u16| {
            v.and_then(|v| v.get(k)).and_then(|n| n.as_u64()).map_or(def, |n| n.min(u16::MAX as u64) as u16)
        };
        Self { sidebar: get("sidebar", d.sidebar), side: get("side", d.side), cover_pct: get("cover_pct", d.cover_pct) }
    }

    /// The layout after dragging `d` so its boundary sits at screen
    /// position `(x, y)`. `main` is the area the panels fill; `side_col` is the
    /// right-hand column (for the cover/queue split).
    pub fn dragged(self, d: Divider, x: u16, y: u16, main: Rect, side_col: Rect) -> Self {
        let mut l = self;
        match d {
            // the boundary column becomes the sidebar's last column
            Divider::Sidebar => l.sidebar = (x + 1).saturating_sub(main.x),
            // the boundary column becomes the right column's first column
            Divider::Side => l.side = (main.x + main.width).saturating_sub(x),
            Divider::Cover => {
                let h = (y + 1).saturating_sub(side_col.y) as u32;
                l.cover_pct = (h * 100 / side_col.height.max(1) as u32).min(100) as u16;
            }
        }
        l.clamped(main.width)
    }
}

/// Screen regions from the last draw, for mouse hit-testing.
#[derive(Default, Clone, Copy)]
pub struct Rects {
    pub sidebar: Rect,
    pub tracks: Rect,
    pub queue: Rect,
    pub bar: Rect,      // seek bar (1 row)
    pub gauge: Rect,    // volume gauge
    pub shuffle: Rect,
    pub repeat: Rect,
    /// grab zones for the draggable boundaries
    pub div_sidebar: Rect,
    pub div_side: Rect,
    pub div_cover: Rect,
    /// the area the three columns fill, and the right-hand column
    pub main: Rect,
    pub side_col: Rect,
}

pub struct App {
    pub dirs: AppDirs,
    pub settings: Settings,
    pub keymap: Keymap,
    pub client: Option<Arc<SubsonicClient>>,
    pub tx: UnboundedSender<Msg>,

    // look
    pub theme_name: String,
    /// square (plain) box corners instead of rounded
    pub square: bool,
    pub tint: Option<Color>,
    pub tick: usize,
    pub logo_phase: f64,
    pub viz: VizModel,
    pub title_flash: u32,
    pub vol_flash: u32,
    pub speed_flash: u32,
    pub toasts: Vec<Toast>,
    pub rects: Rects,
    pub layout: PanelLayout,
    pub drag: Option<Divider>,
    pub last_div_click: Option<(Instant, Divider)>,
    /// cell-size tracking (see `App::maintain_cell_size`)
    pub cell_probe_ok: bool,
    pub resize_at: Option<Instant>,
    pub win_sig: Option<(u16, u16, u16, u16)>,

    // engine
    pub player: Player,
    pub queue: PlayQueue,
    pub now_song: Option<Song>,
    pub now_position: f64,
    pub now_duration: f64,

    // library
    pub view: String,
    pub songs: Vec<Song>,
    pub tracks_title: String,
    pub playlists: Vec<Playlist>,
    pub podcasts: Vec<(PodcastChannel, Vec<Song>)>,
    pub stations: Vec<Song>,
    pub side_rows: Vec<SideRow>,
    pub sidebar: ListState,
    pub tracks: ListState,
    pub queue_state: ListState,
    pub focus: Focus,

    // modes
    pub filtering: bool,
    pub filter_query: String,
    /// indices into `songs` that match the filter
    pub filtered: Vec<usize>,
    pub select_mode: bool,
    pub selected: HashSet<String>,
    pub count: String,
    pub offline: bool,
    pub private_mode: bool,
    pub radio: bool,
    pub radio_filling: bool,
    pub zen: bool,
    pub quit: bool,
    pub notifications: bool,

    // playback bookkeeping
    pub scrobbled: bool,
    pub end_failures: u32,
    pub resume_position: f64,
    pub last_persist: f64,
    pub queue_scrolled_to: i64,
    pub sleep_idx: usize,
    pub sleep_deadline: Option<Instant>,
    pub sleep_label: String,
    pub fade_base: Option<i64>,
    pub prefetched: Option<String>,
    pub crossfade: f64,

    // background-work bookkeeping
    pub songs_token: Arc<AtomicU64>,
    pub loading: u32,
    pub dl_total: usize,
    pub dl_done: usize,
    pub dl_failed: usize,
    pub last_refresh: Instant,
    pub zen_lyrics: Option<(String, Vec<(f64, String)>)>,
    pub zen_lyrics_for: Option<String>,

    pub art: Option<(String, Art)>,
    pub art_path: Option<std::path::PathBuf>,
    pub art_mode: ArtMode,
    pub cell_px: (u16, u16),
    pub sixel: SixelState,
    /// screen regions covered by modals/toasts this frame (they would paint
    /// over a sixel image, which lives outside ratatui's cell buffer)
    pub overlay_rects: Vec<Rect>,
    pub modal: Option<Modal>,
    pub mutations: Arc<Mutex<MutationQueue>>,
    pub stats: StatsStore,
    pub last_click: Option<(Instant, Focus, usize)>,
    pub genre_names: Vec<String>,
    /// queue revision last written to disk
    pub persisted_rev: u64,
    pub quality_now: (i64, &'static str),
}

impl App {
    pub fn new(dirs: AppDirs, settings: Settings, tx: UnboundedSender<Msg>) -> Self {
        let state = dirs.load_state();
        let keymap = Keymap::from_settings(&settings);
        let mutations = Arc::new(Mutex::new(MutationQueue::load(&dirs)));
        let stats = StatsStore::new(&dirs.cache_dir);
        let theme_name = state.get("theme").and_then(|v| v.as_str()).unwrap_or("mocha").to_string();
        let mut queue = PlayQueue::new();
        let mut resume_position = 0.0;
        if let Some(snap) = dirs.read_cache::<crate::queue::QueueSnapshot>("queue") {
            resume_position = snap.position;
            queue = PlayQueue::from_snapshot(snap);
        }
        // the resume position is kept in the small state file between full
        // queue writes — trust it only if it belongs to the song we restored
        if let (Some(p), Some(id)) = (
            state.get("queue_position").and_then(|v| v.as_f64()),
            state.get("queue_position_song").and_then(|v| v.as_str()),
        ) {
            if queue.current().map(|s| s.id.as_str()) == Some(id) {
                resume_position = p;
            }
        }
        if let Some(true) = state.get("shuffle").and_then(|v| v.as_bool()) {
            queue.shuffle = true;
        }
        let now_song = queue.current().cloned();
        let now_duration = now_song.as_ref().map_or(0.0, |s| s.duration as f64);
        let view = state
            .get("view")
            .and_then(|v| v.as_str())
            .filter(|v| {
                VIEWS.iter().any(|(id, _)| id == v) || v.starts_with("pl:") || v.starts_with("podcast:") || *v == "radio"
            })
            .unwrap_or("all-songs")
            .to_string();
        Self {
            keymap,
            client: None,
            tx,
            theme_name,
            square: match state.get("border_style").and_then(|v| v.as_str()) {
                Some(s) => s == "square",
                None => settings.border_style == "square",
            },
            tint: None,
            tick: 0,
            logo_phase: 0.0,
            viz: VizModel::new(5, 7),
            title_flash: 0,
            vol_flash: 0,
            speed_flash: 0,
            toasts: vec![],
            rects: Rects::default(),
            layout: PanelLayout::from_json(state.get("ui_layout")),
            drag: None,
            last_div_click: None,
            cell_probe_ok: false,
            resize_at: None,
            win_sig: None,
            player: Player::null(),
            queue,
            now_song,
            now_position: resume_position,
            now_duration,
            view,
            songs: vec![],
            tracks_title: "tracks".into(),
            playlists: vec![],
            podcasts: vec![],
            stations: vec![],
            side_rows: vec![],
            sidebar: ListState::default(),
            tracks: ListState::default(),
            queue_state: ListState::default(),
            focus: Focus::Sidebar,
            filtering: false,
            filter_query: String::new(),
            filtered: vec![],
            select_mode: false,
            selected: HashSet::new(),
            count: String::new(),
            offline: false,
            private_mode: false,
            radio: state.get("radio").and_then(|v| v.as_bool()).unwrap_or(false),
            radio_filling: false,
            zen: false,
            quit: false,
            notifications: settings.notifications,
            scrobbled: false,
            end_failures: 0,
            resume_position,
            last_persist: 0.0,
            queue_scrolled_to: -2,
            sleep_idx: 0,
            sleep_deadline: None,
            sleep_label: String::new(),
            fade_base: None,
            prefetched: None,
            crossfade: settings.crossfade,
            songs_token: Arc::new(AtomicU64::new(0)),
            loading: 0,
            dl_total: 0,
            dl_done: 0,
            dl_failed: 0,
            last_refresh: Instant::now(),
            zen_lyrics: None,
            zen_lyrics_for: None,
            art: None,
            art_path: None,
            art_mode: ArtMode::HalfCell,
            cell_px: (10, 20),
            sixel: SixelState::default(),
            overlay_rects: Vec::new(),
            modal: None,
            mutations,
            stats,
            last_click: None,
            genre_names: vec![],
            persisted_rev: 0,
            quality_now: (0, ""),
            settings,
            dirs,
        }
    }

    pub fn theme(&self) -> Theme {
        theme::by_name(&self.theme_name).tinted(self.tint).with_square(self.square)
    }

    pub fn spawn<F: Future<Output = ()> + Send + 'static>(&self, f: F) {
        tokio::spawn(f);
    }

    pub fn send(tx: &UnboundedSender<Msg>, m: Msg) {
        let _ = tx.send(m);
    }

    pub fn notify(&mut self, text: impl Into<String>, level: Level) {
        let secs = if level == Level::Info { 3 } else { 5 };
        self.toasts.push(Toast { text: text.into(), level, until: Instant::now() + Duration::from_secs(secs) });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.notify(text, Level::Info)
    }

    // ── startup ───────────────────────────────────────────────────────
    pub fn start(&mut self) {
        Settings::write_template(self.dirs.config_dir());
        if self.player.is_null() {
            self.notify(crate::player::INSTALL_HINT, Level::Warn);
        }
        self.player.set_volume(self.dirs.load_state().get("volume").and_then(|v| v.as_i64()).unwrap_or(80));
        let speed = self.dirs.load_state().get("speed").and_then(|v| v.as_f64()).unwrap_or(1.0);
        self.player.set_speed(speed);
        match self.dirs.load_credentials() {
            Some(c) => self.connect(c),
            None => {
                let (s, u) = self.dirs.load_login_hint();
                self.modal = Some(Modal::Login(Login::new(&s, &u)));
            }
        }
    }

    pub fn connect(&mut self, c: Credentials) {
        self.client = Some(Arc::new(SubsonicClient::new(
            &c.server,
            &c.username,
            &c.token,
            &c.salt,
            self.dirs.art_dir(),
            self.dirs.audio_dir(),
            self.settings.max_bitrate,
            &self.settings.stream_format,
        )));
        if let Some(c) = self.dirs.read_cache::<Vec<Playlist>>("playlists-v2") {
            self.playlists = c;
        } else if let Some(v) = self.dirs.read_cache::<serde_json::Value>("playlists") {
            // the Python cache nests them under "playlists"
            self.playlists = v
                .get("playlists")
                .and_then(|p| serde_json::from_value(p.clone()).ok())
                .unwrap_or_default();
        }
        self.restore_podcasts_radio();
        self.rebuild_sidebar();
        self.focus = Focus::Sidebar;
        self.highlight_view(&self.view.clone());
        self.load_view(&self.view.clone());
        self.load_playlists();
        self.load_podcasts_radio();
        self.flush_mutations();
        if let Some(song) = self.queue.current().cloned() {
            if let Some(cover) = &song.cover_art {
                self.load_art(cover.clone());
            }
        }
        self.render_queue_cursor();
    }

    // ── player events ─────────────────────────────────────────────────
    pub fn on_player_event(&mut self, ev: PlayerEvent) {
        match ev {
            PlayerEvent::Position(p, d) => self.on_position(p, d),
            PlayerEvent::TrackEnd { failed } => self.on_track_end(failed),
        }
    }

    // ── background messages ───────────────────────────────────────────
    pub fn on_msg(&mut self, m: Msg) {
        match m {
            Msg::Toast(t, l) => self.notify(t, l),
            Msg::Playlists(p) => {
                self.playlists = p;
                self.rebuild_sidebar();
            }
            Msg::PodcastsRadio(pods, stations) => {
                self.podcasts = pods;
                self.stations = stations;
                self.rebuild_sidebar();
            }
            Msg::Songs { token, view, title, songs, focus_tracks } => {
                use std::sync::atomic::Ordering::SeqCst;
                self.loading = self.loading.saturating_sub(1);
                // a newer load (or a different view) superseded this one
                if token != self.songs_token.load(SeqCst) || self.view != view {
                    return;
                }
                self.show_songs(songs, &title);
                if focus_tracks {
                    self.focus = Focus::Tracks;
                }
                crate::mem::trim(); // the parse/clone burst is over; hand it back
            }
            Msg::ConnTrouble(e) => {
                self.loading = self.loading.saturating_sub(1);
                self.notify(format!("connection trouble: {e}"), Level::Warn);
            }
            Msg::Search { seq, res } => {
                if let Some(Modal::Search(s)) = &mut self.modal {
                    if s.seq == seq {
                        s.set_results(res);
                    }
                }
            }
            Msg::Lyrics { title, synced, text } => {
                self.modal = Some(Modal::Lyrics(LyricsModal { title, synced, text, scroll: 0 }));
            }
            Msg::Genres(g) => {
                let items = g.iter().map(|g| format!("{}  {}♪", g.name, g.song_count)).collect();
                let names: Vec<String> = g.into_iter().map(|g| g.name).collect();
                self.genre_names = names;
                self.modal = Some(Modal::Picker(Picker::new("browse by genre", items, PickKind::Genre, true)));
            }
            Msg::Bookmarks(b) => {
                if b.is_empty() {
                    self.info("no bookmarks yet — press w while playing");
                } else {
                    let items = b
                        .iter()
                        .map(|m| {
                            format!("{}  @ {}", m.song.title, crate::anim::fmt_time(m.position_ms as f64 / 1000.0))
                        })
                        .collect();
                    self.modal = Some(Modal::Picker(Picker::new("bookmarks", items, PickKind::Bookmark(b), false)));
                }
            }
            Msg::Art { cover_id, path, small } => {
                if self.now_song.as_ref().and_then(|s| s.cover_art.as_deref()) == Some(cover_id.as_str()) {
                    let art = Art::new(small);
                    self.tint = self.settings.art_theming.then(|| art.tint()).flatten();
                    self.art = Some((cover_id, art));
                    self.art_path = Some(path);
                    crate::mem::trim();
                }
            }
            Msg::Sixel { cover, key, enc } => self.on_sixel(cover, key, enc),
            Msg::Share(url) => {
                crate::clipboard::copy(&url);
                self.info(format!("share link copied: {url}"));
            }
            Msg::DlProgress { failed } => {
                self.dl_done += 1;
                if failed {
                    self.dl_failed += 1;
                }
            }
            Msg::DlDone { label } => {
                let ok = self.dl_done.saturating_sub(self.dl_failed);
                if self.dl_failed > 0 {
                    self.notify(format!("downloaded {ok}/{} · {} failed", self.dl_total, self.dl_failed), Level::Warn);
                } else if ok == 1 {
                    self.info(format!("downloaded {label}"));
                } else {
                    self.info(format!("downloaded {ok} tracks"));
                }
                self.dl_total = 0;
                self.dl_done = 0;
                self.dl_failed = 0;
            }
            Msg::RadioRefill { songs, autoplay } => self.on_radio_refill(songs, autoplay),
            Msg::Login(r) => self.on_login_result(r),
            Msg::AlbumSongs { songs, name } => {
                self.queue.add(songs);
                self.persist_queue(None);
                self.info(format!("queued album: {name}"));
            }
            Msg::Reload => {
                self.load_playlists();
                let v = self.view.clone();
                self.load_view(&v);
            }
            Msg::CrossfadeApply(pick, base) => self.apply_crossfade(pick, base),
            Msg::ZenLyrics { song_id, lines } => {
                if self.now_song.as_ref().map(|s| &s.id) == Some(&song_id) {
                    self.zen_lyrics = Some((song_id, lines));
                }
            }
            Msg::Pinned => self.loading = self.loading.saturating_sub(1),
        }
    }

    fn on_login_result(&mut self, r: Result<Credentials, String>) {
        match r {
            Ok(c) => {
                if let Err(e) = self.dirs.save_credentials(&c) {
                    self.notify(format!("couldn't save credentials: {e}"), Level::Error);
                }
                self.modal = None;
                self.info("welcome to Rustdrome ♪");
                self.connect(c);
            }
            Err(e) => {
                if let Some(Modal::Login(l)) = &mut self.modal {
                    l.busy = false;
                    l.status = Some((e, true));
                }
            }
        }
    }

    // ── heartbeat ─────────────────────────────────────────────────────
    pub fn on_tick(&mut self) {
        self.tick += 1;
        self.logo_phase += 0.55;
        let playing = self.player.active() && !self.player.paused();
        self.viz.energy = if playing { 1.0 } else { 0.0 };
        let level = if playing { self.player.level() } else { None };
        self.viz.tick(level);
        self.title_flash = self.title_flash.saturating_sub(1);
        self.vol_flash = self.vol_flash.saturating_sub(1);
        self.speed_flash = self.speed_flash.saturating_sub(1);
        let now = Instant::now();
        self.toasts.retain(|t| t.until > now);
        if let Some(deadline) = self.sleep_deadline {
            if deadline <= now {
                self.sleep_fire();
            } else {
                self.sleep_label = crate::anim::fmt_time((deadline - now).as_secs_f64());
            }
        }
        if self.zen {
            self.refresh_zen_lyrics();
        }
        self.ensure_sixel();
        if self.tick % 480 == 0 {
            crate::mem::trim(); // every ~minute, in case something fragmented
        }
        if self.client.is_some()
            && self.modal.is_none()
            && self.last_refresh.elapsed() > Duration::from_secs(180)
            && self.dl_total == 0
        {
            self.last_refresh = Instant::now();
            self.maybe_auto_refresh();
        }
    }

    // ── input ─────────────────────────────────────────────────────────
    pub fn on_key(&mut self, k: KeyEvent) {
        if self.modal.is_some() {
            let outcome = self.modal.as_mut().map_or(Outcome::None, |m| m.key(&k));
            self.on_modal_outcome(outcome);
            return;
        }
        if k.kind == crossterm::event::KeyEventKind::Release {
            return;
        }
        // an in-progress filter owns printable keys
        if self.filtering && self.on_filter_key(&k) {
            return;
        }
        // select mode: esc exits; space toggles-and-advances over the tracks
        if self.select_mode {
            if k.code == KeyCode::Esc {
                self.exit_select_mode();
                return;
            }
            if self.focus == Focus::Tracks && k.code == KeyCode::Char(' ') {
                self.toggle_current_selection();
                return;
            }
        }
        if k.code == KeyCode::Esc {
            self.count.clear();
            return;
        }
        // vim repeat counts
        if self.handle_count(&k) {
            return;
        }
        // rating is fixed on the number row
        if let KeyCode::Char(c @ '1'..='5') = k.code {
            if k.modifiers.is_empty() {
                self.rate(c as i64 - '0' as i64);
                return;
            }
        }
        if let Some(action) = self.keymap.resolve(&k) {
            self.do_action(action);
            return;
        }
        // fixed list navigation / activation
        match k.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_cursor(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_cursor(-1),
            KeyCode::Char('g') | KeyCode::Home => self.move_cursor_to(0),
            KeyCode::Char('G') | KeyCode::End => self.move_cursor_to(usize::MAX),
            KeyCode::PageDown => self.move_cursor(10),
            KeyCode::PageUp => self.move_cursor(-10),
            KeyCode::Tab => self.focus_panel(1),
            KeyCode::BackTab => self.focus_panel(-1),
            KeyCode::Enter => self.activate(),
            _ => {}
        }
    }

    /// Digits arm a count for the *next* motion key; the digit itself still
    /// rates (1–5). Returns true when the key was consumed as a repeated
    /// motion.
    fn handle_count(&mut self, k: &KeyEvent) -> bool {
        let pending = std::mem::take(&mut self.count);
        let motion = match k.code {
            KeyCode::Char('j') | KeyCode::Down => Some(1i64),
            KeyCode::Char('k') | KeyCode::Up => Some(-1),
            _ => None,
        };
        if let (Some(dir), false) = (motion, pending.is_empty()) {
            let n = pending.parse::<i64>().unwrap_or(1).clamp(1, 500);
            self.move_cursor(dir * n);
            return true;
        }
        if let KeyCode::Char(c) = k.code {
            if c.is_ascii_digit() && k.modifiers.is_empty() && (c != '0' || !pending.is_empty()) {
                self.count = format!("{pending}{c}");
            }
        }
        false
    }

    fn on_filter_key(&mut self, k: &KeyEvent) -> bool {
        match k.code {
            KeyCode::Esc => {
                self.exit_filter();
                true
            }
            KeyCode::Enter | KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::BackTab | KeyCode::Home
            | KeyCode::End | KeyCode::PageUp | KeyCode::PageDown => false,
            KeyCode::Backspace => {
                self.filter_query.pop();
                self.apply_filter();
                true
            }
            KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter_query.push(c);
                self.apply_filter();
                true
            }
            _ => false,
        }
    }

    pub fn on_mouse(&mut self, m: MouseEvent) {
        if self.modal.is_some() {
            return;
        }
        let (x, y) = (m.column, m.row);
        let hit = |r: Rect| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height;
        match m.kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let d = if matches!(m.kind, MouseEventKind::ScrollDown) { 3 } else { -3 };
                for (rect, f) in [(self.rects.sidebar, Focus::Sidebar), (self.rects.tracks, Focus::Tracks), (self.rects.queue, Focus::Queue)] {
                    if hit(rect) {
                        let prev = self.focus;
                        self.focus = f;
                        self.move_cursor(d);
                        self.focus = prev;
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(d) = self.drag {
                    let r = self.rects;
                    self.layout = self.layout.dragged(d, x, y, r.main, r.side_col);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if self.drag.take().is_some() {
                    self.dirs.save_state(serde_json::json!({ "ui_layout": self.layout.to_json() }));
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let r = self.rects;
                let div = [(r.div_sidebar, Divider::Sidebar), (r.div_side, Divider::Side), (r.div_cover, Divider::Cover)]
                    .into_iter()
                    .find(|(z, _)| !self.zen && z.width > 0 && z.height > 0 && hit(*z))
                    .map(|(_, d)| d);
                if let Some(d) = div {
                    // double-click a boundary to put it back to its default
                    let double = self
                        .last_div_click
                        .is_some_and(|(t, pd)| pd == d && t.elapsed() < Duration::from_millis(400));
                    self.last_div_click = Some((Instant::now(), d));
                    if double {
                        let def = PanelLayout::default();
                        match d {
                            Divider::Sidebar => self.layout.sidebar = def.sidebar,
                            Divider::Side => self.layout.side = def.side,
                            Divider::Cover => self.layout.cover_pct = def.cover_pct,
                        }
                        self.dirs.save_state(serde_json::json!({ "ui_layout": self.layout.to_json() }));
                    } else {
                        self.drag = Some(d);
                    }
                    return;
                }
                if hit(r.bar) && r.bar.width > 0 {
                    self.seek_fraction((x - r.bar.x) as f64 / r.bar.width as f64 + 0.5 / r.bar.width as f64);
                } else if hit(r.gauge) && r.gauge.width > 0 {
                    self.set_volume_fraction((x - r.gauge.x) as f64 / r.gauge.width as f64 + 0.5 / r.gauge.width as f64);
                } else if hit(r.shuffle) {
                    self.do_action(Action::Shuffle);
                } else if hit(r.repeat) {
                    self.do_action(Action::Repeat);
                } else {
                    for (rect, f, state_off) in [
                        (r.sidebar, Focus::Sidebar, self.sidebar.offset()),
                        (r.tracks, Focus::Tracks, self.tracks.offset()),
                        (r.queue, Focus::Queue, self.queue_state.offset()),
                    ] {
                        if hit(rect) && y > rect.y && y < rect.y + rect.height - 1 {
                            let idx = state_off + (y - rect.y - 1) as usize;
                            self.click_row(f, idx);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Single click highlights (previews); a second click on the same row
    /// within 400ms selects (acts) — keyboard enter still acts instantly.
    fn click_row(&mut self, f: Focus, idx: usize) {
        let double = self
            .last_click
            .is_some_and(|(t, pf, pi)| pf == f && pi == idx && t.elapsed() < Duration::from_millis(400));
        self.focus = f;
        self.set_cursor(f, idx);
        self.last_click = Some((Instant::now(), f, idx));
        if double {
            self.activate();
        }
    }

    // ── modal outcomes ────────────────────────────────────────────────
    fn on_modal_outcome(&mut self, o: Outcome) {
        match o {
            Outcome::None => {}
            Outcome::Close => {
                // login can't be dismissed without quitting
                if matches!(self.modal, Some(Modal::Login(_))) {
                    self.quit = true;
                }
                self.modal = None;
            }
            Outcome::Login(server, user, pass) => self.try_login(server, user, pass),
            Outcome::Input(purpose, text) => {
                self.modal = None;
                self.on_input(purpose, text);
            }
            Outcome::SearchQuery(q) => self.run_search(q),
            Outcome::SearchSong { songs, index } => {
                self.modal = None;
                self.play_songs(songs, index);
            }
            Outcome::SearchQueue { song, next } => {
                let t = song.title.clone();
                if next {
                    self.queue.add_next(vec![song]);
                } else {
                    self.queue.add(vec![song]);
                }
                self.persist_queue(None);
                self.info(format!("queued {}{t}", if next { "next: " } else { "" }));
            }
            Outcome::SearchAlbum(a) => {
                self.modal = None;
                self.enqueue_album(a);
            }
            Outcome::SearchArtist(a) => {
                self.modal = None;
                self.load_artist_songs(a);
            }
            Outcome::Previewed(PickKind::Theme { .. }, i) => {
                self.theme_name = theme::THEMES[i].name.to_string();
            }
            Outcome::Previewed(..) => {}
            Outcome::Cancelled(kind) => {
                if let PickKind::Theme { original } = kind {
                    self.theme_name = original;
                }
                self.modal = None;
            }
            Outcome::Picked(kind, i) => {
                self.modal = None;
                self.on_picked(kind, i);
            }
        }
    }

    fn try_login(&mut self, server: String, user: String, pass: String) {
        if let Some(Modal::Login(l)) = &mut self.modal {
            l.busy = true;
            l.status = Some((format!("pinging {server} …"), false));
        }
        let tx = self.tx.clone();
        self.spawn(async move {
            let server = crate::api::normalize_server(&server);
            let (token, salt) = crate::api::make_token(&pass);
            let c = SubsonicClient::new(&server, &user, &token, &salt, "/tmp".into(), "/tmp".into(), 0, "");
            let r = match c.ping().await {
                Ok(_) => Ok(Credentials { server, username: user, token, salt }),
                Err(e) => Err(e.to_string()),
            };
            let _ = tx.send(Msg::Login(r));
        });
    }

    fn on_picked(&mut self, kind: PickKind, i: usize) {
        match kind {
            PickKind::Theme { .. } => {
                self.theme_name = theme::THEMES[i].name.to_string();
                self.dirs.save_state(serde_json::json!({ "theme": self.theme_name }));
                self.retint();
            }
            PickKind::Genre => {
                if let Some(g) = self.genre_names.get(i).cloned() {
                    self.load_genre_songs(g);
                }
            }
            PickKind::Bookmark(b) => {
                if let Some(m) = b.get(i) {
                    let (song, pos) = (m.song.clone(), m.position_ms as f64 / 1000.0);
                    self.queue.set_songs(vec![song], 0);
                    self.play_current(pos);
                }
            }
            PickKind::PlaylistAdd(songs) => {
                if i == 0 {
                    self.modal = Some(Modal::Input(InputModal {
                        title: "new playlist".into(),
                        placeholder: "name".into(),
                        field: TextField::default(),
                        purpose: InputPurpose::NewPlaylist(songs),
                    }));
                } else if let Some(p) = self.playlists.get(i - 1).cloned() {
                    self.playlist_append(p, songs);
                }
            }
            PickKind::Command(actions) => {
                if let Some(a) = actions.get(i).copied() {
                    self.do_action(a);
                }
            }
            PickKind::ConfirmDeletePlaylist(id) => self.playlist_delete(id),
        }
    }

    // ── cursor plumbing ───────────────────────────────────────────────
    pub fn list_len(&self, f: Focus) -> usize {
        match f {
            Focus::Sidebar => self.side_rows.len(),
            Focus::Tracks => self.view_len(),
            Focus::Queue => self.queue.songs.len(),
        }
    }

    fn state_mut(&mut self, f: Focus) -> &mut ListState {
        match f {
            Focus::Sidebar => &mut self.sidebar,
            Focus::Tracks => &mut self.tracks,
            Focus::Queue => &mut self.queue_state,
        }
    }

    pub fn cursor(&self, f: Focus) -> Option<usize> {
        match f {
            Focus::Sidebar => self.sidebar.selected(),
            Focus::Tracks => self.tracks.selected(),
            Focus::Queue => self.queue_state.selected(),
        }
    }

    pub fn set_cursor(&mut self, f: Focus, idx: usize) {
        let len = self.list_len(f);
        if len == 0 {
            self.state_mut(f).select(None);
            return;
        }
        let mut idx = idx.min(len - 1);
        if f == Focus::Sidebar {
            // land on the nearest selectable row
            if !self.side_rows[idx].selectable() {
                idx = (idx..len).chain((0..idx).rev()).find(|&i| self.side_rows[i].selectable()).unwrap_or(idx);
            }
        }
        self.state_mut(f).select(Some(idx));
        if f == Focus::Sidebar {
            if let Some(v) = self.side_rows[idx].view_id() {
                if v != self.view {
                    self.view = v.clone();
                    self.dirs.save_state(serde_json::json!({ "view": v }));
                    self.load_view(&v);
                }
            }
        }
    }

    pub fn move_cursor(&mut self, delta: i64) {
        let f = self.focus;
        let len = self.list_len(f);
        if len == 0 {
            return;
        }
        let cur = self.cursor(f).unwrap_or(0) as i64;
        let mut next = (cur + delta).clamp(0, len as i64 - 1) as usize;
        if f == Focus::Sidebar {
            // skip headers/spacers in the direction of travel
            let step = if delta >= 0 { 1i64 } else { -1 };
            let mut i = next as i64;
            while i >= 0 && (i as usize) < len && !self.side_rows[i as usize].selectable() {
                i += step;
            }
            if i < 0 || i as usize >= len {
                return;
            }
            next = i as usize;
        }
        self.set_cursor(f, next);
    }

    pub fn move_cursor_to(&mut self, idx: usize) {
        let f = self.focus;
        self.set_cursor(f, idx);
    }

    pub fn focus_panel(&mut self, dir: i64) {
        let order = [Focus::Sidebar, Focus::Tracks, Focus::Queue];
        let i = order.iter().position(|f| *f == self.focus).unwrap_or(0) as i64;
        self.focus = order[(i + dir).rem_euclid(3) as usize];
        if self.cursor(self.focus).is_none() {
            self.set_cursor(self.focus, 0);
        }
    }

    /// Enter on the focused list.
    pub fn activate(&mut self) {
        match self.focus {
            Focus::Sidebar => self.sidebar_selected(),
            Focus::Tracks => {
                if let Some(i) = self.cursor(Focus::Tracks).filter(|i| *i < self.view_len()) {
                    let view = self.view_songs();
                    self.play_songs(view, i);
                }
            }
            Focus::Queue => {
                if let Some(i) = self.cursor(Focus::Queue) {
                    self.change_track(Box::new(move |q| q.jump(i).is_some()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn main_area() -> (Rect, Rect) {
        let main = Rect::new(0, 1, 150, 36);
        (main, Rect::new(114, 1, 36, 36))
    }

    #[test]
    fn clamp_always_leaves_the_tracks_list_usable() {
        for total in [60u16, 80, 100, 150, 250] {
            for sidebar in [0u16, 14, 26, 50, 200] {
                for side in [0u16, 24, 36, 80, 200] {
                    let l = PanelLayout { sidebar, side, cover_pct: 40 }.clamped(total);
                    assert!((PanelLayout::MIN_SIDEBAR..=PanelLayout::MAX_SIDEBAR).contains(&l.sidebar));
                    assert!((PanelLayout::MIN_SIDE..=PanelLayout::MAX_SIDE).contains(&l.side));
                    // below the hard minimums nothing more can give; above them tracks must keep its room
                    if total >= PanelLayout::MIN_SIDEBAR + PanelLayout::MIN_SIDE + PanelLayout::MIN_TRACKS {
                        assert!(l.sidebar + l.side + PanelLayout::MIN_TRACKS <= total, "{total} {sidebar} {side} -> {l:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn dragging_each_divider_moves_exactly_that_panel() {
        let (main, side_col) = main_area();
        let d = PanelLayout::default();
        // library|tracks: the boundary column becomes the sidebar's last column
        let l = d.dragged(Divider::Sidebar, 39, 10, main, side_col);
        assert_eq!((l.sidebar, l.side, l.cover_pct), (40, d.side, d.cover_pct));
        // tracks|side: dragging left widens the right column
        let l = d.dragged(Divider::Side, 100, 10, main, side_col);
        assert_eq!((l.sidebar, l.side), (d.sidebar, 50));
        // cover/queue: a border half-way down the column → ~50%
        let l = d.dragged(Divider::Cover, 120, 1 + 18 - 1, main, side_col);
        assert_eq!(l.cover_pct, 50);
        assert_eq!((l.sidebar, l.side), (d.sidebar, d.side));
    }

    #[test]
    fn dragging_past_the_edges_is_clamped_not_broken() {
        let (main, side_col) = main_area();
        let d = PanelLayout::default();
        assert_eq!(d.dragged(Divider::Sidebar, 0, 5, main, side_col).sidebar, PanelLayout::MIN_SIDEBAR);
        assert_eq!(d.dragged(Divider::Sidebar, 149, 5, main, side_col).sidebar, PanelLayout::MAX_SIDEBAR);
        assert_eq!(d.dragged(Divider::Side, 0, 5, main, side_col).side, PanelLayout::MAX_SIDE.min(150 - d.sidebar - PanelLayout::MIN_TRACKS));
        assert_eq!(d.dragged(Divider::Side, 149, 5, main, side_col).side, PanelLayout::MIN_SIDE);
        assert_eq!(d.dragged(Divider::Cover, 120, 0, main, side_col).cover_pct, PanelLayout::MIN_COVER);
        assert_eq!(d.dragged(Divider::Cover, 120, 999, main, side_col).cover_pct, PanelLayout::MAX_COVER);
    }

    #[test]
    fn layout_persists_and_survives_garbage() {
        let l = PanelLayout { sidebar: 31, side: 44, cover_pct: 55 };
        assert_eq!(PanelLayout::from_json(Some(&l.to_json())), l);
        assert_eq!(PanelLayout::from_json(None), PanelLayout::default());
        let junk = serde_json::json!({ "sidebar": "wide", "side": -3, "cover_pct": 1e9 });
        let g = PanelLayout::from_json(Some(&junk)).clamped(150);
        assert!((PanelLayout::MIN_SIDEBAR..=PanelLayout::MAX_SIDEBAR).contains(&g.sidebar));
        assert!(g.cover_pct <= PanelLayout::MAX_COVER);
    }
}
