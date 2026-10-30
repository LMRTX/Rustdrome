//! Everything the app *does*: playback, queue edits, library loading,
//! playlists, downloads. Ported action-for-action from `app.py`.

use crossterm::event::KeyEvent;
use std::sync::atomic::Ordering::SeqCst;
use std::time::{Duration, Instant};

use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::app::*;
use crate::dock::{Dir, Dock, PanelId, Zone};
use crate::keys::Action;
use crate::modal::*;
use crate::models::*;
use crate::theme;

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct SongsCache {
    songs: Vec<Song>,
}

/// Same JSON shape as `SongsCache`, but borrowed — written without cloning.
#[derive(Serialize)]
struct SongsRef<'a> {
    songs: &'a [Song],
}

/// Every verb, searchable from the command palette.
pub const COMMANDS: &[(&str, Action)] = &[
    ("play / pause", Action::PlayPause),
    ("next track", Action::NextTrack),
    ("previous track", Action::PrevTrack),
    ("search", Action::Search),
    ("shuffle", Action::Shuffle),
    ("repeat", Action::Repeat),
    ("filter tracks", Action::Filter),
    ("select mode", Action::SelectMode),
    ("queue track", Action::Enqueue),
    ("play next", Action::PlayNext),
    ("remove from queue", Action::QueueRemove),
    ("clear queue", Action::QueueClear),
    ("save queue as playlist", Action::QueueSave),
    ("start radio", Action::StartRadio),
    ("toggle endless radio", Action::RadioToggle),
    ("star / unstar", Action::Star),
    ("add to playlist", Action::PlaylistAdd),
    ("remove from playlist", Action::PlaylistRemove),
    ("rename playlist", Action::PlaylistRename),
    ("delete playlist", Action::PlaylistDelete),
    ("lyrics", Action::Lyrics),
    ("copy share link", Action::Share),
    ("download track", Action::Download),
    ("download view", Action::DownloadView),
    ("download library", Action::DownloadAll),
    ("offline mode", Action::OfflineToggle),
    ("stream quality", Action::QualityCycle),
    ("playback speed", Action::Speed),
    ("sleep timer", Action::SleepTimer),
    ("go to album", Action::GoAlbum),
    ("go to artist", Action::GoArtist),
    ("browse genres", Action::Genres),
    ("bookmark position", Action::Bookmark),
    ("list bookmarks", Action::Bookmarks),
    ("listening stats", Action::Stats),
    ("notifications", Action::Notifications),
    ("private listening", Action::PrivateMode),
    ("refresh", Action::Refresh),
    ("cycle theme", Action::ThemeCycle),
    ("pick theme", Action::ThemePick),
    ("square / rounded boxes", Action::BorderStyle),
    ("layouts…", Action::LayoutMenu),
    ("arrange panels (keyboard)", Action::LayoutEdit),
    ("reset layout", Action::LayoutReset),
    ("zen splash", Action::Zen),
    ("help", Action::Help),
    ("quit", Action::Quit),
];

/// Greedy reorder so the same artist rarely plays back-to-back.
pub fn reorder_for_artist_diversity(songs: Vec<Song>, last_artist: &str) -> Vec<Song> {
    let mut remaining = songs;
    let mut out = Vec::with_capacity(remaining.len());
    let mut prev = last_artist.to_string();
    while !remaining.is_empty() {
        let i = remaining.iter().position(|s| s.artist != prev).unwrap_or(0);
        let pick = remaining.remove(i);
        prev = pick.artist.clone();
        out.push(pick);
    }
    out
}

