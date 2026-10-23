//! Offline mutation queue — buffer library writes, replay them on reconnect.
//!
//! Stars, ratings and scrobbles are *optimistic*: the local cache is updated
//! the instant the user acts, so the UI never waits on the network. When the
//! server is unreachable (or offline mode is on) the write itself is parked
//! here, then flushed in order the next time we know we're online (app start,
//! offline mode off, a successful refresh).
//!
//! Redundant ops on the same target collapse to the user's *last* intent
//! (star→unstar→star ⇒ one star; two ratings ⇒ the latest) — correct because
//! these endpoints are idempotent set-operations. Scrobbles are the
//! exception: each is a distinct play event, so they only de-dupe on an exact
//! `(song, submission)` match.
//!
//! The on-disk format (`mutations.json`) matches the Python version.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::api::SubsonicClient;
use crate::config::AppDirs;

pub const CACHE_KEY: &str = "mutations";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Op {
    Star { item_id: String, kind: String, star: bool },
    Rate { song_id: String, rating: i64 },
    Scrobble { song_id: String, submission: bool },
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct OnDisk {
    #[serde(default)]
    ops: Vec<serde_json::Value>,
}

pub struct MutationQueue {
    ops: Vec<Op>,
    dirs: AppDirs,
}

impl MutationQueue {
    pub fn load(dirs: &AppDirs) -> Self {
        let disk: OnDisk = dirs.read_cache(CACHE_KEY).unwrap_or_default();
        // a corrupt/partial persisted op must not wedge loading
        let ops = disk
            .ops
            .into_iter()
            .filter_map(|v| serde_json::from_value::<Op>(v).ok())
            .collect();
        Self { ops, dirs: dirs.clone() }
    }

    pub fn pending(&self) -> usize {
        self.ops.len()
    }

    fn persist(&self) {
        self.dirs.write_cache(CACHE_KEY, &serde_json::json!({ "ops": self.ops }));
    }

    pub fn star(&mut self, item_id: &str, kind: &str, star: bool) {
        // last write to the same target wins
        self.ops.retain(|op| {
            !matches!(op, Op::Star { item_id: i, kind: k, .. } if i == item_id && k == kind)
        });
        self.ops.push(Op::Star { item_id: item_id.into(), kind: kind.into(), star });
        self.persist();
    }

    pub fn rate(&mut self, song_id: &str, rating: i64) {
        self.ops.retain(|op| !matches!(op, Op::Rate { song_id: s, .. } if s == song_id));
        self.ops.push(Op::Rate { song_id: song_id.into(), rating });
        self.persist();
    }

    pub fn scrobble(&mut self, song_id: &str, submission: bool) {
        let dup = self.ops.iter().any(
            |op| matches!(op, Op::Scrobble { song_id: s, submission: sub } if s == song_id && *sub == submission),
        );
        if !dup {
            self.ops.push(Op::Scrobble { song_id: song_id.into(), submission });
            self.persist();
        }
    }

    fn head(&self) -> Option<Op> {
        self.ops.first().cloned()
    }

    fn pop_if_head(&mut self, op: &Op) {
        // a collapse may have replaced the in-flight op while we awaited
        if self.ops.first() == Some(op) {
            self.ops.remove(0);
        }
    }
}

async fn apply(client: &SubsonicClient, op: &Op) -> crate::api::Result<()> {
    match op {
        Op::Star { item_id, kind, star } => client.set_star(item_id, kind, *star).await,
        Op::Rate { song_id, rating } => client.set_rating(song_id, *rating).await,
        Op::Scrobble { song_id, submission } => client.scrobble(song_id, *submission).await,
    }
}

/// Replay queued ops in order, dropping each on success. A network error
/// stops the flush (keeping this and the rest for next time); a server
/// rejection drops the op so one bad id can't wedge the queue forever.
/// Returns the number of ops flushed.
pub async fn flush(queue: &Arc<Mutex<MutationQueue>>, client: &SubsonicClient) -> usize {
    let mut flushed = 0;
    loop {
        let Some(op) = queue.lock().unwrap().head() else { break };
        match apply(client, &op).await {
            Err(e) if e.is_network() => break,
            _ => {}
        }
        queue.lock().unwrap().pop_if_head(&op);
        flushed += 1;
    }
    queue.lock().unwrap().persist();
    flushed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dirs(tag: &str) -> AppDirs {
        let root = std::env::temp_dir().join(format!("rustdrome-test-{tag}-{}", std::process::id()));
        let mut d = AppDirs::new();
        d.cache_dir = root.join("cache");
        d.state_file = root.join("state.json");
        d.config_file = root.join("config.toml");
        d
    }

    #[test]
    fn star_collapses_to_last_intent() {
        let mut q = MutationQueue::load(&temp_dirs("star"));
        q.star("1", "song", true);
        q.star("1", "song", false);
        q.star("1", "song", true);
        q.star("2", "song", true);
        assert_eq!(q.pending(), 2);
        assert!(q.ops.contains(&Op::Star { item_id: "1".into(), kind: "song".into(), star: true }));
        assert!(q.ops.contains(&Op::Star { item_id: "2".into(), kind: "song".into(), star: true }));
        assert!(!q.ops.contains(&Op::Star { item_id: "1".into(), kind: "song".into(), star: false }));
    }

    #[test]
    fn rate_collapses_and_scrobbles_only_fold_exact_duplicates() {
        let mut q = MutationQueue::load(&temp_dirs("rate"));
        q.rate("a", 3);
        q.rate("a", 5);
        q.scrobble("a", false);
        q.scrobble("a", false); // exact dup — folded
        q.scrobble("a", true); // different submission flag — kept
        assert_eq!(q.pending(), 3);
        assert!(q.ops.contains(&Op::Rate { song_id: "a".into(), rating: 5 }));
    }

    #[test]
    fn persists_and_reloads_python_compatible_json() {
        let dirs = temp_dirs("persist");
        let mut q = MutationQueue::load(&dirs);
        q.star("9", "album", true);
        let raw = std::fs::read_to_string(dirs.cache_dir.join("mutations.json")).unwrap();
        assert!(raw.contains(r#""op":"star""#) && raw.contains(r#""item_id":"9""#));
        // an invalid op written by hand is skipped on load
        std::fs::write(
            dirs.cache_dir.join("mutations.json"),
            r#"{"ops":[{"op":"rate","song_id":"x","rating":2},{"op":"bogus"},{"op":"star"}]}"#,
        )
        .unwrap();
        let q2 = MutationQueue::load(&dirs);
        assert_eq!(q2.pending(), 1);
    }
}
