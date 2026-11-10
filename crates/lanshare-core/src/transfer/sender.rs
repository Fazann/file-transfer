//! Sending side: offers files and folders, sends the chunks the receiver does not have yet over parallel data
//! streams, and reconnects if the connection drops.

use std::{
    collections::HashMap,
    future::Future,
    io,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::watch,
    time::{Duration, Instant},
};
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::{
    chunk::{bitset_contains, bitset_len, root_hash, ChunkLayout, DEFAULT_CHUNK_SIZE},
    control::{ControlWriter, TransferControl},
    hash_cache::HashCache,
    manifest::{build_manifest, ManifestEntry},
    peer_error, read_frame,
    retry::Backoff,
    scheduler::{send_chunks, ChunkJob, ChunkRef, ChunkSource, DEFAULT_STREAMS},
    Progress,
};
use crate::{
    error::{Error, Result},
    identity::{DeviceId, DeviceInfo},
    protocol::{read_message, EntryResume, Message, OfferEntry, MIN_PROTOCOL_VERSION},
    transport::Connection,
};

/// After telling the receiver about a cancel, how long to wait for it to close the connection.
const CANCEL_LINGER: Duration = Duration::from_secs(2);

/// Namespace for transfer ids (UUID v5). Any fixed UUID works; it only has to never change.
const TRANSFER_NAMESPACE: Uuid = Uuid::from_u128(0x5d1f_3c2a_8b4e_4f6a_9c7d_2e1b_0a39_4c58);

/// Settings for [`send_files`].
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

/// Sends one file. Shorthand for [`send_files`] with a single path.
///
/// # Errors
///
/// As [`send_files`].
pub async fn send_file<C, F, Fut>(
    connect: F,
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
    send_files(connect, &[file_path.to_owned()], local, options, control, progress).await
}