impl App {
    // ── dispatch ──────────────────────────────────────────────────────
    pub fn do_action(&mut self, a: Action) {
        use Action::*;
        match a {
            PlayPause => self.action_play_pause(),
            NextTrack => self.change_track(Box::new(|q| q.advance(false).is_some())),
            PrevTrack => {
                // restart the track if it's past the first few seconds
                if self.player.active() && self.player.position() > 3.0 {
                    self.player.seek_to(0.0);
                } else {
                    self.change_track(Box::new(|q| q.prev().is_some()));
                }
            }
            Search => {
                if self.client.is_some() {
                    self.modal = Some(Modal::Search(SearchModal::default()));
                }
            }
            Shuffle => {
                let on = self.queue.toggle_shuffle();
                self.dirs.save_state(serde_json::json!({ "shuffle": on }));
                self.render_queue_cursor();
                self.persist_queue(None);
            }
            Repeat => {
                self.queue.cycle_repeat();
                self.persist_queue(None);
            }
            Filter => self.start_filter(),
            SeekBack => self.player.seek(-5.0),
            SeekForward => self.player.seek(5.0),
            SeekBackBig => self.player.seek(-30.0),
            SeekForwardBig => self.player.seek(30.0),
            VolumeDown => self.change_volume(-5),
            VolumeUp => self.change_volume(5),
            Mute => {
                self.player.toggle_mute();
                self.vol_flash = 10;
            }
            Speed => {
                let cur = self.player.speed();
                let i = SPEED_STEPS.iter().position(|s| (s - cur).abs() < 1e-6).map_or(0, |i| (i + 1) % SPEED_STEPS.len());
                let v = self.player.set_speed(SPEED_STEPS[i]);
                self.speed_flash = 10;
                self.dirs.save_state(serde_json::json!({ "speed": v }));
            }
            SleepTimer => self.cycle_sleep(),
            Enqueue => self.enqueue(false),
            PlayNext => self.enqueue(true),
            QueueRemove => {
                if self.focus == Focus::Queue {
                    if let Some(i) = self.cursor(Focus::Queue) {
                        let was_current = self.queue.current_index() == Some(i);
                        self.queue.remove(i);
                        if was_current {
                            if self.queue.current().is_some() {
                                self.play_current(0.0);
                            } else {
                                self.player.stop();
                                self.now_song = None;
                            }
                        }
                        self.render_queue_cursor();
                        self.persist_queue(None);
                    }
                }
            }
            QueueClear => {
                self.queue.clear();
                self.player.stop();
                self.now_song = None;
                self.set_position(0.0);
                self.now_duration = 0.0;
                self.queue_state.select(None);
                self.persist_queue(None);
                self.info("queue cleared");
            }
            QueueMoveUp => self.queue_move(-1),
            QueueMoveDown => self.queue_move(1),
            Star => self.action_star(),
            SelectMode => self.toggle_select_mode(),
            StartRadio => self.start_radio(),
            RadioToggle => {
                self.radio = !self.radio;
                self.dirs.save_state(serde_json::json!({ "radio": self.radio }));
                self.info(if self.radio { "endless radio on" } else { "endless radio off" });
            }
            Download => self.action_download(),
            DownloadView => {
                if self.songs.is_empty() {
                    self.info("nothing here to download");
                } else {
                    let (songs, label) = (self.songs.clone(), self.tracks_title.clone());
                    self.download_songs(songs, label);
                }
            }
            DownloadAll => {
                let songs = self
                    .dirs
                    .read_cache::<SongsCache>("all-songs")
                    .map(|c| c.songs)
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| self.songs.clone());
                if songs.is_empty() {
                    self.info("library not loaded yet — open 'all tracks' first");
                } else {
                    self.download_songs(songs, "library".into());
                }
            }
            OfflineToggle => {
                self.offline = !self.offline;
                self.info(if self.offline { "offline mode — playing only downloaded tracks" } else { "online mode" });
                if !self.offline {
                    self.flush_mutations();
                }
            }
            QualityCycle => {
                if let Some(c) = &self.client {
                    let cur = self.quality_now;
                    let i = QUALITY_PRESETS.iter().position(|(_, k, f)| (*k, *f) == cur).map_or(0, |i| (i + 1) % QUALITY_PRESETS.len());
                    let (label, kbps, fmt) = QUALITY_PRESETS[i];
                    c.set_quality(kbps, fmt);
                    self.quality_now = (kbps, fmt);
                    self.info(format!("stream quality: {label}"));
                }
            }
            PlaylistAdd => self.playlist_add_prompt(),
            PlaylistRemove => self.playlist_remove(),
            PlaylistMoveUp => self.playlist_move(-1),
            PlaylistMoveDown => self.playlist_move(1),
            PlaylistRename => {
                if let Some(p) = self.open_playlist() {
                    self.modal = Some(Modal::Input(InputModal {
                        title: format!("rename “{}”", p.name),
                        placeholder: "name".into(),
                        field: TextField::new(&p.name),
                        purpose: InputPurpose::RenamePlaylist(p.id),
                    }));
                } else {
                    self.info("open a playlist first");
                }
            }
            PlaylistDelete => {
                if let Some(p) = self.open_playlist() {
                    self.modal = Some(Modal::Picker(Picker::new(
                        &format!("delete “{}”?  (y / n)", p.name),
                        vec!["yes, delete it".into(), "cancel".into()],
                        PickKind::ConfirmDeletePlaylist(p.id),
                        false,
                    )));
                } else {
                    self.info("open a playlist first");
                }
            }
            QueueSave => {
                if self.queue.songs.is_empty() {
                    self.info("queue is empty");
                } else {
                    self.modal = Some(Modal::Input(InputModal {
                        title: "save queue as playlist".into(),
                        placeholder: "name".into(),
                        field: TextField::default(),
                        purpose: InputPurpose::SaveQueue,
                    }));
                }
            }
            Lyrics => self.action_lyrics(),
            Stats => {
                let summary = crate::stats::summarize(&self.stats.load(), crate::stats::now_ts(), 14);
                self.modal = Some(Modal::Stats { summary, scroll: 0 });
            }
            Share => self.action_share(),
            GoAlbum => self.go_album(),
            GoArtist => self.go_artist(),
            Genres => self.action_genres(),
            Bookmark => self.action_bookmark(),
            Bookmarks => {
                if let Some(c) = self.client.clone() {
                    let tx = self.tx.clone();
                    self.spawn(async move {
                        match c.get_bookmarks().await {
                            Ok(b) => App::send(&tx, Msg::Bookmarks(b)),
                            Err(e) => App::send(&tx, Msg::Toast(format!("bookmarks: {e}"), Level::Warn)),
                        }
                    });
                }
            }
            Notifications => {
                self.notifications = !self.notifications;
                self.info(if self.notifications { "notifications on" } else { "notifications off" });
            }
            PanelPrev => self.focus_panel(-1),
            PanelNext => self.focus_panel(1),
            Refresh => {
                self.info("refreshing…");
                self.last_refresh = Instant::now();
                self.maybe_auto_refresh();
            }
            ThemeCycle => {
                self.theme_name = theme::next_theme(&self.theme_name).name.to_string();
                self.dirs.save_state(serde_json::json!({ "theme": self.theme_name }));
                self.info(format!("theme: {}", self.theme_name));
            }
            ThemePick => {
                let items = theme::THEMES.iter().map(|t| t.name.to_string()).collect();
                let mut p = Picker::new("theme", items, PickKind::Theme { original: self.theme_name.clone() }, false);
                p.cursor = theme::THEMES.iter().position(|t| t.name == self.theme_name).unwrap_or(0);
                self.modal = Some(Modal::Picker(p));
            }
            BorderStyle => {
                self.square = !self.square;
                self.dirs.save_state(serde_json::json!({ "border_style": if self.square { "square" } else { "rounded" } }));
                self.info(if self.square { "square boxes" } else { "rounded boxes" });
            }
            LayoutMenu => self.layout_menu(),
            LayoutEdit => self.enter_arrange(),
            LayoutReset => {
                if self.try_dock(Dock::classic(), "the classic layout") {
                    self.info("layout reset");
                }
            }
            Zen => self.zen = !self.zen,
            PrivateMode => {
                self.private_mode = !self.private_mode;
                self.info(if self.private_mode { "private listening — no scrobbles" } else { "private listening off" });
            }
            CommandPalette => {
                let items = COMMANDS.iter().map(|(l, _)| l.to_string()).collect();
                let acts = COMMANDS.iter().map(|(_, a)| *a).collect();
                self.modal = Some(Modal::Picker(Picker::new("command palette", items, PickKind::Command(acts), true)));
            }
            Help => self.modal = Some(Modal::Help { scroll: 0 }),
            Quit => self.shutdown(),
        }
    }

    pub fn shutdown(&mut self) {
        self.persist_queue(None);
        self.dirs.save_state(serde_json::json!({ "volume": self.player.volume(), "speed": self.player.speed() }));
        self.quit = true;
    }

    // ── song access ───────────────────────────────────────────────────
    /// The tracks pane shows `songs`, or — while filtering — just the rows
    /// whose indices are in `filtered`. Storing indices (not clones) means a
    /// filter over a 5000-song library costs a few KB instead of a full copy.
    pub fn view_len(&self) -> usize {
        if self.filtering {
            self.filtered.len()
        } else {
            self.songs.len()
        }
    }

    pub fn view_song(&self, i: usize) -> Option<&Song> {
        if self.filtering {
            self.filtered.get(i).and_then(|&k| self.songs.get(k))
        } else {
            self.songs.get(i)
        }
    }

    /// An owned copy of what's on screen — only for handing to the queue.
    pub fn view_songs(&self) -> Vec<Song> {
        if self.filtering {
            self.filtered.iter().filter_map(|&k| self.songs.get(k)).cloned().collect()
        } else {
            self.songs.clone()
        }
    }

    pub fn highlighted_song(&self) -> Option<Song> {
        match self.focus {
            Focus::Queue => self.cursor(Focus::Queue).and_then(|i| self.queue.songs.get(i)).cloned(),
            _ => self.cursor(Focus::Tracks).and_then(|i| self.view_song(i)).cloned(),
        }
    }

    /// The song an action should apply to: the highlighted row, else the
    /// one that's playing.
    pub fn target_song(&self) -> Option<Song> {
        match self.focus {
            Focus::Tracks | Focus::Queue => self.highlighted_song().or_else(|| self.now_song.clone()),
            Focus::Sidebar => self.now_song.clone(),
        }
    }

    pub fn selected_songs(&self) -> Vec<Song> {
        if self.selected.is_empty() {
            return vec![];
        }
        (0..self.view_len()).filter_map(|i| self.view_song(i)).filter(|s| self.selected.contains(&s.id)).cloned().collect()
    }

    fn update_song_everywhere(&mut self, id: &str, f: impl Fn(&mut Song)) {
        for list in [&mut self.songs, &mut self.queue.songs] {
            for s in list.iter_mut().filter(|s| s.id == id) {
                f(s);
            }
        }
        if let Some(s) = self.now_song.as_mut().filter(|s| s.id == id) {
            f(s);
        }
    }

    // ── library views ─────────────────────────────────────────────────
    pub fn view_title(&self, view: &str) -> String {
        if let Some(pid) = view.strip_prefix("pl:") {
            return self.playlists.iter().find(|p| p.id == pid).map_or("playlist".into(), |p| p.name.clone());
        }
        if let Some(cid) = view.strip_prefix("podcast:") {
            return self
                .podcasts
                .iter()
                .find(|(c, _)| c.id == cid)
                .map_or("podcast".into(), |(c, _)| format!("podcast · {}", c.title));
        }
        if view == "radio" {
            return "internet radio".into();
        }
        VIEWS.iter().find(|(id, _)| *id == view).map_or("tracks".into(), |(_, l)| l.to_string())
    }

    pub fn show_songs(&mut self, songs: Vec<Song>, title: &str) {
        if self.filtering {
            self.exit_filter();
        }
        self.select_mode = false;
        self.selected.clear();
        let keep = self.tracks.selected().unwrap_or(0);
        self.songs = songs;
        self.tracks_title = title.to_string();
        if self.songs.is_empty() {
            self.tracks.select(None);
        } else {
            self.tracks.select(Some(keep.min(self.songs.len() - 1)));
        }
    }

    pub fn rebuild_sidebar(&mut self) {
        let mut rows = vec![SideRow::Header("tracks")];
        rows.extend(VIEWS.iter().map(|(id, label)| SideRow::View { id, label }));
        rows.push(SideRow::Spacer);
        rows.push(SideRow::Header("playlists"));
        rows.extend(self.playlists.iter().cloned().map(SideRow::Playlist));
        rows.push(SideRow::NewPlaylist);
        if !self.podcasts.is_empty() {
            rows.push(SideRow::Spacer);
            rows.push(SideRow::Header("podcasts"));
            rows.extend(self.podcasts.iter().map(|(c, e)| SideRow::Podcast(c.clone(), e.len())));
        }
        if !self.stations.is_empty() {
            rows.push(SideRow::Spacer);
            rows.push(SideRow::Header("internet radio"));
            rows.push(SideRow::Radio(self.stations.len()));
        }
        self.side_rows = rows;
        let view = self.view.clone();
        self.highlight_view(&view);
    }

    /// Move the sidebar highlight without triggering a load.
    pub fn highlight_view(&mut self, view: &str) {
        let idx = self.side_rows.iter().position(|r| r.view_id().as_deref() == Some(view));
        self.sidebar.select(idx.or_else(|| self.side_rows.iter().position(|r| r.selectable())));
    }

    pub fn sidebar_unhighlight(&mut self) {
        self.sidebar.select(None);
    }

    pub fn sidebar_selected(&mut self) {
        let Some(i) = self.cursor(Focus::Sidebar) else { return };
        match self.side_rows.get(i).cloned() {
            Some(SideRow::NewPlaylist) => {
                self.modal = Some(Modal::Input(InputModal {
                    title: "new playlist".into(),
                    placeholder: "name".into(),
                    field: TextField::default(),
                    purpose: InputPurpose::NewPlaylist(vec![]),
                }));
            }
            Some(SideRow::View { id: "shuffle-all", .. }) => self.shuffle_everything(),
            Some(_) => {
                if !self.songs.is_empty() {
                    // enter plays from the top — but with shuffle on, start
                    // somewhere random so it actually feels shuffled
                    let start = if self.queue.shuffle { rand::thread_rng().gen_range(0..self.songs.len()) } else { 0 };
                    self.play_songs(self.songs.clone(), start);
                }
            }
            None => {}
        }
    }

    fn shuffle_everything(&mut self) {
        if self.songs.is_empty() {
            self.info("still fetching the library — try again in a second");
            return;
        }
        if !self.queue.shuffle {
            self.queue.shuffle = true;
            self.dirs.save_state(serde_json::json!({ "shuffle": true }));
        }
        let n = self.songs.len();
        self.play_songs(self.songs.clone(), rand::thread_rng().gen_range(0..n));
        self.info(format!("shuffling all {n} tracks"));
    }

    pub fn load_view(&mut self, view: &str) {
        let title = self.view_title(view);
        if view.starts_with("podcast:") || view == "radio" {
            let rows = if view == "radio" {
                self.stations.clone()
            } else {
                let cid = &view["podcast:".len()..];
                self.podcasts.iter().find(|(c, _)| c.id == cid).map(|(_, e)| e.clone()).unwrap_or_default()
            };
            self.show_songs(rows, &title);
            return;
        }
        let Some(client) = self.client.clone() else { return };
        let (cache_key, kind) = match view {
            "all-songs" | "shuffle-all" => ("all-songs".to_string(), view.to_string()),
            "newest" | "recent" | "frequent" => (format!("songview-{view}"), view.to_string()),
            "starred" => ("starred-songs".to_string(), view.to_string()),
            v if v.starts_with("pl:") => (format!("playlist-songs-{}", &v[3..]), v.to_string()),
            _ => return,
        };
        if let Some(c) = self.dirs.read_cache::<SongsCache>(&cache_key) {
            self.show_songs(c.songs, &title);
        }
        let token = self.songs_token.fetch_add(1, SeqCst) + 1;
        let guard = self.songs_token.clone();
        let (tx, dirs, view) = (self.tx.clone(), self.dirs.clone(), view.to_string());
        self.loading += 1;
        self.spawn(async move {
            tokio::time::sleep(Duration::from_millis(120)).await; // superseded while the cursor moves
            if guard.load(SeqCst) != token {
                App::send(&tx, Msg::Pinned);
                return;
            }
            let res = match kind.as_str() {
                "all-songs" | "shuffle-all" => client.get_all_songs(5000).await,
                "newest" | "recent" | "frequent" => client.get_songs_by_albums(&kind, 15).await,
                "starred" => client.get_starred().await.map(|r| r.songs),
                pl => client.get_playlist_songs(&pl[3..]).await,
            };
            match res {
                Ok(songs) => {
                    dirs.write_cache(&cache_key, &SongsRef { songs: &songs });
                    App::send(&tx, Msg::Songs { token, view, title, songs, focus_tracks: false });
                }
                Err(e) => App::send(&tx, Msg::ConnTrouble(e.to_string())),
            }
        });
    }

    pub fn load_playlists(&mut self) {
        let Some(c) = self.client.clone() else { return };
        let (tx, dirs) = (self.tx.clone(), self.dirs.clone());
        self.spawn(async move {
            if let Ok(p) = c.get_playlists().await {
                dirs.write_cache("playlists-v2", &p);
                App::send(&tx, Msg::Playlists(p));
            }
        });
    }

    pub fn restore_podcasts_radio(&mut self) {
        if let Some(c) = self.dirs.read_cache::<(Vec<(PodcastChannel, Vec<Song>)>, Vec<Song>)>("podcasts-radio-v2") {
            self.podcasts = c.0;
            self.stations = c.1;
        }
    }

    pub fn load_podcasts_radio(&mut self) {
        let Some(c) = self.client.clone() else { return };
        let (tx, dirs) = (self.tx.clone(), self.dirs.clone());
        self.spawn(async move {
            // either may be unsupported (404/501) — each degrades to empty
            let pods = c.get_podcasts().await.unwrap_or_default();
            let stations = c.get_internet_radio_stations().await.unwrap_or_default();
            dirs.write_cache("podcasts-radio-v2", &(&pods, &stations));
            App::send(&tx, Msg::PodcastsRadio(pods, stations));
        });
    }

    pub fn maybe_auto_refresh(&mut self) {
        if self.offline || self.filtering || self.select_mode {
            return;
        }
        self.load_playlists();
        self.load_podcasts_radio();
        let v = self.view.clone();
        if !v.starts_with("artist:") && !v.starts_with("genre:") && !v.starts_with("album:") {
            self.load_view(&v);
        }
        self.flush_mutations();
    }

    pub fn load_artist_songs(&mut self, artist: Artist) {
        let Some(c) = self.client.clone() else { return };
        let view = format!("artist:{}", artist.id);
        let title = format!("artist · {}", artist.name);
        self.adhoc_view(view.clone(), &title);
        let token = self.songs_token.load(SeqCst);
        let (tx, dirs) = (self.tx.clone(), self.dirs.clone());
        self.spawn(async move {
            let albums = match c.get_artist_albums(&artist.id).await {
                Ok(a) => a,
                Err(e) => return App::send(&tx, Msg::ConnTrouble(e.to_string())),
            };
            let results = futures::future::join_all(albums.iter().map(|a| c.get_album_songs(&a.id))).await;
            let songs: Vec<Song> = results.into_iter().filter_map(Result::ok).flatten().collect();
            dirs.write_cache(&format!("artist-songs-{}", artist.id), &SongsRef { songs: &songs });
            App::send(&tx, Msg::Songs { token, view, title, songs, focus_tracks: true });
        });
    }

    /// Switch the tracks pane to an ad-hoc (non-sidebar) view; any in-flight
    /// sidebar load is superseded.
    fn adhoc_view(&mut self, view: String, title: &str) {
        self.songs_token.fetch_add(1, SeqCst);
        self.loading += 1;
        self.view = view;
        self.sidebar_unhighlight();
        self.tracks_title = format!("{title} …");
    }

    pub fn load_genre_songs(&mut self, genre: String) {
        let Some(c) = self.client.clone() else { return };
        let (view, title) = (format!("genre:{genre}"), format!("genre · {genre}"));
        self.adhoc_view(view.clone(), &title);
        let token = self.songs_token.load(SeqCst);
        let tx = self.tx.clone();
        self.spawn(async move {
            match c.get_songs_by_genre(&genre, 500).await {
                Ok(songs) => App::send(&tx, Msg::Songs { token, view, title, songs, focus_tracks: true }),
                Err(e) => App::send(&tx, Msg::ConnTrouble(e.to_string())),
            }
        });
    }

    fn go_album(&mut self) {
        let (Some(song), Some(c)) = (self.target_song(), self.client.clone()) else { return };
        let Some(album_id) = song.album_id.clone() else {
            self.info("no album for this track");
            return;
        };
        let (view, title) = (format!("album:{album_id}"), format!("album · {}", song.album));
        self.adhoc_view(view.clone(), &title);
        let token = self.songs_token.load(SeqCst);
        let tx = self.tx.clone();
        self.spawn(async move {
            match c.get_album_songs(&album_id).await {
                Ok(songs) => App::send(&tx, Msg::Songs { token, view, title, songs, focus_tracks: true }),
                Err(e) => App::send(&tx, Msg::ConnTrouble(e.to_string())),
            }
        });
    }

    fn go_artist(&mut self) {
        let Some(song) = self.target_song() else { return };
        match song.artist_id.clone() {
            Some(id) => self.load_artist_songs(Artist { id, name: song.artist, ..Default::default() }),
            None => self.info("no artist for this track"),
        }
    }

    fn action_genres(&mut self) {
        let Some(c) = self.client.clone() else { return };
        let tx = self.tx.clone();
        self.spawn(async move {
            match c.get_genres().await {
                Ok(g) if !g.is_empty() => App::send(&tx, Msg::Genres(g)),
                Ok(_) => App::send(&tx, Msg::Toast("no genres on this server".into(), Level::Info)),
                Err(e) => App::send(&tx, Msg::Toast(format!("genres: {e}"), Level::Warn)),
            }
        });
    }

    pub fn run_search(&mut self, q: String) {
        let Some(c) = self.client.clone() else { return };
        let Some(Modal::Search(s)) = &mut self.modal else { return };
        if q.chars().count() < 2 {
            s.set_results(SearchResults::default());
            s.searched = false;
            return;
        }
        let (seq, tx) = (s.seq, self.tx.clone());
        self.spawn(async move {
            tokio::time::sleep(Duration::from_millis(180)).await; // debounce
            if let Ok(res) = c.search(&q, 20).await {
                App::send(&tx, Msg::Search { seq, res });
            }
        });
    }

    pub fn enqueue_album(&mut self, album: Album) {
        let Some(c) = self.client.clone() else { return };
        let tx = self.tx.clone();
        self.spawn(async move {
            match c.get_album_songs(&album.id).await {
                Ok(songs) => App::send(&tx, Msg::AlbumSongs { songs, name: album.name }),
                Err(e) => App::send(&tx, Msg::ConnTrouble(e.to_string())),
            }
        });
    }

    // ── filter & select ───────────────────────────────────────────────
    fn start_filter(&mut self) {
        if self.songs.is_empty() {
            self.info("nothing here to filter");
            return;
        }
        self.focus = Focus::Tracks;
        self.filtering = true;
        self.filter_query.clear();
        self.filtered = (0..self.songs.len()).collect();
    }

    pub fn apply_filter(&mut self) {
        let needle = self.filter_query.trim().to_string();
        self.filtered = if needle.is_empty() {
            (0..self.songs.len()).collect()
        } else {
            let mut hay = String::new();
            self.songs
                .iter()
                .enumerate()
                .filter(|(_, s)| {
                    hay.clear(); // one reused buffer, not a String per song per keystroke
                    hay.push_str(&s.title);
                    hay.push(' ');
                    hay.push_str(&s.artist);
                    fuzzy(&hay, &needle)
                })
                .map(|(i, _)| i)
                .collect()
        };
        let n = self.filtered.len();
        self.tracks.select(if n == 0 { None } else { Some(0) });
    }

    pub fn exit_filter(&mut self) {
        if !self.filtering {
            return;
        }
        // keep the cursor on the same song in the full list
        let cur = self.cursor(Focus::Tracks).and_then(|i| self.filtered.get(i).copied());
        self.filtering = false;
        self.filter_query.clear();
        self.filtered = Vec::new(); // release the capacity, not just the length
        self.tracks.select(if self.songs.is_empty() { None } else { Some(cur.unwrap_or(0)) });
    }

    fn highlighted_in_view(&self) -> Option<Song> {
        self.cursor(Focus::Tracks).and_then(|i| self.view_song(i)).cloned()
    }

    pub fn toggle_select_mode(&mut self) {
        if self.select_mode {
            self.exit_select_mode();
        } else if self.songs.is_empty() {
            self.info("nothing here to select");
        } else {
            self.focus = Focus::Tracks;
            self.select_mode = true;
            self.info("select mode — space toggles rows, esc exits");
        }
    }

    pub fn exit_select_mode(&mut self) {
        self.select_mode = false;
        self.selected.clear();
    }

    pub fn toggle_current_selection(&mut self) {
        let Some(song) = self.highlighted_in_view() else { return };
        if !self.selected.remove(&song.id) {
            self.selected.insert(song.id);
        }
        // step down for fast tagging
        if let Some(i) = self.cursor(Focus::Tracks) {
            if i + 1 < self.view_len() {
                self.tracks.select(Some(i + 1));
            }
        }
    }

    // ── playback ──────────────────────────────────────────────────────
    pub fn play_songs(&mut self, songs: Vec<Song>, start: usize) {
        self.change_track(Box::new(move |q| q.set_songs(songs, start).is_some()));
    }

    pub fn change_track(&mut self, pick: Pick) {
        if self.crossfade > 0.0 && self.player.active() {
            // capture the TRUE user volume once so rapid skips don't ratchet it down
            let base = *self.fade_base.get_or_insert(self.player.volume());
            let (h, tx, secs) = (self.player.handle(), self.tx.clone(), self.crossfade);
            self.spawn(async move {
                let steps = ((secs / 0.05) as usize).max(1);
                for i in 0..steps {
                    h.set_volume_raw(base as f64 * (1.0 - (i + 1) as f64 / steps as f64));
                    tokio::time::sleep(Duration::from_secs_f64(secs / steps as f64)).await;
                }
                App::send(&tx, Msg::CrossfadeApply(pick, base));
            });
            return;
        }
        if pick(&mut self.queue) {
            self.play_current(0.0);
        }
    }

    pub fn apply_crossfade(&mut self, pick: Pick, base: i64) {
        if pick(&mut self.queue) {
            self.play_current(0.0);
        }
        let (h, tx, secs) = (self.player.handle(), self.tx.clone(), self.crossfade);
        let _ = tx;
        self.fade_base = None;
        self.spawn(async move {
            let steps = ((secs / 0.05) as usize).max(1);
            for i in 0..steps {
                h.set_volume_raw(base as f64 * (i + 1) as f64 / steps as f64);
                tokio::time::sleep(Duration::from_secs_f64(secs / steps as f64)).await;
            }
            h.set_volume_raw(base as f64);
        });
    }

    /// Where to play `song` from: a pinned local file, else the stream URL.
    /// In offline mode a missing pin means the track is unplayable.
    pub fn stream_source(&self, song: &Song) -> Option<String> {
        if let Some(u) = &song.stream_url {
            return (!self.offline).then(|| u.clone()); // live streams: never pinned
        }
        let c = self.client.as_ref()?;
        if let Some(local) = c.cached_stream(&song.id) {
            return Some(local.to_string_lossy().into_owned());
        }
        (!self.offline).then(|| c.stream_url(&song.id))
    }

    pub fn play_current(&mut self, resume_at: f64) {
        let Some(mut song) = self.queue.current().cloned() else {
            self.player.stop();
            self.now_song = None;
            return;
        };
        let mut source = self.stream_source(&song);
        let mut skipped = 0;
        if source.is_none() {
            // offline and not pinned — scan forward (at most one lap)
            for _ in 0..self.queue.songs.len() {
                let Some(next) = self.queue.advance(false).cloned() else { break };
                skipped += 1;
                if let Some(s) = self.stream_source(&next) {
                    song = next;
                    source = Some(s);
                    break;
                }
            }
        }
        if source.is_some() && skipped > 0 {
            self.info(format!("offline: skipped {skipped} track{} that aren't downloaded", if skipped == 1 { "" } else { "s" }));
        }
        let Some(source) = source else {
            self.info("offline: nothing downloaded to play");
            self.player.stop();
            self.now_song = None;
            return;
        };
        self.player.play(&source, resume_at);
        if self.now_song.as_ref().map(|s| &s.id) != Some(&song.id) {
            self.title_flash = 12;
        }
        self.set_position(resume_at);
        self.now_duration = song.duration as f64;
        self.now_song = Some(song.clone());
        self.scrobbled = false;
        self.scrobble(&song, false);
        match &song.cover_art {
            Some(cover) => self.load_art(cover.clone()),
            None => {
                self.art = None;
                self.tint = None;
            }
        }
        self.render_queue_cursor();
        self.persist_queue(Some(resume_at));
        self.announce_track(&song);
        self.prefetch_next();
        self.prefetch_art();
    }

    pub fn action_play_pause(&mut self) {
        if self.player.active() {
            self.player.toggle_pause();
        } else if self.queue.current().is_some() {
            // resume a restored queue exactly where it left off
            let at = std::mem::take(&mut self.resume_position);
            self.play_current(at);
        }
    }

    pub fn change_volume(&mut self, delta: i64) {
        let v = self.player.volume() + delta;
        self.player.set_volume(v);
        self.vol_flash = 10;
    }

    pub fn set_volume_fraction(&mut self, f: f64) {
        self.player.set_volume((f.clamp(0.0, 1.0) * 100.0).round() as i64);
        self.vol_flash = 10;
    }

    /// Length of the current track: mpv's figure once it knows it, else the server's.
    pub fn track_duration(&self) -> f64 {
        if self.now_duration > 0.0 {
            self.now_duration
        } else {
            self.now_song.as_ref().map_or(0.0, |s| s.duration as f64)
        }
    }

    pub fn seek_fraction(&mut self, f: f64) {
        let f = f.clamp(0.0, 1.0);
        self.player.seek_to(f);
        // show where we are going at once instead of waiting for mpv's report
        let d = self.track_duration();
        if self.player.active() && d > 0.0 {
            let target = f.min(0.99) * d;
            self.now_position = target;
            self.shown_pos = target;
            self.pos_report = Some((target, Instant::now()));
            self.seek_hold = Some(Instant::now() + Duration::from_millis(400));
        }
    }

    /// The pointer is at screen column `x` while the button is held on the seek
    /// line or the volume slider (the column is clamped to the control's ends).
    pub fn scrub_to(&mut self, kind: Scrub, x: u16) {
        let r = if kind == Scrub::Seek { self.rects.bar } else { self.rects.gauge };
        if r.width == 0 {
            return;
        }
        let col = x.clamp(r.x, r.x + r.width - 1) - r.x;
        let f = crate::anim::fraction_at(col as usize, r.width as usize);
        match kind {
            Scrub::Volume => self.set_volume_fraction(f),
            Scrub::Seek => {
                if f != self.scrub_frac || !self.scrub_dirty && self.scrub_sent.is_none() {
                    self.scrub_frac = f;
                    self.scrub_dirty = true;
                }
                self.flush_scrub(false);
            }
        }
    }

    /// Send the pending scrub position to mpv — at most every 50 ms while dragging
    /// (`force` for the final one on release).
    pub fn flush_scrub(&mut self, force: bool) {
        if self.scrub != Some(Scrub::Seek) || !self.scrub_dirty {
            return;
        }
        if force || self.scrub_sent.is_none_or(|t| t.elapsed() >= Duration::from_millis(50)) {
            self.scrub_dirty = false;
            self.scrub_sent = Some(Instant::now());
            self.seek_fraction(self.scrub_frac);
        }
    }

    pub fn end_scrub(&mut self) {
        self.flush_scrub(true);
        self.scrub = None;
        self.scrub_sent = None;
        self.scrub_dirty = false;
    }

    fn cycle_sleep(&mut self) {
        self.sleep_idx = (self.sleep_idx + 1) % SLEEP_PRESETS.len();
        match SLEEP_PRESETS[self.sleep_idx] {
            0 => {
                self.sleep_deadline = None;
                self.sleep_label.clear();
                self.info("sleep timer off");
            }
            -1 => {
                self.sleep_deadline = None;
                self.sleep_label = "end".into();
                self.info("sleep timer: stop at end of track");
            }
            m => {
                self.sleep_deadline = Some(Instant::now() + Duration::from_secs(m as u64 * 60));
                self.info(format!("sleep timer: {m} min"));
            }
        }
    }

    pub fn sleep_fire(&mut self) {
        self.sleep_idx = 0;
        self.sleep_deadline = None;
        self.sleep_label.clear();
        if self.player.active() {
            self.player.set_paused(true);
        }
        self.info("sleep timer — paused");
    }

    /// Put the position somewhere that mpv hasn't reported (a reset, a resume point):
    /// nothing is extrapolated from it until the first real report arrives.
    pub fn set_position(&mut self, p: f64) {
        self.now_position = p;
        self.shown_pos = p;
        self.pos_report = None;
    }

    pub fn on_position(&mut self, position: f64, duration: f64) {
        // right after our own seek, reports from before it are still in flight:
        // taking one would snap the dot back for a moment
        if let Some(until) = self.seek_hold {
            if Instant::now() < until && (position - self.now_position).abs() > 1.5 {
                return;
            }
            self.seek_hold = None;
        }
        self.now_position = position;
        self.pos_report = Some((position, Instant::now()));
        if duration > 0.0 {
            self.now_duration = duration;
        }
        if position > 3.0 {
            self.end_failures = 0;
        }
        if let Some(song) = self.queue.current().cloned() {
            if !self.scrobbled && duration > 0.0 && position >= (duration / 2.0).min(240.0) {
                self.scrobbled = true;
                self.scrobble(&song, true);
                if !self.private_mode {
                    self.stats.log_play(&song.id, &song.title, &song.artist, crate::stats::now_ts());
                }
            }
        }
        // crash-safe resume point, at most every 10s
        if position - self.last_persist >= 10.0 || position < self.last_persist {
            self.last_persist = position;
            self.persist_queue(Some(position));
        }
    }

    pub fn on_track_end(&mut self, failed: bool) {
        if failed {
            self.end_failures += 1;
            let t = self.queue.current().map_or("?".to_string(), |s| s.title.clone());
            self.notify(format!("stream failed: {t}"), Level::Warn);
            if self.end_failures >= 3 {
                self.notify("three failures in a row — stopping", Level::Error);
                self.player.stop();
                return;
            }
        }
        // sleep timer set to "stop at end of track"
        if !failed && SLEEP_PRESETS[self.sleep_idx] == -1 {
            self.sleep_idx = 0;
            self.sleep_label.clear();
            self.player.stop();
            self.set_position(0.0);
            self.info("sleep timer — stopped at end of track");
            return;
        }
        let seed = self.queue.current().cloned();
        if self.queue.advance(!failed).is_some() {
            self.play_current(0.0);
        } else if let (true, Some(seed)) = (self.radio, seed) {
            // queue ran dry with radio on: fetch songs like the one that just played
            self.radio_refill(seed, true);
        } else {
            self.player.stop();
            self.set_position(0.0);
            self.now_duration = 0.0;
        }
    }

    // ── radio ─────────────────────────────────────────────────────────
    fn start_radio(&mut self) {
        let Some(song) = self.target_song() else {
            self.info("highlight a track to start radio");
            return;
        };
        if self.client.is_none() {
            return;
        }
        self.radio = true;
        self.dirs.save_state(serde_json::json!({ "radio": true }));
        self.play_songs(vec![song.clone()], 0);
        self.info(format!("radio: {}", song.title));
        self.radio_refill(song, false);
    }

    fn radio_refill(&mut self, seed: Song, autoplay: bool) {
        let Some(c) = self.client.clone() else { return };
        if self.radio_filling {
            return;
        }
        self.radio_filling = true;
        let tx = self.tx.clone();
        let have: std::collections::HashSet<String> = self.queue.songs.iter().map(|s| s.id.clone()).collect();
        let last_artist = self.queue.songs.last().map(|s| s.artist.clone()).unwrap_or_default();
        self.spawn(async move {
            // similar → artist similar → top songs → random
            let mut batch = c.get_similar_songs(&seed.id, 25).await.unwrap_or_default();
            if batch.is_empty() {
                if let Some(a) = &seed.artist_id {
                    batch = c.get_similar_songs(a, 25).await.unwrap_or_default();
                }
            }
            if batch.is_empty() && !seed.artist.is_empty() {
                batch = c.get_top_songs(&seed.artist, 25).await.unwrap_or_default();
            }
            if batch.is_empty() {
                batch = c.get_random_songs(25).await.unwrap_or_default();
            }
            let fresh: Vec<Song> = batch.into_iter().filter(|s| !have.contains(&s.id)).take(20).collect();
            let songs = reorder_for_artist_diversity(fresh, &last_artist);
            App::send(&tx, Msg::RadioRefill { songs, autoplay });
        });
    }

    pub fn on_radio_refill(&mut self, songs: Vec<Song>, autoplay: bool) {
        self.radio_filling = false;
        if songs.is_empty() {
            if autoplay {
                self.player.stop();
                self.set_position(0.0);
                self.info("radio: no more tracks to play");
            }
            return;
        }
        self.queue.add(songs);
        if autoplay {
            if self.queue.advance(false).is_some() {
                self.play_current(0.0);
            }
        } else {
            self.render_queue_cursor();
            self.persist_queue(None);
        }
    }

    // ── queue ─────────────────────────────────────────────────────────
    fn enqueue(&mut self, next: bool) {
        if self.focus != Focus::Tracks {
            return;
        }
        let selected = self.selected_songs();
        let songs = if selected.is_empty() { self.highlighted_song().into_iter().collect() } else { selected };
        if songs.is_empty() {
            return;
        }
        let msg = if songs.len() == 1 { songs[0].title.clone() } else { format!("{} tracks", songs.len()) };
        if next {
            self.queue.add_next(songs);
        } else {
            self.queue.add(songs);
        }
        self.render_queue_cursor();
        self.persist_queue(None);
        self.info(format!("queued {}{msg}", if next { "next: " } else { "" }));
    }

    fn queue_move(&mut self, delta: i64) {
        if self.focus != Focus::Queue {
            return;
        }
        if let Some(i) = self.cursor(Focus::Queue) {
            if let Some(j) = self.queue.move_by(i, delta) {
                self.queue_state.select(Some(j));
                self.persist_queue(None);
            }
        }
    }

    /// Pin the current track to the top of the queue panel so it reads "up
    /// next" — only when the track changes, so manual scrollback isn't fought.
    pub fn render_queue_cursor(&mut self) {
        let idx = self.queue.index;
        if idx >= 0 && self.queue_scrolled_to != idx {
            self.queue_scrolled_to = idx;
            self.queue_state.select(Some(idx as usize));
            *self.queue_state.offset_mut() = idx as usize;
        } else if self.queue.songs.is_empty() {
            self.queue_state.select(None);
        } else if self.queue_state.selected().is_none() {
            self.queue_state.select(Some(0));
        }
    }

    /// Save the queue. The queue itself (which can be thousands of songs) is
    /// only rewritten when it actually changed; the every-10-seconds resume
    /// position goes to the tiny state file instead. That keeps playback from
    /// re-serialising megabytes (and rewriting them to disk) in the background.
    pub fn persist_queue(&mut self, position: Option<f64>) {
        let pos = position.unwrap_or_else(|| self.player.position());
        if self.queue.rev() != self.persisted_rev {
            self.dirs.write_cache("queue", &self.queue.snapshot_ref(pos));
            self.persisted_rev = self.queue.rev();
        }
        let song = self.queue.current().map(|s| s.id.clone());
        self.dirs.save_state(serde_json::json!({ "queue_position": pos, "queue_position_song": song }));
    }

    // ── star / rate ───────────────────────────────────────────────────
    fn action_star(&mut self) {
        let selected = if self.focus == Focus::Tracks { self.selected_songs() } else { vec![] };
        if !selected.is_empty() {
            // bulk: always set on, so a mixed selection ends up uniformly starred
            let n = selected.len();
            for s in selected.iter().filter(|s| !s.starred) {
                self.update_song_everywhere(&s.id, |x| x.starred = true);
                self.star_remote(s.id.clone(), true);
            }
            self.info(format!("starred {n} tracks"));
            return;
        }
        let Some(song) = self.highlighted_song().or_else(|| self.now_song.clone()) else { return };
        let star = !song.starred;
        self.update_song_everywhere(&song.id, |x| x.starred = star); // optimistic: the cache IS the truth
        self.star_remote(song.id, star);
    }

    fn star_remote(&mut self, id: String, star: bool) {
        if id.starts_with("radio:") {
            return;
        }
        let (Some(c), q, off, tx) = (self.client.clone(), self.mutations.clone(), self.offline, self.tx.clone()) else {
            return;
        };
        self.spawn(async move {
            if off {
                q.lock().unwrap().star(&id, "song", star);
                return;
            }
            if let Err(e) = c.set_star(&id, "song", star).await {
                if e.is_network() {
                    q.lock().unwrap().star(&id, "song", star);
                    App::send(&tx, Msg::Toast("offline — will sync when you're back".into(), Level::Info));
                } else {
                    App::send(&tx, Msg::Toast(format!("couldn't {}: {e}", if star { "star" } else { "unstar" }), Level::Warn));
                }
            }
        });
    }

    pub fn rate(&mut self, n: i64) {
        let Some(song) = self.target_song() else { return };
        let rating = if song.user_rating == n { 0 } else { n }; // same digit clears
        self.update_song_everywhere(&song.id, |x| x.user_rating = rating);
        let (Some(c), q, off) = (self.client.clone(), self.mutations.clone(), self.offline) else { return };
        let id = song.id;
        self.spawn(async move {
            if off {
                q.lock().unwrap().rate(&id, rating);
            } else if let Err(e) = c.set_rating(&id, rating).await {
                if e.is_network() {
                    q.lock().unwrap().rate(&id, rating);
                }
            }
        });
        self.info(if rating == 0 { "rating cleared".to_string() } else { format!("rated {rating}/5") });
    }

    // ── scrobble / mutations ──────────────────────────────────────────
    fn scrobble(&mut self, song: &Song, submission: bool) {
        if self.private_mode || song.stream_url.is_some() {
            return; // private listening / live radio: no scrobble
        }
        let (Some(c), q, off) = (self.client.clone(), self.mutations.clone(), self.offline) else { return };
        let id = song.id.clone();
        self.spawn(async move {
            if off {
                q.lock().unwrap().scrobble(&id, submission);
            } else if let Err(e) = c.scrobble(&id, submission).await {
                if e.is_network() {
                    q.lock().unwrap().scrobble(&id, submission);
                }
            }
        });
    }

    pub fn flush_mutations(&mut self) {
        if self.offline || self.mutations.lock().unwrap().pending() == 0 {
            return;
        }
        let (Some(c), q) = (self.client.clone(), self.mutations.clone()) else { return };
        self.spawn(async move {
            crate::mutations::flush(&q, &c).await;
        });
    }

    // ── art / lyrics / share / bookmarks ──────────────────────────────
    pub fn load_art(&mut self, cover_id: String) {
        if self.art_mode == crate::art::ArtMode::Off {
            return;
        }
        if self.art.as_ref().is_some_and(|(id, _)| *id == cover_id) {
            return;
        }
        // prepared ahead of time while the previous track played: instant
        if let Some(p) = self.art_lru.take(&cover_id) {
            self.apply_prepared(cover_id, p);
            return;
        }
        let Some(c) = self.client.clone() else { return };
        let (tx, hint) = (self.tx.clone(), self.encode_hint());
        self.spawn(async move {
            if let Ok(path) = c.cover_art(&cover_id, crate::art::SERVER_SIZE).await {
                // decoding a JPEG takes tens of ms — never on the UI thread
                if let Some(prepared) = tokio::task::spawn_blocking(move || crate::art::prepare(&path, hint.as_ref())).await.ok().flatten() {
                    App::send(&tx, Msg::Art { cover_id, prepared });
                }
            }
        });
    }

    /// The sixel layout to pre-encode for, if sixel is in use and the panel's size is known.
    fn encode_hint(&self) -> Option<(crate::sixel::Plan, (u16, u16))> {
        (self.art_mode == crate::art::ArtMode::Sixel).then_some(self.sixel.plan_hint).flatten()
    }

    /// Make a prepared cover the current one.
    pub fn apply_prepared(&mut self, cover: String, p: crate::art::Prepared) {
        self.tint = self.settings.art_theming.then_some(p.tint).flatten();
        self.art = Some((cover.clone(), crate::art::Art::new(p.small)));
        self.art_path = Some(p.path);
        if let Some((key, enc)) = p.enc {
            // used only if the panel still has this shape; otherwise `ensure_sixel` re-encodes
            self.sixel.cache = Some((cover, key, enc));
        }
    }

    /// A cover finished preparing: show it if it's the current song's, else keep it for later.
    pub fn art_arrived(&mut self, cover: String, p: crate::art::Prepared) {
        if self.now_song.as_ref().and_then(|s| s.cover_art.as_deref()) == Some(cover.as_str()) {
            self.apply_prepared(cover, p);
        } else {
            self.art_lru.insert(cover, p);
        }
    }

    /// Prepare the NEXT track's cover while this one plays — fetched, decoded
    /// once, and (for sixel) already encoded for the current panel size — so
    /// that when the track changes the picture is there on the first frame.
    pub fn prefetch_art(&mut self) {
        if self.art_mode == crate::art::ArtMode::Off || self.offline {
            return;
        }
        let Some(c) = self.client.clone() else { return };
        let Some(cover) = self.queue.peek_next().and_then(|s| s.cover_art.clone()) else { return };
        let current = self.now_song.as_ref().and_then(|s| s.cover_art.as_deref());
        if current == Some(cover.as_str()) || self.art_lru.contains(&cover) || self.art_prefetching.as_deref() == Some(cover.as_str()) {
            return;
        }
        self.art_prefetching = Some(cover.clone());
        let (tx, hint) = (self.tx.clone(), self.encode_hint());
        self.spawn(async move {
            let mut prepared = None;
            if let Ok(path) = c.cover_art(&cover, crate::art::SERVER_SIZE).await {
                prepared = tokio::task::spawn_blocking(move || crate::art::prepare(&path, hint.as_ref())).await.ok().flatten();
            }
            App::send(&tx, Msg::ArtPrefetched { cover, prepared }); // also clears the in-flight flag on failure
        });
    }

    pub fn retint(&mut self) {
        self.tint = self.art.as_ref().and_then(|(_, a)| self.settings.art_theming.then(|| a.tint()).flatten());
    }

    fn action_lyrics(&mut self) {
        let (Some(song), Some(c)) = (self.target_song(), self.client.clone()) else {
            self.info("nothing to look up");
            return;
        };
        let tx = self.tx.clone();
        self.spawn(async move {
            let title = format!("{} — {}", song.title, song.artist);
            let synced = c.get_synced_lyrics(&song.id).await;
            let text = if synced.is_none() { c.get_lyrics(&song.artist, &song.title).await.unwrap_or_default() } else { String::new() };
            if synced.is_none() && text.trim().is_empty() {
                App::send(&tx, Msg::Toast(format!("no lyrics found for {}", song.title), Level::Info));
            } else {
                App::send(&tx, Msg::Lyrics { title, synced, text });
            }
        });
    }

    /// Zen-splash lyrics follow the current track.
    pub fn refresh_zen_lyrics(&mut self) {
        let Some(song) = self.now_song.clone() else { return };
        if self.zen_lyrics_for.as_deref() == Some(song.id.as_str()) {
            return;
        }
        self.zen_lyrics_for = Some(song.id.clone());
        self.zen_lyrics = None;
        let (Some(c), tx) = (self.client.clone(), self.tx.clone()) else { return };
        self.spawn(async move {
            if let Some(lines) = c.get_synced_lyrics(&song.id).await {
                App::send(&tx, Msg::ZenLyrics { song_id: song.id, lines });
            }
        });
    }

    fn action_share(&mut self) {
        let (Some(song), Some(c)) = (self.target_song(), self.client.clone()) else { return };
        let tx = self.tx.clone();
        self.spawn(async move {
            match c.create_share(&song.id).await {
                Ok(url) => App::send(&tx, Msg::Share(url)),
                Err(e) => App::send(&tx, Msg::Toast(format!("couldn't create share link: {e}"), Level::Warn)),
            }
        });
    }

    fn action_bookmark(&mut self) {
        let (Some(song), Some(c)) = (self.queue.current().cloned(), self.client.clone()) else {
            self.info("play something first");
            return;
        };
        let ms = (self.player.position() * 1000.0) as i64;
        let tx = self.tx.clone();
        self.info(format!("bookmarked {} @ {}", song.title, crate::anim::fmt_time(ms as f64 / 1000.0)));
        self.spawn(async move {
            if let Err(e) = c.create_bookmark(&song.id, ms).await {
                App::send(&tx, Msg::Toast(format!("bookmark failed: {e}"), Level::Warn));
            }
        });
    }

    // ── downloads ─────────────────────────────────────────────────────
    fn action_download(&mut self) {
        let selected = if self.focus == Focus::Tracks { self.selected_songs() } else { vec![] };
        if !selected.is_empty() {
            let label = format!("{} tracks", selected.len());
            self.download_songs(selected, label);
            return;
        }
        let Some(song) = self.target_song() else {
            self.info("highlight a track to download");
            return;
        };
        if song.stream_url.is_some() {
            self.info("internet radio can't be downloaded");
        } else if self.client.as_ref().is_some_and(|c| c.is_pinned(&song.id)) {
            self.info(format!("already downloaded: {}", song.title));
        } else {
            let label = song.title.clone();
            self.download_songs(vec![song], label);
        }
    }

    pub fn download_songs(&mut self, songs: Vec<Song>, label: String) {
        let Some(c) = self.client.clone() else { return };
        let pending: Vec<Song> = songs.into_iter().filter(|s| s.stream_url.is_none() && !c.is_pinned(&s.id)).collect();
        if pending.is_empty() {
            self.info(format!("{label}: already downloaded"));
            return;
        }
        self.dl_total = pending.len();
        self.dl_done = 0;
        self.dl_failed = 0;
        if pending.len() > 1 {
            self.info(format!("downloading {label} · {} tracks", pending.len()));
        }
        let tx = self.tx.clone();
        self.spawn(async move {
            for s in pending {
                let failed = c.download_song(&s.id).await.is_err();
                App::send(&tx, Msg::DlProgress { failed });
            }
            App::send(&tx, Msg::DlDone { label });
        });
    }

    /// Warm the next queued track so its start is instant. Goes to the bounded
    /// look-ahead cache (not your downloads); silent and best-effort.
    fn prefetch_next(&mut self) {
        let Some(c) = self.client.clone() else { return };
        if self.offline {
            return;
        }
        let cur = self.queue.current().map(|s| s.id.clone());
        let Some(next) = self.queue.peek_next().cloned() else { return };
        if Some(&next.id) == cur.as_ref() || next.stream_url.is_some() {
            return;
        }
        // once per (playing, next) pair — and it also runs eviction, so the
        // look-ahead cache stays bounded however long you listen
        let pair = format!("{}>{}", cur.as_deref().unwrap_or(""), next.id);
        if self.prefetched.as_deref() == Some(pair.as_str()) {
            return;
        }
        self.prefetched = Some(pair);
        let keep: Vec<String> = cur.into_iter().chain([next.id.clone()]).collect();
        self.spawn(async move {
            c.prefetch_song(&next.id, &keep).await;
        });
    }

    fn announce_track(&mut self, song: &Song) {
        if !self.notifications {
            return;
        }
        let art = song.cover_art.as_ref().and_then(|c| self.client.as_ref()?.cached_art(c, crate::art::SERVER_SIZE));
        crate::clipboard::desktop_notify(&song.title, &format!("{} · {}", song.artist, song.album), art);
    }

    // ── layout ────────────────────────────────────────────────────────
    pub fn save_dock(&mut self) {
        self.dirs.save_state(serde_json::json!({ "ui_dock": self.dock.to_json() }));
    }

    /// Switch to `new` if it fits this terminal. A layout that needs more room
    /// than there is would leave only the "too small" screen — with no way to
    /// see the menu that undoes it — so it is refused with the size it needs.
    pub fn try_dock(&mut self, new: Dock, what: &str) -> bool {
        let (w, h) = new.min_size();
        let m = self.rects.main;
        if m.width > 0 && (w > m.width || h > m.height) {
            self.notify(
                format!("{what} needs at least {}×{} — this terminal is {}×{}", w, h + crate::ui::CHROME_ROWS, m.width, m.height + crate::ui::CHROME_ROWS),
                Level::Warn,
            );
            return false;
        }
        self.dock = new;
        self.save_dock();
        true
    }

    /// A panel carried with the mouse was dropped at `mv.x, mv.y`.
    pub fn finish_move(&mut self, mv: Moving) {
        let arr = self.rects.arranged.clone();
        let Some(target) = arr.panel_at(mv.x, mv.y).filter(|t| *t != mv.panel) else { return };
        let Some(r) = arr.rect(target) else { return };
        let mut d = self.dock.clone();
        if d.move_panel(mv.panel, target, crate::dock::zone_at(r, mv.x, mv.y)) && self.try_dock(d, "that layout") {
            self.info(format!("moved {}", mv.panel.label()));
        }
    }

    pub fn enter_arrange(&mut self) {
        if self.zen {
            return;
        }
        self.arrange = Some(match self.focus {
            Focus::Sidebar => PanelId::Library,
            Focus::Tracks => PanelId::Tracks,
            Focus::Queue => PanelId::Queue,
        });
    }

    /// Keyboard arrange mode: arrows swap the selected panel with its neighbour
    /// in that direction, shift+arrows dock it beside that neighbour instead.
    /// Returns whether the key was consumed.
    pub fn on_arrange_key(&mut self, sel: PanelId, k: &KeyEvent) -> bool {
        use crossterm::event::{KeyCode as K, KeyModifiers as M};
        let dir = match k.code {
            K::Left => Some(Dir::Left),
            K::Right => Some(Dir::Right),
            K::Up => Some(Dir::Up),
            K::Down => Some(Dir::Down),
            _ => None,
        };
        match (k.code, dir) {
            (K::Esc | K::Enter, _) => self.arrange = None,
            (K::Tab | K::BackTab, _) => {
                let mut order: Vec<(u16, u16, PanelId)> = self.rects.arranged.panels.iter().map(|(p, r)| (r.x, r.y, *p)).collect();
                order.sort_by_key(|(x, y, _)| (*x, *y));
                if let Some(i) = order.iter().position(|(_, _, p)| *p == sel) {
                    let step = if k.code == K::Tab { 1 } else { order.len() - 1 };
                    self.arrange = Some(order[(i + step) % order.len()].2);
                }
            }
            (K::Char('r'), _) => {
                if self.try_dock(Dock::classic(), "the classic layout") {
                    self.info("layout reset");
                }
            }
            (_, Some(dir)) => {
                let Some(n) = self.rects.arranged.neighbor(sel, dir) else { return true };
                let zone = if k.modifiers.contains(M::SHIFT) {
                    match dir {
                        Dir::Left => Zone::Left,
                        Dir::Right => Zone::Right,
                        Dir::Up => Zone::Top,
                        Dir::Down => Zone::Bottom,
                    }
                } else {
                    Zone::Swap
                };
                let mut d = self.dock.clone();
                if d.move_panel(sel, n, zone) {
                    self.try_dock(d, "that layout");
                }
            }
            // let the quit key through so you can never get stuck here
            _ => return !matches!(self.keymap.resolve(k), Some(Action::Quit)),
        }
        true
    }

    /// The picker of presets, saved layouts, and "save current…".
    pub fn layout_menu(&mut self) {
        let mut items = vec!["＋ save current layout…".to_string()];
        let mut choices = vec![LayoutChoice::SaveCurrent];
        for (name, d) in Dock::presets() {
            items.push(name.to_string());
            choices.push(LayoutChoice::Apply(d));
        }
        let state = self.dirs.load_state();
        if let Some(saved) = state.get("ui_layouts").and_then(|v| v.as_object()) {
            let mut names: Vec<&String> = saved.keys().collect();
            names.sort();
            for n in names {
                if let Some(d) = Dock::from_json(&saved[n]) {
                    items.push(format!("★ {n}"));
                    choices.push(LayoutChoice::Apply(d));
                }
            }
        }
        self.modal = Some(Modal::Picker(Picker::new("layouts", items, PickKind::Layout(choices), false)));
    }

    // ── playlists ─────────────────────────────────────────────────────
    fn open_playlist(&self) -> Option<Playlist> {
        let id = self.view.strip_prefix("pl:")?;
        self.playlists.iter().find(|p| p.id == id).cloned()
    }

    fn playlist_add_prompt(&mut self) {
        let selected = if self.focus == Focus::Tracks { self.selected_songs() } else { vec![] };
        let songs: Vec<Song> = if selected.is_empty() { self.target_song().into_iter().collect() } else { selected };
        if songs.is_empty() {
            self.info("highlight a track first");
            return;
        }
        let mut items = vec!["＋ new playlist".to_string()];
        items.extend(self.playlists.iter().map(|p| p.name.clone()));
        self.modal = Some(Modal::Picker(Picker::new("add to playlist", items, PickKind::PlaylistAdd(songs), false)));
    }

    pub fn playlist_append(&mut self, p: Playlist, songs: Vec<Song>) {
        let Some(c) = self.client.clone() else { return };
        let ids: Vec<String> = songs.iter().map(|s| s.id.clone()).collect();
        let (tx, n, name) = (self.tx.clone(), songs.len(), p.name.clone());
        self.spawn(async move {
            match c.add_to_playlist(&p.id, &ids).await {
                Ok(()) => {
                    App::send(&tx, Msg::Toast(format!("added {n} to {name}"), Level::Info));
                    App::send(&tx, Msg::Reload);
                }
                Err(e) => App::send(&tx, Msg::Toast(format!("couldn't add: {e}"), Level::Warn)),
            }
        });
    }

    fn playlist_remove(&mut self) {
        let (Some(p), Some(c)) = (self.open_playlist(), self.client.clone()) else {
            self.info("open a playlist first");
            return;
        };
        if self.filtering {
            self.info("clear the filter first");
            return;
        }
        let Some(i) = self.cursor(Focus::Tracks).filter(|i| *i < self.songs.len()) else { return };
        let title = self.songs[i].title.clone();
        self.songs.remove(i); // optimistic
        if i >= self.songs.len() {
            self.tracks.select(self.songs.len().checked_sub(1));
        }
        let tx = self.tx.clone();
        self.spawn(async move {
            match c.remove_from_playlist(&p.id, &[i]).await {
                Ok(()) => {
                    App::send(&tx, Msg::Toast(format!("removed {title}"), Level::Info));
                    App::send(&tx, Msg::Reload);
                }
                Err(e) => App::send(&tx, Msg::Toast(format!("couldn't remove: {e}"), Level::Warn)),
            }
        });
    }

    fn playlist_move(&mut self, delta: i64) {
        let (Some(p), Some(c)) = (self.open_playlist(), self.client.clone()) else { return };
        if self.filtering || self.focus != Focus::Tracks {
            return;
        }
        let Some(i) = self.cursor(Focus::Tracks) else { return };
        let j = i as i64 + delta;
        if j < 0 || j as usize >= self.songs.len() {
            return;
        }
        self.songs.swap(i, j as usize);
        self.tracks.select(Some(j as usize));
        let ids: Vec<String> = self.songs.iter().map(|s| s.id.clone()).collect();
        let tx = self.tx.clone();
        self.spawn(async move {
            if let Err(e) = c.reorder_playlist(&p.id, &ids).await {
                App::send(&tx, Msg::Toast(format!("couldn't reorder: {e}"), Level::Warn));
                App::send(&tx, Msg::Reload);
            }
        });
    }

    pub fn playlist_delete(&mut self, id: String) {
        let Some(c) = self.client.clone() else { return };
        let tx = self.tx.clone();
        self.view = "all-songs".into();
        self.spawn(async move {
            match c.delete_playlist(&id).await {
                Ok(()) => {
                    App::send(&tx, Msg::Toast("playlist deleted".into(), Level::Info));
                    App::send(&tx, Msg::Reload);
                }
                Err(e) => App::send(&tx, Msg::Toast(format!("couldn't delete: {e}"), Level::Warn)),
            }
        });
    }

    pub fn on_input(&mut self, purpose: InputPurpose, text: String) {
        if matches!(purpose, InputPurpose::SaveLayout) {
            // saved layouts live beside the other UI state; same name overwrites
            let mut saved = self.dirs.load_state().get("ui_layouts").and_then(|v| v.as_object().cloned()).unwrap_or_default();
            saved.insert(text.clone(), self.dock.to_json());
            self.dirs.save_state(serde_json::json!({ "ui_layouts": saved }));
            self.info(format!("saved layout “{text}”"));
            return;
        }
        let Some(c) = self.client.clone() else { return };
        let tx = self.tx.clone();
        match purpose {
            InputPurpose::NewPlaylist(songs) => {
                let ids: Vec<String> = songs.iter().map(|s| s.id.clone()).collect();
                self.spawn(async move {
                    match c.create_playlist(&text, &ids).await {
                        Ok(()) => {
                            App::send(&tx, Msg::Toast(format!("created playlist {text}"), Level::Info));
                            App::send(&tx, Msg::Reload);
                        }
                        Err(e) => App::send(&tx, Msg::Toast(format!("couldn't create playlist: {e}"), Level::Warn)),
                    }
                });
            }
            InputPurpose::SaveQueue => {
                let ids: Vec<String> = self.queue.songs.iter().filter(|s| s.stream_url.is_none()).map(|s| s.id.clone()).collect();
                self.spawn(async move {
                    match c.create_playlist(&text, &ids).await {
                        Ok(()) => {
                            App::send(&tx, Msg::Toast(format!("saved queue as {text}"), Level::Info));
                            App::send(&tx, Msg::Reload);
                        }
                        Err(e) => App::send(&tx, Msg::Toast(format!("couldn't save: {e}"), Level::Warn)),
                    }
                });
            }
            InputPurpose::SaveLayout => {}
            InputPurpose::RenamePlaylist(id) => {
                self.spawn(async move {
                    match c.rename_playlist(&id, &text).await {
                        Ok(()) => App::send(&tx, Msg::Reload),
                        Err(e) => App::send(&tx, Msg::Toast(format!("couldn't rename: {e}"), Level::Warn)),
                    }
                });
            }
        }
    }
}
