//! Domain models — thin structs over the Subsonic JSON shapes.
//!
//! Field names deliberately match the Python implementation's dataclasses so
//! the JSON cache under `~/.cache/rustdrome` stays readable by the Python NaviTui.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── JSON helpers ──────────────────────────────────────────────────────

/// Subsonic ids arrive as strings or numbers depending on the server.
fn id_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn opt_id(v: &Value, key: &str) -> Option<String> {
    match v.get(key) {
        Some(Value::Null) | None => None,
        Some(x) => {
            let s = id_of(x);
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        }
    }
}

fn string(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn opt_string(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn int(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn opt_int(v: &Value, key: &str) -> Option<i64> {
    v.get(key).and_then(Value::as_i64)
}

/// `starred` is a timestamp string when set and absent otherwise.
fn truthy(v: &Value, key: &str) -> bool {
    match v.get(key) {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

// ── models ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Artist {
    pub id: String,
    pub name: String,
    pub album_count: i64,
    pub cover_art: Option<String>,
    pub starred: bool,
}

impl Artist {
    pub fn from_api(d: &Value) -> Self {
        Self {
            id: d.get("id").map(id_of).unwrap_or_default(),
            name: opt_string(d, "name").unwrap_or_else(|| "?".into()),
            album_count: int(d, "albumCount"),
            cover_art: opt_string(d, "coverArt"),
            starred: truthy(d, "starred"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Album {
    pub id: String,
    pub name: String,
    pub artist: String,
    pub artist_id: Option<String>,
    pub year: Option<i64>,
    pub song_count: i64,
    pub duration: i64,
    pub cover_art: Option<String>,
    pub starred: bool,
}

impl Album {
    pub fn from_api(d: &Value) -> Self {
        Self {
            id: d.get("id").map(id_of).unwrap_or_default(),
            name: opt_string(d, "name")
                .or_else(|| opt_string(d, "title"))
                .unwrap_or_else(|| "?".into()),
            artist: string(d, "artist"),
            artist_id: opt_id(d, "artistId"),
            year: opt_int(d, "year"),
            song_count: int(d, "songCount"),
            duration: int(d, "duration"),
            cover_art: opt_string(d, "coverArt"),
            starred: truthy(d, "starred"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Song {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_id: Option<String>,
    pub artist_id: Option<String>,
    pub track: Option<i64>,
    pub disc: Option<i64>,
    pub year: Option<i64>,
    pub duration: i64,
    pub cover_art: Option<String>,
    pub suffix: String,
    pub bit_rate: Option<i64>,
    pub starred: bool,
    pub user_rating: i64,
    /// A direct playable URL, set for internet-radio stations mpv opens
    /// straight. Never comes from song JSON.
    pub stream_url: Option<String>,
}

impl Song {
    pub fn from_api(d: &Value) -> Self {
        Self {
            id: d.get("id").map(id_of).unwrap_or_default(),
            title: opt_string(d, "title").unwrap_or_else(|| "?".into()),
            artist: string(d, "artist"),
            album: string(d, "album"),
            album_id: opt_id(d, "albumId"),
            artist_id: opt_id(d, "artistId"),
            track: opt_int(d, "track"),
            disc: opt_int(d, "discNumber"),
            year: opt_int(d, "year"),
            duration: int(d, "duration"),
            cover_art: opt_string(d, "coverArt"),
            suffix: string(d, "suffix"),
            bit_rate: opt_int(d, "bitRate"),
            starred: truthy(d, "starred"),
            user_rating: int(d, "userRating"),
            stream_url: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub song_count: i64,
    pub duration: i64,
    pub owner: String,
}

impl Playlist {
    pub fn from_api(d: &Value) -> Self {
        Self {
            id: d.get("id").map(id_of).unwrap_or_default(),
            name: opt_string(d, "name").unwrap_or_else(|| "?".into()),
            song_count: int(d, "songCount"),
            duration: int(d, "duration"),
            owner: string(d, "owner"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Genre {
    pub name: String,
    pub song_count: i64,
    pub album_count: i64,
}

impl Genre {
    pub fn from_api(d: &Value) -> Self {
        Self {
            // getGenres returns the name in "value"
            name: opt_string(d, "value")
                .or_else(|| opt_string(d, "name"))
                .unwrap_or_else(|| "?".into()),
            song_count: int(d, "songCount"),
            album_count: int(d, "albumCount"),
        }
    }
}

/// A saved resume point in a track: the song plus a position in ms.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bookmark {
    pub song: Song,
    pub position_ms: i64,
    pub comment: String,
}

impl Bookmark {
    pub fn from_api(d: &Value) -> Self {
        Self {
            song: d.get("entry").map(Song::from_api).unwrap_or_default(),
            position_ms: int(d, "position"),
            comment: string(d, "comment"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PodcastChannel {
    pub id: String,
    pub title: String,
    pub cover_art: Option<String>,
    pub episode_count: i64,
}

impl PodcastChannel {
    pub fn from_api(d: &Value) -> Self {
        Self {
            id: d.get("id").map(id_of).unwrap_or_default(),
            title: opt_string(d, "title")
                .or_else(|| opt_string(d, "url"))
                .unwrap_or_else(|| "?".into()),
            cover_art: opt_string(d, "coverArt"),
            episode_count: d
                .get("episode")
                .and_then(Value::as_array)
                .map_or(0, |a| a.len() as i64),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RadioStation {
    pub id: String,
    pub name: String,
    pub stream_url: String,
    pub home_url: String,
}

impl RadioStation {
    pub fn from_api(d: &Value) -> Self {
        Self {
            id: d.get("id").map(id_of).unwrap_or_default(),
            name: opt_string(d, "name").unwrap_or_else(|| "?".into()),
            stream_url: string(d, "streamUrl"),
            home_url: opt_string(d, "homePageUrl")
                .or_else(|| opt_string(d, "homepageUrl"))
                .unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SearchResults {
    pub artists: Vec<Artist>,
    pub albums: Vec<Album>,
    pub songs: Vec<Song>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn song_from_api_handles_numeric_ids_and_starred_timestamp() {
        let s = Song::from_api(&json!({
            "id": 42, "title": "T", "albumId": "9", "duration": 201,
            "starred": "2024-01-01T00:00:00Z", "userRating": 4
        }));
        assert_eq!(s.id, "42");
        assert_eq!(s.album_id.as_deref(), Some("9"));
        assert!(s.starred);
        assert_eq!(s.user_rating, 4);
        assert_eq!(s.duration, 201);
    }

    #[test]
    fn song_round_trips_through_python_style_cache_json() {
        // exactly what Python's `asdict(Song)` writes
        let raw = r#"{"id":"a","title":"x","artist":"","album":"","album_id":null,
            "artist_id":null,"track":null,"disc":null,"year":null,"duration":10,
            "cover_art":null,"suffix":"mp3","bit_rate":null,"starred":false,
            "user_rating":0,"stream_url":null}"#;
        let s: Song = serde_json::from_str(raw).unwrap();
        assert_eq!(s.suffix, "mp3");
        let back = serde_json::to_string(&s).unwrap();
        let again: Song = serde_json::from_str(&back).unwrap();
        assert_eq!(s, again);
    }

    #[test]
    fn genre_uses_value_key() {
        let g = Genre::from_api(&json!({"value":"Jazz","songCount":3}));
        assert_eq!(g.name, "Jazz");
        assert_eq!(g.song_count, 3);
    }
}
