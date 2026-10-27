//! Sender chunk hash cache. The root hash needs the hash of every chunk, including chunks the receiver already had
//! on resume, so the sender keeps the hashes it computed in `paths::hash_cache_dir()/<transfer_id>.json`.
//!
//! The cache only saves work: if it cannot be read or written, the sender logs a warning and hashes from disk.

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};
use uuid::Uuid;

use super::{
    chunk::{hex_hashes, ChunkHash, ChunkLayout},
    write_file_atomic, SAVE_EVERY_CHUNKS,
};

/// Cache entries older than this are deleted.
pub const HASH_CACHE_MAX_AGE: Duration = Duration::from_hours(7 * 24);

/// What is stored on disk for one transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CacheEntry {
    transfer_id: Uuid,
    size: u64,
    chunk_size: u32,
    #[serde(with = "hex_hashes")]
    hashes: Vec<Option<ChunkHash>>,
}

/// Chunk hashes of one transfer, saved after every few new hashes.
#[derive(Debug)]
pub(crate) struct HashCache {
    /// `None` when the cache directory is unusable; the cache then lives in memory only.
    path: Option<PathBuf>,
    entry: CacheEntry,
    unsaved: u32,
}

impl HashCache {
    /// Opens the cache for `transfer_id` in `dir`, deleting entries older than [`HASH_CACHE_MAX_AGE`] first.
    pub(crate) async fn open(dir: &Path, transfer_id: Uuid, layout: &ChunkLayout) -> Self {
        let dir = dir.to_owned();
        let layout = *layout;
        // `spawn_blocking` only fails if the closure panicked; fall back to an empty in-memory cache then.
        tokio::task::spawn_blocking(move || Self::open_blocking(&dir, transfer_id, &layout))
            .await
            .unwrap_or_else(|_| Self { path: None, entry: empty_entry(transfer_id, &layout), unsaved: 0 })
    }

    fn open_blocking(dir: &Path, transfer_id: Uuid, layout: &ChunkLayout) -> Self {
        if let Err(error) = fs::create_dir_all(dir) {
            warn!(dir = %dir.display(), %error, "hash cache unavailable, keeping hashes in memory");
            return Self { path: None, entry: empty_entry(transfer_id, layout), unsaved: 0 };
        }
        prune(dir, HASH_CACHE_MAX_AGE);
        let path = dir.join(format!("{transfer_id}.json"));
        let entry = load(&path)
            .filter(|entry| {
                entry.transfer_id == transfer_id
                    && entry.size == layout.size()
                    && entry.chunk_size == layout.chunk_size()
                    && entry.hashes.len() as u64 == layout.count()
            })
            .unwrap_or_else(|| empty_entry(transfer_id, layout));
        Self { path: Some(path), entry, unsaved: 0 }
    }

    /// The cached hash of chunk `index`, if known.
    pub(crate) fn get(&self, index: u64) -> Option<ChunkHash> {
        usize::try_from(index).ok().and_then(|index| self.entry.hashes.get(index).copied().flatten())
    }

    /// Stores the hash of chunk `index`, saving to disk after every few new hashes.
    pub(crate) async fn record(&mut self, index: u64, hash: ChunkHash) {
        let Some(slot) = usize::try_from(index).ok().and_then(|index| self.entry.hashes.get_mut(index)) else {
            return;
        };
        if *slot != Some(hash) {
            *slot = Some(hash);
            self.unsaved += 1;
        }
        if self.unsaved >= SAVE_EVERY_CHUNKS {
            self.save().await;
        }
    }

    /// Writes unsaved hashes to disk.
    pub(crate) async fn save(&mut self) {
        let Some(path) = self.path.clone() else { return };
        if self.unsaved == 0 {
            return;
        }
        let text = match serde_json::to_string(&self.entry) {
            Ok(text) => text,
            Err(error) => return warn!(%error, "could not encode hash cache"),
        };
        let result = tokio::task::spawn_blocking(move || write_file_atomic(&path, text.as_bytes()))
            .await
            .unwrap_or_else(|error| Err(io::Error::other(error)));
        match result {
            Ok(()) => self.unsaved = 0,
            Err(error) => warn!(%error, "could not save hash cache"),
        }
    }

    /// Deletes the cache entry, once the transfer is complete.
    pub(crate) async fn remove(self) {
        let Some(path) = self.path else { return };
        let result = tokio::task::spawn_blocking(move || fs::remove_file(path)).await;
        if let Ok(Err(error)) = result {
            if error.kind() != io::ErrorKind::NotFound {
                warn!(%error, "could not delete hash cache entry");
            }
        }
    }
}

fn empty_entry(transfer_id: Uuid, layout: &ChunkLayout) -> CacheEntry {
    let count = usize::try_from(layout.count()).unwrap_or(0);
    CacheEntry { transfer_id, size: layout.size(), chunk_size: layout.chunk_size(), hashes: vec![None; count] }
}

fn load(path: &Path) -> Option<CacheEntry> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).inspect_err(|error| debug!(%error, "ignoring unreadable hash cache entry")).ok()
}

/// Deletes `.json` files in `dir` last modified more than `max_age` ago.
fn prune(dir: &Path, max_age: Duration) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let age =
            entry.metadata().and_then(|metadata| metadata.modified()).ok().and_then(|m| now.duration_since(m).ok());
        if age.is_some_and(|age| age > max_age) {
            debug!(path = %path.display(), "deleting old hash cache entry");
            if let Err(error) = fs::remove_file(&path) {
                warn!(path = %path.display(), %error, "could not delete old hash cache entry");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::FileTimes;

    use super::*;

    fn layout() -> ChunkLayout {
        ChunkLayout::new(100, 10).unwrap()
    }

    #[tokio::test]
    async fn hashes_survive_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let mut cache = HashCache::open(dir.path(), id, &layout()).await;
        cache.record(3, [7; 32]).await;
        cache.save().await;

        let reopened = HashCache::open(dir.path(), id, &layout()).await;
        assert_eq!(reopened.get(3), Some([7; 32]));
        assert_eq!(reopened.get(4), None);

        let other_layout = HashCache::open(dir.path(), id, &ChunkLayout::new(101, 10).unwrap()).await;
        assert_eq!(other_layout.get(3), None, "a cache for another size is not reused");

        reopened.remove().await;
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn saves_after_every_few_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let mut cache = HashCache::open(dir.path(), id, &layout()).await;
        for index in 0..u64::from(SAVE_EVERY_CHUNKS) {
            cache.record(index, [1; 32]).await;
        }
        assert!(dir.path().join(format!("{id}.json")).exists());
    }

    #[tokio::test]
    async fn old_entries_are_pruned_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join(format!("{}.json", Uuid::new_v4()));
        let file = fs::File::create(&old).unwrap();
        file.set_times(FileTimes::new().set_modified(SystemTime::now() - HASH_CACHE_MAX_AGE * 2)).unwrap();

        HashCache::open(dir.path(), Uuid::new_v4(), &layout()).await;
        assert!(!old.exists());
    }
}
