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

use crate::error::{Error, Result};

/// Default port for transfers (TCP, and QUIC/UDP from phase 4).
pub const DEFAULT_PORT: u16 = 52000;

/// How long [`connect`] waits before giving up.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Opens a TCP connection to `addr`, giving up after [`CONNECT_TIMEOUT`].
///
/// # Errors
///
/// [`Error::Io`] with kind `TimedOut` on timeout, or the underlying connect error (for example connection refused).
pub async fn connect(addr: SocketAddr) -> Result<TcpStream> {
    // `timeout` wraps the connect result in another `Result`, so the first `?` handles the timeout and the second
    // handles the connect error itself.
    let stream = timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
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
