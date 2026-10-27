//! Sending side: offers one file, sends the chunks the receiver does not have yet over parallel data streams, and
//! reconnects if the connection drops.

use std::{
    future::Future,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use tokio::{io::AsyncRead, sync::watch, time::Instant};
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::{
    chunk::{bitset_contains, bitset_len, root_hash, ChunkHash, ChunkLayout, DEFAULT_CHUNK_SIZE},
    control::{ControlWriter, TransferControl},
    hash_cache::HashCache,
    peer_error, read_frame,
    retry::Backoff,
    sanitize_file_name,
    scheduler::{send_chunks, ChunkJob, ChunkSource, DEFAULT_STREAMS},
    Progress,
};
use crate::{
    error::{Error, Result},
    identity::{DeviceId, DeviceInfo},
    protocol::{read_message, EntryResume, Message, OfferEntry, PROTOCOL_VERSION},
    transport::Connection,
};

/// Namespace for transfer ids (UUID v5). Any fixed UUID works; it only has to never change.
const TRANSFER_NAMESPACE: Uuid = Uuid::from_u128(0x5d1f_3c2a_8b4e_4f6a_9c7d_2e1b_0a39_4c58);

/// Settings for [`send_file`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendOptions {
    /// Chunk size to offer. Defaults to [`DEFAULT_CHUNK_SIZE`].
    pub chunk_size: u32,
    /// Where chunk hashes are cached between attempts, normally [`paths::hash_cache_dir`](crate::paths).
    pub hash_cache_dir: PathBuf,
    /// Number of parallel data streams, 1 to [`MAX_STREAMS`](super::MAX_STREAMS). Defaults to [`DEFAULT_STREAMS`].
    pub streams: usize,
}

impl SendOptions {
    /// Default options, caching hashes in `hash_cache_dir`.
    #[must_use]
    pub fn new(hash_cache_dir: PathBuf) -> Self {
        Self { chunk_size: DEFAULT_CHUNK_SIZE, hash_cache_dir, streams: DEFAULT_STREAMS }
    }
}

