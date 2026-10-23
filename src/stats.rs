//! Local listening stats — a mini "wrapped", entirely offline.
//!
//! Every confirmed play (the same moment a scrobble is submitted — half the
//! track, or 4 minutes, whichever comes first) appends one line to a JSONL
//! log in the cache dir. Writes are a single cheap append so logging never
//! blocks the UI; a torn last line (a crash mid-write) is skipped on read.
//! The log format is identical to the Python version's `plays.jsonl`.

use std::collections::{BTreeSet, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{Local, NaiveDate, TimeZone};
use serde::{Deserialize, Serialize};

pub const LOG_NAME: &str = "plays.jsonl";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Play {
    pub id: String,
    pub title: String,
    pub artist: String,
    /// unix timestamp, seconds
    pub ts: f64,
}

pub struct StatsStore {
    path: PathBuf,
}

impl StatsStore {
    pub fn new(cache_dir: &Path) -> Self {
        Self { path: cache_dir.join(LOG_NAME) }
    }

    /// Append one play record. Never fails loudly — a stats write must never
    /// take down playback.
    pub fn log_play(&self, id: &str, title: &str, artist: &str, ts: f64) {
        let play = Play { id: id.into(), title: title.into(), artist: artist.into(), ts };
        let Ok(line) = serde_json::to_string(&play) else { return };
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = writeln!(f, "{line}");
        }
    }

    pub fn load(&self) -> Vec<Play> {
        let Ok(raw) = std::fs::read_to_string(&self.path) else { return vec![] };
        raw.lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<Play>(l).ok())
            .collect()
    }
}

pub fn now_ts() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

// ── pure aggregations ─────────────────────────────────────────────────
// `now` is injectable so tests can pin "today".

fn day(ts: f64) -> NaiveDate {
    Local
        .timestamp_opt(ts as i64, 0)
        .single()
        .map_or(NaiveDate::MIN, |d| d.date_naive())
}

pub fn since(plays: &[Play], now: f64, days: u32) -> Vec<Play> {
    let cutoff = now - days as f64 * 86400.0;
    plays.iter().filter(|p| p.ts >= cutoff).cloned().collect()
}

/// Most-played `(title, artist, count)`, highest first. Keyed on
/// title+artist so the same song counts together across ids.
pub fn top_tracks(plays: &[Play], limit: usize) -> Vec<(String, String, usize)> {
    let mut counts: HashMap<(&str, &str), usize> = HashMap::new();
    for p in plays {
        *counts.entry((&p.title, &p.artist)).or_default() += 1;
    }
    let mut v: Vec<_> = counts
        .into_iter()
        .map(|((t, a), n)| (t.to_string(), a.to_string(), n))
        .collect();
    // stable order for ties: name ascending
    v.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    v.truncate(limit);
    v
}

/// Most-played `(artist, count)`. Blank artists are ignored.
pub fn top_artists(plays: &[Play], limit: usize) -> Vec<(String, usize)> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for p in plays.iter().filter(|p| !p.artist.is_empty()) {
        *counts.entry(&p.artist).or_default() += 1;
    }
    let mut v: Vec<_> = counts.into_iter().map(|(a, n)| (a.to_string(), n)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.truncate(limit);
    v
}

/// Play counts for the last `days` days, oldest → newest (today last).
pub fn plays_per_day(plays: &[Play], now: f64, days: usize) -> Vec<usize> {
    let today = day(now);
    let mut buckets = vec![0usize; days];
    for p in plays {
        let delta = (today - day(p.ts)).num_days();
        if (0..days as i64).contains(&delta) {
            buckets[days - 1 - delta as usize] += 1;
        }
    }
    buckets
}

/// Consecutive days with at least one play, counting back from today (or
/// yesterday if nothing yet today). 0 when the chain is already broken.
pub fn current_streak(plays: &[Play], now: f64) -> usize {
    let listened: BTreeSet<NaiveDate> = plays.iter().map(|p| day(p.ts)).collect();
    let mut cursor = day(now);
    if !listened.contains(&cursor) {
        // a fresh day with no plays yet shouldn't zero an active streak
        cursor = cursor.pred_opt().unwrap_or(cursor);
        if !listened.contains(&cursor) {
            return 0;
        }
    }
    let mut streak = 0;
    while listened.contains(&cursor) {
        streak += 1;
        cursor = cursor.pred_opt().unwrap_or(NaiveDate::MIN);
    }
    streak
}

