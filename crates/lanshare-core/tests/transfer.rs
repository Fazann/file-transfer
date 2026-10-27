//! Integration tests: a real sender and receiver talking over localhost TCP, sometimes through a proxy that breaks
//! the connection or corrupts a byte.

// `allow-unwrap-in-tests` in clippy.toml only covers `#[test]` functions, not the shared helpers in this file.
#![allow(clippy::unwrap_used)]

use std::{
    future::Future,
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use lanshare_core::{
    identity::{DeviceId, DeviceInfo, Os},
    protocol::{read_message, write_message, EntryResume, Message, OfferEntry, PROTOCOL_VERSION},
    transfer::{
        progress_channel, receive_file, send_file, IncomingOffer, OfferDecider, ReceiveOutcome, ReceivedFile,
        SendOptions, DEFAULT_CHUNK_SIZE,
    },
    transport, Error, ErrorCode,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use uuid::Uuid;

/// Small chunks keep the tests fast while still giving files many chunks.
const TEST_CHUNK_SIZE: u32 = 256 * 1024;

const MIB: usize = 1024 * 1024;

/// Accepts or declines every offer.
struct FixedDecider(bool);

impl OfferDecider for FixedDecider {
    fn decide(&self, _offer: &IncomingOffer) -> impl Future<Output = bool> + Send {
        std::future::ready(self.0)
    }
}

async fn local_listener() -> (TcpListener, SocketAddr) {
    let listener = transport::listen(0).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
}

/// Runs a receiver for one connection in the background.
fn spawn_receiver(listener: TcpListener, dir: PathBuf, accept: bool) -> JoinHandle<Result<ReceiveOutcome, Error>> {
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let (progress, _watcher) = progress_channel();
        receive_file(&mut stream, &dir, &FixedDecider(accept), &progress).await
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
fn spawn_receiver_until_saved(listener: Arc<TcpListener>, dir: PathBuf) -> JoinHandle<ReceiverLog> {
    tokio::spawn(async move {
        let mut errors = Vec::new();
        let mut resumed = Vec::new();
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (progress, watcher) = progress_channel();
            let result = receive_file(&mut stream, &dir, &FixedDecider(true), &progress).await;
            resumed.push(watcher.borrow().resumed);
            match result {
                Ok(ReceiveOutcome::Saved(saved)) => return ReceiverLog { errors, resumed, saved },
                Ok(ReceiveOutcome::Declined(_)) => panic!("the test decider accepts everything"),
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
    }
}

fn hello(device_name: &str, protocol_version: u16) -> Message {
    Message::Hello { protocol_version, device_id: DeviceId::new_random(), device_name: device_name.into() }
}

fn offer(name: &str, size: u64, chunk_size: u32) -> Message {
    Message::Offer {
        transfer_id: Uuid::new_v4(),
        chunk_size,
        entries: vec![OfferEntry { relative_path: name.into(), size }],
    }
}

fn options(cache: &TempDir, chunk_size: u32) -> SendOptions {
    SendOptions { chunk_size, ..SendOptions::new(cache.path().to_owned()) }
}

async fn send(addr: SocketAddr, file: &Path, options: &SendOptions) -> Result<(), Error> {
    let (progress, _watcher) = progress_channel();
    send_file(|| transport::connect(addr), file, &sender_info(), options, &progress).await
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
    bits[(index / 8) as usize] & (1 << (index % 8)) != 0
}

fn dirs() -> (TempDir, TempDir, TempDir) {
    (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap())
}

/// What the proxy does to the first connection. Later connections pass through untouched.
#[derive(Debug, Clone, Copy, Default)]
struct Fault {
    /// Close both sides after forwarding this many sender bytes.
    cut_after: Option<u64>,
    /// Flip every bit of the sender byte at this offset.
    corrupt_at: Option<u64>,
}

/// Frames the receiver sent back, one list per connection.
type ReplyLog = Arc<Mutex<Vec<Vec<Message>>>>;

/// Starts a proxy in front of `target`. This is the test hook that breaks or corrupts the connection.
async fn spawn_proxy(target: SocketAddr, fault: Fault) -> (SocketAddr, ReplyLog) {
    let (listener, addr) = local_listener().await;
    let log = ReplyLog::default();
    let task_log = Arc::clone(&log);
    tokio::spawn(async move {
        let mut fault = Some(fault);
        while let Ok((client, _)) = listener.accept().await {
            let index = {
                let mut log = task_log.lock().unwrap();
                log.push(Vec::new());
                log.len() - 1
            };
            let connection_fault = fault.take().unwrap_or_default();
            tokio::spawn(proxy_connection(client, target, connection_fault, Arc::clone(&task_log), index));
        }
    });
    (addr, log)
}

async fn proxy_connection(client: TcpStream, target: SocketAddr, fault: Fault, log: ReplyLog, index: usize) {
    let server = TcpStream::connect(target).await.unwrap();
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = server.into_split();

    // Returns `true` if it cut the connection, `false` when the sender closed it.
    let forward = async {
        let mut buffer = vec![0u8; 64 * 1024];
        let mut position = 0u64;
        loop {
            let read = match client_read.read(&mut buffer).await {
                Ok(0) | Err(_) => return false,
                Ok(read) => read,
            };
            let end = position + read as u64;
            if let Some(at) = fault.corrupt_at.filter(|at| (position..end).contains(at)) {
                buffer[usize::try_from(at - position).unwrap()] ^= 0xff;
            }
            if let Some(cut) = fault.cut_after.filter(|cut| *cut <= end) {
                let _ = server_write.write_all(&buffer[..usize::try_from(cut - position).unwrap()]).await;
                return true;
            }
            if server_write.write_all(&buffer[..read]).await.is_err() {
                return false;
            }
            position = end;
        }
    };
    let backward = async {
        while let Ok(message) = read_message(&mut server_read).await {
            log.lock().unwrap()[index].push(message.clone());
            if write_message(&mut client_write, &message).await.is_err() {
                break;
            }
        }
    };
    // `pin!` lets both futures be polled by `select!` and then awaited again afterwards.
    tokio::pin!(forward, backward);
    tokio::select! {
        cut = &mut forward => {
            if !cut {
                (&mut backward).await;
            }
        }
        () = &mut backward => {}
    }
    // Returning drops all four halves, which closes both connections.
}

#[tokio::test(flavor = "multi_thread")]
async fn sends_100_mib_file_over_localhost() {
    let (source_dir, download_dir, cache_dir) = dirs();
    let source = source_dir.path().join("big.bin");
    write_test_file(&source, 100 * MIB);

    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);
    send(addr, &source, &options(&cache_dir, DEFAULT_CHUNK_SIZE)).await.unwrap();

    let ReceiveOutcome::Saved(saved) = receiver.await.unwrap().unwrap() else {
        panic!("expected the file to be saved");
    };
    assert_eq!(saved.path, download_dir.path().join("big.bin"));
    assert_eq!(saved.offer.device_name, "test-sender");
    assert_same_contents(&source, &saved.path);
    assert_eq!(file_names(download_dir.path()), ["big.bin"], "part and state files are gone");
    assert_eq!(file_names(cache_dir.path()), Vec::<String>::new(), "hash cache entry is deleted after success");
}

#[tokio::test]
async fn empty_file_is_sent() {
    let (source_dir, download_dir, cache_dir) = dirs();
    let source = source_dir.path().join("empty.txt");
    std::fs::write(&source, b"").unwrap();

    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);
    send(addr, &source, &options(&cache_dir, TEST_CHUNK_SIZE)).await.unwrap();

    let ReceiveOutcome::Saved(saved) = receiver.await.unwrap().unwrap() else {
        panic!("expected the file to be saved");
    };
    assert_eq!(std::fs::read(&saved.path).unwrap(), b"");
}

#[tokio::test(flavor = "multi_thread")]
async fn dropped_connection_resumes_with_only_missing_chunks() {
    let (source_dir, download_dir, cache_dir) = dirs();
    let source = source_dir.path().join("resume.bin");
    let size = 8 * MIB;
    write_test_file(&source, size);
    let chunk_count = (size / TEST_CHUNK_SIZE as usize) as u64;

    let (listener, receiver_addr) = local_listener().await;
    let receiver = spawn_receiver_until_saved(Arc::new(listener), download_dir.path().to_owned());
    let fault = Fault { cut_after: Some(size as u64 / 2), ..Fault::default() };
    let (proxy_addr, log) = spawn_proxy(receiver_addr, fault).await;
    send(proxy_addr, &source, &options(&cache_dir, TEST_CHUNK_SIZE)).await.unwrap();

    let receiver = receiver.await.unwrap();
    assert_eq!(receiver.errors, [ErrorCode::ConnectionLost]);
    assert_same_contents(&source, &receiver.saved.path);
    assert_eq!(file_names(download_dir.path()), ["resume.bin"]);

    let log = log.lock().unwrap();
    assert_eq!(log.len(), 2, "one reconnect");
    let Message::Accept { resume } = &log[1][0] else {
        panic!("expected Accept, got {:?}", log[1][0]);
    };
    let [EntryResume { entry: 0, have }] = resume.as_slice() else {
        panic!("expected one resume entry, got {resume:?}");
    };
    let resent: Vec<u64> = log[1]
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

#[tokio::test(flavor = "multi_thread")]
async fn corrupted_chunk_is_nacked_and_resent() {
    let (source_dir, download_dir, cache_dir) = dirs();
    let source = source_dir.path().join("corrupt.bin");
    write_test_file(&source, 2 * MIB);

    let (listener, receiver_addr) = local_listener().await;
    let receiver = spawn_receiver_until_saved(Arc::new(listener), download_dir.path().to_owned());
    // Past the hello, offer and first chunk header (well under 1 KiB), inside chunk 0's data.
    let fault = Fault { corrupt_at: Some(100_000), ..Fault::default() };
    let (proxy_addr, log) = spawn_proxy(receiver_addr, fault).await;
    send(proxy_addr, &source, &options(&cache_dir, TEST_CHUNK_SIZE)).await.unwrap();

    let receiver = receiver.await.unwrap();
    assert_eq!(receiver.errors, []);
    assert_same_contents(&source, &receiver.saved.path);

    let log = log.lock().unwrap();
    assert_eq!(log.len(), 1, "no reconnect needed");
    let nack = log[0].iter().position(|message| *message == Message::ChunkNack { entry: 0, index: 0 });
    let ack = log[0].iter().position(|message| *message == Message::ChunkAck { entry: 0, index: 0 });
    assert!(nack.is_some() && ack > nack, "expected ChunkNack then ChunkAck for chunk 0, got {:?}", log[0]);
}

#[tokio::test(flavor = "multi_thread")]
async fn restarted_receiver_resumes() {
    let (source_dir, download_dir, cache_dir) = dirs();
    let source = source_dir.path().join("restart.bin");
    write_test_file(&source, 16 * MIB);
    let (listener, addr) = local_listener().await;
    let listener = Arc::new(listener);

    // The first receiver is stopped halfway, as if the program was closed.
    let (progress, mut watcher) = progress_channel();
    let first_listener = Arc::clone(&listener);
    let first_dir = download_dir.path().to_owned();
    let first = tokio::spawn(async move {
        let (mut stream, _) = first_listener.accept().await.unwrap();
        receive_file(&mut stream, &first_dir, &FixedDecider(true), &progress).await
    });
    let source_path = source.clone();
    let options = options(&cache_dir, TEST_CHUNK_SIZE);
    let sender = tokio::spawn(async move { send(addr, &source_path, &options).await });

    watcher.wait_for(|progress| progress.total > 0 && progress.transferred >= progress.total / 2).await.unwrap();
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    assert!(download_dir.path().join("restart.bin.lanshare-state").exists(), "state saved when stopped");

    let second = spawn_receiver_until_saved(listener, download_dir.path().to_owned());
    sender.await.unwrap().unwrap();
    let second = second.await.unwrap();
    assert!(second.resumed.last().copied().unwrap() > 0, "second receiver resumed");
    assert_same_contents(&source, &second.saved.path);
}

#[tokio::test]
async fn offer_larger_than_free_space_is_rejected() {
    let (_source_dir, download_dir, _cache_dir) = dirs();
    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);

    // 16 TiB: more than any test machine has free, and still within the chunk count limit.
    let size = 1u64 << 44;
    let mut stream = transport::connect(addr).await.unwrap();
    write_message(&mut stream, &hello("huge", PROTOCOL_VERSION)).await.unwrap();
    write_message(&mut stream, &offer("huge.bin", size, DEFAULT_CHUNK_SIZE)).await.unwrap();

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
async fn existing_file_gets_numbered_name() {
    let (source_dir, download_dir, cache_dir) = dirs();
    let source = source_dir.path().join("note.txt");
    std::fs::write(&source, b"new contents").unwrap();
    std::fs::write(download_dir.path().join("note.txt"), b"old contents").unwrap();

    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);
    send(addr, &source, &options(&cache_dir, TEST_CHUNK_SIZE)).await.unwrap();

    let ReceiveOutcome::Saved(saved) = receiver.await.unwrap().unwrap() else {
        panic!("expected the file to be saved");
    };
    assert_eq!(saved.path, download_dir.path().join("note (1).txt"));
    assert_eq!(std::fs::read(&saved.path).unwrap(), b"new contents");
    assert_eq!(std::fs::read(download_dir.path().join("note.txt")).unwrap(), b"old contents");
}

#[tokio::test]
async fn rejected_offer_ends_sender_with_declined() {
    let (source_dir, download_dir, cache_dir) = dirs();
    let source = source_dir.path().join("unwanted.txt");
    std::fs::write(&source, b"no thanks").unwrap();

    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), false);
    let error = send(addr, &source, &options(&cache_dir, TEST_CHUNK_SIZE)).await.unwrap_err();

    assert_eq!(error.code(), ErrorCode::Declined);
    assert!(matches!(receiver.await.unwrap().unwrap(), ReceiveOutcome::Declined(_)));
    assert_eq!(file_names(download_dir.path()), Vec::<String>::new());
}

#[tokio::test]
async fn version_mismatch_is_rejected() {
    let (_source_dir, download_dir, _cache_dir) = dirs();
    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);

    let future_version = PROTOCOL_VERSION + 1;
    let mut stream = transport::connect(addr).await.unwrap();
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
    let mut stream = transport::connect(addr).await.unwrap();
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

    let mut stream = transport::connect(addr).await.unwrap();
    write_message(&mut stream, &hello("attacker", PROTOCOL_VERSION)).await.unwrap();
    write_message(&mut stream, &offer("../..", 1, TEST_CHUNK_SIZE)).await.unwrap();

    let reply = read_message(&mut stream).await.unwrap();
    assert_eq!(reply, Message::Reject { code: ErrorCode::InvalidFileName });
    assert_eq!(receiver.await.unwrap().unwrap_err().code(), ErrorCode::InvalidFileName);
}

