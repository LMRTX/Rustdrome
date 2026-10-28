//! Async Subsonic/OpenSubsonic client for Navidrome.
//!
//! Auth is the salted-token scheme: we store `md5(password + salt)` and the
//! salt, never the password itself. Every call goes through `get`, which
//! unwraps the `subsonic-response` envelope and fails on `status != "ok"`.
//!
//! Cover art and audio are cached as files so anything you've already looked
//! at renders instantly and offline; `download_song` pins the original file
//! and `cached_stream` reports what is already on disk so playback can prefer
//! a local copy over the network.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use rand::Rng;
use reqwest::Url;
use serde_json::Value;

use crate::models::*;

pub const API_VERSION: &str = "1.16.1";
pub const CLIENT_NAME: &str = "rustdrome";

#[derive(Debug)]
pub enum ApiError {
    /// The server said no (bad auth, missing id, …).
    Subsonic(String),
    Http(reqwest::Error),
    Io(std::io::Error),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Subsonic(m) => write!(f, "{m}"),
            ApiError::Http(e) => write!(f, "{}", describe_http(e)),
            ApiError::Io(e) => write!(f, "{e}"),
        }
    }
}

fn describe_http(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "request timed out".into()
    } else if e.is_connect() {
        "can't reach server".into()
    } else if let Some(s) = e.status() {
        format!("server returned {s}")
    } else {
        e.to_string()
    }
}

impl std::error::Error for ApiError {}
impl From<reqwest::Error> for ApiError {
    fn from(e: reqwest::Error) -> Self {
        ApiError::Http(e)
    }
}
impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        ApiError::Io(e)
    }
}

