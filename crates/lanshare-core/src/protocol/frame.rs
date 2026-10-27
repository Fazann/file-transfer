//! Length-prefixed framing: each frame is a big-endian `u32` payload length followed by a `postcard`-encoded
//! [`Message`].

use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::{Message, PROTOCOL_VERSION};
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
/// - [`Error::VersionMismatch`] if the payload does not decode but starts like a `Hello` from another version
/// - [`Error::ProtocolViolation`] for frames larger than [`MAX_FRAME_SIZE`] or other payloads that do not decode
/// - [`Error::ConnectionLost`] if the stream ends mid-frame, or another I/O error from the reader
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
    postcard::from_bytes(&payload).map_err(|error| match peek_hello_version(&payload) {
        Some(theirs) if theirs != PROTOCOL_VERSION => Error::VersionMismatch { ours: PROTOCOL_VERSION, theirs },
        _ => Error::protocol_violation(format!("cannot decode message: {error}")),
    })
}

/// Only the part of `Hello` that never changes between versions: variant 0, first field `protocol_version`.
#[derive(Deserialize)]
enum HelloVersion {
    Hello { protocol_version: u16 },
}

/// Reads the protocol version from a `Hello` whose remaining fields this version cannot decode.
///
/// `take_from_bytes` decodes a prefix and ignores the rest, unlike `from_bytes` which requires every byte to match.
fn peek_hello_version(payload: &[u8]) -> Option<u16> {
    match postcard::take_from_bytes::<HelloVersion>(payload) {
        Ok((HelloVersion::Hello { protocol_version }, _rest)) => Some(protocol_version),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::duplex;
    use uuid::Uuid;

    use super::*;
    use crate::{
        error::ErrorCode,
        identity::DeviceId,
        protocol::{EntryResume, OfferEntry},
    };

    #[tokio::test]
    async fn every_message_round_trips() {
        let messages = [
            Message::Hello { protocol_version: 2, device_id: DeviceId::new_random(), device_name: "laptop".into() },
            Message::Offer {
                transfer_id: Uuid::new_v4(),
                chunk_size: 8 * 1024 * 1024,
                entries: vec![OfferEntry { relative_path: "movie.mkv".into(), size: 50 * 1024 * 1024 * 1024 }],
            },
            Message::Accept { resume: vec![] },
            Message::Accept { resume: vec![EntryResume { entry: 0, have: vec![0b1010_0101, 0b1] }] },
            Message::Reject { code: ErrorCode::InsufficientSpace { needed: 10, available: 5 } },
            Message::ChunkHeader { entry: 0, index: 7, len: 8 * 1024 * 1024, hash: [9; 32] },
            Message::ChunkAck { entry: 0, index: 7 },
            Message::ChunkNack { entry: 0, index: 7 },
            Message::EntryDone { entry: 0, root_hash: [3; 32] },
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
        let have = vec![0; MAX_FRAME_SIZE as usize + 1];
        let message = Message::Accept { resume: vec![EntryResume { entry: 0, have }] };
        let error = write_message(&mut client, &message).await.unwrap_err();
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
    async fn hello_from_another_layout_reports_version_mismatch() {
        let (mut client, mut server) = duplex(64);
        // Variant 0 (`Hello`), varint version 99, then fields this version cannot decode.
        let payload = [0, 99, 0xff, 0xff, 0xff];
        client.write_u32(5).await.unwrap();
        client.write_all(&payload).await.unwrap();
        let error = read_message(&mut server).await.unwrap_err();
        assert_eq!(error.code(), ErrorCode::VersionMismatch { ours: PROTOCOL_VERSION, theirs: 99 });
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
