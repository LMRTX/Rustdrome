//! Modal overlays: login, text input, search, help, lyrics, stats, pickers.
//!
//! Each modal owns its state and turns a key press into a `Outcome` the app
//! acts on; drawing lives in `ui.rs`. Nothing here touches the network.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::keys::Action;
use crate::models::*;
use crate::stats::Summary;

#[derive(Debug, Default, Clone)]
pub struct TextField {
    pub value: String,
}

impl TextField {
    pub fn new(v: &str) -> Self {
        Self { value: v.to_string() }
    }
    /// Feed a key; returns true if the text changed.
    pub fn key(&mut self, k: &KeyEvent) -> bool {
        match k.code {
            KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.value.push(c);
                true
            }
            KeyCode::Backspace => self.value.pop().is_some(),
            KeyCode::Char('u') => {
                let had = !self.value.is_empty();
                self.value.clear();
                had
            }
            _ => false,
        }
    }
}

pub enum Outcome {
    None,
    Close,
    /// submit login form: (server, username, password)
    Login(String, String, String),
    /// text submitted from an input modal
    Input(InputPurpose, String),
    /// the search query changed
    SearchQuery(String),
    SearchSong { songs: Vec<Song>, index: usize },
    SearchQueue { song: Song, next: bool },
    SearchAlbum(Album),
    SearchArtist(Artist),
    /// picker confirmed: index into the picker's ORIGINAL item list
    Picked(PickKind, usize),
    /// picker cursor moved (theme live preview)
    Previewed(PickKind, usize),
    /// picker cancelled
    Cancelled(PickKind),
}

// ── login ─────────────────────────────────────────────────────────────
pub struct Login {
    pub fields: [TextField; 3], // server, user, password
    pub focus: usize,
    pub status: Option<(String, bool)>, // (text, is_error)
    pub busy: bool,
}

impl Login {
    pub fn new(server: &str, user: &str) -> Self {
        Self {
            fields: [TextField::new(server), TextField::new(user), TextField::default()],
            focus: if server.is_empty() { 0 } else { 1 },
            status: None,
            busy: false,
        }
    }

    fn key(&mut self, k: &KeyEvent) -> Outcome {
        if self.busy {
            return Outcome::None;
        }
        match k.code {
            KeyCode::Esc => return Outcome::Close,
            KeyCode::Tab | KeyCode::Down => self.focus = (self.focus + 1) % 3,
            KeyCode::BackTab | KeyCode::Up => self.focus = (self.focus + 2) % 3,
            KeyCode::Enter => {
                if let Some(empty) = self.fields.iter().position(|f| f.value.trim().is_empty()) {
                    self.focus = empty;
                } else {
                    let v = |i: usize| self.fields[i].value.trim().to_string();
                    return Outcome::Login(v(0), v(1), v(2));
                }
            }
            _ => {
                self.fields[self.focus].key(k);
            }
        }
        Outcome::None
    }
}

// ── text input ────────────────────────────────────────────────────────
#[derive(Debug, Clone)]
pub enum InputPurpose {
    NewPlaylist(Vec<Song>),
    RenamePlaylist(String),
    SaveQueue,
}

pub struct InputModal {
    pub title: String,
    pub placeholder: String,
    pub field: TextField,
    pub purpose: InputPurpose,
}

