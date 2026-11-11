//! Integration tests: a real sender and receiver on localhost, over both TCP and QUIC. Faults are injected by
//! wrapping the sender's connection, which works for encrypted QUIC as well as plain TCP.

// `allow-unwrap-in-tests` in clippy.toml only covers `#[test]` functions, not the shared helpers in this file.
#![allow(clippy::unwrap_used)]

use std::{
    borrow::Cow,
    future::Future,
    io::{self, Read, Write},
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex, OnceLock,
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};

use lanshare_core::{
    identity::{DeviceCertificate, DeviceId, DeviceInfo, Os, TrustStore},
    pairing::{PairingLockout, PairingPeer},
    protocol::{read_message, write_message, EntryResume, Message, OfferEntry, MIN_PROTOCOL_VERSION, PROTOCOL_VERSION},
    transfer::{
        progress_channel, receive_file, run_bench, send_file, send_files, send_text, IncomingOffer, OfferDecider,
        Progress, ReceiveOutcome, ReceivePolicy, ReceivedFile, SendOptions, TransferControl, DEFAULT_CHUNK_SIZE,
        MAX_TEXT_BYTES,
    },
    transport::{
        self, AnyConnection, ConnectOptions, Connection, Listener, PeerCheck, PeerSession, RecvHalf, SendHalf,
        TransportKind, TransportPreference,
    },
    Error, ErrorCode,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf},
    sync::watch,
    task::JoinHandle,
};
use uuid::Uuid;

/// Small chunks keep the tests fast while still giving files many chunks.
const TEST_CHUNK_SIZE: u32 = 256 * 1024;

const MIB: usize = 1024 * 1024;

/// Generates one `#[tokio::test]` per transport for a scenario `async fn(TransportPreference)`.
macro_rules! over_both_transports {
    ($scenario:ident: $tcp:ident, $quic:ident) => {
        #[tokio::test(flavor = "multi_thread")]
        async fn $tcp() {
            $scenario(TransportPreference::Tcp).await;
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn $quic() {
            $scenario(TransportPreference::Quic).await;
        }
    };
}

/// Accepts or declines every offer.
struct FixedDecider(bool);

impl OfferDecider for FixedDecider {
    fn decide(&self, _offer: &IncomingOffer) -> impl Future<Output = bool> + Send {
        std::future::ready(self.0)
    }

    fn show_pin(&self, _peer: &PairingPeer, _pin: &str) {}
}

/// One sender and one receiver identity that trust each other, shared by every test in this file. Security checks
/// have their own tests in `security.rs`.
struct Identities {
    sender: DeviceCertificate,
    receiver: DeviceCertificate,
    sender_trust: TrustStore,
    receiver_trust: TrustStore,
}

fn identities() -> &'static Identities {
    static IDENTITIES: OnceLock<Identities> = OnceLock::new();
    IDENTITIES.get_or_init(|| {
        let (sender, receiver) = (DeviceCertificate::generate().unwrap(), DeviceCertificate::generate().unwrap());
        // Kept for the whole test run; the operating system cleans its temporary folder.
        let dir = tempfile::tempdir().unwrap().keep();
        let trust_file = |name: &str, other: &DeviceCertificate| {
            let path = dir.join(name);
            let text = format!(
                "[[devices]]\nid = \"{}\"\nname = \"peer\"\nfingerprint = \"{}\"\npaired_at = 0\n",
                DeviceId::new_random(),
                other.fingerprint().to_hex()
            );
            std::fs::write(&path, text).unwrap();
            TrustStore::open_at(&path).unwrap()
        };
        let sender_trust = trust_file("sender.toml", &receiver);
        let receiver_trust = trust_file("receiver.toml", &sender);
        Identities { sender, receiver, sender_trust, receiver_trust }
    })
}

/// How the test sender connects: with its own certificate, requiring the receiver to be trusted.
fn connect_options(preference: TransportPreference) -> ConnectOptions {
    let identities = identities();
    ConnectOptions {
        preference,
        certificate: identities.sender.clone(),
        check: PeerCheck::Trusted(identities.sender_trust.clone()),
        expected: Some(identities.receiver.fingerprint()),
    }
}

fn receiver_policy() -> ReceivePolicy {
    let identities = identities();
    let local = DeviceInfo {
        id: DeviceId::new_random(),
        name: "test-receiver".into(),
        os: Os::current(),
        port: 0,
        protocol_version: PROTOCOL_VERSION,
        fingerprint: identities.receiver.fingerprint(),
    };
    ReceivePolicy {
        local,
        trust: identities.receiver_trust.clone(),
        allow_untrusted: false,
        accept_clipboard: true,
        lockout: Arc::new(PairingLockout::new()),
    }
}

async fn local_listener() -> (Arc<Listener>, SocketAddr) {
    let listener = Listener::bind(0, &identities().receiver).await.unwrap();
    assert!(listener.quic_enabled());
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, listener.port()));
    (Arc::new(listener), addr)
}

/// Runs a receiver for one connection in the background.
fn spawn_receiver(listener: Arc<Listener>, dir: PathBuf, accept: bool) -> JoinHandle<Result<ReceiveOutcome, Error>> {
    tokio::spawn(async move {
        let connection = listener.accept().await.unwrap();
        let (progress, _watcher) = progress_channel();
        let policy = receiver_policy();
        receive_file(&connection, &dir, &FixedDecider(accept), &policy, &TransferControl::default(), &progress).await
    })
}

/// What a receiver that keeps accepting connections saw before a file was saved.
struct ReceiverLog {
    /// Errors of the connections that failed.
    errors: Vec<ErrorCode>,
    /// `Progress::resumed` at the end of each connection.
    resumed: Vec<u64>,
    saved: ReceivedFile,
}

/// Accepts connections one at a time, like `lanshare receive`, until a file is saved.
fn spawn_receiver_until_saved(
    listener: Arc<Listener>,
    dir: PathBuf,
    control: TransferControl,
) -> JoinHandle<ReceiverLog> {
    tokio::spawn(async move {
        let policy = receiver_policy();
        let mut errors = Vec::new();
        let mut resumed = Vec::new();
        loop {
            let connection = listener.accept().await.unwrap();
            let (progress, watcher) = progress_channel();
            let result = receive_file(&connection, &dir, &FixedDecider(true), &policy, &control, &progress).await;
            resumed.push(watcher.borrow().resumed);
            match result {
                Ok(ReceiveOutcome::Saved(saved)) => return ReceiverLog { errors, resumed, saved },
                Ok(other) => panic!("unexpected outcome {other:?}"),
                Err(error) => errors.push(error.code()),
            }
        }
    })
}

