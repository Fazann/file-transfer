//! QUIC transport (UDP): one QUIC connection per transfer. The first bidirectional stream is the control stream and
//! every data stream is a unidirectional stream from the sender.

use std::{
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use quinn::{
    congestion::BbrConfig,
    crypto::rustls::{QuicClientConfig, QuicServerConfig},
    ClientConfig, Endpoint, EndpointConfig, IdleTimeout, ServerConfig, TokioRuntime, TransportConfig, VarInt,
};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::sync::mpsc;
use tracing::{debug, info};
use uuid::Uuid;

use super::{tls, AnyConnection, Connection, RecvHalf, SendHalf};
use crate::error::{Error, Result};

/// Application protocol name sent in the TLS handshake.
const ALPN: &[u8] = b"lanshare";

/// Name the client asks for. The development certificate check ignores it; phase 5 checks fingerprints instead.
const SERVER_NAME: &str = "lanshare";

/// A QUIC connection with no packets for this long is closed. Keep-alives keep healthy connections busy.
const IDLE_TIMEOUT: Duration = Duration::from_secs(20);

/// How often to send a keep-alive packet when nothing else is sent, for example while a transfer is paused.
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(5);

/// Receive window per stream (32 MiB). Large windows keep fast links busy despite round-trip delay.
const STREAM_RECEIVE_WINDOW: u32 = 32 * 1024 * 1024;

/// Receive and send window for the whole connection (64 MiB).
const CONNECTION_WINDOW: u32 = 64 * 1024 * 1024;

/// Kernel send and receive buffer for the UDP socket (8 MiB).
const UDP_BUFFER_SIZE: usize = 8 * 1024 * 1024;

/// One QUIC connection, from either side.
///
/// `quinn::Connection` is a cheap handle (like an `Arc`); cloning it does not open anything new.
#[derive(Debug, Clone)]
pub struct QuicConnection {
    connection: quinn::Connection,
    is_client: bool,
    /// The client keeps its own UDP endpoint alive for as long as the connection is used.
    _endpoint: Option<Endpoint>,
}

impl QuicConnection {
    /// Address of the other device.
    #[must_use]
    pub fn remote_addr(&self) -> SocketAddr {
        self.connection.remote_address()
    }
}

impl Drop for QuicConnection {
    fn drop(&mut self) {
        // Useful when tuning: run with `RUST_LOG=lanshare_core=debug` to see loss and packet size per connection.
        let path = self.connection.stats().path;
        let (sent, lost, mtu) = (path.sent_packets, path.lost_packets, path.current_mtu);
        debug!(sent, lost, mtu, rtt = ?path.rtt, "QUIC stats");
    }
}

impl Connection for QuicConnection {
    async fn open_control(&self) -> Result<(SendHalf, RecvHalf)> {
        // The sender opens the stream; the receiver accepts it. It reaches the receiver with the first `Hello`.
        let (send, recv) = if self.is_client {
            self.connection.open_bi().await.map_err(|error| connection_lost(&error))?
        } else {
            self.connection.accept_bi().await.map_err(|error| connection_lost(&error))?
        };
        Ok((Box::new(send), Box::new(recv)))
    }

    async fn open_data(&self, _transfer_id: Uuid) -> Result<SendHalf> {
        // A QUIC stream already belongs to its connection, so it needs no `DataStreamHello`.
        Ok(Box::new(self.connection.open_uni().await.map_err(|error| connection_lost(&error))?))
    }

    async fn accept_data(&self, _transfer_id: Uuid) -> Result<RecvHalf> {
        Ok(Box::new(self.connection.accept_uni().await.map_err(|error| connection_lost(&error))?))
    }
}

fn connection_lost(error: &quinn::ConnectionError) -> Error {
    debug!(%error, "QUIC connection ended");
    Error::ConnectionLost
}

/// Connects to `addr`, giving up after `timeout`.
pub(super) async fn connect(addr: SocketAddr, timeout: Duration) -> Result<QuicConnection> {
    let mut crypto = tls::dev_insecure_client_config()?;
    crypto.alpn_protocols = vec![ALPN.to_vec()];
    let crypto = QuicClientConfig::try_from(crypto).map_err(io_error)?;
    let mut config = ClientConfig::new(Arc::new(crypto));
    config.transport_config(Arc::new(transport_config()?));

    let local: SocketAddr =
        if addr.is_ipv4() { (Ipv4Addr::UNSPECIFIED, 0).into() } else { (Ipv6Addr::UNSPECIFIED, 0).into() };
    let mut endpoint = endpoint(local, None)?;
    endpoint.set_default_client_config(config);
    let connecting = endpoint.connect(addr, SERVER_NAME).map_err(io_error)?;
    let connection = tokio::time::timeout(timeout, connecting)
        .await
        .map_err(|_elapsed| Error::Io(io::Error::from(io::ErrorKind::TimedOut)))?
        .map_err(io_error)?;
    Ok(QuicConnection { connection, is_client: true, _endpoint: Some(endpoint) })
}

/// Binds the receiving UDP endpoint on every IPv4 interface.
pub(super) fn server_endpoint(port: u16) -> Result<Endpoint> {
    let mut crypto = tls::dev_server_config()?;
    crypto.alpn_protocols = vec![ALPN.to_vec()];
    let crypto = QuicServerConfig::try_from(crypto).map_err(io_error)?;
    let mut config = ServerConfig::with_crypto(Arc::new(crypto));
    config.transport_config(Arc::new(transport_config()?));
    endpoint((Ipv4Addr::UNSPECIFIED, port).into(), Some(config))
}

/// Binds a UDP socket with large kernel buffers and wraps it in a QUIC endpoint.
///
/// The default UDP buffers (a few hundred KiB on macOS) overflow at gigabit rates; every overflow is a lost packet
/// that makes congestion control slow down.
fn endpoint(local: SocketAddr, server: Option<ServerConfig>) -> Result<Endpoint> {
    let socket = Socket::new(Domain::for_address(local), Type::DGRAM, Some(Protocol::UDP))?;
    // Best effort: the OS may cap the size, which only costs speed.
    let _ = socket.set_recv_buffer_size(UDP_BUFFER_SIZE);
    let _ = socket.set_send_buffer_size(UDP_BUFFER_SIZE);
    socket.bind(&local.into())?;
    Endpoint::new(EndpointConfig::default(), server, socket.into(), Arc::new(TokioRuntime)).map_err(Error::Io)
}

/// Completes handshakes for incoming connections and hands them to `connections`, until the endpoint closes.
///
/// Each handshake runs in its own task, so a slow client cannot hold up others.
pub(super) async fn accept_loop(endpoint: Endpoint, connections: mpsc::Sender<AnyConnection>) {
    while let Some(incoming) = endpoint.accept().await {
        let connections = connections.clone();
        tokio::spawn(async move {
            match incoming.await {
                Ok(connection) => {
                    info!(peer = %connection.remote_address(), "incoming QUIC connection");
                    let connection = QuicConnection { connection, is_client: false, _endpoint: None };
                    // Fails only if the listener was dropped, and then nobody wants the connection.
                    let _ = connections.send(AnyConnection::Quic(connection)).await;
                }
                Err(error) => debug!(%error, "QUIC handshake failed"),
            }
        });
    }
}

/// Settings shared by both sides: large windows, BBR congestion control, idle timeout and keep-alives.
fn transport_config() -> Result<TransportConfig> {
    let mut config = TransportConfig::default();
    config
        .stream_receive_window(VarInt::from_u32(STREAM_RECEIVE_WINDOW))
        .receive_window(VarInt::from_u32(CONNECTION_WINDOW))
        .send_window(u64::from(CONNECTION_WINDOW))
        // BBR estimates the available bandwidth instead of backing off on every lost packet, which suits Wi-Fi.
        .congestion_controller_factory(Arc::new(BbrConfig::default()))
        .max_idle_timeout(Some(IdleTimeout::try_from(IDLE_TIMEOUT).map_err(io_error)?))
        .keep_alive_interval(Some(KEEP_ALIVE_INTERVAL));
    Ok(config)
}

fn io_error(error: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::Io(io::Error::other(error))
}
