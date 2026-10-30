//! TCP transport, the fallback when QUIC cannot connect. The first TCP connection is the control channel; each data
//! stream is an extra TCP connection that starts with a `DataStreamHello { transfer_id }` frame.
//!
//! Every TCP connection, control or data, runs its own TLS 1.3 handshake with the device certificates. The receiver
//! only attaches a data connection to a transfer if it comes from the same certificate as the control connection.

use std::{
    collections::HashMap,
    future::Future,
    io::{self, Cursor},
    net::{Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::Duration,
};

use rustls::pki_types::ServerName;
use socket2::{SockRef, TcpKeepalive};
use tokio::{
    io::AsyncReadExt,
    net::{TcpListener, TcpStream},
    sync::{mpsc, OwnedSemaphorePermit},
    time::timeout,
};
use tokio_rustls::{client, server, TlsAcceptor, TlsConnector};
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::{
    tls::{self, PeerCheck, RefusalSlot},
    AnyConnection, Connection, ConnectionSlots, PeerSession, RecvHalf, SendHalf, HANDSHAKE_TIMEOUT,
};
use crate::{
    error::{Error, Result},
    identity::{DeviceCertificate, Fingerprint},
    protocol::{decode_frame, read_raw_frame, write_message, Message},
};

/// Default port for transfers (TCP, and QUIC over UDP).
pub const DEFAULT_PORT: u16 = 52000;

/// How long connecting to an address typed by the user may take.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long connecting to each address of a discovered peer may take.
pub const PEER_ADDRESS_TIMEOUT: Duration = Duration::from_secs(3);

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
    session: PeerSession,
    role: Role,
    /// Taken by the first `open_control` call.
    control: Mutex<Option<(SendHalf, RecvHalf)>>,
}

enum Role {
    /// The sender, which opens data connections to `remote` with the same TLS settings as the control connection.
    Client(ClientTls),
    /// The receiver, which collects data connections from the listener's mailbox. `_slot` is its place in the
    /// connection limit.
    Server { mailbox: Mailbox, claimed: Mutex<Vec<Uuid>>, _slot: OwnedSemaphorePermit },
    /// A receiver connection over the limit, kept only to answer that the receiver is busy.
    Refused,
}

/// The sender's TLS settings, kept for opening data connections.
#[derive(Clone)]
struct ClientTls {
    connector: TlsConnector,
    refusal: RefusalSlot,
}

impl TcpConnection {
    /// Address of the other device.
    #[must_use]
    pub fn remote_addr(&self) -> SocketAddr {
        self.inner.remote
    }

    fn new(session: PeerSession, role: Role, control: (SendHalf, RecvHalf)) -> Self {
        Self { inner: Arc::new(Inner { remote: session.address, session, role, control: Mutex::new(Some(control)) }) }
    }
}

impl Connection for TcpConnection {
    fn session(&self) -> &PeerSession {
        &self.inner.session
    }

    fn open_control(&self) -> impl Future<Output = Result<(SendHalf, RecvHalf)>> + Send {
        // The control socket already exists, so the answer is ready at once.
        let control = self.inner.control.lock().map_or(None, |mut control| control.take());
        std::future::ready(
            control.ok_or_else(|| Error::protocol_violation("the TCP control channel was already taken")),
        )
    }

    async fn open_data(&self, transfer_id: Uuid) -> Result<SendHalf> {
        let Role::Client(tls) = &self.inner.role else {
            return Err(Error::protocol_violation("only the sender opens data streams"));
        };
        let mut stream = connect_tls(self.inner.remote, CONNECT_TIMEOUT, tls).await?;
        // The same certificate check ran again, but a different device could still answer on the same address.
        if tls::peer_fingerprint(stream.get_ref().1.peer_certificates())? != self.inner.session.fingerprint {
            return Err(Error::FingerprintMismatch);
        }
        write_message(&mut stream, &Message::DataStreamHello { transfer_id }).await?;
        Ok(Box::new(stream))
    }

