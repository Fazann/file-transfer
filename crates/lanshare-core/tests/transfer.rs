//! Integration tests: a real sender and receiver talking over localhost TCP.

// `allow-unwrap-in-tests` in clippy.toml only covers `#[test]` functions, not the shared helpers in this file.
#![allow(clippy::unwrap_used)]

use std::{
    future::Future,
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
};

use lanshare_core::{
    identity::{DeviceId, DeviceInfo, Os},
    protocol::{read_message, write_message, Message, PROTOCOL_VERSION},
    transfer::{progress_channel, receive_file, send_file, IncomingOffer, OfferDecider, ReceiveOutcome},
    transport, Error, ErrorCode,
};
use tempfile::TempDir;
use tokio::{net::TcpListener, task::JoinHandle};

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

async fn send(addr: SocketAddr, file: &Path) -> Result<(), Error> {
    let mut stream = transport::connect(addr).await.unwrap();
    let (progress, _watcher) = progress_channel();
    send_file(&mut stream, file, &sender_info(), &progress).await
}

/// Writes `size` bytes of deterministic pseudo-random data, 1 MiB at a time.
fn write_test_file(path: &Path, size: usize) {
    let mut file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut chunk = vec![0u8; 1024 * 1024];
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
    let mut left_buffer = vec![0u8; 1024 * 1024];
    let mut right_buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = left_file.read(&mut left_buffer).unwrap();
        if read == 0 {
            break;
        }
        right_file.read_exact(&mut right_buffer[..read]).unwrap();
        assert!(left_buffer[..read] == right_buffer[..read], "contents differ");
    }
}

fn dirs() -> (TempDir, TempDir) {
    (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn sends_100_mib_file_over_localhost() {
    let (source_dir, download_dir) = dirs();
    let source = source_dir.path().join("big.bin");
    write_test_file(&source, 100 * 1024 * 1024);

    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);
    send(addr, &source).await.unwrap();

    let ReceiveOutcome::Saved(saved) = receiver.await.unwrap().unwrap() else {
        panic!("expected the file to be saved");
    };
    assert_eq!(saved.path, download_dir.path().join("big.bin"));
    assert_eq!(saved.offer.device_name, "test-sender");
    assert_same_contents(&source, &saved.path);
}

#[tokio::test]
async fn existing_file_gets_numbered_name() {
    let (source_dir, download_dir) = dirs();
    let source = source_dir.path().join("note.txt");
    std::fs::write(&source, b"new contents").unwrap();
    std::fs::write(download_dir.path().join("note.txt"), b"old contents").unwrap();

    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);
    send(addr, &source).await.unwrap();

    let ReceiveOutcome::Saved(saved) = receiver.await.unwrap().unwrap() else {
        panic!("expected the file to be saved");
    };
    assert_eq!(saved.path, download_dir.path().join("note (1).txt"));
    assert_eq!(std::fs::read(&saved.path).unwrap(), b"new contents");
    assert_eq!(std::fs::read(download_dir.path().join("note.txt")).unwrap(), b"old contents");
}

#[tokio::test]
async fn rejected_offer_ends_sender_with_declined() {
    let (source_dir, download_dir) = dirs();
    let source = source_dir.path().join("unwanted.txt");
    std::fs::write(&source, b"no thanks").unwrap();

    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), false);
    let error = send(addr, &source).await.unwrap_err();

    assert_eq!(error.code(), ErrorCode::Declined);
    assert!(matches!(receiver.await.unwrap().unwrap(), ReceiveOutcome::Declined(_)));
    assert_eq!(std::fs::read_dir(download_dir.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn version_mismatch_is_rejected() {
    let (_source_dir, download_dir) = dirs();
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
    let (_source_dir, download_dir) = dirs();
    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);

    // A `Hello` from a future version whose fields after `protocol_version` this version cannot decode.
    let mut stream = transport::connect(addr).await.unwrap();
    tokio::io::AsyncWriteExt::write_all(&mut stream, &[0, 0, 0, 4, 0, 99, 0xff, 0xff]).await.unwrap();

    let expected = ErrorCode::VersionMismatch { ours: PROTOCOL_VERSION, theirs: 99 };
    assert_eq!(read_message(&mut stream).await.unwrap(), Message::Reject { code: expected.clone() });
    assert_eq!(receiver.await.unwrap().unwrap_err().code(), expected);
}

#[tokio::test]
async fn unsafe_file_name_is_rejected() {
    let (_source_dir, download_dir) = dirs();
    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);

    let mut stream = transport::connect(addr).await.unwrap();
    write_message(&mut stream, &hello("attacker", PROTOCOL_VERSION)).await.unwrap();
    write_message(&mut stream, &Message::Offer { file_name: "../..".into(), size: 1 }).await.unwrap();

    let reply = read_message(&mut stream).await.unwrap();
    assert_eq!(reply, Message::Reject { code: ErrorCode::InvalidFileName });
    assert_eq!(receiver.await.unwrap().unwrap_err().code(), ErrorCode::InvalidFileName);
}

#[tokio::test]
async fn dropped_connection_deletes_partial_file() {
    let (_source_dir, download_dir) = dirs();
    let (listener, addr) = local_listener().await;
    let receiver = spawn_receiver(listener, download_dir.path().to_owned(), true);

    let mut stream = transport::connect(addr).await.unwrap();
    write_message(&mut stream, &hello("flaky", PROTOCOL_VERSION)).await.unwrap();
    write_message(&mut stream, &Message::Offer { file_name: "partial.bin".into(), size: 1000 }).await.unwrap();
    assert_eq!(read_message(&mut stream).await.unwrap(), Message::Accept);
    tokio::io::AsyncWriteExt::write_all(&mut stream, &[7u8; 10]).await.unwrap();
    drop(stream);

    assert_eq!(receiver.await.unwrap().unwrap_err().code(), ErrorCode::ConnectionLost);
    assert!(!download_dir.path().join("partial.bin").exists());
}
