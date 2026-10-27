//! Sending side: offers one file, sends the chunks the receiver does not have yet, and reconnects if the connection
//! drops.

use std::{
    collections::{HashSet, VecDeque},
    fs::File,
    future::Future,
    io,
    mem::take,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::watch,
    task::JoinHandle,
    time::Instant,
};
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::{
    chunk::{bitset_contains, bitset_len, read_and_hash, root_hash, ChunkHash, ChunkLayout, DEFAULT_CHUNK_SIZE},
    hash_cache::HashCache,
    read_frame,
    retry::Backoff,
    sanitize_file_name, write_all_idle, write_frame, Progress,
};
use crate::{
    error::{Error, Result},
    identity::{DeviceId, DeviceInfo},
    protocol::{read_message, EntryResume, Message, OfferEntry, PROTOCOL_VERSION},
};

/// Namespace for transfer ids (UUID v5). Any fixed UUID works; it only has to never change.
const TRANSFER_NAMESPACE: Uuid = Uuid::from_u128(0x5d1f_3c2a_8b4e_4f6a_9c7d_2e1b_0a39_4c58);

/// Most chunks sent without an answer. Bounds how far the sender runs ahead, so acks and nacks are read in time.
const MAX_IN_FLIGHT: usize = 8;

/// Settings for [`send_file`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendOptions {
    /// Chunk size to offer. Defaults to [`DEFAULT_CHUNK_SIZE`].
    pub chunk_size: u32,
    /// Where chunk hashes are cached between attempts, normally [`paths::hash_cache_dir`](crate::paths).
    pub hash_cache_dir: PathBuf,
}

impl SendOptions {
    /// Default options, caching hashes in `hash_cache_dir`.
    #[must_use]
    pub fn new(hash_cache_dir: PathBuf) -> Self {
        Self { chunk_size: DEFAULT_CHUNK_SIZE, hash_cache_dir }
    }
}

