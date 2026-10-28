//! Playback engine — a thin wrapper around an `mpv` child process.
//!
//! mpv does the heavy lifting (HTTP streaming, every codec, seeking, volume);
//! we drive it over its JSON IPC socket and observe `time-pos` / `duration`
//! plus the `end-file` event. The Python original links libmpv through ctypes;
//! speaking IPC instead means no libmpv headers at build time — only the
//! `mpv` binary has to be on `PATH` at runtime.
//!
//! If mpv isn't installed the app still runs (browse, search, queue); the
//! player degrades to an inert stand-in and the UI shows an install hint.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::config::Settings;

pub const INSTALL_HINT: &str = "mpv not found — install it for playback:  \
    arch: pacman -S mpv · debian: apt install mpv · fedora: dnf install mpv · macos: brew install mpv";

/// How far short of the known duration an EOF must land to be treated as a
/// truncated source (worth one resume) rather than a real track end. Wide
/// enough to clear end-of-file jitter and encoder padding, tight enough that a
/// stream cut a few seconds early is still caught.
const EOF_SHORT_TOLERANCE: f64 = 5.0;

#[derive(Debug, Clone, Copy)]
pub enum PlayerEvent {
    /// `(position, duration)` seconds, forwarded on ~quarter-second changes
    Position(f64, f64),
    /// natural EOF (`failed == false`) or a stream error
    TrackEnd { failed: bool },
}

#[derive(Debug, Default)]
struct Shared {
    position: f64,
    duration: f64,
    paused: bool,
    want_playing: bool,
    url: Option<String>,
    /// one resume attempt is armed per `play()`
    retried: bool,
    /// seek once the file has loaded (mpv's `loadfile` option syntax differs
    /// across versions, so a post-load seek is the portable way to resume)
    pending_start: f64,
    last_forwarded: f64,
}

/// A cheap handle that can send raw commands (used for crossfade ramps from a
/// background task without borrowing the whole `Player`).
#[derive(Clone)]
pub struct PlayerHandle {
    tx: Option<UnboundedSender<String>>,
}

impl PlayerHandle {
    pub fn set_volume_raw(&self, v: f64) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(cmd(&["set_property".into(), "volume".into(), json!(v)]));
        }
    }
}

fn cmd(args: &[Value]) -> String {
    let mut s = json!({ "command": args }).to_string();
    s.push('\n');
    s
}

pub struct Player {
    shared: Arc<Mutex<Shared>>,
    tx: Option<UnboundedSender<String>>,
    #[allow(dead_code)] // held so `kill_on_drop` fires when the app exits
    child: Option<tokio::process::Child>,
    socket: Option<std::path::PathBuf>,
    volume: i64,
    muted: bool,
    speed: f64,
}

impl Player {
    /// An inert stand-in used when mpv is missing: the rest of the app works.
    pub fn null() -> Self {
        Self {
            shared: Arc::new(Mutex::new(Shared { paused: true, ..Default::default() })),
            tx: None,
            child: None,
            socket: None,
            volume: 100,
            muted: false,
            speed: 1.0,
        }
    }

    pub fn is_null(&self) -> bool {
        self.tx.is_none()
    }

    pub fn handle(&self) -> PlayerHandle {
        PlayerHandle { tx: self.tx.clone() }
    }

    /// Build the argument list for mpv from the user's settings. Split out so
    /// it can be unit-tested without a child process.
    pub fn mpv_args(settings: &Settings, socket: &std::path::Path, ao: Option<&str>) -> Vec<String> {
        let mut a: Vec<String> = vec![
            "--idle=yes".into(),
            "--no-video".into(),
            "--no-terminal".into(),
            "--no-config".into(),
            "--force-window=no".into(),
            "--audio-client-name=rustdrome".into(),
            format!("--input-ipc-server={}", socket.display()),
            // ── memory: this is an audio player with its own UI, so mpv's
            // scripting layer, on-screen display, youtube-dl hook and
            // subtitle/cover handling are all dead weight (~6 MB).
            "--load-scripts=no".into(),
            "--osd-level=0".into(),
            "--ytdl=no".into(),
            "--load-stats-overlay=no".into(),
            "--load-osd-console=no".into(),
            "--load-auto-profiles=no".into(),
            "--sub-auto=no".into(),
            "--audio-display=no".into(),
            // Network streams are buffered *ahead* in RAM, by default up to
            // 150 MiB / an hour of audio — i.e. a whole hi-res FLAC. Cap it:
            // about 25s of lossless, plenty to ride out a stall, and the
            // next track is pinned to disk by the prefetcher anyway.
            "--cache-secs=45".into(),
            "--demuxer-max-bytes=24MiB".into(),
            "--demuxer-max-back-bytes=6MiB".into(),
        ];
        if matches!(settings.replaygain.as_str(), "album" | "track") {
            a.push(format!("--replaygain={}", settings.replaygain));
            // preamp/fallback only shape the ReplayGain path
            a.push(format!("--replaygain-preamp={}", settings.replaygain_preamp));
            a.push(format!("--replaygain-fallback={}", settings.replaygain_fallback));
        }
        if matches!(settings.gapless.as_str(), "yes" | "weak" | "no") {
            a.push(format!("--gapless-audio={}", settings.gapless));
        }
        if settings.audio_exclusive {
            a.push("--audio-exclusive=yes".into());
        }
        if settings.pipewire_buffer > 0 {
            a.push(format!("--audio-buffer={}", settings.pipewire_buffer as f64 / 1000.0));
        }
        if let Some(ao) = ao.filter(|s| !s.is_empty()) {
            a.push(format!("--ao={ao}"));
        }
        a
    }