/// Sends the file at `file_path` as device `local`, publishing progress on `progress`.
///
/// `connect` opens a new connection to the receiver. It is called once at the start, and again after the connection
/// drops: then the sender waits 1 s, 2 s, 5 s, 10 s, then every 10 s between attempts, for up to two minutes
/// ([`RETRY_WINDOW`](super::RETRY_WINDOW)), and sends the same offer so the receiver can resume. The file is read one
/// chunk at a time and never loaded into memory. `control` pauses, resumes and throttles the transfer while it runs.
///
/// `F: FnMut() -> Fut` means "any closure returning a future", like passing a `func() (Conn, error)` in Go.
///
/// # Errors
///
/// - the first `connect` error, unchanged
/// - [`Error::ConnectionLost`] if no reconnect succeeded within the retry window
/// - [`Error::Declined`], [`Error::VersionMismatch`], [`Error::InsufficientSpace`] if the receiver refuses
/// - [`Error::HashMismatch`] if the receiver could not verify the data
/// - [`Error::InvalidFileName`] if the path has no usable file name
/// - [`Error::Io`] if the file cannot be read, is not a regular file, or shrinks while sending
pub async fn send_file<C, F, Fut>(
    mut connect: F,
    file_path: &Path,
    local: &DeviceInfo,
    options: &SendOptions,
    control: &TransferControl,
    progress: &watch::Sender<Progress>,
) -> Result<()>
where
    C: Connection,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<C>>,
{
    let source = Source::open(file_path, local.id, options).await?;
    let mut cache = HashCache::open(&options.hash_cache_dir, source.transfer_id, source.chunks.layout()).await;
    let session = Session { source: &source, local, streams: options.streams, control, progress };
    let mut connection = connect().await?;
    let mut backoff: Option<Backoff> = None;
    loop {
        let mut acked = 0;
        let result = session.run(&connection, &mut cache, &mut acked).await;
        cache.save().await;
        match result {
            Ok(()) => {
                cache.remove().await;
                info!(file_name = %source.entry.relative_path, size = source.entry.size, "file sent");
                return Ok(());
            }
            Err(Error::ConnectionLost) => {
                // Close the old connection now, so the receiver notices while we wait to reconnect.
                drop(connection);
                // A connection that moved data earns a fresh two-minute window; one that died at once does not.
                let backoff = match backoff.as_mut() {
                    Some(backoff) if acked == 0 => backoff,
                    _ => backoff.insert(Backoff::new(Instant::now())),
                };
                warn!("connection lost, reconnecting");
                connection = reconnect(&mut connect, backoff).await?;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Calls `connect` after each backoff delay until it succeeds or the retry window ends.
async fn reconnect<C, F, Fut>(connect: &mut F, backoff: &mut Backoff) -> Result<C>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<C>>,
{
    while let Some(delay) = backoff.next_delay(Instant::now()) {
        tokio::time::sleep(delay).await;
        match connect().await {
            Ok(connection) => {
                info!("reconnected");
                return Ok(connection);
            }
            Err(error) => debug!(%error, "reconnect attempt failed"),
        }
    }
    Err(Error::ConnectionLost)
}

/// The file being sent: everything that stays the same across reconnects.
struct Source {
    chunks: ChunkSource,
    transfer_id: Uuid,
    entry: OfferEntry,
}

impl Source {
    async fn open(file_path: &Path, device_id: DeviceId, options: &SendOptions) -> Result<Self> {
        let relative_path = offer_file_name(file_path)?;
        // Local file errors use `map_err(Error::Io)` so `?` cannot mistake them for a lost connection.
        let file = tokio::fs::File::open(file_path).await.map_err(Error::Io)?;
        let metadata = file.metadata().await.map_err(Error::Io)?;
        if !metadata.is_file() {
            return Err(Error::Io(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file")));
        }
        let size = metadata.len();
        let modified = metadata.modified().map_err(Error::Io)?;
        let absolute_path = tokio::fs::canonicalize(file_path).await.map_err(Error::Io)?;
        let layout = ChunkLayout::new(size, options.chunk_size)?;
        Ok(Self {
            chunks: ChunkSource::new(Arc::new(file.into_std().await), layout),
            transfer_id: transfer_id(device_id, &absolute_path, size, modified),
            entry: OfferEntry { relative_path, size },
        })
    }

    fn layout(&self) -> &ChunkLayout {
        self.chunks.layout()
    }

    fn offer(&self) -> Message {
        // Exactly one entry until phase 7 adds folders.
        Message::Offer {
            transfer_id: self.transfer_id,
            chunk_size: self.layout().chunk_size(),
            entries: vec![self.entry.clone()],
        }
    }

    /// Root hash of the whole file. Chunk hashes come from the cache, or are read and hashed from disk (no network).
    async fn root_hash(&self, cache: &mut HashCache) -> Result<ChunkHash> {
        let mut hashes = Vec::new();
        for index in 0..self.layout().count() {
            let hash = if let Some(hash) = cache.get(index) {
                hash
            } else {
                let (_buffer, hash) = self.chunks.read(index, Vec::new()).await?;
                cache.record(index, hash).await;
                hash
            };
            hashes.push(hash);
        }
        Ok(root_hash(&hashes))
    }

    /// Bytes covered by the chunks set in `have`.
    fn bytes_in(&self, have: &[u8]) -> u64 {
        let layout = self.layout();
        (0..layout.count())
            .filter(|index| bitset_contains(have, *index))
            .map(|index| u64::from(layout.len(index)))
            .sum()
    }
}

/// The name to put in the offer: the last path component, which must be valid UTF-8 and pass sanitizing.
fn offer_file_name(file_path: &Path) -> Result<String> {
    let name = file_path.file_name().and_then(|name| name.to_str()).ok_or(Error::InvalidFileName)?;
    sanitize_file_name(name)
}

/// UUID v5 of (sender device id, absolute source path, size, modified time). Retrying the same unchanged file gives
/// the same id; editing the file gives a new one, so stale chunks are never reused.
fn transfer_id(device_id: DeviceId, absolute_path: &Path, size: u64, modified: SystemTime) -> Uuid {
    let since_epoch = modified.duration_since(UNIX_EPOCH).unwrap_or_default();
    let mut name = Vec::new();
    name.extend_from_slice(device_id.as_bytes());
    name.extend_from_slice(&size.to_le_bytes());
    name.extend_from_slice(&since_epoch.as_secs().to_le_bytes());
    name.extend_from_slice(&since_epoch.subsec_nanos().to_le_bytes());
    // The path goes last because it is the only part with a variable length.
    name.extend_from_slice(absolute_path.as_os_str().as_encoded_bytes());
    Uuid::new_v5(&TRANSFER_NAMESPACE, &name)
}

/// Everything one connection attempt needs that does not change between attempts.
struct Session<'a> {
    source: &'a Source,
    local: &'a DeviceInfo,
    streams: usize,
    control: &'a TransferControl,
    progress: &'a watch::Sender<Progress>,
}

impl Session<'_> {
    /// One connection: hello and offer, the missing chunks, `EntryDone`, `Done`, and the receiver's `Done`.
    async fn run<C: Connection>(&self, connection: &C, cache: &mut HashCache, acked: &mut u64) -> Result<()> {
        let (send, mut replies) = connection.open_control().await?;
        let writer = ControlWriter::new(send);
        let local = self.local;
        let hello =
            Message::Hello { protocol_version: PROTOCOL_VERSION, device_id: local.id, device_name: local.name.clone() };
        writer.send(&hello).await?;
        writer.send(&self.source.offer()).await?;
        let have = wait_for_accept(&mut replies, self.source.layout()).await?;

        let size = self.source.entry.size;
        let resumed = self.source.bytes_in(&have);
        if resumed > 0 {
            info!(resumed, size, "receiver already has part of the file, resuming");
        }
        self.progress.send_replace(Progress { transferred: resumed, total: size, resumed });
        let layout = self.source.layout();
        let missing: Vec<u64> = (0..layout.count()).filter(|index| !bitset_contains(&have, *index)).collect();
        if !missing.is_empty() {
            let job = ChunkJob {
                source: &self.source.chunks,
                transfer_id: self.source.transfer_id,
                missing,
                streams: self.streams,
                control: self.control,
            };
            send_chunks(connection, job, &mut replies, &writer, cache, self.progress, acked).await?;
        }

        let root_hash = self.source.root_hash(cache).await?;
        writer.send(&Message::EntryDone { entry: 0, root_hash }).await?;
        writer.send(&Message::Done).await?;
        wait_for_done(&mut replies).await?;
        writer.shutdown().await;
        Ok(())
    }
}

/// Waits for the receiver's answer to the offer and returns the bitset of chunks it already has.
///
/// No timeout here: the receiver may be waiting for a person to accept.
async fn wait_for_accept<R>(replies: &mut R, layout: &ChunkLayout) -> Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    match read_message(replies).await? {
        Message::Accept { resume } => resume_bitset(&resume, layout),
        Message::Reject { code } => {
            debug!(code = code.name(), "offer rejected");
            Err(Error::from_peer(code))
        }
        Message::Error { code, detail } => Err(peer_error(code, detail.as_deref())),
        other => Err(Error::protocol_violation(format!("expected Accept or Reject, got {other:?}"))),
    }
}

/// Waits for the receiver's final `Done`. A late `Pause` or `Resume` no longer matters and is skipped.
async fn wait_for_done<R>(replies: &mut R) -> Result<()>
where
    R: AsyncRead + Unpin,
{
    loop {
        match read_frame(replies).await? {
            Message::Done => return Ok(()),
            Message::Pause | Message::Resume => {}
            Message::Error { code, detail } => return Err(peer_error(code, detail.as_deref())),
            other => return Err(Error::protocol_violation(format!("expected Done, got {other:?}"))),
        }
    }
}

/// Checks the `resume` list from `Accept` against the single offered entry.
fn resume_bitset(resume: &[EntryResume], layout: &ChunkLayout) -> Result<Vec<u8>> {
    match resume {
        [] => Ok(Vec::new()),
        [EntryResume { entry: 0, have }] if have.len() as u64 == bitset_len(layout.count()) => Ok(have.clone()),
        _ => Err(Error::protocol_violation(format!("bad resume list {resume:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn transfer_id_is_stable_and_changes_with_the_file() {
        let device = DeviceId::new_random();
        let path = Path::new("/home/me/movie.mkv");
        let modified = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let id = transfer_id(device, path, 100, modified);
        assert_eq!(id, transfer_id(device, path, 100, modified));
        assert_ne!(id, transfer_id(device, path, 101, modified));
        assert_ne!(id, transfer_id(device, path, 100, modified + Duration::from_nanos(1)));
        assert_ne!(id, transfer_id(device, Path::new("/home/me/other.mkv"), 100, modified));
        assert_ne!(id, transfer_id(DeviceId::new_random(), path, 100, modified));
    }

    #[test]
    fn resume_list_must_match_the_offer() {
        let layout = ChunkLayout::new(100, 10).unwrap();
        assert_eq!(resume_bitset(&[], &layout).unwrap(), Vec::<u8>::new());
        let good = vec![EntryResume { entry: 0, have: vec![1, 2] }];
        assert_eq!(resume_bitset(&good, &layout).unwrap(), vec![1, 2]);
        assert!(resume_bitset(&[EntryResume { entry: 0, have: vec![1] }], &layout).is_err());
        assert!(resume_bitset(&[EntryResume { entry: 1, have: vec![1, 2] }], &layout).is_err());
    }
}
