//! XDG-flavoured state, cache and config — the cache-first doctrine.
//!
//! * `~/.config/rustdrome/config.toml`  server credentials (chmod 600)
//! * `~/.config/rustdrome/player.toml`  optional settings + keybinds
//! * `~/.local/state/rustdrome/state.json`
//! * `~/.cache/rustdrome/*.json`, `art/`, `audio/`
//!
//! **Legacy fallback:** if there is no `rustdrome` config but a `navitui` one
//! exists (the Python NaviTui, or an early build of this port), all three
//! locations transparently stay on the `navitui` paths, so nobody is logged
//! out or loses their queue, cache and offline downloads by upgrading.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{de::DeserializeOwned, Serialize};
use serde_json::{Map, Value};

pub const APP: &str = "rustdrome";
const LEGACY_APP: &str = "navitui";

/// `RUSTDROME_<name>` (falling back to the old `NAVITUI_<name>`), empty = unset.
pub fn env(name: &str) -> Option<String> {
    [format!("RUSTDROME_{name}"), format!("NAVITUI_{name}")]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        .filter(|v| !v.is_empty())
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[derive(Debug, Clone)]
pub struct AppDirs {
    pub state_file: PathBuf,
    pub cache_dir: PathBuf,
    pub config_file: PathBuf,
}

impl AppDirs {
    pub fn new() -> Self {
        Self::under(&home())
    }

    /// Resolve the three locations beneath `home` (split out for testing).
    pub fn under(h: &Path) -> Self {
        let at = |app: &str| Self {
            state_file: h.join(".local/state").join(app).join("state.json"),
            cache_dir: h.join(".cache").join(app),
            config_file: h.join(".config").join(app).join("config.toml"),
        };
        let fresh = at(APP);
        let legacy = at(LEGACY_APP);
        if !fresh.config_file.exists() && legacy.config_file.exists() {
            legacy
        } else {
            fresh
        }
    }

    pub fn config_dir(&self) -> &Path {
        self.config_file.parent().unwrap_or(Path::new("."))
    }
    pub fn art_dir(&self) -> PathBuf {
        self.cache_dir.join("art")
    }
    pub fn audio_dir(&self) -> PathBuf {
        self.cache_dir.join("audio")
    }

    // ── state ─────────────────────────────────────────────────────────
    pub fn load_state(&self) -> Map<String, Value> {
        fs::read_to_string(&self.state_file)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default()
    }

    /// MERGES a patch over what's on disk — callers never clobber keys owned
    /// by other features.
    pub fn save_state(&self, patch: Value) {
        let mut data = self.load_state();
        if let Value::Object(p) = patch {
            for (k, v) in p {
                data.insert(k, v);
            }
        }
        if let Some(dir) = self.state_file.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(&self.state_file, Value::Object(data).to_string());
    }

    // ── cache ─────────────────────────────────────────────────────────
    /// Parse straight from the file: no intermediate `String` the size of
    /// the whole library.
    pub fn read_cache<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        let f = fs::File::open(self.cache_dir.join(format!("{name}.json"))).ok()?;
        serde_json::from_reader(std::io::BufReader::new(f)).ok()
    }

    /// Stream-serialise to a temp file and rename it into place, so neither a
    /// big in-memory copy nor a half-written file (crash, power cut) can
    /// happen. Takes `?Sized` so borrowed views can be written without cloning.
    pub fn write_cache<T: Serialize + ?Sized>(&self, name: &str, data: &T) {
        if fs::create_dir_all(&self.cache_dir).is_err() {
            return;
        }
        let path = self.cache_dir.join(format!("{name}.json"));
        let tmp = self.cache_dir.join(format!("{name}.json.tmp"));
        let ok = fs::File::create(&tmp)
            .and_then(|f| {
                let mut w = std::io::BufWriter::new(f);
                serde_json::to_writer(&mut w, data).map_err(std::io::Error::from)?;
                std::io::Write::flush(&mut w)
            })
            .is_ok();
        if ok {
            let _ = fs::rename(&tmp, &path);
        } else {
            let _ = fs::remove_file(&tmp);
        }
    }

    // ── credentials ───────────────────────────────────────────────────
    pub fn load_credentials(&self) -> Option<Credentials> {
        let raw = fs::read_to_string(&self.config_file).ok()?;
        let t: toml::Table = raw.parse().ok()?;
        let get = |k: &str| t.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let c = Credentials {
            server: get("server"),
            username: get("username"),
            token: get("token"),
            salt: get("salt"),
        };
        (!c.server.is_empty() && !c.username.is_empty() && !c.token.is_empty() && !c.salt.is_empty())
            .then_some(c)
    }

    /// Partial credentials (server + username) to pre-fill the login form.
    pub fn load_login_hint(&self) -> (String, String) {
        let raw = fs::read_to_string(&self.config_file).unwrap_or_default();
        let t: toml::Table = raw.parse().unwrap_or_default();
        let get = |k: &str| t.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        (get("server"), get("username"))
    }

    pub fn save_credentials(&self, c: &Credentials) -> std::io::Result<()> {
        fs::create_dir_all(self.config_dir())?;
        let body = format!(
            "server = {:?}\nusername = {:?}\ntoken = {:?}\nsalt = {:?}\n",
            c.server, c.username, c.token, c.salt
        );
        fs::write(&self.config_file, body)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.config_file, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}

impl Default for AppDirs {
    fn default() -> Self {
        Self::new()
    }
}

/// The password is never stored — only the salted token.
#[derive(Debug, Clone)]
pub struct Credentials {
    pub server: String,
    pub username: String,
    pub token: String,
    pub salt: String,
}

// ── player.toml ───────────────────────────────────────────────────────

/// action id → default key(s), comma-separated for aliases. The key names are
/// Textual's, so an existing `player.toml` keeps working untouched.
pub const KEYBINDS: &[(&str, &str)] = &[
    ("play_pause", "space"),
    ("next_track", "n"),
    ("prev_track", "b"),
    ("search", "slash"),
    ("shuffle", "s"),
    ("repeat", "r"),
    ("filter", "backslash"),
    ("seek_back", "left"),
    ("seek_forward", "right"),
    ("seek_back_big", "shift+left"),
    ("seek_forward_big", "shift+right"),
    ("volume_down", "minus"),
    ("volume_up", "plus,equals_sign"),
    ("mute", "m"),
    ("speed", "greater_than_sign"),
    ("sleep_timer", "less_than_sign"),
    ("enqueue", "a"),
    ("play_next", "A"),
    ("queue_remove", "x"),
    ("queue_clear", "X"),
    ("queue_move_up", "ctrl+up"),
    ("queue_move_down", "ctrl+down"),
    ("star", "f"),
    ("select_mode", "v"),
    ("start_radio", "i"),
    ("radio_toggle", "I"),
    ("download", "d"),
    ("download_view", "D"),
    ("download_all", "ctrl+d"),
    ("offline_toggle", "O"),
    ("quality_cycle", "Q"),
    ("playlist_add", "p"),
    ("playlist_remove", "P"),
    ("playlist_move_up", "shift+up"),
    ("playlist_move_down", "shift+down"),
    ("playlist_rename", "ctrl+r"),
    ("playlist_delete", "ctrl+x"),
    ("queue_save", "ctrl+s"),
    ("lyrics", "L"),
    ("stats", "ctrl+w"),
    ("share", "S"),
    ("go_album", "e"),
    ("go_artist", "E"),
    ("genres", "y"),
    ("bookmark", "w"),
    ("bookmarks", "W"),
    ("notifications", "N"),
    ("panel_prev", "h"),
    ("panel_next", "l"),
    ("refresh", "R"),
    ("theme_cycle", "t"),
    ("theme_pick", "T"),
    ("border_style", "B"),
    ("layout_menu", "ctrl+l"),
    ("layout_edit", "ctrl+e"),
    ("zen", "z"),
    ("private_mode", "ctrl+y"),
    ("command_palette", "ctrl+p"),
    ("help", "question_mark"),
    ("quit", "q"),
];

#[derive(Debug, Clone)]
pub struct Settings {
    pub replaygain: String, // album | track | no
    pub gapless: String,    // yes | weak | no
    pub max_bitrate: i64,
    pub stream_format: String,
    pub crossfade: f64,
    pub notifications: bool,
    pub art_theming: bool,
    pub replaygain_preamp: f64,
    pub replaygain_fallback: f64,
    pub audio_exclusive: bool,
    pub pipewire_buffer: i64,
    /// `rounded` (default) or `square`
    pub border_style: String,
    pub keybinds: HashMap<String, String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            replaygain: "album".into(),
            gapless: "weak".into(),
            max_bitrate: 0,
            stream_format: String::new(),
            crossfade: 0.0,
            notifications: true,
            art_theming: true,
            replaygain_preamp: 0.0,
            replaygain_fallback: 0.0,
            audio_exclusive: false,
            pipewire_buffer: 0,
            border_style: "rounded".into(),
            keybinds: KEYBINDS
                .iter()
                .map(|(a, k)| (a.to_string(), k.to_string()))
                .collect(),
        }
    }
}