// ── search ────────────────────────────────────────────────────────────
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SearchRow {
    Header(&'static str),
    Song(usize),
    Album(usize),
    Artist(usize),
}

#[derive(Default)]
pub struct SearchModal {
    pub field: TextField,
    pub results: SearchResults,
    pub rows: Vec<SearchRow>,
    pub cursor: usize,
    pub in_list: bool,
    pub seq: u64,
    pub searched: bool,
}

impl SearchModal {
    pub fn set_results(&mut self, res: SearchResults) {
        self.rows.clear();
        if !res.songs.is_empty() {
            self.rows.push(SearchRow::Header("songs"));
            self.rows.extend((0..res.songs.len()).map(SearchRow::Song));
        }
        if !res.albums.is_empty() {
            self.rows.push(SearchRow::Header("albums"));
            self.rows.extend((0..res.albums.len()).map(SearchRow::Album));
        }
        if !res.artists.is_empty() {
            self.rows.push(SearchRow::Header("artists"));
            self.rows.extend((0..res.artists.len()).map(SearchRow::Artist));
        }
        self.cursor = self.rows.iter().position(|r| !matches!(r, SearchRow::Header(_))).unwrap_or(0);
        self.results = res;
        self.searched = true;
    }

    fn step(&mut self, delta: i64) {
        let n = self.rows.len() as i64;
        let mut c = self.cursor as i64;
        loop {
            c += delta;
            if c < 0 || c >= n {
                return;
            }
            if !matches!(self.rows[c as usize], SearchRow::Header(_)) {
                self.cursor = c as usize;
                return;
            }
        }
    }

    fn key(&mut self, k: &KeyEvent) -> Outcome {
        match k.code {
            KeyCode::Esc => return Outcome::Close,
            KeyCode::Down => {
                if !self.in_list && !self.rows.is_empty() {
                    self.in_list = true;
                } else {
                    self.step(1);
                }
            }
            KeyCode::Up => {
                if self.in_list && self.rows.iter().position(|r| !matches!(r, SearchRow::Header(_))) == Some(self.cursor) {
                    self.in_list = false;
                } else {
                    self.step(-1);
                }
            }
            KeyCode::Enter => {
                return match self.rows.get(self.cursor).copied() {
                    Some(SearchRow::Song(i)) => Outcome::SearchSong { songs: self.results.songs.clone(), index: i },
                    Some(SearchRow::Album(i)) => Outcome::SearchAlbum(self.results.albums[i].clone()),
                    Some(SearchRow::Artist(i)) => Outcome::SearchArtist(self.results.artists[i].clone()),
                    _ => Outcome::None,
                };
            }
            KeyCode::Char(c @ ('a' | 'A')) if self.in_list => {
                if let Some(SearchRow::Song(i)) = self.rows.get(self.cursor).copied() {
                    return Outcome::SearchQueue { song: self.results.songs[i].clone(), next: c == 'A' };
                }
            }
            KeyCode::Char('j') if self.in_list => self.step(1),
            KeyCode::Char('k') if self.in_list => self.step(-1),
            _ => {
                if !self.in_list && self.field.key(k) {
                    self.seq += 1;
                    return Outcome::SearchQuery(self.field.value.trim().to_string());
                }
            }
        }
        Outcome::None
    }
}

// ── lyrics ────────────────────────────────────────────────────────────
pub struct LyricsModal {
    pub title: String,
    pub synced: Option<Vec<(f64, String)>>,
    pub text: String,
    pub scroll: usize,
}

impl LyricsModal {
    /// Index of the line that is current at `position` seconds.
    pub fn current_line(&self, position: f64) -> Option<usize> {
        let lines = self.synced.as_ref()?;
        lines.iter().rposition(|(t, _)| *t <= position)
    }
}

// ── pickers ───────────────────────────────────────────────────────────
#[derive(Clone)]
pub enum PickKind {
    Theme { original: String },
    Genre,
    Bookmark(Vec<Bookmark>),
    PlaylistAdd(Vec<Song>),
    Command(Vec<Action>),
    ConfirmDeletePlaylist(String),
}

pub struct Picker {
    pub title: String,
    pub items: Vec<String>,
    pub kind: PickKind,
    pub cursor: usize,
    /// `Some` makes the picker type-to-filter (command palette)
    pub query: Option<String>,
}

/// Case-insensitive substring, falling back to an in-order subsequence match
/// so `dyln` still finds `Bob Dylan`.
pub fn fuzzy(hay: &str, needle: &str) -> bool {
    let (hay, needle) = (hay.to_lowercase(), needle.to_lowercase());
    if hay.contains(&needle) {
        return true;
    }
    let mut it = hay.chars();
    needle.chars().all(|c| it.any(|h| h == c))
}

impl Picker {
    pub fn new(title: &str, items: Vec<String>, kind: PickKind, searchable: bool) -> Self {
        Self { title: title.into(), items, kind, cursor: 0, query: searchable.then(String::new) }
    }

    /// Indices (into `items`) currently visible.
    pub fn visible(&self) -> Vec<usize> {
        match self.query.as_deref() {
            Some(q) if !q.trim().is_empty() => (0..self.items.len())
                .filter(|&i| fuzzy(&self.items[i], q.trim()))
                .collect(),
            _ => (0..self.items.len()).collect(),
        }
    }

    fn selected_index(&self) -> Option<usize> {
        self.visible().get(self.cursor).copied()
    }

    fn key(&mut self, k: &KeyEvent) -> Outcome {
        let n = self.visible().len();
        let preview = |s: &Self| match (&s.kind, s.selected_index()) {
            (PickKind::Theme { .. }, Some(i)) => Outcome::Previewed(s.kind.clone(), i),
            _ => Outcome::None,
        };
        match k.code {
            KeyCode::Esc => return Outcome::Cancelled(self.kind.clone()),
            KeyCode::Enter => {
                return self
                    .selected_index()
                    .map_or(Outcome::Cancelled(self.kind.clone()), |i| Outcome::Picked(self.kind.clone(), i));
            }
            KeyCode::Down => {
                if self.cursor + 1 < n {
                    self.cursor += 1;
                }
                return preview(self);
            }
            KeyCode::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                return preview(self);
            }
            KeyCode::Char('j') if self.query.is_none() => {
                if self.cursor + 1 < n {
                    self.cursor += 1;
                }
                return preview(self);
            }
            KeyCode::Char('k') if self.query.is_none() => {
                self.cursor = self.cursor.saturating_sub(1);
                return preview(self);
            }
            KeyCode::Char(c) if self.query.is_none() => {
                // confirm dialogs: y / n
                if matches!(self.kind, PickKind::ConfirmDeletePlaylist(_)) {
                    match c {
                        'y' => return Outcome::Picked(self.kind.clone(), 0),
                        'n' => return Outcome::Cancelled(self.kind.clone()),
                        _ => {}
                    }
                }
            }
            _ => {
                if let Some(q) = self.query.as_mut() {
                    let mut f = TextField { value: std::mem::take(q) };
                    if f.key(k) {
                        self.cursor = 0;
                    }
                    *q = f.value;
                }
            }
        }
        Outcome::None
    }
}

