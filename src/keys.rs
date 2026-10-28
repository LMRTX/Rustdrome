//! Key specs (Textual's names, so existing `player.toml` files work) and the
//! `Action` every binding resolves to.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::Settings;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeySpec {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

fn named_char(name: &str) -> Option<char> {
    Some(match name {
        "space" => ' ',
        "slash" => '/',
        "backslash" => '\\',
        "minus" | "hyphen_minus" => '-',
        "plus" | "plus_sign" => '+',
        "equals_sign" | "equal" => '=',
        "greater_than_sign" => '>',
        "less_than_sign" => '<',
        "question_mark" => '?',
        "comma" => ',',
        "full_stop" | "period" => '.',
        "colon" => ':',
        "semicolon" => ';',
        "left_square_bracket" => '[',
        "right_square_bracket" => ']',
        "apostrophe" => '\'',
        "quotation_mark" => '"',
        "exclamation_mark" => '!',
        "at" | "commercial_at" => '@',
        "number_sign" => '#',
        "asterisk" => '*',
        "underscore" => '_',
        "tilde" => '~',
        "grave_accent" => '`',
        _ => return None,
    })
}

impl KeySpec {
    fn parse_one(spec: &str) -> Option<Self> {
        let spec = spec.trim();
        if spec.is_empty() {
            return None;
        }
        let (mut ctrl, mut alt, mut shift) = (false, false, false);
        let mut parts: Vec<&str> = spec.split('+').collect();
        // a literal "+" key splits to ["", ""]; Textual spells it "plus"
        let name = parts.pop().filter(|n| !n.is_empty())?;
        for m in parts {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => ctrl = true,
                "alt" | "meta" => alt = true,
                "shift" => shift = true,
                _ => return None,
            }
        }
        let lower = name.to_ascii_lowercase();
        let code = match lower.as_str() {
            "enter" | "return" => KeyCode::Enter,
            "escape" | "esc" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "backspace" => KeyCode::Backspace,
            "delete" => KeyCode::Delete,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            f if f.len() > 1 && f.starts_with('f') && f[1..].parse::<u8>().is_ok() => {
                KeyCode::F(f[1..].parse().ok()?)
            }
            _ => {
                if let Some(c) = named_char(&lower) {
                    KeyCode::Char(c)
                } else {
                    let mut it = name.chars();
                    let c = it.next()?;
                    if it.next().is_some() {
                        return None;
                    }
                    KeyCode::Char(c)
                }
            }
        };
        // an uppercase letter *is* the shifted key; don't track SHIFT twice
        let shift = match code {
            KeyCode::Char(_) => false,
            _ => shift,
        };
        Some(Self { code, ctrl, alt, shift })
    }

    /// `"plus,equals_sign"` → two specs.
    pub fn parse_list(spec: &str) -> Vec<Self> {
        spec.split(',').filter_map(Self::parse_one).collect()
    }

    pub fn matches(&self, ev: &KeyEvent) -> bool {
        let m = ev.modifiers;
        let ctrl = m.contains(KeyModifiers::CONTROL);
        let alt = m.contains(KeyModifiers::ALT);
        if ctrl != self.ctrl || alt != self.alt {
            return false;
        }
        match (self.code, ev.code) {
            (KeyCode::Char(a), KeyCode::Char(b)) => {
                if self.ctrl {
                    a.eq_ignore_ascii_case(&b)
                } else {
                    a == b
                }
            }
            (a, b) => a == b && m.contains(KeyModifiers::SHIFT) == self.shift,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    PlayPause,
    NextTrack,
    PrevTrack,
    Search,
    Shuffle,
    Repeat,
    Filter,
    SeekBack,
    SeekForward,
    SeekBackBig,
    SeekForwardBig,
    VolumeDown,
    VolumeUp,
    Mute,
    Speed,
    SleepTimer,
    Enqueue,
    PlayNext,
    QueueRemove,
    QueueClear,
    QueueMoveUp,
    QueueMoveDown,
    Star,
    SelectMode,
    StartRadio,
    RadioToggle,
    Download,
    DownloadView,
    DownloadAll,
    OfflineToggle,
    QualityCycle,
    PlaylistAdd,
    PlaylistRemove,
    PlaylistMoveUp,
    PlaylistMoveDown,
    PlaylistRename,
    PlaylistDelete,
    QueueSave,
    Lyrics,
    Stats,
    Share,
    GoAlbum,
    GoArtist,
    Genres,
    Bookmark,
    Bookmarks,
    Notifications,
    PanelPrev,
    PanelNext,
    Refresh,
    ThemeCycle,
    ThemePick,
    BorderStyle,
    LayoutMenu,
    LayoutEdit,
    LayoutReset,
    Zen,
    PrivateMode,
    CommandPalette,
    Help,
    Quit,
}

impl Action {
    pub fn from_id(id: &str) -> Option<Self> {
        use Action::*;
        Some(match id {
            "play_pause" => PlayPause,
            "next_track" => NextTrack,
            "prev_track" => PrevTrack,
            "search" => Search,
            "shuffle" => Shuffle,
            "repeat" => Repeat,
            "filter" => Filter,
            "seek_back" => SeekBack,
            "seek_forward" => SeekForward,
            "seek_back_big" => SeekBackBig,
            "seek_forward_big" => SeekForwardBig,
            "volume_down" => VolumeDown,
            "volume_up" => VolumeUp,
            "mute" => Mute,
            "speed" => Speed,
            "sleep_timer" => SleepTimer,
            "enqueue" => Enqueue,
            "play_next" => PlayNext,
            "queue_remove" => QueueRemove,
            "queue_clear" => QueueClear,
            "queue_move_up" => QueueMoveUp,
            "queue_move_down" => QueueMoveDown,
            "star" => Star,
            "select_mode" => SelectMode,
            "start_radio" => StartRadio,
            "radio_toggle" => RadioToggle,
            "download" => Download,
            "download_view" => DownloadView,
            "download_all" => DownloadAll,
            "offline_toggle" => OfflineToggle,
            "quality_cycle" => QualityCycle,
            "playlist_add" => PlaylistAdd,
            "playlist_remove" => PlaylistRemove,
            "playlist_move_up" => PlaylistMoveUp,
            "playlist_move_down" => PlaylistMoveDown,
            "playlist_rename" => PlaylistRename,
            "playlist_delete" => PlaylistDelete,
            "queue_save" => QueueSave,
            "lyrics" => Lyrics,
            "stats" => Stats,
            "share" => Share,
            "go_album" => GoAlbum,
            "go_artist" => GoArtist,
            "genres" => Genres,
            "bookmark" => Bookmark,
            "bookmarks" => Bookmarks,
            "notifications" => Notifications,
            "panel_prev" => PanelPrev,
            "panel_next" => PanelNext,
            "refresh" => Refresh,
            "theme_cycle" => ThemeCycle,
            "theme_pick" => ThemePick,
            "border_style" => BorderStyle,
            "layout_menu" => LayoutMenu,
            "layout_edit" => LayoutEdit,
            "layout_reset" => LayoutReset,
            "zen" => Zen,
            "private_mode" => PrivateMode,
            "command_palette" => CommandPalette,
            "help" => Help,
            "quit" => Quit,
            _ => return None,
        })
    }
}

/// The resolved binding table, built once from `Settings`.
pub struct Keymap {
    binds: Vec<(KeySpec, Action)>,
}

impl Keymap {
    pub fn from_settings(s: &Settings) -> Self {
        let mut binds = Vec::new();
        for (id, keys) in &s.keybinds {
            if let Some(action) = Action::from_id(id) {
                for spec in KeySpec::parse_list(keys) {
                    binds.push((spec, action));
                }
            }
        }
        Self { binds }
    }

    pub fn resolve(&self, ev: &KeyEvent) -> Option<Action> {
        self.binds.iter().find(|(k, _)| k.matches(ev)).map(|(_, a)| *a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn parses_textual_names() {
        let k = KeySpec::parse_list("plus,equals_sign");
        assert_eq!(k.len(), 2);
        assert!(k[0].matches(&ev(KeyCode::Char('+'), KeyModifiers::SHIFT)));
        assert!(k[1].matches(&ev(KeyCode::Char('='), KeyModifiers::NONE)));
    }

    #[test]
    fn uppercase_letter_matches_shifted_char_only() {
        let a = KeySpec::parse_list("A")[0];
        assert!(a.matches(&ev(KeyCode::Char('A'), KeyModifiers::SHIFT)));
        assert!(!a.matches(&ev(KeyCode::Char('a'), KeyModifiers::NONE)));
    }

    #[test]
    fn ctrl_and_shift_arrows_are_distinct() {
        let ctrl_up = KeySpec::parse_list("ctrl+up")[0];
        let shift_up = KeySpec::parse_list("shift+up")[0];
        let ev_ctrl = ev(KeyCode::Up, KeyModifiers::CONTROL);
        let ev_shift = ev(KeyCode::Up, KeyModifiers::SHIFT);
        assert!(ctrl_up.matches(&ev_ctrl) && !ctrl_up.matches(&ev_shift));
        assert!(shift_up.matches(&ev_shift) && !shift_up.matches(&ev_ctrl));
        assert!(!shift_up.matches(&ev(KeyCode::Up, KeyModifiers::NONE)));
    }

    #[test]
    fn every_default_binding_resolves() {
        let s = Settings::default();
        let km = Keymap::from_settings(&s);
        for (id, keys) in crate::config::KEYBINDS {
            assert!(Action::from_id(id).is_some(), "unmapped action id {id}");
            assert!(!KeySpec::parse_list(keys).is_empty(), "unparseable key {keys}");
        }
        assert_eq!(
            km.resolve(&ev(KeyCode::Char(' '), KeyModifiers::NONE)),
            Some(Action::PlayPause)
        );
        assert_eq!(
            km.resolve(&ev(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            Some(Action::CommandPalette)
        );
    }
}