impl Settings {
    /// Defaults overlaid with whatever player.toml sets. Unknown keys are
    /// ignored and a malformed file falls back to defaults, so typos can't
    /// crash startup.
    pub fn load(config_dir: &Path) -> Self {
        let mut cfg = Self::default();
        let Ok(raw) = fs::read_to_string(config_dir.join("player.toml")) else {
            return cfg;
        };
        let Ok(t) = raw.parse::<toml::Table>() else {
            return cfg;
        };
        cfg.apply(&t);
        cfg
    }

    fn apply(&mut self, t: &toml::Table) {
        use toml::Value as V;
        let str_in = |v: &V, ok: &[&str]| v.as_str().filter(|s| ok.contains(s)).map(String::from);
        let num = |v: &V| v.as_float().or_else(|| v.as_integer().map(|i| i as f64));
        for (key, value) in t {
            match key.as_str() {
                "replaygain" => {
                    if let Some(s) = str_in(value, &["album", "track", "no"]) {
                        self.replaygain = s;
                    }
                }
                "gapless" => {
                    if let Some(s) = str_in(value, &["yes", "weak", "no"]) {
                        self.gapless = s;
                    }
                }
                "max_bitrate" => {
                    if let Some(i) = value.as_integer() {
                        self.max_bitrate = i.max(0);
                    }
                }
                "stream_format" => {
                    if let Some(s) = value.as_str() {
                        self.stream_format = s.to_string();
                    }
                }
                "crossfade" => {
                    if let Some(f) = num(value) {
                        self.crossfade = f.max(0.0);
                    }
                }
                "notifications" => {
                    if let Some(b) = value.as_bool() {
                        self.notifications = b;
                    }
                }
                "art_theming" => {
                    if let Some(b) = value.as_bool() {
                        self.art_theming = b;
                    }
                }
                "replaygain_preamp" => {
                    if let Some(f) = num(value) {
                        self.replaygain_preamp = f;
                    }
                }
                "replaygain_fallback" => {
                    if let Some(f) = num(value) {
                        self.replaygain_fallback = f;
                    }
                }
                "audio_exclusive" => {
                    if let Some(b) = value.as_bool() {
                        self.audio_exclusive = b;
                    }
                }
                "pipewire_buffer" => {
                    if let Some(i) = value.as_integer() {
                        self.pipewire_buffer = i.max(0);
                    }
                }
                "border_style" => {
                    if let Some(s) = str_in(value, &["rounded", "square"]) {
                        self.border_style = s;
                    }
                }
                "keybinds" => {
                    if let Some(kb) = value.as_table() {
                        for (action, keys) in kb {
                            if let Some(k) = keys.as_str().filter(|k| !k.is_empty()) {
                                if KEYBINDS.iter().any(|(a, _)| a == action) {
                                    self.keybinds.insert(action.clone(), k.to_string());
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Drop a commented template next to the credentials, once.
    pub fn write_template(config_dir: &Path) {
        let path = config_dir.join("player.toml");
        if path.exists() {
            return;
        }
        let keybinds: String = KEYBINDS
            .iter()
            .map(|(a, k)| format!("#{a} = \"{k}\"\n"))
            .collect();
        let body = format!("{TEMPLATE}{keybinds}");
        if fs::create_dir_all(config_dir).is_ok() {
            let _ = fs::write(path, body);
        }
    }
}

const TEMPLATE: &str = r#"# Rustdrome player settings — restart the app after editing.

# ReplayGain mode: "album", "track", or "no"
#replaygain = "album"

# Gapless playback: "yes", "weak" (default; gapless when formats match), "no"
#gapless = "weak"

# Transcode cap for streaming on low bandwidth. Applies to network streams
# only — offline pins (d / D / ctrl+d) always keep the original file.
# max_bitrate: cap in kbps, 0 = original/unlimited. stream_format: transcode
# target ("mp3", "opus", "raw", …) or "" for the server default/original.
# Cycle presets at runtime with the quality keybind (see [keybinds] below).
#max_bitrate = 0
#stream_format = ""

# Crossfade: seconds of soft volume fade-out/fade-in around a track change
# (0 = off, the default). Your set volume is restored exactly afterwards.
#crossfade = 0.0

# Desktop notification on track change (toggle at runtime with N)
#notifications = true

# Tint the UI with a color pulled from the current song's cover art.
# Truecolor terminals only; the "system" (ANSI) theme leaves this inert.
#art_theming = true

# ReplayGain fine-tuning (in dB). preamp is applied on top of the ReplayGain
# gain; fallback is the gain substituted for tracks that carry no RG tags.
#replaygain_preamp = 0.0
#replaygain_fallback = -6.0

# Take exclusive/direct control of the audio device.
#audio_exclusive = false

# Audio buffer in ms (0 = mpv default). A small value like 150 can smooth
# Bluetooth playback.
#pipewire_buffer = 0

# Box style: "rounded" (default) or "square" corners. Toggle at runtime with
# the border_style keybind (B); the last choice made in the app is remembered.
#border_style = "rounded"

# Remap any key (Textual key names: space, slash, ctrl+up, shift+left, …).
# Action ids and defaults:
#[keybinds]
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("rustdrome-cfg-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn cache_round_trips_atomically_and_leaves_no_temp_file() {
        let h = tmp("cache");
        let d = AppDirs::under(&h);
        let songs: Vec<String> = (0..1000).map(|i| format!("song {i}")).collect();
        d.write_cache("x", &songs[..]); // borrowed slice, unsized
        let back: Vec<String> = d.read_cache("x").unwrap();
        assert_eq!(back, songs);
        assert!(!d.cache_dir.join("x.json.tmp").exists());
        assert!(d.read_cache::<Vec<String>>("missing").is_none());
        // a corrupt file reads as "no cache", not a crash
        fs::write(d.cache_dir.join("bad.json"), "{not json").unwrap();
        assert!(d.read_cache::<Vec<String>>("bad").is_none());
    }

    #[test]
    fn fresh_install_uses_rustdrome_paths() {
        let h = tmp("fresh");
        let d = AppDirs::under(&h);
        assert!(d.config_file.ends_with(".config/rustdrome/config.toml"));
        assert!(d.cache_dir.ends_with(".cache/rustdrome"));
        assert!(d.state_file.ends_with(".local/state/rustdrome/state.json"));
    }

    #[test]
    fn existing_navitui_install_stays_on_legacy_paths() {
        let h = tmp("legacy");
        fs::create_dir_all(h.join(".config/navitui")).unwrap();
        fs::write(h.join(".config/navitui/config.toml"), "server=\"x\"").unwrap();
        let d = AppDirs::under(&h);
        assert!(d.config_file.ends_with(".config/navitui/config.toml"));
        assert!(d.cache_dir.ends_with(".cache/navitui"), "cache follows the config");
        // once a rustdrome config exists it wins
        fs::create_dir_all(h.join(".config/rustdrome")).unwrap();
        fs::write(h.join(".config/rustdrome/config.toml"), "server=\"y\"").unwrap();
        assert!(AppDirs::under(&h).config_file.ends_with(".config/rustdrome/config.toml"));
    }

    #[test]
    fn overrides_apply_and_unknown_keys_are_ignored() {
        let mut s = Settings::default();
        let t: toml::Table = r#"
            replaygain = "track"
            gapless = "bogus"
            border_style = "square"
            crossfade = 2
            nonsense = 1
            [keybinds]
            next_track = "ctrl+n"
            not_an_action = "x"
        "#
        .parse()
        .unwrap();
        s.apply(&t);
        assert_eq!(s.replaygain, "track");
        assert_eq!(s.gapless, "weak", "invalid enum values are rejected");
        assert_eq!(s.border_style, "square");
        assert_eq!(s.crossfade, 2.0, "bare ints are accepted for float knobs");
        assert_eq!(s.keybinds["next_track"], "ctrl+n");
        assert!(!s.keybinds.contains_key("not_an_action"));
    }
}
