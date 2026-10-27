//! Positional chunk writer: each chunk is written at its own offset, so chunks may arrive in any order. Phase 4
//! reuses it unchanged for parallel streams.

use std::{
    fs::{File, OpenOptions},
    io,
    path::Path,
    sync::Arc,
};

use crate::error::{Error, Result};

/// Writes chunks into a preallocated file.
///
/// The file sits behind an `Arc` (a reference-counted shared pointer) so each write can hand a clone to
/// `spawn_blocking`, which needs to own what it uses.
#[derive(Debug, Clone)]
pub(crate) struct ChunkWriter {
    file: Arc<File>,
}

impl ChunkWriter {
    /// Opens or creates `path` for writing and sets its length to `size`. Existing data is kept, for resuming.
    pub(crate) async fn open(path: &Path, size: u64) -> Result<Self> {
        let path = path.to_owned();
        let file = blocking(move || {
            let file = OpenOptions::new().write(true).create(true).truncate(false).open(&path)?;
            // Reserves the full size up front. On most file systems this makes a sparse file, so it is instant.
            file.set_len(size)?;
            Ok(file)
        })
        .await?;
        Ok(Self { file: Arc::new(file) })
    }

    /// Writes `data` at `offset` and gives the buffer back so the caller can reuse it.
    pub(crate) async fn write_chunk(&self, offset: u64, data: Vec<u8>) -> Result<Vec<u8>> {
        let file = Arc::clone(&self.file);
        blocking(move || write_all_at(&file, &data, offset).map(|()| data)).await
    }

    /// Flushes written data to the disk. Call before recording chunks as complete, so a crash cannot mark unwritten
    /// data as received.
    pub(crate) async fn sync(&self) -> Result<()> {
        let file = Arc::clone(&self.file);
        blocking(move || file.sync_data()).await
    }

    /// Blocking version of [`ChunkWriter::sync`], for use in `Drop`.
    pub(crate) fn sync_blocking(&self) -> Result<()> {
        self.file.sync_data().map_err(Error::Io)
    }
}

/// Runs blocking file work on tokio's blocking thread pool.
async fn blocking<T, F>(work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> io::Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(work).await.map_err(|error| Error::Io(io::Error::other(error)))?.map_err(Error::Io)
}

#[cfg(unix)]
fn write_all_at(file: &File, data: &[u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, data, offset)
}

/// Windows has no `write_all_at`, so this loops over `seek_write`, which may write fewer bytes than asked.
#[cfg(windows)]
fn write_all_at(file: &File, mut data: &[u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;

    while !data.is_empty() {
        match file.seek_write(data, offset) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
            Ok(written) => {
                data = &data[written..];
                offset += written as u64;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn chunks_land_at_their_offset_in_any_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.lanshare-part");
        let writer = ChunkWriter::open(&path, 8).await.unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 8, "preallocated");

        writer.write_chunk(4, b"5678".to_vec()).await.unwrap();
        writer.write_chunk(0, b"1234".to_vec()).await.unwrap();
        writer.sync().await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"12345678");
    }

    #[tokio::test]
    async fn reopening_keeps_written_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("resumed.lanshare-part");
        std::fs::write(&path, b"abcd").unwrap();
        let writer = ChunkWriter::open(&path, 6).await.unwrap();
        writer.write_chunk(4, b"ef".to_vec()).await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"abcdef");
    }
}