    #[cfg(unix)]
    pub async fn spawn(
        settings: &Settings,
        events: UnboundedSender<PlayerEvent>,
        ao: Option<&str>,
    ) -> Result<Self, String> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::UnixStream;

        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(std::env::temp_dir);
        let socket = dir.join(format!("rustdrome-mpv-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);

        let mut child = tokio::process::Command::new("mpv")
            .args(Self::mpv_args(settings, &socket, ao))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            // glibc keeps a malloc arena per thread and is slow to give
            // memory back; mpv reloads files constantly, so left alone its
            // heap ratchets up ~1 MB per track. One arena + an eager trim
            // threshold keeps it flat.
            .env("MALLOC_ARENA_MAX", "1")
            .env("MALLOC_TRIM_THRESHOLD_", "131072")
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    "mpv not found".to_string()
                } else {
                    format!("couldn't start mpv: {e}")
                }
            })?;

        // mpv creates the socket a moment after launch
        let mut stream = None;
        for _ in 0..60 {
            if let Ok(Some(status)) = child.try_wait() {
                return Err(format!("mpv exited immediately ({status})"));
            }
            if let Ok(s) = UnixStream::connect(&socket).await {
                stream = Some(s);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let stream = stream.ok_or("timed out waiting for mpv's IPC socket")?;
        let (rd, mut wr) = stream.into_split();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                if wr.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
            }
        });

        let shared = Arc::new(Mutex::new(Shared { paused: true, ..Default::default() }));

        // observe what the UI needs
        let _ = tx.send(cmd(&["observe_property".into(), json!(1), "time-pos".into()]));
        let _ = tx.send(cmd(&["observe_property".into(), json!(2), "duration".into()]));
        let _ = tx.send(cmd(&["observe_property".into(), json!(3), "pause".into()]));

        let sh = shared.clone();
        let tx2 = tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(rd).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                handle_mpv_message(&v, &sh, &tx2, &events);
            }
        });

        Ok(Self {
            shared,
            tx: Some(tx),
            child: Some(child),
            socket: Some(socket),
            volume: 100,
            muted: false,
            speed: 1.0,
        })
    }

    #[cfg(not(unix))]
    pub async fn spawn(
        _settings: &Settings,
        _events: UnboundedSender<PlayerEvent>,
        _ao: Option<&str>,
    ) -> Result<Self, String> {
        Err("playback over mpv IPC currently needs a Unix platform".into())
    }

    fn send(&self, args: &[Value]) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(cmd(args));
        }
    }

    fn set_prop(&self, name: &str, value: Value) {
        self.send(&["set_property".into(), name.into(), value]);
    }

    // ── transport ─────────────────────────────────────────────────────
    pub fn play(&mut self, url: &str, start: f64) {
        {
            // a genuine new track: arm one resume attempt for it
            let mut s = self.shared.lock().unwrap();
            s.url = Some(url.to_string());
            s.retried = false;
        }
        self.reload(url, start);
    }

    fn reload(&mut self, url: &str, start: f64) {
        {
            let mut s = self.shared.lock().unwrap();
            s.want_playing = false; // swallow the end-file of whatever was on
            s.position = start;
            s.duration = 0.0;
            s.last_forwarded = -1.0;
            s.pending_start = start;
        }
        self.send(&["loadfile".into(), url.into(), "replace".into()]);
        self.set_prop("pause", json!(false));
        let mut s = self.shared.lock().unwrap();
        s.paused = false;
        s.want_playing = true;
    }

    pub fn stop(&mut self) {
        {
            let mut s = self.shared.lock().unwrap();
            s.want_playing = false;
            s.position = 0.0;
            s.duration = 0.0;
            s.paused = true;
        }
        self.send(&["stop".into()]);
    }

    pub fn paused(&self) -> bool {
        self.shared.lock().unwrap().paused
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.shared.lock().unwrap().paused = paused;
        self.set_prop("pause", json!(paused));
    }

    pub fn toggle_pause(&mut self) {
        let p = !self.paused();
        self.set_paused(p);
    }

    /// A track is loaded (playing or paused).
    pub fn active(&self) -> bool {
        self.shared.lock().unwrap().want_playing
    }

    pub fn position(&self) -> f64 {
        self.shared.lock().unwrap().position
    }

    pub fn duration(&self) -> f64 {
        self.shared.lock().unwrap().duration
    }

    pub fn seek(&self, seconds: f64) {
        if self.active() {
            self.send(&["seek".into(), json!(seconds), "relative".into()]);
        }
    }

    pub fn seek_to(&self, fraction: f64) {
        let d = self.duration();
        if self.active() && d > 0.0 {
            self.send(&["seek".into(), json!(fraction.clamp(0.0, 0.99) * d), "absolute".into()]);
        }
    }

    // ── volume / speed ────────────────────────────────────────────────
    pub fn volume(&self) -> i64 {
        self.volume
    }

    pub fn set_volume(&mut self, v: i64) -> i64 {
        self.volume = v.clamp(0, 130);
        self.set_prop("volume", json!(self.volume));
        self.volume
    }

    pub fn muted(&self) -> bool {
        self.muted
    }

    pub fn toggle_mute(&mut self) -> bool {
        self.muted = !self.muted;
        self.set_prop("mute", json!(self.muted));
        self.muted
    }

    pub fn speed(&self) -> f64 {
        self.speed
    }

    pub fn set_speed(&mut self, v: f64) -> f64 {
        self.speed = v.clamp(0.25, 4.0);
        self.set_prop("speed", json!(self.speed));
        self.speed
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.shared.lock().unwrap().want_playing = false;
        self.send(&["quit".into()]);
        if let Some(s) = &self.socket {
            let _ = std::fs::remove_file(s);
        }
    }
}

