//! Parallel chunk sending. Missing chunks wait in one shared queue and each data stream has a worker that pulls the
//! next chunk, so faster streams do more of the work, and several small files travel at once. A chunk never spans
//! two files. Acks and nacks come back on the control channel; a nacked chunk goes back into the queue.

use std::{
    collections::HashMap,
    fs::File,
    io,
    mem::take,
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncWriteExt},
    sync::{watch, Semaphore},
    task::JoinSet,
};
use tracing::debug;
use uuid::Uuid;

use super::{
    chunk::{read_and_hash, root_hash, ChunkHash, ChunkLayout},
    control::{Activity, ControlWriter, PauseSync, TransferControl},
    hash_cache::HashCache,
    peer_error, write_all_limited, write_frame, Progress,
};
use crate::{
    error::{Error, Result},
    protocol::{read_message, Message},
    transport::{Connection, SendHalf},
};

/// Data streams per transfer unless `config.toml` says otherwise.
pub const DEFAULT_STREAMS: usize = 4;

/// Most data streams per transfer.
pub const MAX_STREAMS: usize = 16;

/// Chunks each stream may have sent without an answer. Bounds how far the sender runs ahead, so nacks are handled
/// while the chunk is still fresh.
const IN_FLIGHT_PER_STREAM: usize = 4;

/// How long idle workers get to close their streams once every chunk is acknowledged.
const WORKER_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Reads chunks of one file being sent. Cheap to clone: clones share the file.
///
/// The file is opened on the first read and closed with [`ChunkSource::close`], so a folder of thousands of files
/// never has them all open at once.
#[derive(Debug, Clone)]
pub(crate) struct ChunkSource {
    path: Arc<PathBuf>,
    /// `std::fs::File` in an `Arc`, so reads can run in `spawn_blocking` with positional reads.
    file: Arc<Mutex<Option<Arc<File>>>>,
    layout: ChunkLayout,
}

impl ChunkSource {
    pub(crate) fn new(path: PathBuf, layout: ChunkLayout) -> Self {
        Self { path: Arc::new(path), file: Arc::default(), layout }
    }

    pub(crate) fn layout(&self) -> &ChunkLayout {
        &self.layout
    }

    /// Reads chunk `index` into `buffer` and hashes it, on the blocking pool. Returns the filled buffer and the hash.
    ///
    /// With several workers reading at once, chunks are hashed on several CPU cores in parallel.
    pub(crate) async fn read(&self, index: u64, mut buffer: Vec<u8>) -> Result<(Vec<u8>, ChunkHash)> {
        let source = self.clone();
        tokio::task::spawn_blocking(move || {
            let file = source.open()?;
            let hash = read_and_hash(&file, &source.layout, index, &mut buffer).map_err(Error::Io)?;
            Ok((buffer, hash))
        })
        .await
        .map_err(|error| Error::Io(io::Error::other(error)))?
    }

    /// The open file, opening it first if needed. Blocking.
    fn open(&self) -> Result<Arc<File>> {
        let mut file = self.file.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(open) = file.as_ref() {
            return Ok(Arc::clone(open));
        }
        // Local file errors use `map_err(Error::Io)` so they cannot be mistaken for a lost connection.
        let opened = Arc::new(File::open(self.path.as_ref()).map_err(Error::Io)?);
        *file = Some(Arc::clone(&opened));
        Ok(opened)
    }

    /// Closes the file. A later read opens it again.
    pub(crate) fn close(&self) {
        self.file.lock().unwrap_or_else(PoisonError::into_inner).take();
    }

    /// Root hash of the whole file. Chunk hashes come from `cache`, or are read and hashed from disk (no network).
    pub(crate) async fn root_hash(&self, cache: &mut HashCache) -> Result<ChunkHash> {
        let mut hashes = Vec::new();
        for index in 0..self.layout.count() {
            let hash = if let Some(hash) = cache.get(index) {
                hash
            } else {
                let (_buffer, hash) = self.read(index, Vec::new()).await?;
                cache.record(index, hash).await;
                hash
            };
            hashes.push(hash);
        }
        Ok(root_hash(&hashes))
    }
}

/// One chunk of one entry: `(entry index, chunk index)`.
pub(crate) type ChunkRef = (u32, u64);

