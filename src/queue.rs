//! The play queue — ordering, shuffle and repeat live here, not in the UI.
//!
//! `songs` IS the play order (what the queue panel shows is exactly what will
//! play). Toggling shuffle on keeps the current track first and shuffles the
//! rest; toggling it off restores the original order with the current track
//! still current.

use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

use crate::models::Song;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

impl Repeat {
    pub fn next(self) -> Self {
        match self {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        }
    }
}

/// Serialized form — same keys as the Python cache (`queue.json`), so a queue
/// left by either version restores in the other.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct QueueSnapshot {
    pub songs: Vec<Song>,
    pub original: Vec<Song>,
    pub index: i64,
    pub repeat: Repeat,
    pub shuffle: bool,
    /// position inside the current song, seconds
    pub position: f64,
}

/// Borrowed form used for writing: serialising a 5000-song queue must not
/// first clone it (that doubled the process's memory every few seconds).
#[derive(Serialize)]
pub struct QueueSnapshotRef<'a> {
    pub songs: &'a [Song],
    pub original: &'a [Song],
    pub index: i64,
    pub repeat: Repeat,
    pub shuffle: bool,
    pub position: f64,
}

#[derive(Debug, Default)]
pub struct PlayQueue {
    pub songs: Vec<Song>,
    /// `-1` when nothing is current, matching the on-disk format.
    pub index: i64,
    pub repeat: Repeat,
    pub shuffle: bool,
    /// The pre-shuffle order. `Some` only while shuffle is on — when it is
    /// off the play order *is* the original order, so keeping a second copy
    /// of every song would just double the queue's memory.
    original: Option<Vec<Song>>,
    /// Bumped by every change, so callers can tell "same queue" cheaply.
    rev: u64,
}

impl PlayQueue {
    pub fn new() -> Self {
        Self { index: -1, ..Default::default() }
    }

    pub fn rev(&self) -> u64 {
        self.rev
    }

    fn touch(&mut self) {
        self.rev = self.rev.wrapping_add(1);
    }

    pub fn current(&self) -> Option<&Song> {
        usize::try_from(self.index).ok().and_then(|i| self.songs.get(i))
    }

    pub fn current_index(&self) -> Option<usize> {
        usize::try_from(self.index).ok().filter(|i| *i < self.songs.len())
    }

    // ── content ───────────────────────────────────────────────────────
    pub fn set_songs(&mut self, songs: Vec<Song>, start: usize) -> Option<&Song> {
        self.songs = songs;
        self.index = if start < self.songs.len() {
            start as i64
        } else if self.songs.is_empty() {
            -1
        } else {
            0
        };
        self.original = None;
        if self.shuffle && !self.songs.is_empty() {
            self.original = Some(self.songs.clone());
            self.shuffle_rest();
        }
        self.touch();
        self.current()
    }

    pub fn add(&mut self, songs: Vec<Song>) {
        if let Some(o) = &mut self.original {
            o.extend(songs.iter().cloned());
        }
        self.songs.extend(songs);
        if self.index < 0 && !self.songs.is_empty() {
            self.index = 0;
        }
        self.touch();
    }

    pub fn add_next(&mut self, songs: Vec<Song>) {
        let at = if self.index >= 0 { self.index as usize + 1 } else { 0 };
        let at = at.min(self.songs.len());
        if let Some(o) = &mut self.original {
            o.extend(songs.iter().cloned());
        }
        self.songs.splice(at..at, songs);
        self.touch();
    }

    pub fn remove(&mut self, i: usize) -> Option<Song> {
        if i >= self.songs.len() {
            return None;
        }
        let song = self.songs.remove(i);
        if let Some(o) = &mut self.original {
            if let Some(p) = o.iter().position(|s| *s == song) {
                o.remove(p);
            }
        }
        self.touch();
        let i = i as i64;
        if i < self.index {
            self.index -= 1;
        } else if i == self.index {
            self.index = self.index.min(self.songs.len() as i64 - 1);
        }
        Some(song)
    }

    /// Swap song `i` with its neighbour; returns the new position. The current
    /// track can be moved too — `index` follows it.
    pub fn move_by(&mut self, i: usize, delta: i64) -> Option<usize> {
        let j = i as i64 + delta;
        if i >= self.songs.len() || j < 0 || j as usize >= self.songs.len() {
            return None;
        }
        let j = j as usize;
        self.songs.swap(i, j);
        self.touch();
        if self.index == i as i64 {
            self.index = j as i64;
        } else if self.index == j as i64 {
            self.index = i as i64;
        }
        Some(j)
    }

    pub fn clear(&mut self) {
        self.songs.clear();
        self.original = None;
        self.index = -1;
        self.touch();
    }

