//! Sending side: offers one file and streams its bytes once the receiver accepts.

use std::{io, path::Path};

use tokio::{
    fs::File,
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::watch,
};
use tracing::{debug, info, warn};

use super::{chunk_len, sanitize_file_name, Progress, BUFFER_SIZE};
use crate::{
    error::{Error, Result},
    identity::DeviceInfo,
    protocol::{read_message, write_message, Message, PROTOCOL_VERSION},
};

/// Sends the file at `file_path` over `stream` as device `local`, publishing progress on `progress`.
///
/// Flow: `Hello`, `Offer`, wait for `Accept`, stream the raw bytes, then `Done`. The file is streamed through a
/// 1 MiB buffer and is never loaded into memory.
///
/// # Errors
///
/// - [`Error::Declined`] or [`Error::VersionMismatch`] if the receiver refuses
/// - [`Error::InvalidFileName`] if the path has no usable file name
/// - [`Error::ConnectionLost`] if the connection drops
/// - [`Error::Io`] if the file cannot be read, is not a regular file, or shrinks while sending
pub async fn send_file<S>(
    stream: &mut S,
    file_path: &Path,
    local: &DeviceInfo,
    progress: &watch::Sender<Progress>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let file_name = offer_file_name(file_path)?;
    let file = File::open(file_path).await?;
    let metadata = file.metadata().await?;
    if !metadata.is_file() {
        return Err(Error::Io(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file")));
    }
    let size = metadata.len();

    let hello =
        Message::Hello { protocol_version: PROTOCOL_VERSION, device_id: local.id, device_name: local.name.clone() };
    write_message(stream, &hello).await?;
    write_message(stream, &Message::Offer { file_name: file_name.clone(), size }).await?;
    wait_for_accept(stream).await?;

    info!(%file_name, size, "offer accepted, sending");
    progress.send_replace(Progress { transferred: 0, total: size });
    stream_file(stream, file, size, progress).await?;
    write_message(stream, &Message::Done).await?;
    info!(%file_name, size, "file sent");
    Ok(())
}

/// The name to put in the offer: the last path component, which must be valid UTF-8 and pass sanitizing.
fn offer_file_name(file_path: &Path) -> Result<String> {
    let name = file_path.file_name().and_then(|name| name.to_str()).ok_or(Error::InvalidFileName)?;
    sanitize_file_name(name)
}

/// Waits for the receiver's answer to the offer.
async fn wait_for_accept<S>(stream: &mut S) -> Result<()>
where
    S: AsyncRead + Unpin,
{
    match read_message(stream).await? {
        Message::Accept => Ok(()),
        Message::Reject { code } => {
            debug!(code = code.name(), "offer rejected");
            Err(Error::from_peer(code))
        }
        Message::Error { code, detail } => {
            warn!(code = code.name(), ?detail, "receiver reported an error");
            Err(Error::from_peer(code))
        }
        other => Err(Error::protocol_violation(format!("expected Accept or Reject, got {other:?}"))),
    }
}

/// Copies exactly `size` bytes from `file` to `stream`, updating progress after every chunk.
async fn stream_file<S>(stream: &mut S, mut file: File, size: u64, progress: &watch::Sender<Progress>) -> Result<()>
where
    S: AsyncWrite + Unpin,
{
    let mut buffer = vec![0; BUFFER_SIZE];
    let mut transferred = 0;
    while transferred < size {
        let want = chunk_len(size - transferred);
        let read = file.read(&mut buffer[..want]).await.map_err(Error::Io)?;
        if read == 0 {
            // The file got shorter after we offered it. Build the error directly so it is not mistaken for a
            // lost connection by the `From<io::Error>` conversion.
            return Err(Error::Io(io::Error::new(io::ErrorKind::UnexpectedEof, "file shrank while sending")));
        }
        stream.write_all(&buffer[..read]).await?;
        transferred += read as u64;
        progress.send_replace(Progress { transferred, total: size });
    }
    stream.flush().await?;
    Ok(())
}
