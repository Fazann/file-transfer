//! TCP transport, the fallback when QUIC cannot connect. The first TCP connection is the control channel; each data
//! stream is an extra TCP connection that starts with a `DataStreamHello { transfer_id }` frame.

use std::{
    collections::HashMap,
    future::Future,
    io::{self, Cursor},
    net::{Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::Duration,
};

use socket2::{SockRef, TcpKeepalive};
use tokio::{
    io::AsyncReadExt,
    net::{TcpListener, TcpStream},
    sync::mpsc,
    time::timeout,
};
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::{AnyConnection, Connection, RecvHalf, SendHalf};
use crate::{
    error::{Error, Result},
    protocol::{decode_frame, read_raw_frame, write_message, Message},
};

/// Default port for transfers (TCP, and QUIC over UDP).
pub const DEFAULT_PORT: u16 = 52000;

/// How long connecting to an address typed by the user may take.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long connecting to each address of a discovered peer may take.
pub const PEER_ADDRESS_TIMEOUT: Duration = Duration::from_secs(3);

/// How long a new incoming connection may take to send its first frame.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(10);

/// Most transfer ids with data connections waiting to be claimed. Extra unknown data connections are closed.
const MAX_PENDING_TRANSFERS: usize = 64;

/// Most unclaimed data connections kept per transfer.
const MAX_PENDING_STREAMS: usize = 16;

/// One TCP "connection" in the [`Connection`] sense: the control socket plus the data sockets that join it.
///
/// Cloning shares the same sockets.
#[derive(Clone)]
pub struct TcpConnection {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for TcpConnection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("TcpConnection").field("remote", &self.inner.remote).finish_non_exhaustive()
    }
}

struct Inner {
    remote: SocketAddr,
    role: Role,
    /// Taken by the first `open_control` call.
    control: Mutex<Option<(SendHalf, RecvHalf)>>,
}

enum Role {
    /// The sender, which opens data connections to `remote`.
    Client,
    /// The receiver, which collects data connections from the listener's mailbox.
    Server { mailbox: Mailbox, claimed: Mutex<Vec<Uuid>> },
}

impl TcpConnection {
    /// Address of the other device.
    #[must_use]
    pub fn remote_addr(&self) -> SocketAddr {
        self.inner.remote
    }

    fn new(remote: SocketAddr, role: Role, control: (SendHalf, RecvHalf)) -> Self {
        Self { inner: Arc::new(Inner { remote, role, control: Mutex::new(Some(control)) }) }
    }
}

impl Connection for TcpConnection {
    fn open_control(&self) -> impl Future<Output = Result<(SendHalf, RecvHalf)>> + Send {
        // The control socket already exists, so the answer is ready at once.
        let control = self.inner.control.lock().map_or(None, |mut control| control.take());
        std::future::ready(
            control.ok_or_else(|| Error::protocol_violation("the TCP control channel was already taken")),
        )
    }

    async fn open_data(&self, transfer_id: Uuid) -> Result<SendHalf> {
        if !matches!(self.inner.role, Role::Client) {
            return Err(Error::protocol_violation("only the sender opens data streams"));
        }
        let mut stream = connect_stream(self.inner.remote, CONNECT_TIMEOUT).await?;
        write_message(&mut stream, &Message::DataStreamHello { transfer_id }).await?;
        Ok(Box::new(stream))
    }

    async fn accept_data(&self, transfer_id: Uuid) -> Result<RecvHalf> {
        let Role::Server { mailbox, claimed } = &self.inner.role else {
            return Err(Error::protocol_violation("only the receiver accepts data streams"));
        };
        if let Ok(mut claimed) = claimed.lock() {
            if !claimed.contains(&transfer_id) {
                claimed.push(transfer_id);
            }
        }
        let stream = mailbox.receiver(transfer_id).recv().await.map_err(|_closed| Error::ConnectionLost)?;
        Ok(Box::new(stream))
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Forget the mailbox slots of this connection's transfers, closing any data sockets nobody picked up.
        if let Role::Server { mailbox, claimed } = &self.role {
            if let Ok(claimed) = claimed.lock() {
                for transfer_id in claimed.iter() {
                    mailbox.remove(*transfer_id);
                }
            }
        }
    }
}

/// Opens the control connection to `addr`.
pub(super) async fn connect(addr: SocketAddr, connect_timeout: Duration) -> Result<TcpConnection> {
    let stream = connect_stream(addr, connect_timeout).await?;
    let (read, write) = stream.into_split();
    Ok(TcpConnection::new(addr, Role::Client, (Box::new(write), Box::new(read))))
}