impl ApiError {
    /// Transport-level failure (offline, DNS, timeout) as opposed to the
    /// server rejecting a request — only these get parked in the offline
    /// mutation queue.
    pub fn is_network(&self) -> bool {
        match self {
            ApiError::Http(e) => e.is_connect() || e.is_timeout() || e.is_request(),
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, ApiError>;

/// Return `(token, salt)` for the salted md5 auth scheme.
pub fn make_token(password: &str) -> (String, String) {
    let salt: String = (0..8).map(|_| format!("{:02x}", rand::thread_rng().gen::<u8>())).collect();
    let token = format!("{:x}", md5::compute(format!("{password}{salt}")));
    (token, salt)
}

pub fn normalize_server(url: &str) -> String {
    let url = url.trim().trim_end_matches('/').to_string();
    if !url.is_empty() && !url.contains("://") {
        format!("https://{url}")
    } else {
        url
    }
}

type Params = Vec<(String, String)>;

fn p(k: &str, v: impl ToString) -> (String, String) {
    (k.to_string(), v.to_string())
}

pub struct SubsonicClient {
    pub server: String,
    pub username: String,
    token: String,
    salt: String,
    art_dir: PathBuf,
    /// explicit downloads — kept until the user removes them
    audio_dir: PathBuf,
    /// look-ahead files — bounded, evicted automatically (`audio-cache/`)
    prefetch_dir: PathBuf,
    /// Streaming transcode cap `(kbps, format)`; `(0, "")` is original
    /// quality. Mutable so a runtime toggle retunes later streams.
    quality: Mutex<(i64, String)>,
    http: reqwest::Client,
}

impl SubsonicClient {
    pub fn new(
        server: &str,
        username: &str,
        token: &str,
        salt: &str,
        art_dir: PathBuf,
        audio_dir: PathBuf,
        max_bitrate: i64,
        stream_format: &str,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(format!("{CLIENT_NAME}/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("http client");
        Self {
            server: normalize_server(server),
            username: username.into(),
            token: token.into(),
            salt: salt.into(),
            art_dir,
            prefetch_dir: audio_dir.with_file_name("audio-cache"),
            audio_dir,
            quality: Mutex::new((max_bitrate, stream_format.into())),
            http,
        }
    }

    pub fn set_quality(&self, kbps: i64, format: &str) {
        *self.quality.lock().unwrap() = (kbps, format.to_string());
    }

    // ── plumbing ──────────────────────────────────────────────────────
    fn params(&self, extra: Params) -> Params {
        let mut v = vec![
            p("u", &self.username),
            p("t", &self.token),
            p("s", &self.salt),
            p("v", API_VERSION),
            p("c", CLIENT_NAME),
            p("f", "json"),
        ];
        v.extend(extra);
        v
    }

    async fn get(&self, endpoint: &str, extra: Params) -> Result<Value> {
        let url = format!("{}/rest/{endpoint}", self.server);
        let resp = self
            .http
            .get(url)
            .query(&self.params(extra))
            .send()
            .await?
            .error_for_status()?;
        let mut json: Value = resp.json().await?;
        let body = json
            .get_mut("subsonic-response")
            .map(Value::take)
            .unwrap_or(Value::Null);
        if body.get("status").and_then(Value::as_str) != Some("ok") {
            let err = body.get("error");
            let msg = err
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| {
                    format!(
                        "error {}",
                        err.and_then(|e| e.get("code")).map_or("?".into(), |c| c.to_string())
                    )
                });
            return Err(ApiError::Subsonic(msg));
        }
        Ok(body)
    }

    fn arr<'a>(v: &'a Value, path: &[&str]) -> &'a [Value] {
        let mut cur = v;
        for k in path {
            match cur.get(*k) {
                Some(n) => cur = n,
                None => return &[],
            }
        }
        cur.as_array().map_or(&[], Vec::as_slice)
    }

    // ── library ───────────────────────────────────────────────────────
    pub async fn ping(&self) -> Result<Value> {
        self.get("ping", vec![]).await
    }

    pub async fn get_artist_albums(&self, artist_id: &str) -> Result<Vec<Album>> {
        let b = self.get("getArtist", vec![p("id", artist_id)]).await?;
        Ok(Self::arr(&b, &["artist", "album"]).iter().map(Album::from_api).collect())
    }

    pub async fn get_album_songs(&self, album_id: &str) -> Result<Vec<Song>> {
        let b = self.get("getAlbum", vec![p("id", album_id)]).await?;
        Ok(Self::arr(&b, &["album", "song"]).iter().map(Song::from_api).collect())
    }

    /// list_type: newest | recent | frequent | random | starred | alphabeticalByName
    pub async fn get_album_list(&self, list_type: &str, size: u32) -> Result<Vec<Album>> {
        let b = self
            .get("getAlbumList2", vec![p("type", list_type), p("size", size), p("offset", 0)])
            .await?;
        Ok(Self::arr(&b, &["albumList2", "album"]).iter().map(Album::from_api).collect())
    }

    pub async fn get_playlists(&self) -> Result<Vec<Playlist>> {
        let b = self.get("getPlaylists", vec![]).await?;
        Ok(Self::arr(&b, &["playlists", "playlist"]).iter().map(Playlist::from_api).collect())
    }

    pub async fn create_playlist(&self, name: &str, song_ids: &[String]) -> Result<()> {
        let mut q = vec![p("name", name)];
        q.extend(song_ids.iter().map(|i| p("songId", i)));
        self.get("createPlaylist", q).await.map(drop)
    }

    pub async fn add_to_playlist(&self, playlist_id: &str, song_ids: &[String]) -> Result<()> {
        let mut q = vec![p("playlistId", playlist_id)];
        q.extend(song_ids.iter().map(|i| p("songIdToAdd", i)));
        self.get("updatePlaylist", q).await.map(drop)
    }

    /// Drop entries by zero-based position (indices into the playlist as it
    /// stands server-side).
    pub async fn remove_from_playlist(&self, playlist_id: &str, indices: &[usize]) -> Result<()> {
        let mut q = vec![p("playlistId", playlist_id)];
        q.extend(indices.iter().map(|i| p("songIndexToRemove", i)));
        self.get("updatePlaylist", q).await.map(drop)
    }

    pub async fn rename_playlist(&self, playlist_id: &str, name: &str) -> Result<()> {
        self.get("updatePlaylist", vec![p("playlistId", playlist_id), p("name", name)])
            .await
            .map(drop)
    }

    pub async fn delete_playlist(&self, playlist_id: &str) -> Result<()> {
        self.get("deletePlaylist", vec![p("id", playlist_id)]).await.map(drop)
    }

    /// Persist an arbitrary new order. `updatePlaylist` has no move verb, so
    /// the entry list is rebuilt in a single request: every current entry is
    /// removed (`songIndexToRemove` 0..n) and the ids re-added in the desired
    /// order. Removes apply before adds server-side, so the playlist never
    /// briefly empties and its name/owner are kept.
    pub async fn reorder_playlist(&self, playlist_id: &str, song_ids: &[String]) -> Result<()> {
        let mut q = vec![p("playlistId", playlist_id)];
        q.extend((0..song_ids.len()).map(|i| p("songIndexToRemove", i)));
        q.extend(song_ids.iter().map(|i| p("songIdToAdd", i)));
        self.get("updatePlaylist", q).await.map(drop)
    }

    pub async fn get_playlist_songs(&self, playlist_id: &str) -> Result<Vec<Song>> {
        let b = self.get("getPlaylist", vec![p("id", playlist_id)]).await?;
        Ok(Self::arr(&b, &["playlist", "entry"]).iter().map(Song::from_api).collect())
    }

    pub async fn get_starred(&self) -> Result<SearchResults> {
        let b = self.get("getStarred2", vec![]).await?;
        Ok(SearchResults {
            artists: Self::arr(&b, &["starred2", "artist"]).iter().map(Artist::from_api).collect(),
            albums: Self::arr(&b, &["starred2", "album"]).iter().map(Album::from_api).collect(),
            songs: Self::arr(&b, &["starred2", "song"]).iter().map(Song::from_api).collect(),
        })
    }

    pub async fn search(&self, query: &str, limit: u32) -> Result<SearchResults> {
        let b = self
            .get(
                "search3",
                vec![
                    p("query", query),
                    p("artistCount", limit),
                    p("albumCount", limit),
                    p("songCount", limit * 2),
                ],
            )
            .await?;
        Ok(SearchResults {
            artists: Self::arr(&b, &["searchResult3", "artist"]).iter().map(Artist::from_api).collect(),
            albums: Self::arr(&b, &["searchResult3", "album"]).iter().map(Album::from_api).collect(),
            songs: Self::arr(&b, &["searchResult3", "song"]).iter().map(Song::from_api).collect(),
        })
    }

    /// All genres, most-populated first so the picker leads with the genres
    /// worth browsing.
    pub async fn get_genres(&self) -> Result<Vec<Genre>> {
        let b = self.get("getGenres", vec![]).await?;
        let mut g: Vec<Genre> = Self::arr(&b, &["genres", "genre"]).iter().map(Genre::from_api).collect();
        g.sort_by(|a, b| b.song_count.cmp(&a.song_count));
        Ok(g)
    }

    pub async fn get_songs_by_genre(&self, genre: &str, count: u32) -> Result<Vec<Song>> {
        let b = self
            .get("getSongsByGenre", vec![p("genre", genre), p("count", count)])
            .await?;
        Ok(Self::arr(&b, &["songsByGenre", "song"]).iter().map(Song::from_api).collect())
    }

    pub async fn get_bookmarks(&self) -> Result<Vec<Bookmark>> {
        let b = self.get("getBookmarks", vec![]).await?;
        Ok(Self::arr(&b, &["bookmarks", "bookmark"]).iter().map(Bookmark::from_api).collect())
    }

    pub async fn create_bookmark(&self, song_id: &str, position_ms: i64) -> Result<()> {
        self.get("createBookmark", vec![p("id", song_id), p("position", position_ms)])
            .await
            .map(drop)
    }

    /// Songs similar to a song/artist id (the seed for endless radio).
    /// Prefers OpenSubsonic's `getSimilarSongs2`, falling back to the older
    /// `getSimilarSongs`; servers with neither just yield an empty list.
    pub async fn get_similar_songs(&self, item_id: &str, count: u32) -> Result<Vec<Song>> {
        for (endpoint, key) in [("getSimilarSongs2", "similarSongs2"), ("getSimilarSongs", "similarSongs")] {
            let Ok(b) = self.get(endpoint, vec![p("id", item_id), p("count", count)]).await else {
                continue;
            };
            let songs: Vec<Song> = Self::arr(&b, &[key, "song"]).iter().map(Song::from_api).collect();
            if !songs.is_empty() {
                return Ok(songs);
            }
        }
        Ok(vec![])
    }

    /// An artist's top tracks by name — a decent radio seed when
    /// similar-songs comes back empty.
    pub async fn get_top_songs(&self, artist: &str, count: u32) -> Result<Vec<Song>> {
        let b = self
            .get("getTopSongs", vec![p("artist", artist), p("count", count)])
            .await?;
        Ok(Self::arr(&b, &["topSongs", "song"]).iter().map(Song::from_api).collect())
    }

    pub async fn get_random_songs(&self, size: u32) -> Result<Vec<Song>> {
        let b = self.get("getRandomSongs", vec![p("size", size)]).await?;
        Ok(Self::arr(&b, &["randomSongs", "song"]).iter().map(Song::from_api).collect())
    }

    /// Songs-first view of an album list: flatten the songs of the top N
    /// albums for `newest` / `recent` / `frequent`, keeping album order.
    pub async fn get_songs_by_albums(&self, list_type: &str, albums: u32) -> Result<Vec<Song>> {
        let list = self.get_album_list(list_type, albums).await?;
        let results = futures::future::join_all(list.iter().map(|a| self.get_album_songs(&a.id))).await;
        Ok(results.into_iter().filter_map(|r| r.ok()).flatten().collect())
    }

    /// Every song in the library, paged through `search3` with the empty
    /// query (the Navidrome/OpenSubsonic "list everything" convention).
    pub async fn get_all_songs(&self, max_songs: usize) -> Result<Vec<Song>> {
        let page = 500usize;
        let mut songs: Vec<Song> = Vec::new();
        while songs.len() < max_songs {
            let b = self
                .get(
                    "search3",
                    vec![
                        p("query", "\"\""),
                        p("artistCount", 0),
                        p("albumCount", 0),
                        p("songCount", page),
                        p("songOffset", songs.len()),
                    ],
                )
                .await?;
            let batch = Self::arr(&b, &["searchResult3", "song"]);
            songs.extend(batch.iter().map(Song::from_api));
            if batch.len() < page {
                break;
            }
        }
        songs.truncate(max_songs);
        Ok(songs)
    }

    // ── podcasts & internet radio ─────────────────────────────────────
    /// A podcast episode as a playable Song. `streamId` is the id the
    /// `stream`/`download` endpoints accept, so we key the Song on it and
    /// everything (playback, offline pins) works unchanged. Episodes still
    /// downloading or errored have no streamId and are dropped.
    fn episode_song(ep: &Value, channel_title: &str) -> Option<Song> {
        let sid = ep.get("streamId")?;
        let id = match sid {
            Value::String(s) if !s.is_empty() => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => return None,
        };
        let mut s = Song::from_api(ep);
        s.id = id;
        s.artist = channel_title.into();
        s.album = channel_title.into();
        Some(s)
    }

    /// Every subscribed channel with its episodes flattened into Songs.
    pub async fn get_podcasts(&self) -> Result<Vec<(PodcastChannel, Vec<Song>)>> {
        let b = self.get("getPodcasts", vec![p("includeEpisodes", "true")]).await?;
        Ok(Self::arr(&b, &["podcasts", "channel"])
            .iter()
            .map(|ch| {
                let channel = PodcastChannel::from_api(ch);
                let eps = Self::arr(ch, &["episode"])
                    .iter()
                    .filter_map(|e| Self::episode_song(e, &channel.title))
                    .collect();
                (channel, eps)
            })
            .collect())
    }

    /// Internet-radio stations as playable Songs. The station's direct
    /// `streamUrl` rides on `Song::stream_url` so mpv opens it straight.
    pub async fn get_internet_radio_stations(&self) -> Result<Vec<Song>> {
        let b = self.get("getInternetRadioStations", vec![]).await?;
        Ok(Self::arr(&b, &["internetRadioStations", "internetRadioStation"])
            .iter()
            .map(RadioStation::from_api)
            .filter(|s| !s.stream_url.is_empty())
            .map(|st| Song {
                id: format!("radio:{}", st.id),
                title: st.name,
                artist: "internet radio".into(),
                album: st.home_url,
                stream_url: Some(st.stream_url),
                ..Default::default()
            })
            .collect())
    }

    // ── playback side-channel ─────────────────────────────────────────
    pub fn stream_url(&self, song_id: &str) -> String {
        // maxBitRate/format only when set — 0/"" means original quality.
        // Downloads go through `download_song`, which never touches these.
        let (kbps, fmt) = self.quality.lock().unwrap().clone();
        let mut extra = vec![p("id", song_id)];
        if kbps > 0 {
            extra.push(p("maxBitRate", kbps));
        }
        if !fmt.is_empty() {
            extra.push(p("format", fmt));
        }
        let base = format!("{}/rest/stream", self.server);
        Url::parse_with_params(&base, self.params(extra))
            .map(|u| u.to_string())
            .unwrap_or(base)
    }

    pub async fn scrobble(&self, song_id: &str, submission: bool) -> Result<()> {
        self.get(
            "scrobble",
            vec![p("id", song_id), p("submission", if submission { "true" } else { "false" })],
        )
        .await
        .map(drop)
    }

    /// kind: song | album | artist
    pub async fn set_star(&self, item_id: &str, kind: &str, star: bool) -> Result<()> {
        let key = match kind {
            "album" => "albumId",
            "artist" => "artistId",
            _ => "id",
        };
        self.get(if star { "star" } else { "unstar" }, vec![p(key, item_id)])
            .await
            .map(drop)
    }

    /// rating 1-5, or 0 to clear.
    pub async fn set_rating(&self, song_id: &str, rating: i64) -> Result<()> {
        self.get("setRating", vec![p("id", song_id), p("rating", rating.clamp(0, 5))])
            .await
            .map(drop)
    }

    pub async fn get_lyrics(&self, artist: &str, title: &str) -> Result<String> {
        let b = self
            .get("getLyrics", vec![p("artist", artist), p("title", title)])
            .await?;
        Ok(b.get("lyrics")
            .and_then(|l| l.get("value"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string())
    }

    /// Timed lyrics via OpenSubsonic `getLyricsBySongId`: `(start_seconds,
    /// text)` lines when the server has SYNCED structured lyrics, else `None`
    /// (no endpoint, unsynced only, nothing found) so the caller can fall
    /// back to plain `get_lyrics`.
    pub async fn get_synced_lyrics(&self, song_id: &str) -> Option<Vec<(f64, String)>> {
        let b = self.get("getLyricsBySongId", vec![p("id", song_id)]).await.ok()?;
        for track in Self::arr(&b, &["lyricsList", "structuredLyrics"]) {
            if track.get("synced").and_then(Value::as_bool) != Some(true) {
                continue;
            }
            let mut lines: Vec<(f64, String)> = Self::arr(track, &["line"])
                .iter()
                .filter_map(|l| {
                    let start = l.get("start").and_then(Value::as_f64)?;
                    let text = l.get("value").and_then(Value::as_str).unwrap_or("");
                    Some((start / 1000.0, text.to_string()))
                })
                .collect();
            if !lines.is_empty() {
                lines.sort_by(|a, b| a.0.total_cmp(&b.0));
                return Some(lines);
            }
        }
        None
    }

    /// Public share link for a song/album (needs sharing enabled server-side).
    pub async fn create_share(&self, item_id: &str) -> Result<String> {
        let b = self.get("createShare", vec![p("id", item_id)]).await?;
        Self::arr(&b, &["shares", "share"])
            .first()
            .and_then(|s| s.get("url"))
            .and_then(Value::as_str)
            .filter(|u| !u.is_empty())
            .map(str::to_string)
            .ok_or_else(|| ApiError::Subsonic("server did not return a share url".into()))
    }

    // ── cover art ─────────────────────────────────────────────────────
    fn art_path(&self, cover_id: &str, size: u32) -> PathBuf {
        self.art_dir.join(format!("{}-{size}", cover_id.replace('/', "_")))
    }

    pub fn cached_art(&self, cover_id: &str, size: u32) -> Option<PathBuf> {
        Some(self.art_path(cover_id, size)).filter(|p| p.exists())
    }

    pub async fn cover_art(&self, cover_id: &str, size: u32) -> Result<PathBuf> {
        let path = self.art_path(cover_id, size);
        if path.exists() {
            return Ok(path);
        }
        let resp = self
            .http
            .get(format!("{}/rest/getCoverArt", self.server))
            .query(&self.params(vec![p("id", cover_id), p("size", size)]))
            .send()
            .await?
            .error_for_status()?;
        if is_json(&resp) {
            return Err(ApiError::Subsonic("no cover art".into()));
        }
        let bytes = resp.bytes().await?;
        write_atomic(&path, &bytes).await?;
        Ok(path)
    }

    // ── audio files on disk ───────────────────────────────────────────
    // Two kinds, kept apart on purpose:
    //  * `audio/`        — explicit downloads (d / D / ctrl+d). Yours: never evicted.
    //  * `audio-cache/`  — the next track, fetched ahead of time so it starts
    //                      instantly. Bounded: old ones are deleted as new ones
    //                      arrive, so playing for hours doesn't fill the disk
    //                      (or the page cache) with every track you ever heard.
    // The id is kept opaque (no suffix): the server may transcode or the suffix
    // may be unknown offline, and mpv probes the container regardless.
    fn audio_path(&self, song_id: &str) -> PathBuf {
        self.audio_dir.join(song_id.replace('/', "_"))
    }

    fn prefetch_path(&self, song_id: &str) -> PathBuf {
        self.prefetch_dir.join(song_id.replace('/', "_"))
    }

    /// An explicit download of this song exists (what the ✓ in the list means).
    pub fn is_pinned(&self, song_id: &str) -> bool {
        self.audio_path(song_id).exists()
    }

    /// A local file to play instead of streaming: a download, else a prefetch.
    pub fn cached_stream(&self, song_id: &str) -> Option<PathBuf> {
        [self.audio_path(song_id), self.prefetch_path(song_id)].into_iter().find(|p| p.exists())
    }

    /// Download for keeps. Idempotent. Promotes an already-prefetched copy with
    /// a rename (no second download). Fetches the ORIGINAL file via `download`,
    /// falling back to `stream`.
    pub async fn download_song(&self, song_id: &str) -> Result<PathBuf> {
        let path = self.audio_path(song_id);
        if path.exists() {
            return Ok(path);
        }
        let prefetched = self.prefetch_path(song_id);
        if prefetched.exists() {
            tokio::fs::create_dir_all(&self.audio_dir).await?;
            if tokio::fs::rename(&prefetched, &path).await.is_ok() {
                return Ok(path);
            }
        }
        if self.fetch_audio_to("download", song_id, &path).await || self.fetch_audio_to("stream", song_id, &path).await {
            Ok(path)
        } else {
            Err(ApiError::Subsonic("could not download song".into()))
        }
    }

    /// Fetch `song_id` into the bounded look-ahead cache, then evict old
    /// entries. `keep` are the ids that must survive (what's playing, what's
    /// next). Best-effort and silent.
    pub async fn prefetch_song(&self, song_id: &str, keep: &[String]) {
        if self.cached_stream(song_id).is_none() {
            let path = self.prefetch_path(song_id);
            if !(self.fetch_audio_to("download", song_id, &path).await || self.fetch_audio_to("stream", song_id, &path).await) {
                return;
            }
        }
        let keep: Vec<String> = keep.iter().map(|k| k.replace('/', "_")).collect();
        evict_prefetch(&self.prefetch_dir, &keep, PREFETCH_HISTORY);
    }

    /// Stream an audio endpoint straight to `dest` in chunks: peak memory is one
    /// network chunk, not the file (a 30 MB FLAC used to cost ~70 MB of RAM —
    /// the body, then a copy of it). Fails — leaving nothing behind — on any
    /// error, on the JSON error envelope some servers send instead of a 4xx, and
    /// on a body shorter than its Content-Length (a link that drops mid-transfer
    /// can close cleanly; keeping that would play seconds short of the end).
    async fn fetch_audio_to(&self, endpoint: &str, song_id: &str, dest: &Path) -> bool {
        use tokio::io::AsyncWriteExt;
        let Ok(Ok(mut resp)) = self
            .http
            .get(format!("{}/rest/{endpoint}", self.server))
            .query(&self.params(vec![p("id", song_id)]))
            .timeout(Duration::from_secs(600))
            .send()
            .await
            .map(|r| r.error_for_status())
        else {
            return false;
        };
        if is_json(&resp) {
            return false;
        }
        let expected = resp.content_length();
        if let Some(dir) = dest.parent() {
            if tokio::fs::create_dir_all(dir).await.is_err() {
                return false;
            }
        }
        let mut tmp = dest.as_os_str().to_owned();
        tmp.push(".part");
        let tmp = PathBuf::from(tmp);
        let Ok(mut file) = tokio::fs::File::create(&tmp).await else { return false };
        let mut total = 0u64;
        let ok = loop {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    if file.write_all(&chunk).await.is_err() {
                        break false;
                    }
                    total += chunk.len() as u64;
                }
                Ok(None) => break expected.map_or(true, |n| n == total) && total > 0,
                Err(_) => break false,
            }
        };
        let ok = ok && file.flush().await.is_ok();
        drop(file);
        if ok && tokio::fs::rename(&tmp, dest).await.is_ok() {
            true
        } else {
            let _ = tokio::fs::remove_file(&tmp).await;
            false
        }
    }

    /// Drop a pinned file. Returns true if something was removed.
    #[allow(dead_code)]
    pub fn remove_pin(&self, song_id: &str) -> bool {
        std::fs::remove_file(self.audio_path(song_id)).is_ok()
    }
}

/// How many *previously played* prefetched tracks to keep besides the ones in
/// `keep`: enough that skipping back a track or two doesn't re-download.
pub const PREFETCH_HISTORY: usize = 2;

/// Delete look-ahead files beyond the newest `history` that aren't in `keep`
/// (and abandoned `.part` files). Pure file-system logic, so it's testable.
pub fn evict_prefetch(dir: &Path, keep: &[String], history: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut old: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for e in rd.flatten() {
        let (path, name) = (e.path(), e.file_name().to_string_lossy().into_owned());
        let modified = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
        if name.ends_with(".part") {
            // an in-flight download is young; anything older was abandoned
            if modified.elapsed().map_or(false, |d| d > Duration::from_secs(600)) {
                let _ = std::fs::remove_file(&path);
            }
        } else if !keep.contains(&name) {
            old.push((modified, path));
        }
    }
    old.sort_by(|a, b| b.0.cmp(&a.0)); // newest first
    for (_, path) in old.into_iter().skip(history) {
        let _ = std::fs::remove_file(path);
    }
}

fn is_json(resp: &reqwest::Response) -> bool {
    resp.headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"))
}

/// `.part` then rename, so a killed download never leaves a half file that
/// reads as "downloaded".
async fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".part");
    let tmp = PathBuf::from(tmp);
    tokio::fs::write(&tmp, bytes).await?;
    tokio::fs::rename(&tmp, path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_md5_of_password_plus_salt() {
        let (token, salt) = make_token("sesame");
        assert_eq!(salt.len(), 16);
        assert_eq!(token, format!("{:x}", md5::compute(format!("sesame{salt}"))));
    }

    #[test]
    fn normalize_server_adds_scheme_and_trims() {
        assert_eq!(normalize_server("  music.example.com/ "), "https://music.example.com");
        assert_eq!(normalize_server("http://localhost:4533/"), "http://localhost:4533");
        assert_eq!(normalize_server(""), "");
    }

    #[test]
    fn stream_url_encodes_and_honours_quality() {
        let c = SubsonicClient::new("http://h:1", "u", "tok", "salt", "/tmp/a".into(), "/tmp/b".into(), 0, "");
        let url = c.stream_url("a b/c");
        assert!(url.starts_with("http://h:1/rest/stream?"));
        assert!(url.contains("id=a+b%2Fc") || url.contains("id=a%20b%2Fc"), "{url}");
        assert!(!url.contains("maxBitRate"));
        c.set_quality(192, "mp3");
        let url = c.stream_url("x");
        assert!(url.contains("maxBitRate=192") && url.contains("format=mp3"));
    }

    #[test]
    fn eviction_keeps_the_playing_and_next_tracks_plus_a_short_history() {
        use std::time::{Duration, SystemTime};
        let dir = std::env::temp_dir().join(format!("rustdrome-evict-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let now = SystemTime::now();
        // ten tracks, "t0" the oldest … "t9" the newest
        for i in 0..10u64 {
            let f = std::fs::File::create(dir.join(format!("t{i}"))).unwrap();
            f.set_modified(now - Duration::from_secs((10 - i) * 60)).unwrap();
        }
        // the user skipped back: t2 is playing, t3 is next — those must survive even though they're old
        evict_prefetch(&dir, &["t2".into(), "t3".into()], 2);
        let mut left: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        left.sort();
        assert_eq!(left, vec!["t2", "t3", "t8", "t9"], "keep + the 2 newest others; everything else gone");
        // a fresh .part (a download in flight) is left alone, an abandoned one is removed
        let fresh = dir.join("t10.part");
        std::fs::File::create(&fresh).unwrap();
        let stale = dir.join("t11.part");
        std::fs::File::create(&stale).unwrap().set_modified(now - Duration::from_secs(3600)).unwrap();
        evict_prefetch(&dir, &[], 4);
        assert!(fresh.exists() && !stale.exists());
        // a missing directory is not an error
        evict_prefetch(&dir.join("nope"), &[], 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn downloads_and_prefetches_live_in_separate_places() {
        let dir = std::env::temp_dir().join(format!("rustdrome-two-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let c = SubsonicClient::new("http://h:1", "u", "t", "s", dir.join("art"), dir.join("audio"), 0, "");
        std::fs::create_dir_all(dir.join("audio-cache")).unwrap();
        std::fs::write(dir.join("audio-cache/p1"), b"x").unwrap();
        assert!(c.cached_stream("p1").is_some(), "a prefetched file is playable…");
        assert!(!c.is_pinned("p1"), "…but is not a download (no ✓, and it may be evicted)");
        std::fs::create_dir_all(dir.join("audio")).unwrap();
        std::fs::write(dir.join("audio/d1"), b"x").unwrap();
        assert!(c.is_pinned("d1") && c.cached_stream("d1").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn episode_without_stream_id_is_dropped() {
        let ep = serde_json::json!({"title": "e", "streamId": "s1", "duration": 5});
        let s = SubsonicClient::episode_song(&ep, "Chan").unwrap();
        assert_eq!((s.id.as_str(), s.artist.as_str()), ("s1", "Chan"));
        assert!(SubsonicClient::episode_song(&serde_json::json!({"title": "x"}), "c").is_none());
    }
}
