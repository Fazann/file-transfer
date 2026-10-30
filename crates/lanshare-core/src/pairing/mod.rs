//! PIN pairing: two devices that do not trust each other yet prove they see the same 6-digit PIN, then add each other
//! to their trust stores.
//!
//! The PIN never travels over the network. Both sides run SPAKE2 (a password-authenticated key exchange) with the PIN,
//! bound to the TLS session's channel binding value, and prove to each other that they derived the same key. Someone
//! who does not know the PIN gets one guess per attempt, and an attacker relaying between two TLS sessions derives
//! different keys on each side, so pairing fails.
//!
//! On a pairing connection (sender = the device that started pairing):
//!
//! ```text
//! sender                                   receiver
//!   Hello, PairRequest  ------------------>  shows a new PIN
//!                       <------------------  Hello (its own id and name)
//!   user types the PIN
//!   PairMessage         ------------------>
//!                       <------------------  PairMessage
//!   PairConfirm         ------------------>  checks it (wrong PIN: PairResult { ok: false })
//!                       <------------------  PairConfirm
//!   checks it, PairResult { ok: true } --->  trusts the sender
//!                       <------------------  PairResult { ok: true }
//!   trusts the receiver
//! ```

mod lockout;
mod pin;

use std::{future::Future, time::Duration};

pub use lockout::{PairingLockout, LOCK_DURATION, MAX_FAILURES, WINDOW};
pub use pin::{generate_pin, normalize_pin, PIN_LENGTH};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use tokio::{io::AsyncRead, time::timeout};
use tracing::{debug, info, warn};

use crate::{
    error::{Error, Result},
    identity::{DeviceId, DeviceInfo, Fingerprint, TrustStore, TrustedDevice},
    protocol::{read_message, Message, PROTOCOL_VERSION},
    transfer::ControlWriter,
    transport::{Connection, PeerSession, HANDSHAKE_TIMEOUT},
};

/// How long the receiver waits for the sender's user to type the PIN.
pub const PIN_ENTRY_TIMEOUT: Duration = Duration::from_secs(120);

/// Mixed into the SPAKE2 identity and the confirmation key, so these values are never reused for anything else.
const CONTEXT: &str = "lanshare pairing v1";

/// The other device in a pairing, as shown to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingPeer {
    /// Its stable device id, as it reported it.
    pub device_id: DeviceId,
    /// Its name, as it reported it.
    pub device_name: String,
    /// Its certificate fingerprint, proven by TLS.
    pub fingerprint: Fingerprint,
}

/// Asks the user of the sending device for the PIN the receiver shows.
pub trait PinPrompt: Send + Sync {
    /// Returns the PIN as typed, or `None` if the user cancelled.
    fn ask_pin(&self, receiver: &PairingPeer) -> impl Future<Output = Option<String>> + Send;
}

/// Pairs with the receiver at the other end of `connection`, which must have been opened with
/// [`PeerCheck::Pairing`](crate::transport::PeerCheck::Pairing). On success the receiver is added to `trust` and
/// returned.
///
/// # Errors
///
/// - [`Error::PairingFailed`] if the PIN was wrong or the receiver could not confirm it
/// - [`Error::PairingLocked`] if the receiver refuses pairing after too many failures
/// - [`Error::Cancelled`] if `prompt` returned `None`
/// - [`Error::ConnectionLost`] if the receiver disconnects or does not answer within
///   [`HANDSHAKE_TIMEOUT`]
pub async fn pair<C, P>(connection: &C, local: &DeviceInfo, trust: &TrustStore, prompt: &P) -> Result<TrustedDevice>
where
    C: Connection,
    P: PinPrompt,
{
    let (send, mut recv) = connection.open_control().await?;
    let writer = ControlWriter::new(send);
    let hello =
        Message::Hello { protocol_version: PROTOCOL_VERSION, device_id: local.id, device_name: local.name.clone() };
    writer.send(&hello).await?;
    writer.send(&Message::PairRequest).await?;
    let receiver = read_receiver_hello(&mut recv, connection.session()).await?;

    let Some(pin) = prompt.ask_pin(&receiver).await.as_deref().and_then(normalize_pin) else {
        let _ = writer.send(&Message::PairResult { ok: false }).await;
        writer.shutdown().await;
        return Err(Error::Cancelled);
    };
    let keys = Keys { sender: local.fingerprint, receiver: receiver.fingerprint };
    let result = confirm_as_sender(&mut recv, &writer, connection.session(), &pin, keys).await;
    writer.shutdown().await;
    result?;

    let device = TrustedDevice::paired_now(receiver.device_id, receiver.device_name, receiver.fingerprint);
    trust.add(device.clone()).await?;
    info!(name = %device.name, fingerprint = %device.fingerprint, "paired");
    Ok(device)
}

/// The fingerprints both sides put into their confirmations.
#[derive(Debug, Clone, Copy)]
struct Keys {
    sender: Fingerprint,
    receiver: Fingerprint,
}

