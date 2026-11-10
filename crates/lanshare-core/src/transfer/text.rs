//! Short texts (a link, a password, a snippet) sent with one `TextOffer` instead of files: no chunks, nothing on
//! disk, and only between paired devices.

use std::fmt;

use tracing::info;

use super::{control::ControlWriter, peer_error, read_frame};
use crate::{
    error::{Error, Result},
    identity::{DeviceId, DeviceInfo, Fingerprint},
    protocol::{Message, PROTOCOL_VERSION},
    transport::Connection,
};

/// The most bytes of UTF-8 one text may have (64 KiB). Anything longer is better sent as a file.
pub const MAX_TEXT_BYTES: usize = 64 * 1024;

/// A text received from a paired device.
///
/// `Debug` leaves the text out, so it never ends up in a log by accident: it may be a password.
#[derive(Clone, PartialEq, Eq)]
pub struct ReceivedText {
    /// Stable id of the sending device.
    pub device_id: DeviceId,
    /// Name the sender reported for its device.
    pub device_name: String,
    /// Fingerprint of the sender's certificate, proven by TLS.
    pub fingerprint: Fingerprint,
    /// The text.
    pub text: String,
}

impl fmt::Debug for ReceivedText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReceivedText")
            .field("device_id", &self.device_id)
            .field("device_name", &self.device_name)
            .field("fingerprint", &self.fingerprint)
            .field("bytes", &self.text.len())
            .finish()
    }
}

/// Checks that `text` may be sent: 1 to [`MAX_TEXT_BYTES`] bytes.
///
/// # Errors
///
/// [`Error::TextTooLong`] for longer text, [`Error::ProtocolViolation`] for empty text.
pub fn check_text(text: &str) -> Result<()> {
    if text.is_empty() {
        return Err(Error::protocol_violation("empty text"));
    }
    if text.len() > MAX_TEXT_BYTES {
        return Err(Error::TextTooLong { max_bytes: MAX_TEXT_BYTES as u64 });
    }
    Ok(())
}

/// Sends `text` to the receiver at the other end of `connection`, as device `local`, and waits until it is accepted.
///
/// The `Hello` announces [`PROTOCOL_VERSION`] (7, the first with `TextOffer`), so a receiver too old for text
/// refuses with [`Error::VersionMismatch`] instead of failing on a message it cannot read.
///
/// # Errors
///
/// - [`Error::TextTooLong`] or [`Error::ProtocolViolation`] if [`check_text`] fails; nothing is sent then
/// - [`Error::UntrustedDevice`], [`Error::FingerprintMismatch`], [`Error::VersionMismatch`] if the receiver refuses
/// - [`Error::ConnectionLost`] if the receiver disconnects or does not answer in time
pub async fn send_text<C: Connection>(connection: &C, local: &DeviceInfo, text: &str) -> Result<()> {
    check_text(text)?;
    let (send, mut replies) = connection.open_control().await?;
    let writer = ControlWriter::new(send);
    let hello =
        Message::Hello { protocol_version: PROTOCOL_VERSION, device_id: local.id, device_name: local.name.clone() };
    writer.send(&hello).await?;
    writer.send(&Message::TextOffer { text: text.to_owned() }).await?;
    let result = match read_frame(&mut replies).await? {
        Message::Done => Ok(()),
        Message::Reject { code } => Err(Error::from_peer(code)),
        Message::Error { code, detail } => Err(peer_error(code, detail.as_deref())),
        other => Err(Error::protocol_violation(format!("expected Done or Reject, got {other:?}"))),
    };
    writer.shutdown().await;
    if result.is_ok() {
        info!(bytes = text.len(), "text sent");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;

    #[test]
    fn text_length_is_checked_in_bytes() {
        assert!(check_text("a").is_ok());
        assert!(check_text(&"a".repeat(MAX_TEXT_BYTES)).is_ok());
        let error = check_text(&"a".repeat(MAX_TEXT_BYTES + 1)).unwrap_err();
        assert_eq!(error.code(), ErrorCode::TextTooLong { max_bytes: 65_536 });
        // "é" is 2 bytes: half as many characters fit.
        assert!(check_text(&"é".repeat(MAX_TEXT_BYTES / 2)).is_ok());
        assert!(check_text(&"é".repeat(MAX_TEXT_BYTES / 2 + 1)).is_err());
        assert_eq!(check_text("").unwrap_err().code(), ErrorCode::ProtocolViolation);
    }

    #[test]
    fn debug_leaves_the_text_out() {
        let text = ReceivedText {
            device_id: DeviceId::new_random(),
            device_name: "Desk".into(),
            fingerprint: Fingerprint::from_bytes([7; 32]),
            text: "hunter2".into(),
        };
        let debug = format!("{text:?}");
        assert!(!debug.contains("hunter2"), "{debug}");
        assert!(debug.contains("bytes: 7"), "{debug}");
    }
}