/// Parse one line of mpv's IPC output and update shared state / emit events.
fn handle_mpv_message(
    v: &Value,
    sh: &Arc<Mutex<Shared>>,
    tx: &UnboundedSender<String>,
    events: &UnboundedSender<PlayerEvent>,
) {
    let Some(event) = v.get("event").and_then(Value::as_str) else { return };
    match event {
        "property-change" => {
            let name = v.get("name").and_then(Value::as_str).unwrap_or("");
            let data = v.get("data");
            let mut s = sh.lock().unwrap();
            match name {
                "time-pos" => {
                    let Some(pos) = data.and_then(Value::as_f64) else { return };
                    s.position = pos;
                    // mpv fires this many times a second; only cross into the
                    // UI on ~quarter-second boundaries
                    if (pos - s.last_forwarded).abs() >= 0.25 {
                        s.last_forwarded = pos;
                        let _ = events.send(PlayerEvent::Position(pos, s.duration));
                    }
                }
                "duration" => {
                    if let Some(d) = data.and_then(Value::as_f64) {
                        s.duration = d;
                    }
                }
                "pause" => {
                    if let Some(p) = data.and_then(Value::as_bool) {
                        s.paused = p;
                    }
                }
                _ => {}
            }
        }
        "file-loaded" => {
            let mut s = sh.lock().unwrap();
            if s.pending_start > 0.0 {
                let start = s.pending_start;
                s.pending_start = 0.0;
                let _ = tx.send(cmd(&["seek".into(), json!(start), "absolute".into()]));
            }
        }
        "end-file" => {
            let reason = v.get("reason").and_then(Value::as_str).unwrap_or("");
            let mut s = sh.lock().unwrap();
            if !s.want_playing {
                return; // we stopped/replaced it ourselves
            }
            match reason {
                "eof" => {
                    // A dropped/truncated source (a network stream cut short
                    // by a flaky link) surfaces as a *clean* EOF,
                    // indistinguishable from a real finish — so the track
                    // "skips" with seconds still on the clock. If we ended
                    // well short of the known length, resume from here once
                    // before giving up and advancing.
                    let short = s.duration > 0.0 && s.position < s.duration - EOF_SHORT_TOLERANCE;
                    if let (true, false, Some(url)) = (short, s.retried, s.url.clone()) {
                        s.retried = true;
                        s.pending_start = (s.position - 1.0).max(0.0);
                        s.last_forwarded = -1.0;
                        let _ = tx.send(cmd(&["loadfile".into(), url.into(), "replace".into()]));
                        return;
                    }
                    s.want_playing = false;
                    let _ = events.send(PlayerEvent::TrackEnd { failed: false });
                }
                "error" => {
                    s.want_playing = false;
                    let _ = events.send(PlayerEvent::TrackEnd { failed: true });
                }
                _ => {}
            }
        }
        _ => {}
    }
}