    // ── movement ──────────────────────────────────────────────────────
    /// Next track. `natural` means the previous one finished on its own, which
    /// is the only case where repeat-one repeats.
    pub fn advance(&mut self, natural: bool) -> Option<&Song> {
        if self.songs.is_empty() {
            return None;
        }
        if natural && self.repeat == Repeat::One {
            return self.current();
        }
        self.touch();
        if ((self.index + 1) as usize) < self.songs.len() {
            self.index += 1;
            return self.current();
        }
        if self.repeat != Repeat::Off || !natural {
            self.index = 0;
            return self.current();
        }
        None // queue ran out
    }

    /// The track a natural `advance()` would land on, without mutating state.
    pub fn peek_next(&self) -> Option<&Song> {
        if self.songs.is_empty() {
            return None;
        }
        if self.repeat == Repeat::One {
            return self.current();
        }
        let next = (self.index + 1) as usize;
        if next < self.songs.len() {
            return self.songs.get(next);
        }
        if self.repeat != Repeat::Off {
            return self.songs.first();
        }
        None
    }

    pub fn prev(&mut self) -> Option<&Song> {
        if self.songs.is_empty() {
            return None;
        }
        self.index = (self.index - 1).max(0);
        self.touch();
        self.current()
    }

    pub fn jump(&mut self, i: usize) -> Option<&Song> {
        if i < self.songs.len() {
            self.index = i as i64;
            self.touch();
        }
        self.current()
    }

    // ── modes ─────────────────────────────────────────────────────────
    pub fn toggle_shuffle(&mut self) -> bool {
        self.shuffle = !self.shuffle;
        let current = self.current().cloned();
        if self.shuffle {
            self.original = Some(self.songs.clone());
            self.shuffle_rest();
        } else if let Some(orig) = self.original.take() {
            self.songs = orig;
            self.index = current
                .and_then(|c| self.songs.iter().position(|s| *s == c))
                .map_or(-1, |p| p as i64);
        }
        self.touch();
        self.shuffle
    }