    async fn accept_data(&self, transfer_id: Uuid) -> Result<RecvHalf> {
        let Role::Server { mailbox, claimed, .. } = &self.inner.role else {
            return Err(Error::protocol_violation("only the receiver accepts data streams"));
        };
        if let Ok(mut claimed) = claimed.lock() {
            if !claimed.contains(&transfer_id) {
                claimed.push(transfer_id);
            }
        }
        let receiver = mailbox.receiver(transfer_id);
        loop {
            let (fingerprint, stream) = receiver.recv().await.map_err(|_closed| Error::ConnectionLost)?;
            // Transfer ids are not secret, so anyone could open a data connection for one. Only the sender's own
            // certificate may add data to its transfer.
            if fingerprint == self.inner.session.fingerprint {
                return Ok(Box::new(stream));
            }
            warn!(%transfer_id, %fingerprint, "ignoring a data connection from a different device");
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Forget the mailbox slots of this connection's transfers, closing any data sockets nobody picked up.
        if let Role::Server { mailbox, claimed, .. } = &self.role {
            if let Ok(claimed) = claimed.lock() {
                for transfer_id in claimed.iter() {
                    mailbox.remove(*transfer_id);
                }
            }
        }
    }
}

/// Opens the control connection to `addr` and checks the receiver as `check` and `expected` say.
pub(super) async fn connect(
    addr: SocketAddr,
    connect_timeout: Duration,
    certificate: &DeviceCertificate,
    check: &PeerCheck,
    expected: Option<Fingerprint>,
) -> Result<TcpConnection> {
    let (config, refusal) = tls::client_config(certificate, check, expected)?;
    let tls = ClientTls { connector: TlsConnector::from(Arc::new(config)), refusal };
    let stream = connect_tls(addr, connect_timeout, &tls).await?;
    let session = client_session(addr, &stream)?;
    // `split` works on any stream that can read and write, so each half can be used by a different task.
    let (read, write) = tokio::io::split(stream);
    Ok(TcpConnection::new(session, Role::Client(tls), (Box::new(write), Box::new(read))))
}

/// Connects and runs the TLS handshake, both with time limits.
async fn connect_tls(
    addr: SocketAddr,
    connect_timeout: Duration,
    tls: &ClientTls,
) -> Result<client::TlsStream<TcpStream>> {
    let stream = connect_stream(addr, connect_timeout).await?;
    let name = ServerName::try_from(tls::SERVER_NAME).map_err(|error| Error::Io(io::Error::other(error)))?;
    match timeout(HANDSHAKE_TIMEOUT, tls.connector.connect(name, stream)).await {
        Ok(Ok(stream)) => Ok(stream),
        Ok(Err(error)) => Err(tls.refusal.error_or(Error::Io(error))),
        Err(_elapsed) => Err(Error::Io(io::Error::from(io::ErrorKind::TimedOut))),
    }
}

fn client_session(addr: SocketAddr, stream: &client::TlsStream<TcpStream>) -> Result<PeerSession> {
    let connection = stream.get_ref().1;
    let fingerprint = tls::peer_fingerprint(connection.peer_certificates())?;
    let channel_binding =
        tls::channel_binding(|output, label| connection.export_keying_material(output, label, None).map(|_| ()))?;
    Ok(PeerSession { address: addr, fingerprint, channel_binding })
}

fn server_session(addr: SocketAddr, stream: &server::TlsStream<TcpStream>) -> Result<PeerSession> {
    let connection = stream.get_ref().1;
    let fingerprint = tls::peer_fingerprint(connection.peer_certificates())?;
    let channel_binding =
        tls::channel_binding(|output, label| connection.export_keying_material(output, label, None).map(|_| ()))?;
    Ok(PeerSession { address: addr, fingerprint, channel_binding })
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
pub(super) async fn accept_loop(
    listener: TcpListener,
    certificate: DeviceCertificate,
    connections: mpsc::Sender<AnyConnection>,
    slots: ConnectionSlots,
) {
    let acceptor = match tls::server_config(&certificate) {
        Ok(config) => TlsAcceptor::from(Arc::new(config)),
        Err(error) => {
            warn!(%error, "cannot set up TLS, accepting no TCP connections");
            return;
        }
    };
    let mailbox = Mailbox::default();
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                warn!(%error, "could not accept a TCP connection");
                continue;
            }
        };
        let incoming =
            Incoming { acceptor: acceptor.clone(), mailbox: mailbox.clone(), connections: connections.clone() };
        let slots = slots.clone();
        // The handshake and the first frame wait on the peer, so they run in their own task.
        tokio::spawn(async move {
            if let Err(error) = incoming.sort(stream, peer, slots).await {
                debug!(%peer, %error, "dropping TCP connection");
            }
        });
    }
}