/// What one session needs to send its missing chunks.
pub(crate) struct ChunkJob<'a> {
    /// The source of each entry of the offer, `None` for folders.
    pub sources: &'a [Option<ChunkSource>],
    pub transfer_id: Uuid,
    /// Chunks the receiver does not have, in entry order, so files finish one after another.
    pub missing: Vec<ChunkRef>,
    /// Requested number of data streams, clamped to 1..=[`MAX_STREAMS`].
    pub streams: usize,
    pub control: &'a TransferControl,
}

/// State shared by the workers (each in its own task) and the reply loop.
struct Shared {
    sources: Vec<Option<ChunkSource>>,
    transfer_id: Uuid,
    queue: async_channel::Sender<ChunkRef>,
    pending: async_channel::Receiver<ChunkRef>,
    /// One permit per chunk that may be in flight.
    window: Semaphore,
    /// Chunks sent but not answered yet, with their hashes for the cache.
    in_flight: Mutex<HashMap<ChunkRef, ChunkHash>>,
    activity: Activity,
    control: TransferControl,
}

impl Shared {
    fn source(&self, entry: u32) -> Result<&ChunkSource> {
        self.sources
            .get(entry as usize)
            .and_then(Option::as_ref)
            .ok_or_else(|| Error::protocol_violation(format!("entry {entry} has no data")))
    }

    /// Removes an answered chunk from the in-flight list and frees its window slot.
    fn answered(&self, chunk: ChunkRef) -> Result<ChunkHash> {
        let hash = self.in_flight.lock().ok().and_then(|mut in_flight| in_flight.remove(&chunk));
        let hash =
            hash.ok_or_else(|| Error::protocol_violation(format!("answer for chunk {chunk:?}, not in flight")))?;
        self.window.add_permits(1);
        Ok(hash)
    }
}

/// Sends every chunk in `job.missing` over parallel data streams and returns once each one is acknowledged.
///
/// As soon as every chunk of an entry is acknowledged, its `EntryDone` (with the root hash) is sent and its file is
/// closed. Acknowledged hashes go into `caches` (one per entry), progress is published on `progress`, and `acked`
/// counts acknowledged chunks (so the caller knows whether the session made progress).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn send_chunks<C, R>(
    connection: &C,
    job: ChunkJob<'_>,
    replies: &mut R,
    writer: &ControlWriter,
    caches: &mut [HashCache],
    progress: &watch::Sender<Progress>,
    acked: &mut u64,
) -> Result<()>
where
    C: Connection,
    R: AsyncRead + Unpin,
{
    let streams = job.streams.clamp(1, MAX_STREAMS).min(job.missing.len());
    let mut remaining: HashMap<u32, usize> = HashMap::new();
    let (queue, pending) = async_channel::unbounded();
    for chunk in &job.missing {
        *remaining.entry(chunk.0).or_default() += 1;
        // An unbounded channel that is still open always accepts.
        let _ = queue.try_send(*chunk);
    }
    let shared = Arc::new(Shared {
        sources: job.sources.to_vec(),
        transfer_id: job.transfer_id,
        queue,
        pending,
        window: Semaphore::new(streams * IN_FLIGHT_PER_STREAM),
        in_flight: Mutex::new(HashMap::new()),
        activity: Activity::new(),
        control: job.control.clone(),
    });

    // A `JoinSet` owns the worker tasks and aborts any still running when it is dropped.
    let mut workers = JoinSet::new();
    for _ in 0..streams {
        let stream = connection.open_data(job.transfer_id).await?;
        workers.spawn(run_worker(stream, Arc::clone(&shared)));
    }
    debug!(streams, chunks = job.missing.len(), "sending chunks");

    let pause = PauseSync::new(job.control);
    let replies_loop = Replies { shared: &shared, pause: &pause, writer, progress };
    let replies_done = replies_loop.handle(replies, remaining, caches, acked);
    let pause_updates = pause.run(writer);
    let watchdog = shared.activity.watchdog(job.control);
    // `pin!` lets `select!` poll the same futures again on every loop iteration.
    tokio::pin!(replies_done, pause_updates, watchdog);
    loop {
        tokio::select! {
            result = &mut replies_done => break result?,
            Some(joined) = workers.join_next() => {
                joined.map_err(|error| Error::Io(io::Error::other(error)))??;
            }
            Err(error) = &mut pause_updates => return Err(error),
            Err(error) = &mut watchdog => return Err(error),
        }
    }

    // Everything is acknowledged: wake idle workers so they close their streams, then stop the rest.
    shared.queue.close();
    shared.window.close();
    let _ = tokio::time::timeout(WORKER_SHUTDOWN_GRACE, async { while workers.join_next().await.is_some() {} }).await;
    Ok(())
}