    /// Current track stays put; everything after plays in random order.
    fn shuffle_rest(&mut self) {
        let cur = self.current_index();
        let current = cur.map(|i| self.songs[i].clone());
        let mut rest: Vec<Song> = self
            .songs
            .iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != cur)
            .map(|(_, s)| s.clone())
            .collect();
        rest.shuffle(&mut rand::thread_rng());
        match current {
            Some(c) => {
                self.songs = std::iter::once(c).chain(rest).collect();
                self.index = 0;
            }
            None => self.songs = rest,
        }
    }

    pub fn cycle_repeat(&mut self) -> Repeat {
        self.repeat = self.repeat.next();
        self.touch();
        self.repeat
    }

    // ── persistence ───────────────────────────────────────────────────
    /// Borrow the queue in its on-disk shape. When shuffle is off the
    /// "original" order is simply the play order.
    pub fn snapshot_ref(&self, position: f64) -> QueueSnapshotRef<'_> {
        QueueSnapshotRef {
            songs: &self.songs,
            original: self.original.as_deref().unwrap_or(&self.songs),
            index: self.index,
            repeat: self.repeat,
            shuffle: self.shuffle,
            position,
        }
    }

    pub fn from_snapshot(s: QueueSnapshot) -> Self {
        let index = if (-1..s.songs.len() as i64).contains(&s.index) { s.index } else { -1 };
        let original = s.shuffle.then(|| if s.original.is_empty() { s.songs.clone() } else { s.original });
        Self { songs: s.songs, index, repeat: s.repeat, shuffle: s.shuffle, original, rev: 0 }
    }

    /// Songs after the current one (or everything when nothing is current).
    pub fn upcoming(&self) -> &[Song] {
        if self.index >= 0 {
            self.songs.get(self.index as usize + 1..).unwrap_or(&[])
        } else {
            &self.songs
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(n: u32) -> Song {
        Song { id: n.to_string(), title: format!("t{n}"), ..Default::default() }
    }
    fn songs(n: u32) -> Vec<Song> {
        (0..n).map(song).collect()
    }
    fn ids(q: &PlayQueue) -> Vec<String> {
        q.songs.iter().map(|s| s.id.clone()).collect()
    }

    #[test]
    fn set_songs_and_advance_runs_out_without_repeat() {
        let mut q = PlayQueue::new();
        assert_eq!(q.set_songs(songs(3), 1).unwrap().id, "1");
        assert_eq!(q.advance(true).unwrap().id, "2");
        assert!(q.advance(true).is_none(), "ran out");
        assert_eq!(q.advance(false).unwrap().id, "0", "manual skip wraps");
    }

    #[test]
    fn repeat_one_only_repeats_on_natural_end() {
        let mut q = PlayQueue::new();
        q.set_songs(songs(3), 0);
        q.repeat = Repeat::One;
        assert_eq!(q.advance(true).unwrap().id, "0");
        assert_eq!(q.advance(false).unwrap().id, "1");
    }

    #[test]
    fn repeat_all_wraps_and_peek_matches_advance() {
        let mut q = PlayQueue::new();
        q.set_songs(songs(2), 1);
        q.repeat = Repeat::All;
        assert_eq!(q.peek_next().unwrap().id, "0");
        assert_eq!(q.advance(true).unwrap().id, "0");
    }

    #[test]
    fn shuffle_keeps_current_first_and_restores_order() {
        let mut q = PlayQueue::new();
        q.set_songs(songs(20), 7);
        assert!(q.toggle_shuffle());
        assert_eq!(q.index, 0);
        assert_eq!(q.current().unwrap().id, "7");
        let mut sorted = ids(&q);
        sorted.sort_by_key(|s| s.parse::<u32>().unwrap());
        assert_eq!(sorted, (0..20).map(|n| n.to_string()).collect::<Vec<_>>());
        assert!(!q.toggle_shuffle());
        assert_eq!(ids(&q), (0..20).map(|n| n.to_string()).collect::<Vec<_>>());
        assert_eq!(q.current().unwrap().id, "7", "current survives un-shuffle");
    }

    #[test]
    fn remove_adjusts_index() {
        let mut q = PlayQueue::new();
        q.set_songs(songs(5), 2);
        q.remove(0);
        assert_eq!(q.current().unwrap().id, "2");
        q.remove(1); // id 2 is now at index 1 — the current track
        assert_eq!(q.current().unwrap().id, "3");
        // removing the last track while it is current clamps the index
        q.index = (q.songs.len() - 1) as i64;
        q.remove(q.songs.len() - 1);
        assert_eq!(q.index, q.songs.len() as i64 - 1);
    }

    #[test]
    fn add_next_inserts_after_current() {
        let mut q = PlayQueue::new();
        q.set_songs(songs(3), 0);
        q.add_next(vec![song(9)]);
        assert_eq!(ids(&q), ["0", "9", "1", "2"]);
        let mut empty = PlayQueue::new();
        empty.add_next(vec![song(5)]);
        assert_eq!(ids(&empty), ["5"]);
    }

    #[test]
    fn move_follows_current_track() {
        let mut q = PlayQueue::new();
        q.set_songs(songs(3), 1);
        assert_eq!(q.move_by(1, 1), Some(2));
        assert_eq!(q.current().unwrap().id, "1");
        assert_eq!(q.move_by(2, 1), None);
        assert_eq!(q.move_by(0, -1), None);
    }

    #[test]
    fn snapshot_round_trip_and_python_compat() {
        let mut q = PlayQueue::new();
        q.set_songs(songs(4), 2);
        q.repeat = Repeat::All;
        let json = serde_json::to_string(&q.snapshot_ref(12.5)).unwrap();
        let back = PlayQueue::from_snapshot(serde_json::from_str(&json).unwrap());
        assert_eq!(back.index, 2);
        assert_eq!(back.repeat, Repeat::All);
        // Python writes repeat as "one"/"all"/"off"
        let py = r#"{"songs":[],"original":[],"index":-1,"repeat":"one","shuffle":true}"#;
        let q2 = PlayQueue::from_snapshot(serde_json::from_str(py).unwrap());
        assert_eq!(q2.repeat, Repeat::One);
        assert!(q2.shuffle);
        // out-of-range index is sanitised
        let bad = r#"{"songs":[],"index":9}"#;
        assert_eq!(PlayQueue::from_snapshot(serde_json::from_str(bad).unwrap()).index, -1);
    }

    #[test]
    fn no_second_copy_is_kept_unless_shuffled() {
        let mut q = PlayQueue::new();
        q.set_songs(songs(100), 0);
        assert!(q.original.is_none(), "unshuffled queues hold one copy");
        q.toggle_shuffle();
        assert!(q.original.is_some());
        q.toggle_shuffle();
        assert!(q.original.is_none(), "dropped again when shuffle turns off");
        // the on-disk shape still carries `original` (for the Python version)
        let snap = q.snapshot_ref(0.0);
        assert_eq!(snap.original.len(), 100);
    }

    #[test]
    fn revision_changes_on_every_mutation_but_not_on_reads() {
        let mut q = PlayQueue::new();
        let r0 = q.rev();
        q.set_songs(songs(5), 0);
        let mut last = q.rev();
        assert_ne!(last, r0);
        let _ = (q.current(), q.peek_next(), q.upcoming().len(), q.snapshot_ref(1.0));
        assert_eq!(q.rev(), last, "reads must not bump the revision");
        for step in 0..8 {
            match step {
                0 => { q.add(vec![song(9)]); }
                1 => { q.add_next(vec![song(8)]); }
                2 => { q.move_by(0, 1); }
                3 => { q.remove(0); }
                4 => { q.advance(false); }
                5 => { q.prev(); }
                6 => { q.jump(1); }
                _ => { q.cycle_repeat(); }
            }
            assert_ne!(q.rev(), last, "step {step} should bump rev");
            last = q.rev();
        }
    }

    #[test]
    fn upcoming_excludes_played_and_current() {
        let mut q = PlayQueue::new();
        q.set_songs(songs(4), 1);
        assert_eq!(q.upcoming().len(), 2);
    }
}
