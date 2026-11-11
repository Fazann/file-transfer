//! Network transports: QUIC (preferred) and TCP (fallback) behind one [`Connection`] trait, so sender and receiver
//! code does not care which one carries a transfer.
//!
//! A connection has one **control** channel for framed messages and any number of one-way **data** streams that
//! carry chunks from the sender to the receiver. Both transports use TLS 1.3 with a certificate on each side (see
//! `tls.rs`).

mod diagnosis;
mod quic;
mod tcp;
mod tls;

use std::{future::Future, net::SocketAddr, sync::Arc, time::Duration};

pub use diagnosis::{ConnectAttempt, ConnectDiagnosis, ConnectFailure, ConnectHint, ConnectReport};
pub use quic::QuicConnection;
pub use tcp::{TcpConnection, CONNECT_TIMEOUT, DEFAULT_PORT, PEER_ADDRESS_TIMEOUT};
pub use tls::PeerCheck;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{mpsc, Mutex, OwnedSemaphorePermit, Semaphore},
    task::JoinHandle,
    time::timeout,
};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::{
    error::{Error, ErrorCode, Result},
    identity::{DeviceCertificate, Fingerprint, Os},
    protocol::{read_message, write_message, Message},
};

/// How long to wait for a QUIC handshake before falling back to TCP.
pub const QUIC_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Limit for each step of setting up a connection: a TLS handshake, or one read of a hello or pairing message.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Most incoming connections a [`Listener`] keeps at once, waiting or being handled. Extra ones are answered with
/// [`ErrorCode::Busy`].
pub const MAX_INCOMING_CONNECTIONS: usize = 8;

/// Incoming connections waiting for [`Listener::accept`].
const ACCEPT_QUEUE: usize = 16;

/// How many random ports [`Listener::bind`] tries to find one that is free on both TCP and UDP.
const ANY_PORT_ATTEMPTS: usize = 10;

/// The sending half of a stream. `Box<dyn ...>` hides which transport it came from, like an interface value in Go.
pub type SendHalf = Box<dyn AsyncWrite + Send + Unpin>;

/// The receiving half of a stream.
pub type RecvHalf = Box<dyn AsyncRead + Send + Unpin>;

/// What the TLS handshake established about the other device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerSession {
    /// Address of the other device.
    pub address: SocketAddr,
    /// Fingerprint of the other device's certificate. The handshake proved it holds the matching private key.
    pub fingerprint: Fingerprint,
    /// A value both ends derive from this TLS session (RFC 5705 exporter). Someone relaying between two separate
    /// TLS sessions gets different values on each side, which is how pairing detects them.
    pub channel_binding: [u8; 32],
}

/// How the sender connects: transports to try, its certificate, and how to check the receiver's.
#[derive(Debug, Clone)]
pub struct ConnectOptions {
    /// Which transports to try.
    pub preference: TransportPreference,
    /// This device's certificate, shown to the receiver.
    pub certificate: DeviceCertificate,
    /// Whether the receiver must be trusted or this is a pairing connection.
    pub check: PeerCheck,
    /// The fingerprint discovery advertised for the receiver, if it was found that way. A different certificate
    /// fails with [`Error::FingerprintMismatch`].
    pub expected: Option<Fingerprint>,
    /// The receiver's operating system, if discovery knew it. Only used for the hint of a failed connection.
    pub peer_os: Option<Os>,
}

/// One connection between a sender and a receiver, over either transport.
///
/// The methods return futures that must be `Send`, like [`OfferDecider`](crate::transfer::OfferDecider), so
/// implementors can still write `async fn`.
pub trait Connection: Send + Sync {
    /// What the TLS handshake established about the other device.
    fn session(&self) -> &PeerSession;

    /// The control channel. The sender opens it; the receiver accepts it. Call once per connection.
    fn open_control(&self) -> impl Future<Output = Result<(SendHalf, RecvHalf)>> + Send;

    /// Sender: opens one more data stream for the transfer `transfer_id`.
    fn open_data(&self, transfer_id: Uuid) -> impl Future<Output = Result<SendHalf>> + Send;

    /// Receiver: waits for the sender's next data stream for the transfer `transfer_id`.
    ///
    /// The id lets the TCP listener hand each extra TCP connection to the right transfer.
    fn accept_data(&self, transfer_id: Uuid) -> impl Future<Output = Result<RecvHalf>> + Send;
}

/// Which transport a connection uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    /// TCP: one TCP connection for control plus one per data stream.
    Tcp,
    /// QUIC over UDP: one connection with multiple streams.
    Quic,
}

/// Which transports [`connect`] may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransportPreference {
    /// QUIC first, then TCP if QUIC does not connect within [`QUIC_CONNECT_TIMEOUT`].
    #[default]
    Auto,
    /// TCP only.
    Tcp,
    /// QUIC only.
    Quic,
}

/// A connection over whichever transport was available.
#[derive(Debug, Clone)]
pub enum AnyConnection {
    /// Over TCP.
    Tcp(TcpConnection),
    /// Over QUIC.
    Quic(QuicConnection),
}

