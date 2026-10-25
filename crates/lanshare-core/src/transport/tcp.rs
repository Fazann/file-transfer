//! Plain TCP transport: connecting with a timeout and listening for incoming transfers.

use std::{
    io,
    net::{Ipv4Addr, SocketAddr},
    time::Duration,
};

use tokio::{
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tracing::debug;

use crate::error::{Error, Result};

/// Default port for transfers (TCP, and QUIC/UDP from phase 4).
pub const DEFAULT_PORT: u16 = 52000;

/// How long [`connect`] waits before giving up.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long [`connect_any`] waits for each address of a discovered peer.
pub const PEER_ADDRESS_TIMEOUT: Duration = Duration::from_secs(3);

/// Opens a TCP connection to `addr`, giving up after [`CONNECT_TIMEOUT`].
///
/// # Errors
///
/// [`Error::Io`] with kind `TimedOut` on timeout, or the underlying connect error (for example connection refused).
pub async fn connect(addr: SocketAddr) -> Result<TcpStream> {
    connect_with_timeout(addr, CONNECT_TIMEOUT).await
}

/// Tries each address in order, waiting up to `timeout_each` for each, and returns the first connection.
///
/// Use [`Peer::connect_order`](crate::discovery::Peer::connect_order) to get the addresses in the right order.
///
/// # Errors
///
/// [`Error::PeerUnreachable`] if no address accepts a connection (or `addrs` is empty).
pub async fn connect_any(addrs: &[SocketAddr], timeout_each: Duration) -> Result<TcpStream> {
    for addr in addrs {
        match connect_with_timeout(*addr, timeout_each).await {
            Ok(stream) => return Ok(stream),
            Err(error) => debug!(%addr, %error, "address unreachable, trying the next one"),
        }
    }
    Err(Error::PeerUnreachable)
}

async fn connect_with_timeout(addr: SocketAddr, timeout_each: Duration) -> Result<TcpStream> {
    // `timeout` wraps the connect result in another `Result`, so the first `?` handles the timeout and the second
    // handles the connect error itself.
    let stream = timeout(timeout_each, TcpStream::connect(addr))
        .await
        .map_err(|_elapsed| Error::Io(io::Error::from(io::ErrorKind::TimedOut)))??;
    // We send small frames followed by bulk data; do not let Nagle's algorithm delay the frames.
    stream.set_nodelay(true)?;
    Ok(stream)
}

/// Listens for incoming connections on `port` on every IPv4 interface. Port `0` picks a free port.
///
/// # Errors
///
/// The bind error, for example when the port is already in use.
pub async fn listen(port: u16) -> Result<TcpListener> {
    Ok(TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).await?)
}