fn sender_info() -> DeviceInfo {
    DeviceInfo {
        id: DeviceId::new_random(),
        name: "test-sender".into(),
        os: Os::current(),
        port: 0,
        protocol_version: PROTOCOL_VERSION,
        fingerprint: identities().sender.fingerprint(),
    }
}

fn hello(device_name: &str, protocol_version: u16) -> Message {
    Message::Hello { protocol_version, device_id: DeviceId::new_random(), device_name: device_name.into() }
}

fn offer(transfer_id: Uuid, name: &str, size: u64, chunk_size: u32) -> Message {
    Message::Offer { transfer_id, chunk_size, entries: vec![OfferEntry { relative_path: name.into(), size }] }
}

fn options(cache: &TempDir, chunk_size: u32) -> SendOptions {
    SendOptions { chunk_size, ..SendOptions::new(cache.path().to_owned()) }
}

/// Sends `file` with a fresh control handle and no faults.
async fn send(
    addr: SocketAddr,
    preference: TransportPreference,
    file: &Path,
    options: &SendOptions,
) -> Result<(), Error> {
    let (progress, _watcher) = progress_channel();
    let control = TransferControl::default();
    let options_for_connect = connect_options(preference);
    let connect = || transport::connect(addr, &options_for_connect);
    send_file(connect, file, &sender_info(), options, &control, &progress).await
}

/// Writes `size` bytes of deterministic pseudo-random data, 1 MiB at a time.
fn write_test_file(path: &Path, size: usize) {
    let mut file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut chunk = vec![0u8; MIB];
    let mut written = 0;
    while written < size {
        for byte in &mut chunk {
            // xorshift64: fast and good enough to catch reordered or dropped bytes.
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *byte = state.to_le_bytes()[0];
        }
        let len = chunk.len().min(size - written);
        file.write_all(&chunk[..len]).unwrap();
        written += len;
    }
    file.flush().unwrap();
}

/// Compares two files chunk by chunk without loading either into memory.
fn assert_same_contents(left: &Path, right: &Path) {
    let mut left_file = std::fs::File::open(left).unwrap();
    let mut right_file = std::fs::File::open(right).unwrap();
    assert_eq!(left_file.metadata().unwrap().len(), right_file.metadata().unwrap().len(), "sizes differ");
    let mut left_buffer = vec![0u8; MIB];
    let mut right_buffer = vec![0u8; MIB];
    loop {
        let read = left_file.read(&mut left_buffer).unwrap();
        if read == 0 {
            break;
        }
        right_file.read_exact(&mut right_buffer[..read]).unwrap();
        assert!(left_buffer[..read] == right_buffer[..read], "contents differ");
    }
}

/// Names of every file in `dir`, sorted.
fn file_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn bit_is_set(bits: &[u8], index: u64) -> bool {
    bits[usize::try_from(index / 8).unwrap()] & (1 << (index % 8)) != 0
}

fn dirs() -> (TempDir, TempDir, TempDir) {
    (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap())
}

/// Waits until `condition` holds for the watched value, failing the test after 20 seconds.
async fn wait_until<T>(watcher: &mut watch::Receiver<T>, condition: impl FnMut(&T) -> bool) {
    tokio::time::timeout(Duration::from_secs(20), watcher.wait_for(condition)).await.unwrap().unwrap();
}

// ---------------------------------------------------------------------------------------------------------------
// The fault-injecting connection: the test hook that breaks or corrupts a transfer.

/// What to do to the first connection. Later connections pass through untouched.
#[derive(Debug, Clone, Copy, Default)]
struct Fault {
    /// Fail every data stream once this many chunk bytes were written in total, as if the network went away.
    cut_after: Option<u64>,
    /// How long streams stall at the cut before failing, so the receiver can store what already arrived.
    cut_grace: Duration,
    /// Flip every bit of the byte at this offset of the first data stream.
    corrupt_at: Option<u64>,
}

/// Wraps a real connection: injects `fault` into the sender's data streams and records every byte the receiver
/// sends back on the control channel.
struct FaultyConnection {
    inner: AnyConnection,
    fault: Fault,
    streams_opened: AtomicUsize,
    data_written: Arc<AtomicU64>,
    corrupted: Arc<AtomicBool>,
    replies: Arc<Mutex<Vec<u8>>>,
}

impl Connection for FaultyConnection {
    fn session(&self) -> &PeerSession {
        self.inner.session()
    }

    async fn open_control(&self) -> lanshare_core::Result<(SendHalf, RecvHalf)> {
        let (send, recv) = self.inner.open_control().await?;
        Ok((send, Box::new(RecordingReader { inner: recv, log: Arc::clone(&self.replies) })))
    }

    async fn open_data(&self, transfer_id: Uuid) -> lanshare_core::Result<SendHalf> {
        let stream = self.inner.open_data(transfer_id).await?;
        Ok(Box::new(FaultyWriter {
            inner: stream,
            fault: self.fault,
            is_first: self.streams_opened.fetch_add(1, Ordering::SeqCst) == 0,
            written: 0,
            total: Arc::clone(&self.data_written),
            corrupted: Arc::clone(&self.corrupted),
            cut_timer: None,
        }))
    }

    async fn accept_data(&self, transfer_id: Uuid) -> lanshare_core::Result<RecvHalf> {
        self.inner.accept_data(transfer_id).await
    }
}

struct FaultyWriter {
    inner: SendHalf,
    fault: Fault,
    is_first: bool,
    written: u64,
    total: Arc<AtomicU64>,
    corrupted: Arc<AtomicBool>,
    cut_timer: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl AsyncWrite for FaultyWriter {
    fn poll_write(mut self: Pin<&mut Self>, context: &mut Context<'_>, buffer: &[u8]) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        if this.fault.cut_after.is_some_and(|cut| this.total.load(Ordering::SeqCst) >= cut) {
            let grace = this.fault.cut_grace;
            std::task::ready!(this
                .cut_timer
                .get_or_insert_with(|| Box::pin(tokio::time::sleep(grace)))
                .as_mut()
                .poll(context));
            return Poll::Ready(Err(io::Error::from(io::ErrorKind::BrokenPipe)));
        }
        let start = this.written;
        let flip = this
            .fault
            .corrupt_at
            .filter(|_| this.is_first && !this.corrupted.load(Ordering::SeqCst))
            .filter(|at| (start..start + buffer.len() as u64).contains(at))
            .map(|at| usize::try_from(at - start).unwrap());
        let mut data = Cow::Borrowed(buffer);
        if let Some(position) = flip {
            data.to_mut()[position] ^= 0xff;
        }
        let result = Pin::new(&mut this.inner).poll_write(context, &data);
        if let Poll::Ready(Ok(written)) = result {
            if flip.is_some_and(|position| position < written) {
                this.corrupted.store(true, Ordering::SeqCst);
            }
            this.written += written as u64;
            this.total.fetch_add(written as u64, Ordering::SeqCst);
        }
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(context)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(context)
    }
}