// ── the enum the app holds ────────────────────────────────────────────
pub enum Modal {
    Login(Login),
    Input(InputModal),
    Search(SearchModal),
    Help { scroll: usize },
    Lyrics(LyricsModal),
    Stats { summary: Summary, scroll: usize },
    Picker(Picker),
}

impl Modal {
    pub fn key(&mut self, k: &KeyEvent) -> Outcome {
        match self {
            Modal::Login(m) => m.key(k),
            Modal::Search(m) => m.key(k),
            Modal::Picker(m) => m.key(k),
            Modal::Input(m) => match k.code {
                KeyCode::Esc => Outcome::Close,
                KeyCode::Enter => {
                    let v = m.field.value.trim().to_string();
                    if v.is_empty() {
                        Outcome::Close
                    } else {
                        Outcome::Input(m.purpose.clone(), v)
                    }
                }
                _ => {
                    m.field.key(k);
                    Outcome::None
                }
            },
            Modal::Help { scroll } | Modal::Stats { scroll, .. } => {
                match k.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => return Outcome::Close,
                    KeyCode::Down | KeyCode::Char('j') => *scroll += 1,
                    KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
                    KeyCode::PageDown => *scroll += 10,
                    KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
                    _ => {}
                }
                Outcome::None
            }
            Modal::Lyrics(m) => {
                match k.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('L') => return Outcome::Close,
                    KeyCode::Down | KeyCode::Char('j') => m.scroll += 1,
                    KeyCode::Up | KeyCode::Char('k') => m.scroll = m.scroll.saturating_sub(1),
                    _ => {}
                }
                Outcome::None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    #[test]
    fn fuzzy_substring_and_subsequence() {
        assert!(fuzzy("Bob Dylan", "dyln"));
        assert!(fuzzy("play / pause", "pause"));
        assert!(!fuzzy("abc", "cba"));
    }

    #[test]
    fn picker_filters_and_maps_back_to_original_index() {
        let items = vec!["play".into(), "next track".into(), "quit".into()];
        let mut p = Picker::new("cmd", items, PickKind::Command(vec![Action::PlayPause, Action::NextTrack, Action::Quit]), true);
        for c in "qu".chars() {
            p.key(&key(KeyCode::Char(c)));
        }
        assert_eq!(p.visible(), vec![2]);
        match p.key(&key(KeyCode::Enter)) {
            Outcome::Picked(_, 2) => {}
            _ => panic!("expected original index 2"),
        }
    }

    #[test]
    fn search_skips_headers_and_requires_down_to_enter_list() {
        let mut s = SearchModal::default();
        s.set_results(SearchResults {
            songs: vec![Song { id: "1".into(), ..Default::default() }],
            albums: vec![Album { id: "a".into(), ..Default::default() }],
            artists: vec![],
        });
        assert_eq!(s.rows[s.cursor], SearchRow::Song(0));
        s.key(&key(KeyCode::Down));
        assert!(s.in_list);
        s.key(&key(KeyCode::Down));
        assert_eq!(s.rows[s.cursor], SearchRow::Album(0), "header skipped");
    }

    #[test]
    fn typing_in_search_emits_a_query() {
        let mut s = SearchModal::default();
        match s.key(&key(KeyCode::Char('a'))) {
            Outcome::SearchQuery(q) => assert_eq!(q, "a"),
            _ => panic!(),
        }
    }

    #[test]
    fn login_requires_all_fields() {
        let mut l = Login::new("https://x", "me");
        assert_eq!(l.focus, 1);
        assert!(matches!(l.key(&key(KeyCode::Enter)), Outcome::None));
        assert_eq!(l.focus, 2, "focus jumps to the empty password field");
        for c in "pw".chars() {
            l.key(&key(KeyCode::Char(c)));
        }
        assert!(matches!(l.key(&key(KeyCode::Enter)), Outcome::Login(..)));
    }

    #[test]
    fn lyrics_current_line_tracks_position() {
        let m = LyricsModal {
            title: String::new(),
            synced: Some(vec![(1.0, "a".into()), (5.0, "b".into())]),
            text: String::new(),
            scroll: 0,
        };
        assert_eq!(m.current_line(0.5), None);
        assert_eq!(m.current_line(6.0), Some(1));
    }
}