/// Sends the file at `file_path` as device `local`, publishing progress on `progress`.
///
/// `connect` opens a new connection to the receiver. It is called once at the start, and again after the connection
/// drops: then the sender waits 1 s, 2 s, 5 s, 10 s, then every 10 s between attempts, for up to two minutes
/// ([`RETRY_WINDOW`](super::RETRY_WINDOW)), and sends the same offer so the receiver can resume. The file is read one
/// chunk at a time and never loaded into memory.
///
/// `C: FnMut() -> F` means "any closure returning a future", like passing a `func() (Conn, error)` in Go.
///
/// # Errors
///
/// - the first `connect` error, unchanged
/// - [`Error::ConnectionLost`] if no reconnect succeeded within the retry window
/// - [`Error::Declined`], [`Error::VersionMismatch`], [`Error::InsufficientSpace`] if the receiver refuses
/// - [`Error::HashMismatch`] if the receiver could not verify the data
/// - [`Error::InvalidFileName`] if the path has no usable file name
/// - [`Error::Io`] if the file cannot be read, is not a regular file, or shrinks while sending
pub async fn send_file<S, C, F>(
    mut connect: C,
    file_path: &Path,
    local: &DeviceInfo,
    options: &SendOptions,
    progress: &watch::Sender<Progress>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
    C: FnMut() -> F,
    F: Future<Output = Result<S>>,
{
    let mut source = Source::open(file_path, local.id, options).await?;
    let mut stream = connect().await?;
    let mut backoff: Option<Backoff> = None;
    loop {
        source.acked_this_session = 0;
        let result = run_session(&mut stream, &mut source, local, progress).await;
        source.cache.save().await;
        match result {
            Ok(()) => {
                source.cache.remove().await;
                info!(file_name = %source.entry.relative_path, size = source.entry.size, "file sent");
                return Ok(());
            }
            Err(Error::ConnectionLost) => {
                // A connection that moved data earns a fresh two-minute window; one that died at once does not.
                if source.acked_this_session > 0 || backoff.is_none() {
                    backoff = Some(Backoff::new(Instant::now()));
                }
                warn!("connection lost, reconnecting");
                if let Some(backoff) = backoff.as_mut() {
                    stream = reconnect(&mut connect, backoff).await?;
                }
            }
            Err(error) => return Err(error),
        }
    }
}

/// Calls `connect` after each backoff delay until it succeeds or the retry window ends.
async fn reconnect<S, C, F>(connect: &mut C, backoff: &mut Backoff) -> Result<S>
where
    C: FnMut() -> F,
    F: Future<Output = Result<S>>,
{
    while let Some(delay) = backoff.next_delay(Instant::now()) {
        tokio::time::sleep(delay).await;
        match connect().await {
            Ok(stream) => {
                info!("reconnected");
                return Ok(stream);
            }
            Err(error) => debug!(%error, "reconnect attempt failed"),
        }
    }
    Err(Error::ConnectionLost)
}

/// The file being sent, plus everything that stays the same across reconnects.
struct Source {
    /// `std::fs::File` in an `Arc`, so chunk reads can run in `spawn_blocking` with positional reads.
    file: Arc<File>,
    layout: ChunkLayout,
    transfer_id: Uuid,
    entry: OfferEntry,
    cache: HashCache,
    acked_this_session: u64,
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
        let transfer_id = transfer_id(device_id, &absolute_path, size, modified);
        let layout = ChunkLayout::new(size, options.chunk_size)?;
        let cache = HashCache::open(&options.hash_cache_dir, transfer_id, &layout).await;
        Ok(Self {
            file: Arc::new(file.into_std().await),
            layout,
            transfer_id,
            entry: OfferEntry { relative_path, size },
            cache,
            acked_this_session: 0,
        })
    }

    fn offer(&self) -> Message {
        Message::Offer {
            transfer_id: self.transfer_id,
            chunk_size: self.layout.chunk_size(),
            entries: vec![self.entry.clone()],
        }
    }

    /// Starts reading and hashing chunk `index` into `buffer` on the blocking pool. It runs in the background until
    /// [`PendingRead::finish`] is awaited.
    fn start_read(&self, index: u64, mut buffer: Vec<u8>) -> PendingRead {
        let file = Arc::clone(&self.file);
        let layout = self.layout;
        let task = tokio::task::spawn_blocking(move || {
            let hash = read_and_hash(&file, &layout, index, &mut buffer).map_err(Error::Io)?;
            Ok((buffer, hash))
        });
        PendingRead { index, task }
    }

    /// Hash of chunk `index`: from the cache, or read and hashed from disk (no network).
    async fn chunk_hash(&mut self, index: u64) -> Result<ChunkHash> {
        if let Some(hash) = self.cache.get(index) {
            return Ok(hash);
        }
        let (_buffer, hash) = self.start_read(index, Vec::new()).finish().await?;
        self.cache.record(index, hash).await;
        Ok(hash)
    }

    /// Root hash of the whole file, from every chunk hash.
    async fn root_hash(&mut self) -> Result<ChunkHash> {
        let mut hashes = Vec::new();
        for index in 0..self.layout.count() {
            hashes.push(self.chunk_hash(index).await?);
        }
        Ok(root_hash(&hashes))
    }

    /// Bytes covered by the chunks set in `have`.
    fn bytes_in(&self, have: &[u8]) -> u64 {
        (0..self.layout.count())
            .filter(|index| bitset_contains(have, *index))
            .map(|i| u64::from(self.layout.len(i)))
            .sum()
    }
}

/// A chunk being read from disk and hashed in the background.
struct PendingRead {
    index: u64,
    task: JoinHandle<Result<(Vec<u8>, ChunkHash)>>,
}

