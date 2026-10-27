//! Receiver resume state. Data goes to `<name>.lanshare-part`, and `<name>.lanshare-state` (JSON) records which
//! chunks of it are written, so an interrupted transfer can continue where it stopped.

use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::{
    chunk::{bitset_from, hex_hashes, root_hash, ChunkHash, ChunkLayout},
    write_file_atomic,
};
use crate::error::{Error, Result};

/// Suffix added to the final file name while data is arriving.
pub const PART_SUFFIX: &str = ".lanshare-part";

/// Suffix added to the final file name for the resume state.
pub const STATE_SUFFIX: &str = ".lanshare-state";

/// `<final_path>.lanshare-part`.
pub(crate) fn part_path(final_path: &Path) -> PathBuf {
    with_suffix(final_path, PART_SUFFIX)
}

/// `<final_path>.lanshare-state`.
pub(crate) fn state_path(final_path: &Path) -> PathBuf {
    with_suffix(final_path, STATE_SUFFIX)
}

/// Appends `suffix` to the whole file name, so `movie.mkv` becomes `movie.mkv.lanshare-part`.
///
/// `Path::with_extension` would replace `.mkv` instead.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(suffix);
    PathBuf::from(name)
}

/// What the receiver knows about one partly received file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ResumeState {
    /// Id from the sender's offer.
    pub transfer_id: Uuid,
    /// File size in bytes.
    pub size: u64,
    /// Chunk size from the offer.
    pub chunk_size: u32,
    /// Hash of every chunk that is written and synced, `None` for the rest.
    #[serde(with = "hex_hashes")]
    pub chunks: Vec<Option<ChunkHash>>,
}

impl ResumeState {
    /// A state with no chunks received yet.
    pub(crate) fn new(transfer_id: Uuid, layout: &ChunkLayout) -> Self {
        let count = usize::try_from(layout.count()).unwrap_or(0);
        Self { transfer_id, size: layout.size(), chunk_size: layout.chunk_size(), chunks: vec![None; count] }
    }

    /// Whether this state belongs to the offered transfer.
    pub(crate) fn matches(&self, transfer_id: Uuid, layout: &ChunkLayout) -> bool {
        self.transfer_id == transfer_id
            && self.size == layout.size()
            && self.chunk_size == layout.chunk_size()
            && self.chunks.len() as u64 == layout.count()
    }

    /// Records chunk `index` as received with `hash`.
    pub(crate) fn mark(&mut self, index: u64, hash: ChunkHash) {
        if let Some(slot) = usize::try_from(index).ok().and_then(|index| self.chunks.get_mut(index)) {
            *slot = Some(hash);
        }
    }

    /// Bitset of received chunks, as sent in `Accept`.
    pub(crate) fn have_bitset(&self) -> Vec<u8> {
        bitset_from(self.chunks.len() as u64, self.chunks.iter().map(Option::is_some))
    }

    /// Total bytes of all received chunks.
    pub(crate) fn received_bytes(&self, layout: &ChunkLayout) -> u64 {
        (0..layout.count())
            .zip(&self.chunks)
            .filter(|(_, hash)| hash.is_some())
            .map(|(i, _)| u64::from(layout.len(i)))
            .sum()
    }

    /// Root hash over every chunk hash, or `None` while any chunk is missing.
    pub(crate) fn root_hash(&self) -> Option<ChunkHash> {
        let hashes: Option<Vec<ChunkHash>> = self.chunks.iter().copied().collect();
        hashes.map(|hashes| root_hash(&hashes))
    }

    /// Loads the state at `path`. Returns `None` if there is no file, or it cannot be parsed. Blocking.
    pub(crate) fn load(path: &Path) -> Result<Option<Self>> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(Error::Io(error)),
        };
        match serde_json::from_str(&text) {
            Ok(state) => Ok(Some(state)),
            Err(error) => {
                warn!(path = %path.display(), %error, "ignoring unreadable resume state");
                Ok(None)
            }
        }
    }

    /// Writes the state to `path`. Blocking.
    ///
    /// Writes a temporary file first and renames it, so a crash never leaves a half-written state.
    pub(crate) fn save(&self, path: &Path) -> Result<()> {
        let text = serde_json::to_string(self).map_err(|error| Error::Io(io::Error::other(error)))?;
        write_file_atomic(path, text.as_bytes()).map_err(Error::Io)
    }
}

/// Deletes `.lanshare-part` and `.lanshare-state` files in `dir` that were last modified more than `max_age` ago.
///
/// Active transfers touch both files at least every few seconds, so only abandoned ones are removed. Returns how many
/// files were deleted.
///
/// # Errors
///
/// [`Error::Io`] if `dir` cannot be listed. Files that cannot be deleted are logged and skipped.
pub async fn cleanup_stale_parts(dir: &Path, max_age: Duration) -> Result<usize> {
    let dir = dir.to_owned();
    tokio::task::spawn_blocking(move || cleanup_stale_parts_blocking(&dir, max_age))
        .await
        .map_err(|error| Error::Io(io::Error::other(error)))?
}