async fn connect_stream(addr: SocketAddr, connect_timeout: Duration) -> Result<TcpStream> {
    // `timeout` wraps the connect result in another `Result`, so the first `?` handles the timeout and the second
    // handles the connect error itself.
    let stream = timeout(connect_timeout, TcpStream::connect(addr))
        .await
        .map_err(|_elapsed| Error::Io(io::Error::from(io::ErrorKind::TimedOut)))??;
    configure(&stream)?;
    Ok(stream)
}

/// Turns off Nagle's algorithm (frames should not wait for more data) and turns on keep-alive probes, so a peer
/// that vanished is noticed within about 15 seconds even while the connection is quiet, for example when paused.
fn configure(stream: &TcpStream) -> Result<()> {
    stream.set_nodelay(true)?;
    let keepalive = TcpKeepalive::new().with_time(Duration::from_secs(5)).with_interval(Duration::from_secs(2));
    #[cfg(not(windows))]
    let keepalive = keepalive.with_retries(4);
    SockRef::from(stream).set_tcp_keepalive(&keepalive)?;
    Ok(())
}

/// Binds the receiving TCP socket on every IPv4 interface. Port `0` picks a free port.
pub(super) async fn bind(port: u16) -> Result<TcpListener> {
    Ok(TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).await?)
}

/// Accepts TCP connections until the listener is dropped. Control connections go to `connections`; data
/// connections go to the mailbox of their transfer.
pub(super) async fn accept_loop(listener: TcpListener, connections: mpsc::Sender<AnyConnection>) {
    let mailbox = Mailbox::default();
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                warn!(%error, "could not accept a TCP connection");
                continue;
            }
        };
        let (mailbox, connections) = (mailbox.clone(), connections.clone());
        // Reading the first frame waits on the peer, so it runs in its own task.
        tokio::spawn(async move {
            if let Err(error) = sort_connection(stream, peer, mailbox, connections).await {
                debug!(%peer, %error, "dropping TCP connection");
            }
        });
    }
}

/// Reads the first frame of a new connection and decides whether it is a data stream or a new control channel.
async fn sort_connection(
    mut stream: TcpStream,
    peer: SocketAddr,
    mailbox: Mailbox,
    connections: mpsc::Sender<AnyConnection>,
) -> Result<()> {
    configure(&stream)?;
    let frame =
        timeout(FIRST_FRAME_TIMEOUT, read_raw_frame(&mut stream)).await.map_err(|_| Error::ConnectionLost)??;
    if let Ok(Message::DataStreamHello { transfer_id }) = decode_frame(&frame) {
        mailbox.deliver(transfer_id, stream);
        return Ok(());
    }
    info!(%peer, "incoming TCP connection");
    // The receiver still has to read the first frame (normally `Hello`), so it goes back in front of the stream.
    let (read, write) = stream.into_split();
    let read: RecvHalf = Box::new(Cursor::new(frame).chain(read));
    let role = Role::Server { mailbox, claimed: Mutex::new(Vec::new()) };
    let _ = connections.send(AnyConnection::Tcp(TcpConnection::new(peer, role, (Box::new(write), read)))).await;
    Ok(())
}

/// Where data connections wait until the receiver of their transfer picks them up.
///
/// `async_channel` channels can be cloned on both ends, so either side may create a transfer's slot first.
#[derive(Debug, Clone, Default)]
struct Mailbox {
    slots: Arc<Mutex<HashMap<Uuid, Slot>>>,
}

type Slot = (async_channel::Sender<TcpStream>, async_channel::Receiver<TcpStream>);

impl Mailbox {
    fn deliver(&self, transfer_id: Uuid, stream: TcpStream) {
        let Ok(mut slots) = self.slots.lock() else { return };
        if !slots.contains_key(&transfer_id) && slots.len() >= MAX_PENDING_TRANSFERS {
            warn!(%transfer_id, "too many unclaimed data connections, closing this one");
            return;
        }
        let (sender, _) = slots.entry(transfer_id).or_insert_with(|| async_channel::bounded(MAX_PENDING_STREAMS));
        if sender.try_send(stream).is_err() {
            warn!(%transfer_id, "too many data connections for one transfer, closing this one");
        }
    }

    fn receiver(&self, transfer_id: Uuid) -> async_channel::Receiver<TcpStream> {
        let mut slots = self.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (_, receiver) = slots.entry(transfer_id).or_insert_with(|| async_channel::bounded(MAX_PENDING_STREAMS));
        receiver.clone()
    }

    fn remove(&self, transfer_id: Uuid) {
        if let Ok(mut slots) = self.slots.lock() {
            slots.remove(&transfer_id);
        }
    }
}