/// One data stream: pull the next chunk, read and hash it, send its header and bytes. Stops when the queue closes.
async fn run_worker(mut stream: SendHalf, shared: Arc<Shared>) -> Result<()> {
    let mut paused = shared.control.subscribe();
    let mut buffer = Vec::new();
    loop {
        // A paused transfer starts no new chunks. `wait_for` fails only if the control handle is gone.
        if paused.wait_for(|paused| !*paused).await.is_err() {
            break;
        }
        let Ok(permit) = shared.window.acquire().await else { break };
        // The permit comes back through `Shared::answered` when the chunk's ack or nack arrives.
        permit.forget();
        let Ok((entry, index)) = shared.pending.recv().await else { break };

        let source = shared.source(entry)?;
        let (data, hash) = source.read(index, take(&mut buffer)).await?;
        if let Ok(mut in_flight) = shared.in_flight.lock() {
            in_flight.insert((entry, index), hash);
        }
        let len = source.layout().len(index);
        write_frame(&mut stream, &Message::ChunkHeader { entry, index, len, hash }).await?;
        write_all_limited(&mut stream, &data, shared.control.rate_limit(), &shared.activity).await?;
        buffer = data;
    }
    debug!(transfer_id = %shared.transfer_id, "data stream finished");
    // Ends the stream cleanly; the receiver then sees the end between two chunks.
    let _ = stream.shutdown().await;
    Ok(())
}

/// What the reply loop works with.
struct Replies<'a> {
    shared: &'a Shared,
    pause: &'a PauseSync<'a>,
    writer: &'a ControlWriter,
    progress: &'a watch::Sender<Progress>,
}

impl Replies<'_> {
    /// Reads the receiver's answers until every chunk is acknowledged, sending `EntryDone` for each entry as its last
    /// chunk is acknowledged. Also handles `Pause` and `Resume`.
    async fn handle<R: AsyncRead + Unpin>(
        &self,
        replies: &mut R,
        mut remaining: HashMap<u32, usize>,
        caches: &mut [HashCache],
        acked: &mut u64,
    ) -> Result<()> {
        while !remaining.is_empty() {
            // No timeout here: the watchdog notices silence, and allows it while paused.
            let message = read_message(replies).await?;
            self.shared.activity.touch();
            match message {
                Message::ChunkAck { entry, index } => {
                    let hash = self.shared.answered((entry, index))?;
                    let Some(cache) = caches.get_mut(entry as usize) else {
                        return Err(Error::protocol_violation(format!("ack for unknown entry {entry}")));
                    };
                    cache.record(index, hash).await;
                    *acked += 1;
                    let len = u64::from(self.shared.source(entry)?.layout().len(index));
                    self.progress.send_modify(|progress| {
                        progress.transferred += len;
                        progress.current_entry = entry;
                    });
                    if let Some(left) = remaining.get_mut(&entry) {
                        *left -= 1;
                        if *left == 0 {
                            remaining.remove(&entry);
                            self.entry_done(entry, cache).await?;
                        }
                    }
                }
                Message::ChunkNack { entry, index } => {
                    self.shared.answered((entry, index))?;
                    debug!(entry, index, "receiver reported a hash mismatch, resending chunk");
                    self.shared.queue.try_send((entry, index)).map_err(|_closed| Error::Cancelled)?;
                }
                Message::Pause => self.pause.apply_from_peer(true),
                Message::Resume => self.pause.apply_from_peer(false),
                Message::Error { code, detail } => return Err(peer_error(code, detail.as_deref())),
                other => {
                    return Err(Error::protocol_violation(format!("expected ChunkAck or ChunkNack, got {other:?}")));
                }
            }
        }
        Ok(())
    }

    /// Sends `EntryDone` for an entry whose chunks are all acknowledged, and closes its file.
    async fn entry_done(&self, entry: u32, cache: &mut HashCache) -> Result<()> {
        let source = self.shared.source(entry)?;
        let root_hash = source.root_hash(cache).await?;
        source.close();
        self.writer.send(&Message::EntryDone { entry, root_hash }).await
    }
}