impl AnyConnection {
    /// The transport in use.
    #[must_use]
    pub fn kind(&self) -> TransportKind {
        match self {
            Self::Tcp(_) => TransportKind::Tcp,
            Self::Quic(_) => TransportKind::Quic,
        }
    }

    /// Address of the other device.
    #[must_use]
    pub fn remote_addr(&self) -> SocketAddr {
        match self {
            Self::Tcp(connection) => connection.remote_addr(),
            Self::Quic(connection) => connection.remote_addr(),
        }
    }
}

impl Connection for AnyConnection {
    fn session(&self) -> &PeerSession {
        match self {
            Self::Tcp(connection) => connection.session(),
            Self::Quic(connection) => connection.session(),
        }
    }

    async fn open_control(&self) -> Result<(SendHalf, RecvHalf)> {
        match self {
            Self::Tcp(connection) => connection.open_control().await,
            Self::Quic(connection) => connection.open_control().await,
        }
    }

    async fn open_data(&self, transfer_id: Uuid) -> Result<SendHalf> {
        match self {
            Self::Tcp(connection) => connection.open_data(transfer_id).await,
            Self::Quic(connection) => connection.open_data(transfer_id).await,
        }
    }

    async fn accept_data(&self, transfer_id: Uuid) -> Result<RecvHalf> {
        match self {
            Self::Tcp(connection) => connection.accept_data(transfer_id).await,
            Self::Quic(connection) => connection.accept_data(transfer_id).await,
        }
    }
}

/// Connects to an address the user typed, giving TCP up to [`CONNECT_TIMEOUT`].
///
/// # Errors
///
/// - [`Error::UntrustedDevice`] or [`Error::FingerprintMismatch`] if the receiver's certificate fails the check
/// - [`Error::TransportUnavailable`] if no allowed transport connects, with each attempt in its [`ConnectReport`]
pub async fn connect(addr: SocketAddr, options: &ConnectOptions) -> Result<AnyConnection> {
    let mut report = ConnectReport { attempts: Vec::new(), peer_os: options.peer_os };
    match connect_with_timeout(addr, options, CONNECT_TIMEOUT, &mut report).await {
        Err(Error::TransportUnavailable { .. }) => Err(Error::TransportUnavailable { report }),
        result => result,
    }
}

/// Tries each address of a discovered peer in order and returns the first connection.
///
/// Use [`Peer::connect_order`](crate::discovery::Peer::connect_order) to get the addresses in the right order.
///
/// # Errors
///
/// - [`Error::UntrustedDevice`] or [`Error::FingerprintMismatch`] as soon as an address answers with a certificate
///   that fails the check
/// - [`Error::PeerUnreachable`] if no address accepts a connection (or `addrs` is empty), with every attempt in its
///   [`ConnectReport`]
pub async fn connect_any(addrs: &[SocketAddr], options: &ConnectOptions) -> Result<AnyConnection> {
    let mut report = ConnectReport { attempts: Vec::new(), peer_os: options.peer_os };
    for addr in addrs {
        match connect_with_timeout(*addr, options, PEER_ADDRESS_TIMEOUT, &mut report).await {
            Ok(connection) => return Ok(connection),
            Err(error) if is_certificate_refusal(&error) => return Err(error),
            Err(error) => debug!(%addr, %error, "address unreachable, trying the next one"),
        }
    }
    Err(Error::PeerUnreachable { report })
}

/// A refused certificate is an answer, not an unreachable address: trying another transport would not change it.
fn is_certificate_refusal(error: &Error) -> bool {
    matches!(error, Error::UntrustedDevice | Error::FingerprintMismatch)
}

/// Tries the allowed transports on `addr`, adding each failure to `report`. Fails with a certificate refusal, or
/// with an empty [`Error::TransportUnavailable`] whose attempts are in `report`.
async fn connect_with_timeout(
    addr: SocketAddr,
    options: &ConnectOptions,
    tcp_timeout: Duration,
    report: &mut ConnectReport,
) -> Result<AnyConnection> {
    let (certificate, check, expected) = (&options.certificate, &options.check, options.expected);
    if options.preference != TransportPreference::Tcp {
        match quic::connect(addr, QUIC_CONNECT_TIMEOUT, certificate, check, expected).await {
            Ok(connection) => {
                info!(%addr, "connected over QUIC");
                return Ok(AnyConnection::Quic(connection));
            }
            Err(error) if is_certificate_refusal(&error) => return Err(error),
            Err(error) => {
                debug!(%addr, %error, "QUIC did not connect");
                report.push(addr, TransportKind::Quic, &error);
            }
        }
    }
    if options.preference != TransportPreference::Quic {
        match tcp::connect(addr, tcp_timeout, certificate, check, expected).await {
            Ok(connection) => {
                info!(%addr, "connected over TCP");
                return Ok(AnyConnection::Tcp(connection));
            }
            Err(error) if is_certificate_refusal(&error) => return Err(error),
            Err(error) => {
                debug!(%addr, %error, "TCP did not connect");
                report.push(addr, TransportKind::Tcp, &error);
            }
        }
    }
    Err(Error::TransportUnavailable { report: ConnectReport::default() })
}