struct RecordingReader {
    inner: RecvHalf,
    log: Arc<Mutex<Vec<u8>>>,
}

impl AsyncRead for RecordingReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buffer.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(context, buffer);
        if let Poll::Ready(Ok(())) = result {
            self.log.lock().unwrap().extend_from_slice(&buffer.filled()[before..]);
        }
        result
    }
}

/// The receiver's control bytes, one buffer per connection attempt.
type ReplyLogs = Arc<Mutex<Vec<Arc<Mutex<Vec<u8>>>>>>;

/// Sends `file` through a [`FaultyConnection`]: the first attempt gets `fault`, reconnects are clean.
async fn send_with_fault(
    addr: SocketAddr,
    preference: TransportPreference,
    file: &Path,
    options: &SendOptions,
    fault: Fault,
) -> (Result<(), Error>, ReplyLogs) {
    send_paths_with_fault(addr, preference, &[file.to_owned()], options, fault).await
}

/// [`send_with_fault`] for several files and folders in one transfer.
async fn send_paths_with_fault(
    addr: SocketAddr,
    preference: TransportPreference,
    paths: &[PathBuf],
    options: &SendOptions,
    fault: Fault,
) -> (Result<(), Error>, ReplyLogs) {
    let logs = ReplyLogs::default();
    let attempts = AtomicUsize::new(0);
    let connect_options = connect_options(preference);
    let connect = || async {
        let inner = transport::connect(addr, &connect_options).await?;
        let replies = Arc::new(Mutex::new(Vec::new()));
        logs.lock().unwrap().push(Arc::clone(&replies));
        let first = attempts.fetch_add(1, Ordering::SeqCst) == 0;
        Ok(FaultyConnection {
            inner,
            fault: if first { fault } else { Fault::default() },
            streams_opened: AtomicUsize::new(0),
            data_written: Arc::default(),
            corrupted: Arc::default(),
            replies,
        })
    };
    let (progress, _watcher) = progress_channel();
    let result = send_files(connect, paths, &sender_info(), options, &TransferControl::default(), &progress).await;
    (result, logs)
}

/// Decodes the recorded control bytes of one connection attempt.
async fn replies(logs: &ReplyLogs, attempt: usize) -> Vec<Message> {
    let bytes = logs.lock().unwrap()[attempt].lock().unwrap().clone();
    let mut reader = bytes.as_slice();
    let mut messages = Vec::new();
    while let Ok(message) = read_message(&mut reader).await {
        messages.push(message);
    }
    messages
}

// ---------------------------------------------------------------------------------------------------------------
// Scenarios that run over both transports.

async fn sends_file(preference: TransportPreference) {
    let (source_dir, download_dir, cache_dir) = dirs();
    let source = source_dir.path().join("big.bin");
    write_test_file(&source, 40 * MIB);

    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);
    send(addr, preference, &source, &options(&cache_dir, DEFAULT_CHUNK_SIZE)).await.unwrap();

    let ReceiveOutcome::Saved(saved) = receiver.await.unwrap().unwrap() else {
        panic!("expected the file to be saved");
    };
    assert_eq!(saved.path, download_dir.path().join("big.bin"));
    assert_eq!(saved.offer.device_name, "test-sender");
    assert_same_contents(&source, &saved.path);
    assert_eq!(file_names(download_dir.path()), ["big.bin"], "part and state files are gone");
    assert_eq!(file_names(cache_dir.path()), Vec::<String>::new(), "hash cache entry is deleted after success");
}
over_both_transports!(sends_file: sends_file_over_tcp, sends_file_over_quic);

async fn sends_empty_file(preference: TransportPreference) {
    let (source_dir, download_dir, cache_dir) = dirs();
    let source = source_dir.path().join("empty.txt");
    std::fs::write(&source, b"").unwrap();

    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);
    send(addr, preference, &source, &options(&cache_dir, TEST_CHUNK_SIZE)).await.unwrap();

    let ReceiveOutcome::Saved(saved) = receiver.await.unwrap().unwrap() else {
        panic!("expected the file to be saved");
    };
    assert_eq!(std::fs::read(&saved.path).unwrap(), b"");
}
over_both_transports!(sends_empty_file: sends_empty_file_over_tcp, sends_empty_file_over_quic);

async fn dropped_connection_resumes_with_only_missing_chunks(preference: TransportPreference) {
    let (source_dir, download_dir, cache_dir) = dirs();
    let source = source_dir.path().join("resume.bin");
    let size = 8 * MIB;
    write_test_file(&source, size);
    let chunk_count = (size / TEST_CHUNK_SIZE as usize) as u64;

    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver_until_saved(listener, download_dir.path().to_owned(), TransferControl::default());
    let fault = Fault { cut_after: Some(size as u64 / 2), ..Fault::default() };
    let (result, logs) = send_with_fault(addr, preference, &source, &options(&cache_dir, TEST_CHUNK_SIZE), fault).await;
    result.unwrap();

    let receiver = receiver.await.unwrap();
    assert_eq!(receiver.errors, [ErrorCode::ConnectionLost]);
    assert_same_contents(&source, &receiver.saved.path);
    assert_eq!(file_names(download_dir.path()), ["resume.bin"]);

    assert_eq!(logs.lock().unwrap().len(), 2, "one reconnect");
    let second = replies(&logs, 1).await;
    let Message::Accept { resume } = &second[0] else {
        panic!("expected Accept, got {:?}", second[0]);
    };
    let [EntryResume { entry: 0, have }] = resume.as_slice() else {
        panic!("expected one resume entry, got {resume:?}");
    };
    let resent: Vec<u64> = second
        .iter()
        .filter_map(|message| match message {
            Message::ChunkAck { index, .. } => Some(*index),
            _ => None,
        })
        .collect();
    let already_had = (0..chunk_count).filter(|index| bit_is_set(have, *index)).count();
    assert!(already_had > 0 && !resent.is_empty(), "had {already_had}, resent {}", resent.len());
    for index in 0..chunk_count {
        assert_ne!(bit_is_set(have, index), resent.contains(&index), "chunk {index} must be sent exactly once more");
    }
    assert_eq!(receiver.resumed[1], already_had as u64 * u64::from(TEST_CHUNK_SIZE));
}
over_both_transports!(
    dropped_connection_resumes_with_only_missing_chunks: drop_and_resume_over_tcp, drop_and_resume_over_quic
);

