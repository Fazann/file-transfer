//! Length-prefixed framing: each frame is a big-endian `u32` payload length followed by a `postcard`-encoded
//! [`Message`].

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::Message;
use crate::error::{Error, Result};

/// Largest allowed frame payload in bytes (1 MiB). Larger frames are a protocol violation.
pub const MAX_FRAME_SIZE: u32 = 1024 * 1024;

/// Encodes `message` and writes it as one frame, then flushes.
///
/// `W: AsyncWrite + Unpin` works like accepting an `io.Writer` interface in Go: any async byte sink fits.
///
/// # Errors
///
/// [`Error::ProtocolViolation`] if the encoded message is larger than [`MAX_FRAME_SIZE`], or an I/O error from the
/// writer.
pub async fn write_message<W>(writer: &mut W, message: &Message) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let payload = postcard::to_allocvec(message)
        .map_err(|error| Error::protocol_violation(format!("cannot encode message: {error}")))?;
    let length = u32::try_from(payload.len()).unwrap_or(u32::MAX);
    if length > MAX_FRAME_SIZE {
        return Err(Error::protocol_violation(format!("outgoing frame of {length} bytes is too large")));
    }
    writer.write_u32(length).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

/// Reads one frame and decodes the [`Message`] inside it.
///
/// # Errors
///
/// [`Error::ProtocolViolation`] for frames larger than [`MAX_FRAME_SIZE`] or payloads that do not decode,
/// [`Error::ConnectionLost`] if the stream ends mid-frame, or another I/O error from the reader.
pub async fn read_message<R>(reader: &mut R) -> Result<Message>
where
    R: AsyncRead + Unpin,
{
    let length = reader.read_u32().await?;
    if length > MAX_FRAME_SIZE {
        return Err(Error::protocol_violation(format!("incoming frame of {length} bytes is too large")));
    }
    let mut payload = vec![0; length as usize];
    reader.read_exact(&mut payload).await?;
    postcard::from_bytes(&payload).map_err(|error| Error::protocol_violation(format!("cannot decode message: {error}")))
}

#[cfg(test)]
mod tests {
    use tokio::io::duplex;

    use super::*;
    use crate::error::ErrorCode;

    #[tokio::test]
    async fn every_message_round_trips() {
        let messages = [
            Message::Hello { device_name: "laptop".into(), protocol_version: 1 },
            Message::Offer { file_name: "movie.mkv".into(), size: 50 * 1024 * 1024 * 1024 },
            Message::Accept,
            Message::Reject { code: ErrorCode::VersionMismatch { ours: 1, theirs: 2 } },
            Message::Done,
            Message::Error { code: ErrorCode::Io, detail: Some("disk full".into()) },
        ];
        let (mut client, mut server) = duplex(64 * 1024);
        for message in &messages {
            write_message(&mut client, message).await.unwrap();
        }
        for message in &messages {
            assert_eq!(&read_message(&mut server).await.unwrap(), message);
        }
    }

    #[tokio::test]
    async fn oversized_incoming_frame_is_rejected() {
        let (mut client, mut server) = duplex(64);
        client.write_u32(MAX_FRAME_SIZE + 1).await.unwrap();
        let error = read_message(&mut server).await.unwrap_err();
        assert_eq!(error.code(), ErrorCode::ProtocolViolation);
    }

    #[tokio::test]
    async fn oversized_outgoing_message_is_rejected() {
        let (mut client, _server) = duplex(64);
        let file_name = "a".repeat(MAX_FRAME_SIZE as usize + 1);
        let error = write_message(&mut client, &Message::Offer { file_name, size: 1 }).await.unwrap_err();
        assert_eq!(error.code(), ErrorCode::ProtocolViolation);
    }

    #[tokio::test]
    async fn garbage_payload_is_rejected() {
        let (mut client, mut server) = duplex(64);
        client.write_u32(2).await.unwrap();
        client.write_all(&[0xff, 0xff]).await.unwrap();
        let error = read_message(&mut server).await.unwrap_err();
        assert_eq!(error.code(), ErrorCode::ProtocolViolation);
    }

    #[tokio::test]
    async fn truncated_frame_is_connection_lost() {
        let (mut client, mut server) = duplex(64);
        client.write_u32(10).await.unwrap();
        drop(client);
        let error = read_message(&mut server).await.unwrap_err();
        assert_eq!(error.code(), ErrorCode::ConnectionLost);
    }
}