/// Sends the files and folders at `paths` in one transfer as device `local`, publishing progress on `progress`.
///
/// Folders are sent with everything inside them (see `manifest.rs`). `connect` opens a new connection to the
/// receiver. It is called once at the start, and again after the connection drops: then the sender waits 1 s, 2 s,
/// 5 s, 10 s, then every 10 s between attempts, for up to two minutes ([`RETRY_WINDOW`](super::RETRY_WINDOW)), and
/// sends the same offer so the receiver can resume. Files are read one chunk at a time and never loaded into memory.
/// `control` pauses, resumes, cancels and throttles the transfer while it runs.
///
/// `F: FnMut() -> Fut` means "any closure returning a future", like passing a `func() (Conn, error)` in Go.
///
/// # Errors
///
/// - the first `connect` error, unchanged
/// - [`Error::ConnectionLost`] if no reconnect succeeded within the retry window
/// - [`Error::Declined`], [`Error::VersionMismatch`], [`Error::InsufficientSpace`] if the receiver refuses
/// - [`Error::HashMismatch`] if the receiver could not verify the data
/// - [`Error::InvalidFileName`] if a name cannot be sent safely, or two picked items have the same name
/// - [`Error::Io`] if a file cannot be read or shrinks while sending
/// - [`Error::Cancelled`] if `control` was cancelled
pub async fn send_files<C, F, Fut>(
    mut connect: F,
    paths: &[PathBuf],
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
    let source = Source::open(paths, local.id, options).await?;
    let mut caches = source.open_caches(&options.hash_cache_dir).await;
    let session = Session { source: &source, local, streams: options.streams, control, progress };
    // A running session watches for cancelling itself, so it can tell the receiver; connecting and waiting to
    // reconnect stop here.
    let mut connection = until_cancelled(control, connect()).await?;
    let mut backoff: Option<Backoff> = None;
    loop {
        let mut acked = 0;
        let result = session.run(&connection, &mut caches, &mut acked).await;
        for cache in &mut caches {
            cache.save().await;
        }
        match result {
            Ok(()) => {
                for cache in caches {
                    cache.remove().await;
                }
                info!(entries = source.entries.len(), size = source.total_size(), "sent");
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
                connection = until_cancelled(control, reconnect(&mut connect, backoff)).await?;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Runs `operation`, or fails with [`Error::Cancelled`] as soon as `control` is cancelled.
async fn until_cancelled<T>(control: &TransferControl, operation: impl Future<Output = Result<T>>) -> Result<T> {
    tokio::select! {
        result = operation => result,
        () = control.cancelled() => Err(Error::Cancelled),
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

/// What is being sent: everything that stays the same across reconnects.
struct Source {
    transfer_id: Uuid,
    chunk_size: u32,
    /// What the offer lists, in order.
    entries: Vec<OfferEntry>,
    /// The data of each entry, `None` for folders.
    chunks: Vec<Option<ChunkSource>>,
}

impl Source {
    async fn open(paths: &[PathBuf], device_id: DeviceId, options: &SendOptions) -> Result<Self> {
        let paths = paths.to_vec();
        let manifest = tokio::task::spawn_blocking(move || build_manifest(&paths))
            .await
            .map_err(|error| Error::Io(io::Error::other(error)))??;
        let mut entries = Vec::with_capacity(manifest.len());
        let mut chunks = Vec::with_capacity(manifest.len());
        for item in &manifest {
            entries.push(OfferEntry { relative_path: item.relative.as_wire(), size: item.size });
            chunks.push(match &item.source {
                Some(path) => Some(ChunkSource::new(path.clone(), ChunkLayout::new(item.size, options.chunk_size)?)),
                None => None,
            });
        }
        Ok(Self { transfer_id: transfer_id(device_id, &manifest), chunk_size: options.chunk_size, entries, chunks })
    }

    fn total_size(&self) -> u64 {
        self.entries.iter().map(|entry| entry.size).sum()
    }

    fn offer(&self) -> Message {
        Message::Offer { transfer_id: self.transfer_id, chunk_size: self.chunk_size, entries: self.entries.clone() }
    }

    /// One hash cache per entry. Only files of several chunks keep theirs on disk; one chunk is cheap to hash again,
    /// and a folder of thousands of small files should not leave thousands of cache files.
    async fn open_caches(&self, dir: &Path) -> Vec<HashCache> {
        let mut caches = Vec::with_capacity(self.chunks.len());
        for (index, chunks) in self.chunks.iter().enumerate() {
            let layout = chunks.as_ref().map_or_else(|| ChunkLayout::empty(self.chunk_size), |chunks| *chunks.layout());
            let id = entry_cache_id(self.transfer_id, index);
            caches.push(if layout.count() > 1 {
                HashCache::open(dir, id, &layout).await
            } else {
                HashCache::in_memory(id, &layout)
            });
        }
        caches
    }

    /// Bytes covered by the chunks the receiver already has.
    fn bytes_in(&self, have: &HashMap<u32, Vec<u8>>) -> u64 {
        have.iter()
            .filter_map(|(entry, bits)| Some((self.chunks.get(*entry as usize)?.as_ref()?.layout(), bits)))
            .map(|(layout, bits)| {
                (0..layout.count())
                    .filter(|index| bitset_contains(bits, *index))
                    .map(|index| u64::from(layout.len(index)))
                    .sum::<u64>()
            })
            .sum()
    }

    /// Chunks the receiver lacks, in entry order, and the entries it lacks nothing of (folders, empty files, and
    /// files it already has completely).
    fn plan(&self, have: &HashMap<u32, Vec<u8>>) -> (Vec<ChunkRef>, Vec<u32>) {
        let mut missing = Vec::new();
        let mut complete = Vec::new();
        for (index, chunks) in self.chunks.iter().enumerate() {
            let entry = u32::try_from(index).unwrap_or(u32::MAX);
            let before = missing.len();
            if let Some(chunks) = chunks {
                let bits = have.get(&entry).map_or(&[][..], Vec::as_slice);
                let count = chunks.layout().count();
                missing.extend((0..count).filter(|chunk| !bitset_contains(bits, *chunk)).map(|chunk| (entry, chunk)));
            }
            if missing.len() == before {
                complete.push(entry);
            }
        }
        (missing, complete)
    }
}

/// UUID v5 over the sender's device id and every entry's path, size, modified time and source. Retrying the same
/// unchanged selection gives the same id; any change gives a new one, so stale chunks are never reused.
fn transfer_id(device_id: DeviceId, manifest: &[ManifestEntry]) -> Uuid {
    let mut name = Vec::new();
    name.extend_from_slice(device_id.as_bytes());
    for entry in manifest {
        let since_epoch = entry.modified.duration_since(UNIX_EPOCH).unwrap_or_default();
        let wire = entry.relative.as_wire();
        // Lengths go before variable-length parts, so two different lists can never produce the same bytes.
        name.extend_from_slice(&(wire.len() as u64).to_le_bytes());
        name.extend_from_slice(wire.as_bytes());
        name.extend_from_slice(&entry.size.to_le_bytes());
        name.extend_from_slice(&since_epoch.as_secs().to_le_bytes());
        name.extend_from_slice(&since_epoch.subsec_nanos().to_le_bytes());
        let source = entry.source.as_deref().map(|path| path.as_os_str().as_encoded_bytes()).unwrap_or_default();
        name.extend_from_slice(&(source.len() as u64).to_le_bytes());
        name.extend_from_slice(source);
    }
    Uuid::new_v5(&TRANSFER_NAMESPACE, &name)
}

/// The hash cache id of one entry: derived from the transfer id, so each entry has its own cache file.
fn entry_cache_id(transfer_id: Uuid, entry: usize) -> Uuid {
    Uuid::new_v5(&transfer_id, &(entry as u64).to_le_bytes())
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
    /// One connection: hello and offer, the missing chunks with an `EntryDone` per entry, `Done`, and the receiver's
    /// `Done`.
    async fn run<C: Connection>(&self, connection: &C, caches: &mut [HashCache], acked: &mut u64) -> Result<()> {
        let (send, mut replies) = connection.open_control().await?;
        let writer = ControlWriter::new(send);
        tokio::select! {
            result = self.exchange(connection, &writer, &mut replies, caches, acked) => return result,
            () = self.control.cancelled() => {}
        }
        info!("transfer cancelled on this device");
        // Best effort: tell the receiver, so it does not wait for a reconnect. Dropping a QUIC connection discards
        // data still on its way, so wait briefly for the receiver to close first.
        let _ = writer.send(&Message::Error { code: crate::ErrorCode::Cancelled, detail: None }).await;
        writer.shutdown().await;
        let mut buffer = [0; 256];
        let drain = async { while matches!(replies.read(&mut buffer).await, Ok(read) if read > 0) {} };
        let _ = tokio::time::timeout(CANCEL_LINGER, drain).await;
        Err(Error::Cancelled)
    }

    async fn exchange<C: Connection, R: AsyncRead + Unpin>(
        &self,
        connection: &C,
        writer: &ControlWriter,
        replies: &mut R,
        caches: &mut [HashCache],
        acked: &mut u64,
    ) -> Result<()> {
        let local = self.local;
        let hello = Message::Hello {
            protocol_version: MIN_PROTOCOL_VERSION,
            device_id: local.id,
            device_name: local.name.clone(),
        };
        writer.send(&hello).await?;
        writer.send(&self.source.offer()).await?;
        let have = wait_for_accept(replies, self.source).await?;

        let total = self.source.total_size();
        let resumed = self.source.bytes_in(&have);
        if resumed > 0 {
            info!(resumed, total, "receiver already has part of the transfer, resuming");
        }
        self.progress.send_replace(Progress { transferred: resumed, total, resumed, current_entry: 0 });
        let (missing, complete) = self.source.plan(&have);
        for entry in complete {
            self.entry_done(writer, entry, caches).await?;
        }
        if !missing.is_empty() {
            let job = ChunkJob {
                sources: &self.source.chunks,
                transfer_id: self.source.transfer_id,
                missing,
                streams: self.streams,
                control: self.control,
            };
            send_chunks(connection, job, replies, writer, caches, self.progress, acked).await?;
        }
        writer.send(&Message::Done).await?;
        wait_for_done(replies).await?;
        writer.shutdown().await;
        Ok(())
    }

    /// Sends `EntryDone` for an entry the receiver needs no chunks of.
    async fn entry_done(&self, writer: &ControlWriter, entry: u32, caches: &mut [HashCache]) -> Result<()> {
        let index = entry as usize;
        let root_hash = match (self.source.chunks.get(index).and_then(Option::as_ref), caches.get_mut(index)) {
            (Some(chunks), Some(cache)) => {
                let root = chunks.root_hash(cache).await?;
                chunks.close();
                root
            }
            // A folder has no chunks; its root is the hash of an empty list, like an empty file's.
            _ => root_hash(&[]),
        };
        writer.send(&Message::EntryDone { entry, root_hash }).await
    }
}

/// Waits for the receiver's answer to the offer and returns the chunks it already has, per entry.
///
/// No timeout here: the receiver may be waiting for a person to accept.
async fn wait_for_accept<R>(replies: &mut R, source: &Source) -> Result<HashMap<u32, Vec<u8>>>
where
    R: AsyncRead + Unpin,
{
    match read_message(replies).await? {
        Message::Accept { resume } => resume_bitsets(&resume, &source.chunks),
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

/// Checks the `resume` list from `Accept`: each entry at most once, a file entry, with a bitset of the right length.
fn resume_bitsets(resume: &[EntryResume], chunks: &[Option<ChunkSource>]) -> Result<HashMap<u32, Vec<u8>>> {
    let mut have = HashMap::new();
    for EntryResume { entry, have: bits } in resume {
        let layout = chunks.get(*entry as usize).and_then(Option::as_ref).map(ChunkSource::layout);
        let fits = layout.is_some_and(|layout| bits.len() as u64 == bitset_len(layout.count()));
        if !fits || have.insert(*entry, bits.clone()).is_some() {
            return Err(Error::protocol_violation(format!("bad resume list entry {entry}")));
        }
    }
    Ok(have)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::transfer::sanitize_relative_path;

    fn entry(path: &str, size: u64, modified: SystemTime) -> ManifestEntry {
        let relative = sanitize_relative_path(path).unwrap();
        ManifestEntry { source: Some(PathBuf::from("/home/me").join(path)), relative, size, modified }
    }

    #[test]
    fn transfer_id_is_stable_and_changes_with_the_files() {
        let device = DeviceId::new_random();
        let modified = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let files = vec![entry("movie.mkv", 100, modified)];
        let id = transfer_id(device, &files);
        assert_eq!(id, transfer_id(device, &files));
        assert_ne!(id, transfer_id(device, &[entry("movie.mkv", 101, modified)]));
        assert_ne!(id, transfer_id(device, &[entry("movie.mkv", 100, modified + Duration::from_nanos(1))]));
        assert_ne!(id, transfer_id(device, &[entry("other.mkv", 100, modified)]));
        assert_ne!(id, transfer_id(DeviceId::new_random(), &files));
        let more = vec![entry("movie.mkv", 100, modified), entry("notes.txt", 1, modified)];
        assert_ne!(id, transfer_id(device, &more));
    }

    #[test]
    fn resume_list_must_match_the_offer() {
        let source = |size| Some(ChunkSource::new(PathBuf::from("/x"), ChunkLayout::new(size, 10).unwrap()));
        let chunks = vec![source(100), None, source(5)];
        assert!(resume_bitsets(&[], &chunks).unwrap().is_empty());
        let good = [EntryResume { entry: 0, have: vec![1, 2] }, EntryResume { entry: 2, have: vec![1] }];
        assert_eq!(resume_bitsets(&good, &chunks).unwrap().len(), 2);
        let short = [EntryResume { entry: 0, have: vec![1] }];
        assert!(resume_bitsets(&short, &chunks).is_err());
        let folder = [EntryResume { entry: 1, have: vec![] }];
        assert!(resume_bitsets(&folder, &chunks).is_err());
        let twice = [EntryResume { entry: 2, have: vec![1] }, EntryResume { entry: 2, have: vec![1] }];
        assert!(resume_bitsets(&twice, &chunks).is_err());
        assert!(resume_bitsets(&[EntryResume { entry: 9, have: vec![] }], &chunks).is_err());
    }
}