async fn read_receiver_hello<R: AsyncRead + Unpin>(recv: &mut R, session: &PeerSession) -> Result<PairingPeer> {
    match read(recv, HANDSHAKE_TIMEOUT).await? {
        Message::Hello { protocol_version, device_id, device_name } if protocol_version == PROTOCOL_VERSION => {
            Ok(PairingPeer { device_id, device_name, fingerprint: session.fingerprint })
        }
        Message::Hello { protocol_version, .. } => {
            Err(Error::VersionMismatch { ours: PROTOCOL_VERSION, theirs: protocol_version })
        }
        Message::Reject { code } => Err(Error::from_peer(code)),
        other => Err(Error::protocol_violation(format!("expected Hello, got {other:?}"))),
    }
}

/// The sender's half of the key exchange and confirmation, ending with both `PairResult { ok: true }`.
async fn confirm_as_sender<R: AsyncRead + Unpin>(
    recv: &mut R,
    writer: &ControlWriter,
    session: &PeerSession,
    pin: &str,
    keys: Keys,
) -> Result<()> {
    let (state, outbound) = start_exchange(pin, session);
    writer.send(&Message::PairMessage { data: outbound }).await?;
    let inbound = match read(recv, HANDSHAKE_TIMEOUT).await? {
        Message::PairMessage { data } => data,
        other => return Err(unexpected(other)),
    };
    let key = state.finish(&inbound).map_err(|_| Error::PairingFailed)?;
    writer.send(&Message::PairConfirm { mac: confirmation(&key, Role::Sender, keys) }).await?;

    let theirs = match read(recv, HANDSHAKE_TIMEOUT).await? {
        Message::PairConfirm { mac } => mac,
        other => return Err(unexpected(other)),
    };
    if !macs_equal(&theirs, &confirmation(&key, Role::Receiver, keys)) {
        warn!("the receiver's pairing confirmation does not match");
        let _ = writer.send(&Message::PairResult { ok: false }).await;
        return Err(Error::PairingFailed);
    }
    writer.send(&Message::PairResult { ok: true }).await?;
    match read(recv, HANDSHAKE_TIMEOUT).await? {
        Message::PairResult { ok: true } => Ok(()),
        other => Err(unexpected(other)),
    }
}

/// What the receiver needs to answer a `PairRequest`.
pub(crate) struct PairingAnswer<'a> {
    /// This device, to introduce itself with its own `Hello`.
    pub(crate) local: &'a DeviceInfo,
    /// Where the sender is added on success.
    pub(crate) trust: &'a TrustStore,
    /// Failed attempts per peer address.
    pub(crate) lockout: &'a PairingLockout,
    /// The sender, from its `Hello` and the TLS session.
    pub(crate) sender: PairingPeer,
}

impl PairingAnswer<'_> {
    /// The receiver's side of pairing, after `Hello` and `PairRequest` were read. `show_pin` shows the PIN to the
    /// user. On success the sender is added to the trust store and returned.
    pub(crate) async fn run<R: AsyncRead + Unpin>(
        &self,
        recv: &mut R,
        writer: &ControlWriter,
        session: &PeerSession,
        show_pin: impl FnOnce(&PairingPeer, &str),
    ) -> Result<TrustedDevice> {
        let address = session.address.ip();
        if let Err(retry_after_secs) = self.lockout.check(address, std::time::Instant::now()) {
            info!(%address, retry_after_secs, "pairing locked for this address");
            let error = Error::PairingLocked { retry_after_secs };
            writer.send(&Message::Reject { code: error.code() }).await?;
            return Err(error);
        }
        let local = self.local;
        let hello =
            Message::Hello { protocol_version: PROTOCOL_VERSION, device_id: local.id, device_name: local.name.clone() };
        writer.send(&hello).await?;
        let pin = generate_pin()?;
        show_pin(&self.sender, &pin);

        let keys = Keys { sender: self.sender.fingerprint, receiver: local.fingerprint };
        match confirm_as_receiver(recv, writer, session, &pin, keys).await {
            Ok(()) => {
                self.lockout.record_success(address);
                let sender = &self.sender;
                let device =
                    TrustedDevice::paired_now(sender.device_id, sender.device_name.clone(), sender.fingerprint);
                self.trust.add(device.clone()).await?;
                writer.send(&Message::PairResult { ok: true }).await?;
                info!(name = %device.name, fingerprint = %device.fingerprint, "paired");
                Ok(device)
            }
            Err(error) => {
                // Every attempt that saw a PIN and did not succeed counts, whatever the reason.
                debug!(%address, %error, "pairing attempt failed");
                self.lockout.record_failure(address, std::time::Instant::now());
                Err(error)
            }
        }
    }
}