pub type PlayerEvents = UnboundedReceiver<PlayerEvent>;

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::unbounded_channel;

    fn feed(sh: &Arc<Mutex<Shared>>, msgs: &[&str]) -> (Vec<PlayerEvent>, Vec<String>) {
        let (etx, mut erx) = unbounded_channel();
        let (ctx, mut crx) = unbounded_channel();
        for m in msgs {
            handle_mpv_message(&serde_json::from_str(m).unwrap(), sh, &ctx, &etx);
        }
        let (mut ev, mut cm) = (vec![], vec![]);
        while let Ok(e) = erx.try_recv() {
            ev.push(e);
        }
        while let Ok(c) = crx.try_recv() {
            cm.push(c);
        }
        (ev, cm)
    }

    fn playing(duration: f64, position: f64) -> Arc<Mutex<Shared>> {
        Arc::new(Mutex::new(Shared {
            want_playing: true,
            duration,
            position,
            url: Some("http://x/stream".into()),
            ..Default::default()
        }))
    }

    #[test]
    fn eof_near_the_end_advances() {
        let sh = playing(200.0, 199.0);
        let (ev, cm) = feed(&sh, &[r#"{"event":"end-file","reason":"eof"}"#]);
        assert!(matches!(ev.as_slice(), [PlayerEvent::TrackEnd { failed: false }]));
        assert!(cm.is_empty());
        assert!(!sh.lock().unwrap().want_playing);
    }

    #[test]
    fn premature_eof_resumes_once_then_gives_up() {
        let sh = playing(200.0, 120.0);
        let (ev, cm) = feed(&sh, &[r#"{"event":"end-file","reason":"eof"}"#]);
        assert!(ev.is_empty(), "first short EOF is swallowed");
        assert!(cm[0].contains("loadfile"));
        assert_eq!(sh.lock().unwrap().pending_start, 119.0);
        // the resumed stream is cut short again → now we advance
        let (ev, _) = feed(&sh, &[r#"{"event":"end-file","reason":"eof"}"#]);
        assert!(matches!(ev.as_slice(), [PlayerEvent::TrackEnd { failed: false }]));
    }

    #[test]
    fn errors_and_replaced_files() {
        let sh = playing(0.0, 0.0);
        let (ev, _) = feed(&sh, &[r#"{"event":"end-file","reason":"stop"}"#]);
        assert!(ev.is_empty(), "stop = we replaced it ourselves");
        let (ev, _) = feed(&sh, &[r#"{"event":"end-file","reason":"error"}"#]);
        assert!(matches!(ev.as_slice(), [PlayerEvent::TrackEnd { failed: true }]));
        // nothing wanted → ignored even for eof
        let (ev, _) = feed(&sh, &[r#"{"event":"end-file","reason":"eof"}"#]);
        assert!(ev.is_empty());
    }

    #[test]
    fn position_is_throttled_and_pending_start_seeks_after_load() {
        let sh = playing(100.0, 0.0);
        sh.lock().unwrap().pending_start = 42.0;
        sh.lock().unwrap().last_forwarded = -1.0;
        let pc = |p: f64| format!(r#"{{"event":"property-change","name":"time-pos","data":{p}}}"#);
        let msgs = [pc(1.0), pc(1.1), pc(1.2), pc(1.3), pc(1.6)];
        let refs: Vec<&str> = msgs.iter().map(String::as_str).collect();
        let (ev, _) = feed(&sh, &refs);
        assert_eq!(ev.len(), 3, "1.0, 1.3 and 1.6 cross the 0.25s threshold");
        let (_, cm) = feed(&sh, &[r#"{"event":"file-loaded"}"#]);
        assert!(cm[0].contains("seek") && cm[0].contains("42"));
        let (_, cm) = feed(&sh, &[r#"{"event":"file-loaded"}"#]);
        assert!(cm.is_empty(), "pending start is consumed once");
    }

    #[test]
    fn mpv_args_reflect_settings() {
        let mut s = Settings::default();
        s.replaygain = "no".into();
        s.pipewire_buffer = 150;
        let a = Player::mpv_args(&s, std::path::Path::new("/tmp/s.sock"), Some("null"));
        assert!(a.contains(&"--input-ipc-server=/tmp/s.sock".to_string()));
        assert!(a.contains(&"--ao=null".to_string()));
        assert!(a.contains(&"--audio-buffer=0.15".to_string()));
        assert!(!a.iter().any(|x| x.starts_with("--replaygain")));
        // memory flags are always present
        assert!(a.contains(&"--load-scripts=no".to_string()));
        assert!(a.contains(&"--demuxer-max-bytes=24MiB".to_string()), "network read-ahead must be capped");
    }
}