async fn corrupted_chunk_is_nacked_and_resent(preference: TransportPreference) {
    let (source_dir, download_dir, cache_dir) = dirs();
    let source = source_dir.path().join("corrupt.bin");
    write_test_file(&source, 2 * MIB);

    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver_until_saved(listener, download_dir.path().to_owned(), TransferControl::default());
    // Past the first chunk header (under 100 bytes), inside the first chunk's data.
    let fault = Fault { corrupt_at: Some(100_000), ..Fault::default() };
    let (result, logs) = send_with_fault(addr, preference, &source, &options(&cache_dir, TEST_CHUNK_SIZE), fault).await;
    result.unwrap();

    let receiver = receiver.await.unwrap();
    assert_eq!(receiver.errors, []);
    assert_same_contents(&source, &receiver.saved.path);

    assert_eq!(logs.lock().unwrap().len(), 1, "no reconnect needed");
    let messages = replies(&logs, 0).await;
    let nack = messages.iter().position(|message| matches!(message, Message::ChunkNack { entry: 0, .. }));
    let Some(Message::ChunkNack { index, .. }) = nack.map(|position| &messages[position]) else {
        panic!("expected a ChunkNack, got {messages:?}");
    };
    let ack = messages.iter().position(|message| *message == Message::ChunkAck { entry: 0, index: *index });
    assert!(ack > nack, "expected ChunkAck for chunk {index} after its ChunkNack");
}
over_both_transports!(corrupted_chunk_is_nacked_and_resent: corruption_over_tcp, corruption_over_quic);

async fn restarted_receiver_resumes(preference: TransportPreference) {
    let (source_dir, download_dir, cache_dir) = dirs();
    let source = source_dir.path().join("restart.bin");
    write_test_file(&source, 16 * MIB);
    let (listener, addr) = local_listener().await;

    // The first receiver is stopped halfway, as if the program was closed.
    let (progress, mut watcher) = progress_channel();
    let first_listener = Arc::clone(&listener);
    let first_dir = download_dir.path().to_owned();
    let first = tokio::spawn(async move {
        let connection = first_listener.accept().await.unwrap();
        let policy = receiver_policy();
        receive_file(&connection, &first_dir, &FixedDecider(true), &policy, &TransferControl::default(), &progress)
            .await
    });
    let source_path = source.clone();
    // A bandwidth limit keeps the transfer slow enough to stop it in the middle.
    let sender = tokio::spawn(async move {
        let (progress, _watcher) = progress_channel();
        let control = TransferControl::new(20);
        let options = options(&cache_dir, TEST_CHUNK_SIZE);
        let connect_options = connect_options(preference);
        let connect = || transport::connect(addr, &connect_options);
        send_file(connect, &source_path, &sender_info(), &options, &control, &progress).await
    });

    wait_until(&mut watcher, |progress: &Progress| progress.total > 0 && progress.transferred >= progress.total / 2)
        .await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    assert!(download_dir.path().join("restart.bin.lanshare-state").exists(), "state saved when stopped");

    let second = spawn_receiver_until_saved(listener, download_dir.path().to_owned(), TransferControl::default());
    sender.await.unwrap().unwrap();
    let second = second.await.unwrap();
    assert!(second.resumed.last().copied().unwrap() > 0, "second receiver resumed");
    assert_same_contents(&source, &second.saved.path);
}
over_both_transports!(restarted_receiver_resumes: restart_over_tcp, restart_over_quic);

async fn pause_and_resume_from_either_side(preference: TransportPreference) {
    let (source_dir, download_dir, cache_dir) = dirs();
    let source = source_dir.path().join("pause.bin");
    write_test_file(&source, 24 * MIB);
    let (listener, addr) = local_listener().await;
    let receiver_control = TransferControl::default();
    let receiver = spawn_receiver_until_saved(listener, download_dir.path().to_owned(), receiver_control.clone());

    // Limited to 10 MB/s so the transfer lasts long enough to pause twice.
    let sender_control = TransferControl::new(10);
    let (progress, mut watcher) = progress_channel();
    let sender = {
        let (control, source) = (sender_control.clone(), source.clone());
        let options = options(&cache_dir, TEST_CHUNK_SIZE);
        tokio::spawn(async move {
            let connect_options = connect_options(preference);
            let connect = || transport::connect(addr, &connect_options);
            send_file(connect, &source, &sender_info(), &options, &control, &progress).await
        })
    };

    // The sender pauses; the receiver hears about it; nothing moves; the receiver resumes.
    wait_until(&mut watcher, |progress: &Progress| progress.total > 0 && progress.transferred >= progress.total / 4)
        .await;
    sender_control.pause();
    wait_until(&mut receiver_control.subscribe(), |paused| *paused).await;
    assert_stalled(&watcher).await;
    receiver_control.resume();
    wait_until(&mut sender_control.subscribe(), |paused| !*paused).await;

    // The receiver pauses; the sender stops; the sender resumes.
    wait_until(&mut watcher, |progress: &Progress| progress.transferred >= progress.total / 2).await;
    receiver_control.pause();
    wait_until(&mut sender_control.subscribe(), |paused| *paused).await;
    assert_stalled(&watcher).await;
    sender_control.resume();

    sender.await.unwrap().unwrap();
    let receiver = receiver.await.unwrap();
    assert_eq!(receiver.errors, []);
    assert!(!receiver_control.is_paused());
    assert_same_contents(&source, &receiver.saved.path);
}
over_both_transports!(pause_and_resume_from_either_side: pause_over_tcp, pause_over_quic);