#[tokio::test]
async fn offer_with_several_entries_is_rejected() {
    let (_source_dir, download_dir, _cache_dir) = dirs();
    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);

    let mut stream = transport::connect(addr).await.unwrap();
    write_message(&mut stream, &hello("folder", PROTOCOL_VERSION)).await.unwrap();
    let entries = vec![
        OfferEntry { relative_path: "a.txt".into(), size: 1 },
        OfferEntry { relative_path: "b.txt".into(), size: 1 },
    ];
    write_message(&mut stream, &Message::Offer { transfer_id: Uuid::new_v4(), chunk_size: TEST_CHUNK_SIZE, entries })
        .await
        .unwrap();

    assert_eq!(read_message(&mut stream).await.unwrap(), Message::Reject { code: ErrorCode::ProtocolViolation });
    assert_eq!(receiver.await.unwrap().unwrap_err().code(), ErrorCode::ProtocolViolation);
}

#[tokio::test]
async fn dropped_connection_keeps_part_and_state_for_resume() {
    let (_source_dir, download_dir, _cache_dir) = dirs();
    let (listener, addr) = local_listener().await;
    let listener = Arc::new(listener);
    let same_offer = offer("partial.bin", 10, 4);

    // First connection: send chunk 0 of 3, then vanish.
    let receiver = {
        let listener = Arc::clone(&listener);
        let dir = download_dir.path().to_owned();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (progress, _watcher) = progress_channel();
            receive_file(&mut stream, &dir, &FixedDecider(true), &progress).await
        })
    };
    let mut stream = transport::connect(addr).await.unwrap();
    write_message(&mut stream, &hello("flaky", PROTOCOL_VERSION)).await.unwrap();
    write_message(&mut stream, &same_offer).await.unwrap();
    assert_eq!(read_message(&mut stream).await.unwrap(), Message::Accept { resume: vec![] });
    let hash = *blake3::hash(b"abcd").as_bytes();
    write_message(&mut stream, &Message::ChunkHeader { entry: 0, index: 0, len: 4, hash }).await.unwrap();
    stream.write_all(b"abcd").await.unwrap();
    assert_eq!(read_message(&mut stream).await.unwrap(), Message::ChunkAck { entry: 0, index: 0 });
    drop(stream);

    assert_eq!(receiver.await.unwrap().unwrap_err().code(), ErrorCode::ConnectionLost);
    assert_eq!(file_names(download_dir.path()), ["partial.bin.lanshare-part", "partial.bin.lanshare-state"]);

    // Second connection with the same offer: the receiver reports chunk 0 as already received.
    let receiver = spawn_receiver(Arc::into_inner(listener).unwrap(), download_dir.path().to_owned(), false);
    let mut stream = transport::connect(addr).await.unwrap();
    write_message(&mut stream, &hello("flaky", PROTOCOL_VERSION)).await.unwrap();
    write_message(&mut stream, &same_offer).await.unwrap();
    let expected = Message::Accept { resume: vec![EntryResume { entry: 0, have: vec![0b001] }] };
    assert_eq!(read_message(&mut stream).await.unwrap(), expected, "resumes without asking the decider again");
    drop(stream);
    assert_eq!(receiver.await.unwrap().unwrap_err().code(), ErrorCode::ConnectionLost);
}
