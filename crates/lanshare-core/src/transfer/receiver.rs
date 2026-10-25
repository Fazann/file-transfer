//! Receiving side: checks the hello, asks for a decision on the offer, and saves the streamed bytes.

use std::{
    future::Future,
    io,
    path::{Path, PathBuf},
};

use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::watch,
};
use tracing::{debug, info, warn};

use super::{chunk_len, sanitize_file_name, Progress, BUFFER_SIZE};
use crate::{
    error::{Error, ErrorCode, Result},
    identity::DeviceId,
    protocol::{read_message, write_message, Message, PROTOCOL_VERSION},
};

/// How many `name (n).ext` candidates to try before giving up on finding a free file name.
const MAX_NAME_ATTEMPTS: u32 = 10_000;

/// An offer received from a sender, after its file name was sanitized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingOffer {
    /// Stable id of the sending device.
    pub device_id: DeviceId,
    /// Name the sender reported for its device.
    pub device_name: String,
    /// Sanitized file name.
    pub file_name: String,
    /// File size in bytes.
    pub size: u64,
}

/// Decides whether to accept an incoming offer. The CLI asks on the terminal; the app will ask in the UI.
///
/// The method is written as "returns a `Future`" instead of `async fn` so we can require the future to be `Send`
/// (safe to move between threads). Implementors can still write a plain `async fn decide(...)`.
pub trait OfferDecider: Send + Sync {
    /// Returns `true` to accept the offer.
    fn decide(&self, offer: &IncomingOffer) -> impl Future<Output = bool> + Send;
}

/// A file that was received and saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedFile {
    /// Where the file was saved. May differ from the offered name if that name was taken.
    pub path: PathBuf,
    /// The offer that was accepted.
    pub offer: IncomingOffer,
}

/// How a successful receive ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiveOutcome {
    /// The file was saved.
    Saved(ReceivedFile),
    /// The decider declined the offer and the sender was told so.
    Declined(IncomingOffer),
}

/// Handles one incoming transfer on `stream`, saving the file into `download_dir`.
///
/// If `download_dir/<file_name>` exists, the file is saved as `name (1).ext`, `name (2).ext` and so on. If the
/// transfer fails, or this future is dropped (cancelled) after the file was created, the partial file is deleted.
///
/// # Errors
///
/// - [`Error::VersionMismatch`] if the sender speaks another protocol version (the sender is told with `Reject`)
/// - [`Error::InvalidFileName`] if the offered name fails sanitizing (the sender is told with `Reject`)
/// - [`Error::ProtocolViolation`] if messages arrive out of order
/// - [`Error::ConnectionLost`] if the sender disconnects early
/// - [`Error::Io`] if the file cannot be created or written
pub async fn receive_file<S, D>(
    stream: &mut S,
    download_dir: &Path,
    decider: &D,
    progress: &watch::Sender<Progress>,
) -> Result<ReceiveOutcome>
where
    S: AsyncRead + AsyncWrite + Unpin,
    D: OfferDecider,
{
    let (device_id, device_name) = read_hello(stream).await?;
    let (file_name, size) = read_offer(stream).await?;
    let offer = IncomingOffer { device_id, device_name, file_name, size };

    if !decider.decide(&offer).await {
        info!(file_name = %offer.file_name, "offer declined");
        write_message(stream, &Message::Reject { code: ErrorCode::Declined }).await?;
        return Ok(ReceiveOutcome::Declined(offer));
    }

    let (file, path) = match create_unique_file(download_dir, &offer.file_name).await {
        Ok(created) => created,
        Err(error) => {
            write_message(stream, &Message::Reject { code: error.code() }).await?;
            return Err(error);
        }
    };
    // From here on, any early return or cancellation deletes the file, until `keep()` is called.
    let partial = PartialFile { path };
    write_message(stream, &Message::Accept).await?;
    info!(path = %partial.path.display(), size, "receiving");
    progress.send_replace(Progress { transferred: 0, total: size });

    receive_body(stream, file, size, progress).await?;
    let path = partial.keep();
    info!(path = %path.display(), size, "file received");
    Ok(ReceiveOutcome::Saved(ReceivedFile { path, offer }))
}

/// Deletes the file at `path` when dropped, unless [`PartialFile::keep`] was called.
///
/// `Drop` runs when the value goes out of scope, on success, on `?` early returns and when the receiving future is
/// cancelled, much like a `defer` in Go. Resume comes in phase 3; until then a partial file is useless.
struct PartialFile {
    path: PathBuf,
}

impl PartialFile {
    /// Marks the file as complete and returns its path without deleting it.
    fn keep(self) -> PathBuf {
        // `ManuallyDrop` stops `drop` from running, and `take` moves the path out of it.
        let mut this = std::mem::ManuallyDrop::new(self);
        std::mem::take(&mut this.path)
    }
}

impl Drop for PartialFile {
    fn drop(&mut self) {
        // `Drop` cannot be async, so this uses blocking `std::fs`. Deleting one file is quick.
        if let Err(error) = std::fs::remove_file(&self.path) {
            warn!(path = %self.path.display(), %error, "could not delete partial file");
        }
    }
}