/// Lets chunks already in flight finish, then checks that no more are acknowledged for a while.
async fn assert_stalled(watcher: &watch::Receiver<Progress>) {
    tokio::time::sleep(Duration::from_millis(600)).await;
    let before = watcher.borrow().transferred;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(watcher.borrow().transferred, before, "no chunks may be sent while paused");
}

async fn bandwidth_limit_holds(preference: TransportPreference) {
    let (source_dir, download_dir, cache_dir) = dirs();
    let source = source_dir.path().join("limited.bin");
    write_test_file(&source, 30_000_000);
    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);

    let control = TransferControl::new(10);
    let (progress, _watcher) = progress_channel();
    let start = Instant::now();
    let options = options(&cache_dir, DEFAULT_CHUNK_SIZE);
    let connect_options = connect_options(preference);
    let connect = || transport::connect(addr, &connect_options);
    send_file(connect, &source, &sender_info(), &options, &control, &progress).await.unwrap();
    let elapsed = start.elapsed().as_secs_f64();

    assert!(matches!(receiver.await.unwrap().unwrap(), ReceiveOutcome::Saved(_)));
    // 30 MB at 10 MB/s is 3 seconds; allow ±10%.
    assert!((2.7..=3.3).contains(&elapsed), "30 MB at 10 MB/s took {elapsed:.2} s");
}
over_both_transports!(bandwidth_limit_holds: limit_over_tcp, limit_over_quic);

async fn benchmark_round_trip(preference: TransportPreference) {
    let (_source_dir, download_dir, _cache_dir) = dirs();
    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), false);

    let connection = transport::connect(addr, &connect_options(preference)).await.unwrap();
    let result = run_bench(&connection, &sender_info(), 50_000_000, 4).await.unwrap();
    assert_eq!(result.bytes, 50_000_000);
    assert!(result.megabytes_per_second() > 0.0);

    let ReceiveOutcome::Benchmark { device_name, bytes } = receiver.await.unwrap().unwrap() else {
        panic!("expected a benchmark");
    };
    assert_eq!((device_name.as_str(), bytes), ("test-sender", 50_000_000));
    assert_eq!(file_names(download_dir.path()), Vec::<String>::new(), "benchmarks never touch the disk");
}
over_both_transports!(benchmark_round_trip: bench_over_tcp, bench_over_quic);

async fn existing_file_gets_numbered_name(preference: TransportPreference) {
    let (source_dir, download_dir, cache_dir) = dirs();
    let source = source_dir.path().join("note.txt");
    std::fs::write(&source, b"new contents").unwrap();
    std::fs::write(download_dir.path().join("note.txt"), b"old contents").unwrap();

    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);
    send(addr, preference, &source, &options(&cache_dir, TEST_CHUNK_SIZE)).await.unwrap();

    let ReceiveOutcome::Saved(saved) = receiver.await.unwrap().unwrap() else {
        panic!("expected the file to be saved");
    };
    assert_eq!(saved.path, download_dir.path().join("note (1).txt"));
    assert_eq!(std::fs::read(&saved.path).unwrap(), b"new contents");
    assert_eq!(std::fs::read(download_dir.path().join("note.txt")).unwrap(), b"old contents");
}
over_both_transports!(existing_file_gets_numbered_name: numbered_name_over_tcp, numbered_name_over_quic);

async fn rejected_offer_ends_sender_with_declined(preference: TransportPreference) {
    let (source_dir, download_dir, cache_dir) = dirs();
    let source = source_dir.path().join("unwanted.txt");
    std::fs::write(&source, b"no thanks").unwrap();

    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), false);
    let error = send(addr, preference, &source, &options(&cache_dir, TEST_CHUNK_SIZE)).await.unwrap_err();

    assert_eq!(error.code(), ErrorCode::Declined);
    assert!(matches!(receiver.await.unwrap().unwrap(), ReceiveOutcome::Declined(_)));
    assert_eq!(file_names(download_dir.path()), Vec::<String>::new());
}
over_both_transports!(rejected_offer_ends_sender_with_declined: declined_over_tcp, declined_over_quic);

// ---------------------------------------------------------------------------------------------------------------
// Folders and several files in one transfer.

/// Writes `files` (relative path and contents) under `root`, and creates `empty_folders`.
fn write_tree(root: &Path, files: &[(&str, Vec<u8>)], empty_folders: &[&str]) {
    for (path, contents) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    for folder in empty_folders {
        std::fs::create_dir_all(root.join(folder)).unwrap();
    }
}