/// The receiver's places for incoming connections, shared by the TCP and QUIC accept loops.
#[derive(Debug, Clone)]
struct ConnectionSlots(Arc<Semaphore>);

impl ConnectionSlots {
    fn new() -> Self {
        Self(Arc::new(Semaphore::new(MAX_INCOMING_CONNECTIONS)))
    }

    /// A free place, held until the connection is dropped, or `None` if all are taken.
    fn take(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.0).try_acquire_owned().ok()
    }
}

/// Answers a connection over the limit: reads its first message, replies `Reject { Busy }`, and closes.
async fn refuse_busy(connection: AnyConnection) {
    info!(peer = %connection.session().address, "too many connections, refusing one as busy");
    let result: Result<()> = async {
        let (mut send, mut recv) = connection.open_control().await?;
        timeout(HANDSHAKE_TIMEOUT, read_message(&mut recv)).await.map_err(|_| Error::ConnectionLost)??;
        write_message(&mut send, &Message::Reject { code: ErrorCode::Busy }).await?;
        send.shutdown().await?;
        // Wait for the sender to close, so the reply is not cut off when the connection is dropped.
        let mut buffer = [0; 256];
        let _ =
            timeout(HANDSHAKE_TIMEOUT, async { while matches!(recv.read(&mut buffer).await, Ok(read) if read > 0) {} })
                .await;
        Ok(())
    }
    .await;
    if let Err(error) = result {
        debug!(%error, "could not tell a sender the receiver is busy");
    }
}

/// Accepts incoming connections over TCP and QUIC on the same port number.
///
/// Background tasks accept and sort connections even while a transfer is running, so a sender's reconnect and its
/// extra TCP data connections are never stuck in a queue. Dropping the listener stops them.
pub struct Listener {
    port: u16,
    quic: bool,
    incoming: Mutex<mpsc::Receiver<AnyConnection>>,
    tasks: Vec<JoinHandle<()>>,
}

impl Listener {
    /// Listens on `port` (TCP and UDP) on every IPv4 interface, presenting `certificate`. Port `0` picks a free port.
    ///
    /// If the UDP port cannot be bound, the listener runs with TCP only and senders fall back to TCP. At most
    /// [`MAX_INCOMING_CONNECTIONS`] connections are kept at once; extra ones are answered with
    /// [`ErrorCode::Busy`].
    ///
    /// # Errors
    ///
    /// The TCP bind error, for example when the port is already in use.
    pub async fn bind(port: u16, certificate: &DeviceCertificate) -> Result<Self> {
        // With port 0 the OS picks a free TCP port, but the same number may already be taken on UDP. Pick again a
        // few times before giving up on QUIC, so a random port gets both transports.
        let attempts = if port == 0 { ANY_PORT_ATTEMPTS } else { 1 };
        let mut last = None;
        for _ in 0..attempts {
            let tcp_listener = tcp::bind(port).await?;
            let port = tcp_listener.local_addr()?.port();
            match quic::server_endpoint(port, certificate) {
                Ok(endpoint) => return Ok(Self::start(port, tcp_listener, certificate, Some(endpoint))),
                Err(error) => last = Some((tcp_listener, port, error)),
            }
        }
        let (tcp_listener, port, error) =
            last.ok_or(Error::TransportUnavailable { report: ConnectReport::default() })?;
        warn!(port, %error, "QUIC is unavailable, accepting TCP only");
        Ok(Self::start(port, tcp_listener, certificate, None))
    }

    fn start(
        port: u16,
        tcp_listener: tokio::net::TcpListener,
        certificate: &DeviceCertificate,
        endpoint: Option<quinn::Endpoint>,
    ) -> Self {
        let (sender, receiver) = mpsc::channel(ACCEPT_QUEUE);
        let slots = ConnectionSlots::new();
        let tcp_task = tcp::accept_loop(tcp_listener, certificate.clone(), sender.clone(), slots.clone());
        let mut tasks = vec![tokio::spawn(tcp_task)];
        let quic = endpoint.is_some();
        if let Some(endpoint) = endpoint {
            tasks.push(tokio::spawn(quic::accept_loop(endpoint, sender, slots)));
        }
        Self { port, quic, incoming: Mutex::new(receiver), tasks }
    }

    /// The port in use (TCP, and UDP if QUIC is enabled).
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Whether QUIC connections are accepted too.
    #[must_use]
    pub fn quic_enabled(&self) -> bool {
        self.quic
    }

    /// Waits for the next incoming connection.
    ///
    /// # Errors
    ///
    /// [`Error::ConnectionLost`] if the background accept tasks stopped.
    pub async fn accept(&self) -> Result<AnyConnection> {
        self.incoming.lock().await.recv().await.ok_or(Error::ConnectionLost)
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