impl PendingRead {
    /// Waits for the read. Returns the buffer holding the chunk, and its hash.
    async fn finish(self) -> Result<(Vec<u8>, ChunkHash)> {
        self.task.await.map_err(|error| Error::Io(io::Error::other(error)))?
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

/// One connection: hello, offer, the missing chunks, `EntryDone`, `Done`, and the receiver's `Done`.
async fn run_session<S>(
    stream: &mut S,
    source: &mut Source,
    local: &DeviceInfo,
    progress: &watch::Sender<Progress>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let hello =
        Message::Hello { protocol_version: PROTOCOL_VERSION, device_id: local.id, device_name: local.name.clone() };
    write_frame(stream, &hello).await?;
    write_frame(stream, &source.offer()).await?;
    let have = wait_for_accept(stream, &source.layout).await?;

    let resumed = source.bytes_in(&have);
    if resumed > 0 {
        info!(resumed, size = source.entry.size, "receiver already has part of the file, resuming");
    }
    progress.send_replace(Progress { transferred: resumed, total: source.entry.size, resumed });
    send_chunks(stream, source, &have, progress).await?;

    let root_hash = source.root_hash().await?;
    write_frame(stream, &Message::EntryDone { entry: 0, root_hash }).await?;
    write_frame(stream, &Message::Done).await?;
    match read_frame(stream).await? {
        Message::Done => Ok(()),
        Message::Error { code, detail } => Err(peer_error(code, detail.as_deref())),
        other => Err(Error::protocol_violation(format!("expected Done, got {other:?}"))),
    }
}

/// Waits for the receiver's answer to the offer and returns the bitset of chunks it already has.
///
/// No idle timeout here: the receiver may be waiting for a person to accept.
async fn wait_for_accept<S>(stream: &mut S, layout: &ChunkLayout) -> Result<Vec<u8>>
where
    S: AsyncRead + Unpin,
{
    match read_message(stream).await? {
        Message::Accept { resume } => resume_bitset(&resume, layout),
        Message::Reject { code } => {
            debug!(code = code.name(), "offer rejected");
            Err(Error::from_peer(code))
        }
        Message::Error { code, detail } => Err(peer_error(code, detail.as_deref())),
        other => Err(Error::protocol_violation(format!("expected Accept or Reject, got {other:?}"))),
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

/// Sends every chunk not in `have`, keeping up to [`MAX_IN_FLIGHT`] unanswered. Resends chunks the receiver nacks.
async fn send_chunks<S>(
    stream: &mut S,
    source: &mut Source,
    have: &[u8],
    progress: &watch::Sender<Progress>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut queue: VecDeque<u64> = (0..source.layout.count()).filter(|index| !bitset_contains(have, *index)).collect();
    let mut in_flight = HashSet::new();
    let mut next_read: Option<PendingRead> = None;
    // Two buffers take turns: one is on its way to the network while the other is filled from disk.
    let mut spare = Vec::new();
    while !queue.is_empty() || next_read.is_some() || !in_flight.is_empty() {
        if in_flight.len() < MAX_IN_FLIGHT {
            let current =
                next_read.take().or_else(|| queue.pop_front().map(|index| source.start_read(index, take(&mut spare))));
            if let Some(current) = current {
                let index = current.index;
                let (buffer, hash) = current.finish().await?;
                // Read the next chunk from disk while this one goes over the network.
                next_read = queue.pop_front().map(|next| source.start_read(next, take(&mut spare)));
                send_chunk(stream, source, index, &buffer, hash).await?;
                spare = buffer;
                in_flight.insert(index);
                continue;
            }
        }
        match read_frame(stream).await? {
            Message::ChunkAck { entry: 0, index } if in_flight.remove(&index) => {
                source.acked_this_session += 1;
                let len = u64::from(source.layout.len(index));
                progress.send_modify(|progress| progress.transferred += len);
            }
            Message::ChunkNack { entry: 0, index } if in_flight.remove(&index) => {
                debug!(index, "receiver reported a hash mismatch, resending chunk");
                queue.push_front(index);
            }
            Message::Error { code, detail } => return Err(peer_error(code, detail.as_deref())),
            other => return Err(Error::protocol_violation(format!("expected ChunkAck or ChunkNack, got {other:?}"))),
        }
    }
    Ok(())
}

/// Sends one `ChunkHeader` and the chunk's raw bytes, and caches its hash.
async fn send_chunk<S>(stream: &mut S, source: &mut Source, index: u64, data: &[u8], hash: ChunkHash) -> Result<()>
where
    S: AsyncWrite + Unpin,
{
    source.cache.record(index, hash).await;
    let len = source.layout.len(index);
    write_frame(stream, &Message::ChunkHeader { entry: 0, index, len, hash }).await?;
    write_all_idle(stream, data).await
}

/// Turns an `Error` message from the receiver into a local error, logging its detail.
fn peer_error(code: crate::ErrorCode, detail: Option<&str>) -> Error {
    warn!(code = code.name(), ?detail, "receiver reported an error");
    Error::from_peer(code)
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