/// Every file under `dir` with its contents, and every empty folder with a trailing `/`, sorted by path.
fn read_tree(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, Vec<u8>)>) {
        let mut children: Vec<_> = std::fs::read_dir(dir).unwrap().map(|entry| entry.unwrap()).collect();
        if children.is_empty() && !prefix.is_empty() {
            out.push((prefix.to_owned(), Vec::new()));
        }
        children.sort_by_key(std::fs::DirEntry::file_name);
        for child in children {
            let name = format!("{prefix}{}", child.file_name().to_string_lossy());
            if child.file_type().unwrap().is_dir() {
                walk(&child.path(), &format!("{name}/"), out);
            } else {
                out.push((name, std::fs::read(child.path()).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, "", &mut out);
    out
}

fn pattern(size: usize, seed: u8) -> Vec<u8> {
    (0..size).map(|i| u8::try_from(i % 251).unwrap() ^ seed).collect()
}

async fn sends_folder_with_its_structure(preference: TransportPreference) {
    let (source_dir, download_dir, cache_dir) = dirs();
    let files = [
        ("Photos/2024/a.jpg", pattern(3_000, 1)),
        ("Photos/2024/b.jpg", pattern(1_500_000, 2)),
        ("Photos/notes.txt", Vec::new()),
        ("report.pdf", pattern(700_000, 3)),
    ];
    write_tree(source_dir.path(), &files, &["Photos/empty"]);

    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);
    let paths = [source_dir.path().join("Photos"), source_dir.path().join("report.pdf")];
    let (result, _logs) =
        send_paths_with_fault(addr, preference, &paths, &options(&cache_dir, TEST_CHUNK_SIZE), Fault::default()).await;
    result.unwrap();

    let ReceiveOutcome::Saved(saved) = receiver.await.unwrap().unwrap() else {
        panic!("expected the files to be saved");
    };
    assert_eq!(saved.path, download_dir.path(), "several items: the download folder");
    assert_eq!((saved.offer.file_name.as_str(), saved.offer.file_count), ("Photos", 4));
    assert_eq!(read_tree(download_dir.path()), read_tree(source_dir.path()), "same files, folders and contents");
}
over_both_transports!(sends_folder_with_its_structure: folder_over_tcp, folder_over_quic);

async fn folder_transfer_resumes_without_saving_twice(preference: TransportPreference) {
    let (source_dir, download_dir, cache_dir) = dirs();
    let files: Vec<(String, Vec<u8>)> =
        (0..6).map(|i| (format!("album/track{i}.flac"), pattern(MIB, u8::try_from(i).unwrap()))).collect();
    let borrowed: Vec<(&str, Vec<u8>)> = files.iter().map(|(path, data)| (path.as_str(), data.clone())).collect();
    write_tree(source_dir.path(), &borrowed, &[]);

    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver_until_saved(listener, download_dir.path().to_owned(), TransferControl::default());
    // Cut halfway: some tracks are saved, one is partly written, the rest have not started.
    let fault = Fault { cut_after: Some(3 * MIB as u64), cut_grace: Duration::from_millis(500), ..Fault::default() };
    let paths = [source_dir.path().join("album")];
    let (result, logs) =
        send_paths_with_fault(addr, preference, &paths, &options(&cache_dir, TEST_CHUNK_SIZE), fault).await;
    result.unwrap();

    let log = receiver.await.unwrap();
    assert!(log.resumed.last().copied().unwrap() > 0, "the second connection resumed");
    let Message::Accept { resume } = &replies(&logs, 1).await[0] else { panic!("expected Accept") };
    assert!(resume.len() >= 2, "saved and partial tracks are reported, got {resume:?}");
    assert_eq!(read_tree(download_dir.path()), read_tree(source_dir.path()), "every track exactly once, intact");
}
over_both_transports!(folder_transfer_resumes_without_saving_twice: folder_resume_over_tcp, folder_resume_over_quic);

#[tokio::test(flavor = "multi_thread")]
async fn sending_a_folder_again_keeps_both_copies() {
    let (source_dir, download_dir, cache_dir) = dirs();
    write_tree(source_dir.path(), &[("docs/a.txt", b"first".to_vec())], &[]);
    let (listener, addr) = local_listener().await;
    let paths = [source_dir.path().join("docs")];
    for _ in 0..2 {
        let receiver = spawn_receiver(Arc::clone(&listener), download_dir.path().to_owned(), true);
        let options = options(&cache_dir, TEST_CHUNK_SIZE);
        let (result, _logs) =
            send_paths_with_fault(addr, TransportPreference::Quic, &paths, &options, Fault::default()).await;
        result.unwrap();
        receiver.await.unwrap().unwrap();
    }
    // The folder is reused; the file that is already there is never overwritten.
    assert_eq!(file_names(&download_dir.path().join("docs")), ["a (1).txt", "a.txt"]);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn read_only_download_folder_is_refused_before_asking() {
    use std::os::unix::fs::PermissionsExt;

    let (source_dir, download_dir, cache_dir) = dirs();
    let source = source_dir.path().join("a.txt");
    std::fs::write(&source, b"data").unwrap();
    std::fs::set_permissions(download_dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();

    let (listener, addr) = local_listener().await;
    // The decider would decline, so this error can only come from the check that runs before it.
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), false);
    let paths = [source];
    let (result, _logs) = send_paths_with_fault(
        addr,
        TransportPreference::Auto,
        &paths,
        &options(&cache_dir, TEST_CHUNK_SIZE),
        Fault::default(),
    )
    .await;
    let folder = download_dir.path().display().to_string();
    assert_eq!(result.unwrap_err().code(), ErrorCode::PermissionDenied { folder: folder.clone() });
    assert_eq!(receiver.await.unwrap().unwrap_err().code(), ErrorCode::PermissionDenied { folder });
    std::fs::set_permissions(download_dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn ten_thousand_small_files_transfer_in_reasonable_time() {
    let (source_dir, download_dir, cache_dir) = dirs();
    let root = source_dir.path().join("many");
    for folder in 0..100 {
        let dir = root.join(format!("folder{folder:03}"));
        std::fs::create_dir_all(&dir).unwrap();
        for file in 0..100 {
            let size = (folder * 100 + file) % 200 + 1;
            std::fs::write(dir.join(format!("file{file:03}.txt")), pattern(size, u8::try_from(file).unwrap())).unwrap();
        }
    }

    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);
    let start = Instant::now();
    let (result, _logs) = send_paths_with_fault(
        addr,
        TransportPreference::Auto,
        &[root],
        &options(&cache_dir, DEFAULT_CHUNK_SIZE),
        Fault::default(),
    )
    .await;
    result.unwrap();
    let ReceiveOutcome::Saved(saved) = receiver.await.unwrap().unwrap() else { panic!("expected the files") };
    let elapsed = start.elapsed();

    assert_eq!(saved.offer.file_count, 10_000);
    assert_eq!(saved.path, download_dir.path().join("many"));
    assert_eq!(read_tree(download_dir.path()), read_tree(source_dir.path()));
    println!("10,000 small files in {elapsed:.1?}");
    assert!(elapsed < Duration::from_secs(120), "took {elapsed:?}");
}

// ---------------------------------------------------------------------------------------------------------------
// Transport selection.

#[tokio::test]
async fn auto_prefers_quic() {
    let (_listener, addr) = local_listener().await;
    let connection = transport::connect(addr, &connect_options(TransportPreference::Auto)).await.unwrap();
    assert_eq!(connection.kind(), TransportKind::Quic);
}

#[tokio::test(flavor = "multi_thread")]
async fn falls_back_to_tcp_when_udp_is_blocked() {
    // A UDP socket that never answers holds the port, so QUIC packets vanish like behind a firewall.
    let black_hole = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
    let port = black_hole.local_addr().unwrap().port();
    let listener = Arc::new(Listener::bind(port, &identities().receiver).await.unwrap());
    assert!(!listener.quic_enabled(), "the UDP port is taken, so the listener runs TCP only");

    let (source_dir, download_dir, cache_dir) = dirs();
    let source = source_dir.path().join("fallback.txt");
    std::fs::write(&source, b"over TCP").unwrap();
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);

    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let start = Instant::now();
    let connection = transport::connect(addr, &connect_options(TransportPreference::Auto)).await.unwrap();
    assert_eq!(connection.kind(), TransportKind::Tcp);
    assert!(start.elapsed() >= transport::QUIC_CONNECT_TIMEOUT, "QUIC was tried first");

    let (progress, _watcher) = progress_channel();
    let options = options(&cache_dir, TEST_CHUNK_SIZE);
    let mut connection = Some(connection);
    let connect = || std::future::ready(connection.take().ok_or(Error::ConnectionLost));
    send_file(connect, &source, &sender_info(), &options, &TransferControl::default(), &progress).await.unwrap();
    let ReceiveOutcome::Saved(saved) = receiver.await.unwrap().unwrap() else {
        panic!("expected the file to be saved");
    };
    assert_eq!(std::fs::read(saved.path).unwrap(), b"over TCP");
}

#[tokio::test]
async fn nothing_listening_is_transport_unavailable() {
    // Bind and drop a listener to find a port with nothing on it.
    let port = Listener::bind(0, &identities().receiver).await.unwrap().port();
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let error = transport::connect(addr, &connect_options(TransportPreference::Auto)).await.unwrap_err();
    assert_eq!(error.code(), ErrorCode::TransportUnavailable);
}

// ---------------------------------------------------------------------------------------------------------------
// Protocol checks with hand-written messages over the control channel of a TCP connection.

/// The control channel of a TCP connection as one stream that hand-written frames go through.
struct RawControl {
    connection: AnyConnection,
    send: SendHalf,
    recv: RecvHalf,
}

impl AsyncRead for RawControl {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.recv).poll_read(context, buffer)
    }
}