/// What a new TCP connection may be handed to.
struct Incoming {
    acceptor: TlsAcceptor,
    mailbox: Mailbox,
    connections: mpsc::Sender<AnyConnection>,
}

impl Incoming {
    /// Runs the TLS handshake, reads the first frame and decides whether the connection is a data stream or a new
    /// control channel. Both steps have a time limit.
    async fn sort(self, stream: TcpStream, peer: SocketAddr, slots: ConnectionSlots) -> Result<()> {
        configure(&stream)?;
        let mut stream =
            timeout(HANDSHAKE_TIMEOUT, self.acceptor.accept(stream)).await.map_err(|_| Error::ConnectionLost)??;
        let session = server_session(peer, &stream)?;
        let frame =
            timeout(HANDSHAKE_TIMEOUT, read_raw_frame(&mut stream)).await.map_err(|_| Error::ConnectionLost)??;
        if let Ok(Message::DataStreamHello { transfer_id }) = decode_frame(&frame) {
            self.mailbox.deliver(transfer_id, session.fingerprint, stream);
            return Ok(());
        }
        info!(%peer, "incoming TCP connection");
        // The receiver still has to read the first frame (normally `Hello`), so it goes back in front of the stream.
        let (read, write) = tokio::io::split(stream);
        let read: RecvHalf = Box::new(Cursor::new(frame).chain(read));
        let Some(slot) = slots.take() else {
            // A connection without a slot only lives long enough to be told the receiver is busy.
            let connection = TcpConnection::new(session, Role::Refused, (Box::new(write), read));
            tokio::spawn(super::refuse_busy(AnyConnection::Tcp(connection)));
            return Ok(());
        };
        let role = Role::Server { mailbox: self.mailbox, claimed: Mutex::new(Vec::new()), _slot: slot };
        let connection = TcpConnection::new(session, role, (Box::new(write), read));
        let _ = self.connections.send(AnyConnection::Tcp(connection)).await;
        Ok(())
    }
}

/// Where data connections wait until the receiver of their transfer picks them up.
///
/// `async_channel` channels can be cloned on both ends, so either side may create a transfer's slot first.
#[derive(Debug, Clone, Default)]
struct Mailbox {
    slots: Arc<Mutex<HashMap<Uuid, Slot>>>,
}

/// A data connection waiting in the mailbox, with the fingerprint of the device that opened it.
type DataStream = (Fingerprint, server::TlsStream<TcpStream>);

type Slot = (async_channel::Sender<DataStream>, async_channel::Receiver<DataStream>);

impl Mailbox {
    fn deliver(&self, transfer_id: Uuid, fingerprint: Fingerprint, stream: server::TlsStream<TcpStream>) {
        let Ok(mut slots) = self.slots.lock() else { return };
        if !slots.contains_key(&transfer_id) && slots.len() >= MAX_PENDING_TRANSFERS {
            warn!(%transfer_id, "too many unclaimed data connections, closing this one");
            return;
        }
        let (sender, _) = slots.entry(transfer_id).or_insert_with(|| async_channel::bounded(MAX_PENDING_STREAMS));
        if sender.try_send((fingerprint, stream)).is_err() {
            warn!(%transfer_id, "too many data connections for one transfer, closing this one");
        }
    }

    fn receiver(&self, transfer_id: Uuid) -> async_channel::Receiver<DataStream> {
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
