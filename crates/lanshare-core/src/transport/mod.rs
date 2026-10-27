//! Network transports: QUIC (preferred) and TCP (fallback) behind one [`Connection`] trait, so sender and receiver
//! code does not care which one carries a transfer.
//!
//! A connection has one **control** channel for framed messages and any number of one-way **data** streams that
//! carry chunks from the sender to the receiver.

mod quic;
mod tcp;
mod tls;

use std::{future::Future, net::SocketAddr, time::Duration};

pub use quic::QuicConnection;
pub use tcp::{TcpConnection, CONNECT_TIMEOUT, DEFAULT_PORT, PEER_ADDRESS_TIMEOUT};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{mpsc, Mutex},
    task::JoinHandle,
};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::error::{Error, Result};

/// How long to wait for a QUIC handshake before falling back to TCP.
pub const QUIC_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Incoming connections waiting for [`Listener::accept`].
const ACCEPT_QUEUE: usize = 16;

/// The sending half of a stream. `Box<dyn ...>` hides which transport it came from, like an interface value in Go.
pub type SendHalf = Box<dyn AsyncWrite + Send + Unpin>;

/// The receiving half of a stream.
pub type RecvHalf = Box<dyn AsyncRead + Send + Unpin>;

/// One connection between a sender and a receiver, over either transport.
///
/// The methods return futures that must be `Send`, like [`OfferDecider`](crate::transfer::OfferDecider), so
/// implementors can still write `async fn`.
pub trait Connection: Send + Sync {
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
/// [`Error::TransportUnavailable`] if no allowed transport connects.
pub async fn connect(addr: SocketAddr, preference: TransportPreference) -> Result<AnyConnection> {
    connect_with_timeout(addr, preference, CONNECT_TIMEOUT).await
}

/// Tries each address of a discovered peer in order and returns the first connection.
///
/// Use [`Peer::connect_order`](crate::discovery::Peer::connect_order) to get the addresses in the right order.
///
/// # Errors
///
/// [`Error::PeerUnreachable`] if no address accepts a connection (or `addrs` is empty).
pub async fn connect_any(addrs: &[SocketAddr], preference: TransportPreference) -> Result<AnyConnection> {
    for addr in addrs {
        match connect_with_timeout(*addr, preference, PEER_ADDRESS_TIMEOUT).await {
            Ok(connection) => return Ok(connection),
            Err(error) => debug!(%addr, %error, "address unreachable, trying the next one"),
        }
    }
    Err(Error::PeerUnreachable)
}

async fn connect_with_timeout(
    addr: SocketAddr,
    preference: TransportPreference,
    tcp_timeout: Duration,
) -> Result<AnyConnection> {
    if preference != TransportPreference::Tcp {
        match quic::connect(addr, QUIC_CONNECT_TIMEOUT).await {
            Ok(connection) => {
                info!(%addr, "connected over QUIC");
                return Ok(AnyConnection::Quic(connection));
            }
            Err(error) => debug!(%addr, %error, "QUIC did not connect"),
        }
    }
    if preference != TransportPreference::Quic {
        match tcp::connect(addr, tcp_timeout).await {
            Ok(connection) => {
                info!(%addr, "connected over TCP");
                return Ok(AnyConnection::Tcp(connection));
            }
            Err(error) => debug!(%addr, %error, "TCP did not connect"),
        }
    }
    Err(Error::TransportUnavailable)
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
    /// Listens on `port` (TCP and UDP) on every IPv4 interface. Port `0` picks a free port.
    ///
    /// If the UDP port cannot be bound, the listener runs with TCP only and senders fall back to TCP.
    ///
    /// # Errors
    ///
    /// The TCP bind error, for example when the port is already in use.
    pub async fn bind(port: u16) -> Result<Self> {
        let tcp_listener = tcp::bind(port).await?;
        let port = tcp_listener.local_addr()?.port();
        let (sender, receiver) = mpsc::channel(ACCEPT_QUEUE);
        let mut tasks = vec![tokio::spawn(tcp::accept_loop(tcp_listener, sender.clone()))];
        let quic = match quic::server_endpoint(port) {
            Ok(endpoint) => {
                tasks.push(tokio::spawn(quic::accept_loop(endpoint, sender)));
                true
            }
            Err(error) => {
                warn!(port, %error, "QUIC is unavailable, accepting TCP only");
                false
            }
        };
        Ok(Self { port, quic, incoming: Mutex::new(receiver), tasks })
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