/// Reads `Hello` and checks the protocol version. Returns the sender's device id and name.
async fn read_hello<S>(stream: &mut S) -> Result<(DeviceId, String)>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let message = match read_message(stream).await {
        // A `Hello` laid out by another version, which only the version field could be read from.
        Err(Error::VersionMismatch { theirs, .. }) => return reject_version(stream, theirs).await,
        other => other?,
    };
    match message {
        Message::Hello { protocol_version, device_id, device_name } if protocol_version == PROTOCOL_VERSION => {
            Ok((device_id, device_name))
        }
        Message::Hello { protocol_version, .. } => reject_version(stream, protocol_version).await,
        other => Err(Error::protocol_violation(format!("expected Hello, got {other:?}"))),
    }
}

/// Tells the sender its version is not supported, then fails with [`Error::VersionMismatch`].
async fn reject_version<S, T>(stream: &mut S, theirs: u16) -> Result<T>
where
    S: AsyncWrite + Unpin,
{
    let code = ErrorCode::VersionMismatch { ours: PROTOCOL_VERSION, theirs };
    write_message(stream, &Message::Reject { code }).await?;
    Err(Error::VersionMismatch { ours: PROTOCOL_VERSION, theirs })
}

/// Reads `Offer` and sanitizes the file name. Returns the safe name and the size.
async fn read_offer<S>(stream: &mut S) -> Result<(String, u64)>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match read_message(stream).await? {
        Message::Offer { file_name, size } => match sanitize_file_name(&file_name) {
            Ok(safe_name) => Ok((safe_name, size)),
            Err(error) => {
                debug!(?file_name, "rejecting unsafe file name");
                write_message(stream, &Message::Reject { code: error.code() }).await?;
                Err(error)
            }
        },
        other => Err(Error::protocol_violation(format!("expected Offer, got {other:?}"))),
    }
}

/// Creates a new file in `dir`, adding ` (n)` before the extension until the name is free.
///
/// `create_new` fails if the file exists, so two transfers can never pick the same name.
async fn create_unique_file(dir: &Path, file_name: &str) -> Result<(File, PathBuf)> {
    for attempt in 0..MAX_NAME_ATTEMPTS {
        let path = dir.join(candidate_name(file_name, attempt));
        match OpenOptions::new().write(true).create_new(true).open(&path).await {
            Ok(file) => return Ok((file, path)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(Error::Io(error)),
        }
    }
    Err(Error::Io(io::Error::new(io::ErrorKind::AlreadyExists, "no free file name")))
}

/// `report.pdf` with attempt 0 is `report.pdf`, with attempt 2 it is `report (2).pdf`.
fn candidate_name(file_name: &str, attempt: u32) -> String {
    if attempt == 0 {
        return file_name.to_owned();
    }
    // A leading dot marks a hidden file (`.bashrc`), not an extension.
    match file_name.rfind('.').filter(|&dot| dot > 0) {
        Some(dot) => format!("{} ({attempt}){}", &file_name[..dot], &file_name[dot..]),
        None => format!("{file_name} ({attempt})"),
    }
}

/// Reads exactly `size` raw bytes into `file`, then expects `Done`.
async fn receive_body<S>(stream: &mut S, mut file: File, size: u64, progress: &watch::Sender<Progress>) -> Result<()>
where
    S: AsyncRead + Unpin,
{
    let mut buffer = vec![0; BUFFER_SIZE];
    let mut transferred = 0;
    while transferred < size {
        let want = chunk_len(size - transferred);
        let read = stream.read(&mut buffer[..want]).await?;
        if read == 0 {
            return Err(Error::ConnectionLost);
        }
        file.write_all(&buffer[..read]).await.map_err(Error::Io)?;
        transferred += read as u64;
        progress.send_replace(Progress { transferred, total: size });
    }
    file.flush().await.map_err(Error::Io)?;
    file.sync_all().await.map_err(Error::Io)?;

    match read_message(stream).await? {
        Message::Done => Ok(()),
        other => Err(Error::protocol_violation(format!("expected Done, got {other:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_names_number_before_extension() {
        assert_eq!(candidate_name("report.pdf", 0), "report.pdf");
        assert_eq!(candidate_name("report.pdf", 1), "report (1).pdf");
        assert_eq!(candidate_name("archive.tar.gz", 2), "archive.tar (2).gz");
        assert_eq!(candidate_name("README", 3), "README (3)");
        assert_eq!(candidate_name(".bashrc", 1), ".bashrc (1)");
    }

    #[tokio::test]
    async fn existing_files_are_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"old").unwrap();
        std::fs::write(dir.path().join("a (1).txt"), b"old").unwrap();
        let (_file, path) = create_unique_file(dir.path(), "a.txt").await.unwrap();
        assert_eq!(path, dir.path().join("a (2).txt"));
        assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), b"old");
    }
}