/// The receiver's half of the key exchange and confirmation, up to the sender's `PairResult { ok: true }`.
async fn confirm_as_receiver<R: AsyncRead + Unpin>(
    recv: &mut R,
    writer: &ControlWriter,
    session: &PeerSession,
    pin: &str,
    keys: Keys,
) -> Result<()> {
    // The only read that waits for a person: the sender's user reads the PIN here and types it there.
    let inbound = match read(recv, PIN_ENTRY_TIMEOUT).await? {
        Message::PairMessage { data } => data,
        other => return Err(unexpected(other)),
    };
    let (state, outbound) = start_exchange(pin, session);
    writer.send(&Message::PairMessage { data: outbound }).await?;
    let key = state.finish(&inbound).map_err(|_| Error::PairingFailed)?;

    let theirs = match read(recv, HANDSHAKE_TIMEOUT).await? {
        Message::PairConfirm { mac } => mac,
        other => return Err(unexpected(other)),
    };
    if !macs_equal(&theirs, &confirmation(&key, Role::Sender, keys)) {
        info!("wrong pairing PIN");
        writer.send(&Message::PairResult { ok: false }).await?;
        return Err(Error::PairingFailed);
    }
    writer.send(&Message::PairConfirm { mac: confirmation(&key, Role::Receiver, keys) }).await?;
    match read(recv, HANDSHAKE_TIMEOUT).await? {
        Message::PairResult { ok: true } => Ok(()),
        other => Err(unexpected(other)),
    }
}

/// Starts SPAKE2 with the PIN. Both sides use the symmetric mode with the same identity, which includes the TLS
/// channel binding: if the two TLS sessions differ, so do the keys.
fn start_exchange(pin: &str, session: &PeerSession) -> (Spake2<Ed25519Group>, Vec<u8>) {
    let mut identity = CONTEXT.as_bytes().to_vec();
    identity.extend_from_slice(&session.channel_binding);
    Spake2::<Ed25519Group>::start_symmetric(&Password::new(pin.as_bytes()), &Identity::new(&identity))
}

#[derive(Debug, Clone, Copy)]
enum Role {
    Sender,
    Receiver,
}

/// The `PairConfirm` value for `role`: keyed BLAKE3 over the role and both fingerprints, so a confirmation cannot be
/// reflected back or reused for other devices.
fn confirmation(key: &[u8], role: Role, keys: Keys) -> [u8; 32] {
    let mac_key = blake3::derive_key(CONTEXT, key);
    let mut hasher = blake3::Hasher::new_keyed(&mac_key);
    hasher.update(match role {
        Role::Sender => b"sender",
        Role::Receiver => b"receiver",
    });
    hasher.update(keys.sender.as_bytes());
    hasher.update(keys.receiver.as_bytes());
    *hasher.finalize().as_bytes()
}

/// Compares in constant time (`blake3::Hash` equality does), so timing reveals nothing about a wrong guess.
fn macs_equal(left: &[u8; 32], right: &[u8; 32]) -> bool {
    blake3::Hash::from(*left) == blake3::Hash::from(*right)
}

/// Reads one message, failing with [`Error::ConnectionLost`] after `limit`.
async fn read<R: AsyncRead + Unpin>(recv: &mut R, limit: Duration) -> Result<Message> {
    timeout(limit, read_message(recv)).await.map_err(|_elapsed| Error::ConnectionLost)?
}

/// The error for a message that does not belong at this step. A refusal from the other side is a failed pairing.
fn unexpected(message: Message) -> Error {
    match message {
        Message::PairResult { ok: false } => Error::PairingFailed,
        Message::Reject { code } | Message::Error { code, .. } => Error::from_peer(code),
        other => Error::protocol_violation(format!("unexpected message during pairing: {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> Keys {
        Keys { sender: Fingerprint::from_bytes([1; 32]), receiver: Fingerprint::from_bytes([2; 32]) }
    }

    fn session(binding: u8) -> PeerSession {
        PeerSession {
            address: ([127, 0, 0, 1], 52000).into(),
            fingerprint: Fingerprint::from_bytes([3; 32]),
            channel_binding: [binding; 32],
        }
    }

    fn exchange(sender_pin: &str, receiver_pin: &str, sender_binding: u8, receiver_binding: u8) -> (Vec<u8>, Vec<u8>) {
        let (sender, sender_message) = start_exchange(sender_pin, &session(sender_binding));
        let (receiver, receiver_message) = start_exchange(receiver_pin, &session(receiver_binding));
        (sender.finish(&receiver_message).unwrap(), receiver.finish(&sender_message).unwrap())
    }

    #[test]
    fn same_pin_and_session_give_the_same_key() {
        let (sender_key, receiver_key) = exchange("123456", "123456", 9, 9);
        assert_eq!(sender_key, receiver_key);
        let mac = confirmation(&sender_key, Role::Sender, keys());
        assert!(macs_equal(&mac, &confirmation(&receiver_key, Role::Sender, keys())));
        assert!(!macs_equal(&mac, &confirmation(&receiver_key, Role::Receiver, keys())), "roles differ");
    }

    #[test]
    fn wrong_pin_gives_different_keys() {
        let (sender_key, receiver_key) = exchange("123456", "123457", 9, 9);
        assert_ne!(sender_key, receiver_key);
    }

    #[test]
    fn relayed_sessions_give_different_keys() {
        // Same PIN, but each side sees a different TLS session, as with someone relaying in between.
        let (sender_key, receiver_key) = exchange("123456", "123456", 9, 10);
        assert_ne!(sender_key, receiver_key);
    }
}