/// Counts as a unicode block sparkline, scaled to the window's max.
pub fn sparkline(counts: &[usize]) -> String {
    const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let peak = counts.iter().copied().max().unwrap_or(0);
    counts
        .iter()
        .map(|&c| {
            if c == 0 || peak == 0 {
                BLOCKS[0]
            } else {
                let idx = 1 + ((c as f64 / peak as f64) * (BLOCKS.len() - 2) as f64).round() as usize;
                BLOCKS[idx.min(BLOCKS.len() - 1)]
            }
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct Summary {
    pub total: usize,
    pub week_total: usize,
    pub top_tracks_week: Vec<(String, String, usize)>,
    pub top_tracks_all: Vec<(String, String, usize)>,
    pub top_artists_week: Vec<(String, usize)>,
    pub top_artists_all: Vec<(String, usize)>,
    pub per_day: Vec<usize>,
    pub streak: usize,
    pub days_window: usize,
}

impl Summary {
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }
}

pub fn summarize(plays: &[Play], now: f64, days_window: usize) -> Summary {
    let week = since(plays, now, 7);
    Summary {
        total: plays.len(),
        week_total: week.len(),
        top_tracks_week: top_tracks(&week, 5),
        top_tracks_all: top_tracks(plays, 5),
        top_artists_week: top_artists(&week, 5),
        top_artists_all: top_artists(plays, 5),
        per_day: plays_per_day(plays, now, days_window),
        streak: current_streak(plays, now),
        days_window,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, m: u32, d: u32, h: u32) -> f64 {
        Local.with_ymd_and_hms(y, m, d, h, 30, 0).unwrap().timestamp() as f64
    }
    fn play(title: &str, artist: &str, ts: f64) -> Play {
        Play { id: title.into(), title: title.into(), artist: artist.into(), ts }
    }

    #[test]
    fn streak_counts_back_and_tolerates_an_empty_today() {
        let now = at(2025, 6, 10, 9);
        let plays = vec![
            play("a", "x", at(2025, 6, 9, 20)),
            play("b", "x", at(2025, 6, 8, 20)),
            play("c", "x", at(2025, 6, 7, 20)),
            play("d", "x", at(2025, 6, 5, 20)), // gap on the 6th
        ];
        assert_eq!(current_streak(&plays, now), 3);
        assert_eq!(current_streak(&plays, at(2025, 6, 12, 9)), 0, "chain already broken");
        assert_eq!(current_streak(&[], now), 0);
    }

    #[test]
    fn per_day_is_oldest_to_newest_with_fixed_length() {
        let now = at(2025, 6, 10, 23);
        let plays = vec![
            play("a", "x", at(2025, 6, 10, 1)),
            play("b", "x", at(2025, 6, 10, 2)),
            play("c", "x", at(2025, 6, 8, 12)),
            play("old", "x", at(2025, 1, 1, 12)),
        ];
        let d = plays_per_day(&plays, now, 14);
        assert_eq!(d.len(), 14);
        assert_eq!(d[13], 2);
        assert_eq!(d[11], 1);
        assert_eq!(d.iter().sum::<usize>(), 3, "out-of-window plays are ignored");
    }

    #[test]
    fn top_lists_rank_by_count_and_skip_blank_artists() {
        let plays = vec![
            play("s1", "A", 1.0),
            play("s1", "A", 2.0),
            play("s2", "B", 3.0),
            play("s3", "", 4.0),
        ];
        assert_eq!(top_tracks(&plays, 2)[0], ("s1".into(), "A".into(), 2));
        let ta = top_artists(&plays, 5);
        assert_eq!(ta[0], ("A".into(), 2));
        assert!(!ta.iter().any(|(a, _)| a.is_empty()));
    }

    #[test]
    fn sparkline_scales_to_peak() {
        assert_eq!(sparkline(&[]), "");
        assert_eq!(sparkline(&[0, 0, 0]), "▁▁▁");
        let s: Vec<char> = sparkline(&[0, 1, 5, 10]).chars().collect();
        assert_eq!(s[0], '▁');
        assert_eq!(s[3], '█');
        assert!(s[1] < s[2] && s[2] < s[3]);
    }

    #[test]
    fn log_round_trips_and_skips_torn_lines() {
        let dir = std::env::temp_dir().join(format!("rustdrome-stats-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = StatsStore::new(&dir);
        store.log_play("1", "Song", "Art", 100.0);
        store.log_play("2", "Song 2", "Art", 200.0);
        // simulate a crash mid-write
        let mut f = std::fs::OpenOptions::new().append(true).open(dir.join(LOG_NAME)).unwrap();
        write!(f, "{{\"id\":\"3\",\"tit").unwrap();
        let plays = store.load();
        assert_eq!(plays.len(), 2);
        assert_eq!(plays[1].title, "Song 2");
    }
}
