//! Length-prefixed framing: each frame is a big-endian `u32` payload length followed by a `postcard`-encoded
//! [`Message`].

use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::{is_supported_version, Message, PROTOCOL_VERSION};
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
    decode_frame(&read_raw_frame(reader).await?)
}

/// Like [`read_message`], but returns `None` if the stream ends cleanly before the next frame starts.
///
/// Data streams end this way once the sender has nothing more to send.
///
/// # Errors
///
/// The same as [`read_message`]; a stream that ends inside a frame is still [`Error::ConnectionLost`].
pub async fn read_message_or_end<R>(reader: &mut R) -> Result<Option<Message>>
where
    R: AsyncRead + Unpin,
{
    let mut first = [0u8; 1];
    if reader.read(&mut first).await? == 0 {
        return Ok(None);
    }
    let mut rest = [0u8; 3];
    reader.read_exact(&mut rest).await?;
    let length = u32::from_be_bytes([first[0], rest[0], rest[1], rest[2]]);
    let payload = read_payload(reader, length).await?;
    decode_payload(&payload).map(Some)
}

/// Reads one whole frame, length prefix included, without decoding it.
///
/// The TCP listener uses this to look at the first frame of a connection and then hand the untouched bytes on.
pub(crate) async fn read_raw_frame<R>(reader: &mut R) -> Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let length = reader.read_u32().await?;
    let payload = read_payload(reader, length).await?;
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Decodes a frame read by [`read_raw_frame`].
pub(crate) fn decode_frame(frame: &[u8]) -> Result<Message> {
    decode_payload(frame.get(4..).unwrap_or_default())
}

async fn read_payload<R>(reader: &mut R, length: u32) -> Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    if length > MAX_FRAME_SIZE {
        return Err(Error::protocol_violation(format!("incoming frame of {length} bytes is too large")));
    }
    let mut payload = vec![0; length as usize];
    reader.read_exact(&mut payload).await?;
    Ok(payload)
}

fn decode_payload(payload: &[u8]) -> Result<Message> {
    postcard::from_bytes(payload).map_err(|error| match peek_hello_version(payload) {
        Some(theirs) if !is_supported_version(theirs) => Error::VersionMismatch { ours: PROTOCOL_VERSION, theirs },
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

    #[test]
    fn text_offer_is_variant_18() {
        // Version 6 ends at `PairResult` (17); `TextOffer` must come right after it.
        let payload = postcard::to_allocvec(&Message::TextOffer { text: "hi".into() }).unwrap();
        assert_eq!(payload, [18, 2, b'h', b'i']);
    }

    #[test]
    fn clipboard_offer_is_variant_19() {
        let payload = postcard::to_allocvec(&Message::ClipboardOffer { text: "hi".into() }).unwrap();
        assert_eq!(payload, [19, 2, b'h', b'i']);
    }

    #[test]
    fn supported_hello_versions_do_not_count_as_a_mismatch() {
        // A `Hello` with trailing bytes this version cannot decode, but a version it accepts.
        for (version, mismatch) in [(5, true), (6, false), (7, false), (8, false), (9, true)] {
            let error = decode_payload(&[0, version, 0xff]).unwrap_err();
            assert_eq!(matches!(error, Error::VersionMismatch { .. }), mismatch, "version {version}");
        }
    }

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
            Message::DataStreamHello { transfer_id: Uuid::new_v4() },
            Message::Pause,
            Message::Resume,
            Message::BenchOffer { transfer_id: Uuid::new_v4(), size: 2_000_000_000 },
            Message::Error { code: ErrorCode::Io, detail: Some("disk full".into()) },
            Message::TextOffer { text: "https://example.com/ünïcode ✓".into() },
            Message::ClipboardOffer { text: "copied ✓".into() },
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
    async fn clean_end_between_frames_is_none() {
        let (mut client, mut server) = duplex(64);
        write_message(&mut client, &Message::Done).await.unwrap();
        drop(client);
        assert_eq!(read_message_or_end(&mut server).await.unwrap(), Some(Message::Done));
        assert_eq!(read_message_or_end(&mut server).await.unwrap(), None);
    }

    #[tokio::test]
    async fn raw_frames_decode_to_the_same_message() {
        let (mut client, mut server) = duplex(64);
        write_message(&mut client, &Message::Pause).await.unwrap();
        let frame = read_raw_frame(&mut server).await.unwrap();
        assert_eq!(decode_frame(&frame).unwrap(), Message::Pause);
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