impl AsyncWrite for RawControl {
    fn poll_write(mut self: Pin<&mut Self>, context: &mut Context<'_>, buffer: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.send).poll_write(context, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.send).poll_flush(context)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.send).poll_shutdown(context)
    }
}

/// Opens a TCP connection to the listener and takes its control channel.
async fn raw_control(addr: SocketAddr) -> RawControl {
    let connection = transport::connect(addr, &connect_options(TransportPreference::Tcp)).await.unwrap();
    let (send, recv) = connection.open_control().await.unwrap();
    RawControl { connection, send, recv }
}

#[tokio::test]
async fn offer_larger_than_free_space_is_rejected() {
    let (_source_dir, download_dir, _cache_dir) = dirs();
    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);

    // 16 TiB: more than any test machine has free, and still within the chunk count limit.
    let size = 1u64 << 44;
    let mut stream = raw_control(addr).await;
    write_message(&mut stream, &hello("huge", PROTOCOL_VERSION)).await.unwrap();
    write_message(&mut stream, &offer(Uuid::new_v4(), "huge.bin", size, DEFAULT_CHUNK_SIZE)).await.unwrap();

    let Message::Reject { code: ErrorCode::InsufficientSpace { needed, available } } =
        read_message(&mut stream).await.unwrap()
    else {
        panic!("expected Reject with INSUFFICIENT_SPACE");
    };
    assert_eq!(needed, size);
    assert!(available < needed);
    let error = receiver.await.unwrap().unwrap_err();
    assert_eq!(error.code(), ErrorCode::InsufficientSpace { needed, available });
    assert_eq!(file_names(download_dir.path()), Vec::<String>::new());
}

#[tokio::test]
async fn version_mismatch_is_rejected() {
    let (_source_dir, download_dir, _cache_dir) = dirs();
    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);

    let future_version = PROTOCOL_VERSION + 1;
    let mut stream = raw_control(addr).await;
    write_message(&mut stream, &hello("future", future_version)).await.unwrap();

    let expected = ErrorCode::VersionMismatch { ours: PROTOCOL_VERSION, theirs: future_version };
    assert_eq!(read_message(&mut stream).await.unwrap(), Message::Reject { code: expected.clone() });
    assert_eq!(receiver.await.unwrap().unwrap_err().code(), expected);
}

#[tokio::test]
async fn hello_with_unknown_layout_is_rejected_as_version_mismatch() {
    let (_source_dir, download_dir, _cache_dir) = dirs();
    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);

    // A `Hello` from a future version whose fields after `protocol_version` this version cannot decode.
    let mut stream = raw_control(addr).await;
    stream.write_all(&[0, 0, 0, 4, 0, 99, 0xff, 0xff]).await.unwrap();

    let expected = ErrorCode::VersionMismatch { ours: PROTOCOL_VERSION, theirs: 99 };
    assert_eq!(read_message(&mut stream).await.unwrap(), Message::Reject { code: expected.clone() });
    assert_eq!(receiver.await.unwrap().unwrap_err().code(), expected);
}

#[tokio::test]
async fn unsafe_file_name_is_rejected() {
    let (_source_dir, download_dir, _cache_dir) = dirs();
    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);

    let mut stream = raw_control(addr).await;
    write_message(&mut stream, &hello("attacker", PROTOCOL_VERSION)).await.unwrap();
    write_message(&mut stream, &offer(Uuid::new_v4(), "../..", 1, TEST_CHUNK_SIZE)).await.unwrap();

    let reply = read_message(&mut stream).await.unwrap();
    assert_eq!(reply, Message::Reject { code: ErrorCode::InvalidFileName });
    assert_eq!(receiver.await.unwrap().unwrap_err().code(), ErrorCode::InvalidFileName);
}

#[tokio::test]
async fn malicious_relative_paths_are_rejected() {
    for path in ["../x", "C:\\x", "CON/x", "a/../../x", "/etc/passwd"] {
        let (_source_dir, download_dir, _cache_dir) = dirs();
        let (listener, addr) = local_listener().await;
        let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);

        let mut stream = raw_control(addr).await;
        write_message(&mut stream, &hello("attacker", PROTOCOL_VERSION)).await.unwrap();
        let entries = vec![
            OfferEntry { relative_path: "ok.txt".into(), size: 1 },
            OfferEntry { relative_path: path.into(), size: 1 },
        ];
        let offer = Message::Offer { transfer_id: Uuid::new_v4(), chunk_size: TEST_CHUNK_SIZE, entries };
        write_message(&mut stream, &offer).await.unwrap();

        let reply = read_message(&mut stream).await.unwrap();
        assert_eq!(reply, Message::Reject { code: ErrorCode::InvalidFileName }, "{path:?}");
        assert_eq!(receiver.await.unwrap().unwrap_err().code(), ErrorCode::InvalidFileName);
        assert_eq!(file_names(download_dir.path()), Vec::<String>::new(), "nothing written for {path:?}");
    }
}

