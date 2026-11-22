//! Persistent agent state: files we decided not to convert, and catch-up markers.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::write_atomic;
use crate::convert::Fingerprint;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkipEntry {
    pub size: u64,
    /// Modification time in nanoseconds since the Unix epoch.
    pub mtime_ns: i128,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// macOS: newest FSEvents event id whose events were fully processed.
    pub last_event_id: Option<u64>,
    /// Unix seconds when the agent last finished all pending work (catch-up threshold).
    pub last_idle_unix: Option<u64>,
    /// Files skipped on purpose, keyed by path. Re-tried automatically if the file changes.
    pub skipped: BTreeMap<String, SkipEntry>,
}

pub fn mtime_ns(t: SystemTime) -> i128 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    }
}

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl State {
    /// Missing or unreadable state is treated as empty (worst case: some work is redone).
    pub fn load(path: &Path) -> State {
        fs::read_to_string(path).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let body = toml::to_string(self)?;
        write_atomic(path, body.as_bytes())
    }

    pub fn is_skipped(&self, path: &Path, fp: Fingerprint) -> bool {
        self.skipped
            .get(&path.to_string_lossy().into_owned())
            .is_some_and(|e| Fingerprint { size: e.size, mtime_ns: e.mtime_ns } == fp)
    }

    pub fn record_skip(&mut self, path: &Path, fp: Fingerprint, reason: &str) {
        let entry = SkipEntry { size: fp.size, mtime_ns: fp.mtime_ns, reason: reason.to_string() };
        self.skipped.insert(path.to_string_lossy().into_owned(), entry);
    }

    /// Drop entries whose file no longer exists, so the map does not grow forever.
    pub fn prune_missing(&mut self) -> bool {
        let before = self.skipped.len();
        self.skipped.retain(|p, _| Path::new(p).exists());
        before != self.skipped.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn skip_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        let mut s = State { last_event_id: Some(42), last_idle_unix: Some(7), ..State::default() };
        let t = mtime_ns(UNIX_EPOCH + Duration::from_nanos(1_700_000_000_123_456_789));
        let photo = dir.path().join("a \"b\".jpg");
        let fp = |size, mtime_ns| Fingerprint { size, mtime_ns };
        s.record_skip(&photo, fp(10, t), "not smaller");
        s.save(&path).unwrap();
        let loaded = State::load(&path);
        assert_eq!(loaded, s);
        assert!(loaded.is_skipped(&photo, fp(10, t)));
        assert!(!loaded.is_skipped(&photo, fp(11, t)));
        assert!(!loaded.is_skipped(&photo, fp(10, t + 1)));
    }

    #[test]
    fn missing_file_is_default() {
        assert_eq!(State::load(Path::new("/definitely/not/here.toml")), State::default());
    }
}