fn cleanup_stale_parts_blocking(dir: &Path, max_age: Duration) -> Result<usize> {
    let now = SystemTime::now();
    let mut removed = 0;
    for entry in fs::read_dir(dir).map_err(Error::Io)? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Temporary state files from an interrupted save are named `<name>.lanshare-state.<n>.tmp`.
        let is_temporary_state = name.contains(".lanshare-state.") && name.ends_with(".tmp");
        let is_ours = name.ends_with(PART_SUFFIX) || name.ends_with(STATE_SUFFIX) || is_temporary_state;
        if !is_ours {
            continue;
        }
        let modified = entry.metadata().and_then(|metadata| metadata.modified());
        // `duration_since` fails for times in the future (a clock change); such files are kept.
        let age = modified.ok().and_then(|modified| now.duration_since(modified).ok());
        if age.is_some_and(|age| age > max_age) {
            match fs::remove_file(entry.path()) {
                Ok(()) => {
                    debug!(path = %entry.path().display(), "deleted stale partial transfer file");
                    removed += 1;
                }
                Err(error) => warn!(path = %entry.path().display(), %error, "could not delete stale file"),
            }
        }
    }
    if removed > 0 {
        info!(removed, dir = %dir.display(), "cleaned up stale partial transfers");
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use std::fs::FileTimes;

    use super::*;
    use crate::transfer::chunk::hash_chunk;

    fn layout() -> ChunkLayout {
        ChunkLayout::new(20, 8).unwrap()
    }

    #[test]
    fn suffixes_are_appended_to_the_whole_name() {
        assert_eq!(part_path(Path::new("/d/movie.mkv")), Path::new("/d/movie.mkv.lanshare-part"));
        assert_eq!(state_path(Path::new("/d/README")), Path::new("/d/README.lanshare-state"));
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.bin.lanshare-state");
        let mut state = ResumeState::new(Uuid::new_v4(), &layout());
        state.mark(1, hash_chunk(b"second"));
        state.save(&path).unwrap();

        let loaded = ResumeState::load(&path).unwrap().unwrap();
        assert_eq!(loaded, state);
        assert!(loaded.matches(state.transfer_id, &layout()));
        assert!(!loaded.matches(Uuid::new_v4(), &layout()));
        assert!(!loaded.matches(state.transfer_id, &ChunkLayout::new(21, 8).unwrap()));
    }

    #[test]
    fn missing_or_broken_state_loads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.lanshare-state");
        assert_eq!(ResumeState::load(&path).unwrap(), None);
        fs::write(&path, "{ not json").unwrap();
        assert_eq!(ResumeState::load(&path).unwrap(), None);
    }

    #[test]
    fn progress_and_bitset_follow_marked_chunks() {
        let mut state = ResumeState::new(Uuid::new_v4(), &layout());
        state.mark(2, hash_chunk(b"last"));
        assert_eq!(state.have_bitset(), vec![0b100]);
        assert_eq!(state.received_bytes(&layout()), 4);
        assert_eq!(state.root_hash(), None);
    }

    #[test]
    fn root_hash_is_independent_of_arrival_order() {
        let hashes: Vec<ChunkHash> = [b"a".as_slice(), b"b", b"c"].iter().map(|data| hash_chunk(data)).collect();
        let mut state = ResumeState::new(Uuid::new_v4(), &layout());
        for index in [2, 0, 1] {
            state.mark(index as u64, hashes[index]);
        }
        assert_eq!(state.root_hash(), Some(root_hash(&hashes)));
    }

    #[tokio::test]
    async fn only_old_transfer_files_are_cleaned_up() {
        let dir = tempfile::tempdir().unwrap();
        let old = SystemTime::now() - Duration::from_hours(8 * 24);
        for name in ["old.bin.lanshare-part", "old.bin.lanshare-state", "old.bin.lanshare-state.3.tmp", "old.bin"] {
            let file = fs::File::create(dir.path().join(name)).unwrap();
            file.set_times(FileTimes::new().set_modified(old)).unwrap();
        }
        fs::write(dir.path().join("new.bin.lanshare-part"), b"").unwrap();

        let removed = cleanup_stale_parts(dir.path(), Duration::from_hours(7 * 24)).await.unwrap();
        assert_eq!(removed, 3);
        assert!(dir.path().join("old.bin").exists(), "other files are never touched");
        assert!(dir.path().join("new.bin.lanshare-part").exists());
    }
}