#[tokio::test]
async fn dropped_connection_keeps_part_and_state_for_resume() {
    let (_source_dir, download_dir, _cache_dir) = dirs();
    let (listener, addr) = local_listener().await;
    let transfer_id = Uuid::new_v4();
    let same_offer = offer(transfer_id, "partial.bin", 10, 4);

    // First connection: send chunk 0 of 3 on a data connection, then vanish.
    let receiver = spawn_receiver(Arc::clone(&listener), download_dir.path().to_owned(), true);
    let mut control = raw_control(addr).await;
    write_message(&mut control, &hello("flaky", PROTOCOL_VERSION)).await.unwrap();
    write_message(&mut control, &same_offer).await.unwrap();
    assert_eq!(read_message(&mut control).await.unwrap(), Message::Accept { resume: vec![] });
    // Over TCP, `open_data` sends the `DataStreamHello` itself.
    let mut data = control.connection.open_data(transfer_id).await.unwrap();
    let hash = *blake3::hash(b"abcd").as_bytes();
    write_message(&mut data, &Message::ChunkHeader { entry: 0, index: 0, len: 4, hash }).await.unwrap();
    data.write_all(b"abcd").await.unwrap();
    assert_eq!(read_message(&mut control).await.unwrap(), Message::ChunkAck { entry: 0, index: 0 });
    drop((control, data));

    assert_eq!(receiver.await.unwrap().unwrap_err().code(), ErrorCode::ConnectionLost);
    assert_eq!(file_names(download_dir.path()), ["partial.bin.lanshare-part", "partial.bin.lanshare-state"]);

    // Second connection with the same offer: the receiver reports chunk 0 as already received.
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), false);
    let mut control = raw_control(addr).await;
    write_message(&mut control, &hello("flaky", PROTOCOL_VERSION)).await.unwrap();
    write_message(&mut control, &same_offer).await.unwrap();
    let expected = Message::Accept { resume: vec![EntryResume { entry: 0, have: vec![0b001] }] };
    assert_eq!(read_message(&mut control).await.unwrap(), expected, "resumes without asking the decider again");
    drop(control);
    assert_eq!(receiver.await.unwrap().unwrap_err().code(), ErrorCode::ConnectionLost);
}

#[tokio::test]
async fn chunk_data_on_the_control_channel_is_a_violation() {
    let (_source_dir, download_dir, _cache_dir) = dirs();
    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);

    let mut control = raw_control(addr).await;
    write_message(&mut control, &hello("old", PROTOCOL_VERSION)).await.unwrap();
    write_message(&mut control, &offer(Uuid::new_v4(), "x.bin", 4, 4)).await.unwrap();
    assert_eq!(read_message(&mut control).await.unwrap(), Message::Accept { resume: vec![] });
    let hash = *blake3::hash(b"abcd").as_bytes();
    write_message(&mut control, &Message::ChunkHeader { entry: 0, index: 0, len: 4, hash }).await.unwrap();

    assert_eq!(receiver.await.unwrap().unwrap_err().code(), ErrorCode::ProtocolViolation);
}

// ---------------------------------------------------------------------------------------------------------------
// Text.

async fn text_round_trips(preference: TransportPreference) {
    let (_source_dir, download_dir, _cache_dir) = dirs();
    // Exactly the limit, with two-byte characters and a line break in the middle.
    let mut text = "é".repeat(MAX_TEXT_BYTES / 2 - 1);
    text.insert_str(10, "\n\t");
    assert_eq!(text.len(), MAX_TEXT_BYTES);
    for text in ["https://example.com/a?b=c", text.as_str()] {
        let (listener, addr) = local_listener().await;
        let receiver = spawn_receiver(listener, download_dir.path().to_owned(), false);
        let connection = transport::connect(addr, &connect_options(preference)).await.unwrap();
        send_text(&connection, &sender_info(), text).await.unwrap();
        let ReceiveOutcome::Text(got) = receiver.await.unwrap().unwrap() else { panic!("expected a text") };
        assert_eq!(got.text, text);
        assert_eq!(got.device_name, "test-sender");
        assert_eq!(got.fingerprint, identities().sender.fingerprint());
    }
    assert_eq!(file_names(download_dir.path()), Vec::<String>::new(), "a text writes nothing to disk");
}
over_both_transports!(text_round_trips: text_round_trips_over_tcp, text_round_trips_over_quic);

#[tokio::test]
async fn text_too_long_is_refused_by_the_receiver() {
    let (_source_dir, download_dir, _cache_dir) = dirs();
    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);
    let expected = ErrorCode::TextTooLong { max_bytes: 65_536 };

    // `send_text` checks the length itself, so this sender writes the message by hand.
    let mut stream = raw_control(addr).await;
    write_message(&mut stream, &hello("long", PROTOCOL_VERSION)).await.unwrap();
    write_message(&mut stream, &Message::TextOffer { text: "a".repeat(MAX_TEXT_BYTES + 1) }).await.unwrap();
    assert_eq!(read_message(&mut stream).await.unwrap(), Message::Reject { code: expected.clone() });
    assert_eq!(receiver.await.unwrap().unwrap_err().code(), expected);
}

#[tokio::test]
async fn hello_versions_six_to_eight_are_accepted() {
    for version in MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION {
        let (_source_dir, download_dir, _cache_dir) = dirs();
        let (listener, addr) = local_listener().await;
        let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);
        let mut stream = raw_control(addr).await;
        write_message(&mut stream, &hello("any", version)).await.unwrap();
        write_message(&mut stream, &Message::TextOffer { text: "hi".into() }).await.unwrap();
        assert_eq!(read_message(&mut stream).await.unwrap(), Message::Done, "version {version}");
        // Closing at once spares the receiver its wait for the sender to close.
        drop(stream);
        assert!(matches!(receiver.await.unwrap().unwrap(), ReceiveOutcome::Text(_)));
    }
}

#[tokio::test]
async fn hello_older_than_version_six_is_a_mismatch() {
    let (_source_dir, download_dir, _cache_dir) = dirs();
    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);
    let mut stream = raw_control(addr).await;
    write_message(&mut stream, &hello("old", MIN_PROTOCOL_VERSION - 1)).await.unwrap();
    let expected = ErrorCode::VersionMismatch { ours: PROTOCOL_VERSION, theirs: MIN_PROTOCOL_VERSION - 1 };
    assert_eq!(read_message(&mut stream).await.unwrap(), Message::Reject { code: expected.clone() });
    assert_eq!(receiver.await.unwrap().unwrap_err().code(), expected);
}
