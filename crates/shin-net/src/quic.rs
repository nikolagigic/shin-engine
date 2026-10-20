//! [`QuicTransport`]: the engine's polled [`Transport`] over QUIC (quinn).
//!
//! A small tokio runtime inside the transport owns the socket and runs one
//! task per connection. The synchronous side, which the game calls once
//! per tick, only sends into channels and drains what the tasks received,
//! so no call ever blocks on the network (except [`QuicTransport::connect`],
//! which waits for the handshake, and dropping the transport, which waits
//! briefly for queued reliable messages to arrive).
//!
//! On the wire, each connection is:
//!
//! - one bidirectional stream, opened by the client, which starts with the
//!   four bytes [`MAGIC`] and then carries [`Channel::Reliable`] messages
//!   as frames: a little-endian `u32` length, then the [`encode`]d message;
//! - QUIC datagrams for [`Channel::Unreliable`] messages, one message per
//!   datagram. A message too large for a datagram on this path (about
//!   1200 bytes) goes over the stream instead, so big snapshots still
//!   arrive.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use anyhow::Context as _;
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use shin::net::{Channel, Event, Message, PeerId, Transport, decode, encode};
use tokio::runtime::{Handle, Runtime};
use tokio::sync::mpsc as async_mpsc;
use tokio::task::JoinHandle;

use crate::cert::{self, DevCert, SERVER_NAME, ServerTrust};

/// The bytes a client writes first on its stream: "this is a Shin peer".
const MAGIC: &[u8; 4] = b"shin";
/// The largest reliable frame accepted, so a corrupt length cannot make a
/// peer allocate gigabytes.
const MAX_FRAME: usize = 16 * 1024 * 1024;
/// How long [`QuicTransport::connect`] waits for the server.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a new connection may take to open its stream.
const STREAM_TIMEOUT: Duration = Duration::from_secs(5);
/// How long dropping the transport waits for queued reliable messages
/// (such as a goodbye) to be acknowledged before closing.
const CLOSE_GRACE: Duration = Duration::from_secs(1);
/// Keep-alive interval, so a quiet but live connection never times out.
const KEEP_ALIVE: Duration = Duration::from_secs(2);
/// A peer that sends nothing (not even keep-alives) for this long is
/// disconnected.
const IDLE_TIMEOUT: Duration = Duration::from_secs(10);

/// What a connection task tells the synchronous side.
enum Incoming {
    /// A peer finished connecting; `PeerLink` is how to send to it.
    Connected(PeerId, PeerLink),
    /// A peer sent a message.
    Message(PeerId, Message),
    /// A peer's connection ended; nothing more comes from it.
    Disconnected(PeerId),
}

/// The synchronous side's handle on one connection.
struct PeerLink {
    /// For datagrams, which quinn sends without awaiting.
    conn: Connection,
    /// Frames for the stream writer task.
    reliable: async_mpsc::UnboundedSender<Vec<u8>>,
    /// The stream writer task, awaited on drop so queued frames go out.
    writer: JoinHandle<()>,
}

/// A [`Transport`] over QUIC: [`Self::listen`] makes a server that any
/// number of clients reach with [`Self::connect`].
///
/// Peer ids follow [`shin::net::LoopbackTransport`]: a client sees the
/// server as `PeerId(0)`; the server numbers clients from `PeerId(1)` in
/// the order they connect and never reuses an id. Each end reports
/// [`Event::Connected`] on the first [`Transport::poll`] after a peer
/// connects, and [`Event::Disconnected`] after the last message from it.
/// A client may send to the server as soon as [`Self::connect`] returns.
///
/// Reliable messages arrive once and in order. Unreliable ones may be
/// lost or reordered; on localhost practically all of them arrive.
///
/// Dropping the transport finishes every stream (waiting up to a second
/// for the peers to acknowledge what was queued, so a final
/// [`Message::Goodbye`] is delivered) and closes the connections.
pub struct QuicTransport {
    /// The UDP socket and its connections.
    endpoint: Endpoint,
    /// What the connection tasks received, oldest first.
    incoming: mpsc::Receiver<Incoming>,
    /// Connected peers: on a server, those [`Transport::poll`] has
    /// reported; on a client, the server from the start.
    peers: BTreeMap<PeerId, PeerLink>,
    /// Peers whose [`Event::Connected`] the next poll still reports (the
    /// server, on a client).
    unannounced: Vec<PeerId>,
    /// The server's certificate (DER); `None` on a client.
    cert: Option<Vec<u8>>,
    /// Runs the connection tasks. `None` only while dropping.
    runtime: Option<Runtime>,
}

impl QuicTransport {
    /// A server listening on `addr` (e.g. `0.0.0.0:7777`; port 0 picks a
    /// free port, see [`Self::local_addr`]).
    ///
    /// It generates a fresh self-signed certificate for `localhost` and
    /// logs its SHA-256 fingerprint; [`Self::cert_der`] returns it, to save
    /// for clients that pass [`ServerTrust::Cert`].
    ///
    /// Fails if the address cannot be bound (e.g. the port is in use).
    pub fn listen(addr: SocketAddr) -> anyhow::Result<Self> {
        let runtime = runtime()?;
        let dev = DevCert::generate()?;
        let cert_der = dev.cert.to_vec();
        let mut config = quinn::ServerConfig::with_single_cert(vec![dev.cert], dev.key)
            .context("configuring TLS with the development certificate")?;
        config.transport_config(transport_config());
        let endpoint = {
            let _context = runtime.enter();
            Endpoint::server(config, addr).with_context(|| format!("listening on {addr}"))?
        };
        let (tx, incoming) = mpsc::channel();
        runtime.spawn(accept_loop(endpoint.clone(), tx));
        let local = endpoint.local_addr()?;
        log::info!(
            "net: QUIC server listening on {local}; certificate SHA-256 fingerprint {}",
            cert::fingerprint(&cert_der)
        );
        Ok(Self {
            endpoint,
            incoming,
            peers: BTreeMap::new(),
            unannounced: Vec::new(),
            cert: Some(cert_der),
            runtime: Some(runtime),
        })
    }

    /// A client connected to the server at `addr`, trusting the server's
    /// certificate as `trust` says. Waits for the handshake (up to five
    /// seconds); the server is `PeerId(0)`.
    ///
    /// With [`ServerTrust::Insecure`] it logs a warning: any server is
    /// accepted.
    ///
    /// Fails if the server does not answer in time, its certificate is not
    /// trusted, or it is not a Shin server.
    pub fn connect(addr: SocketAddr, trust: &ServerTrust) -> anyhow::Result<Self> {
        if matches!(trust, ServerTrust::Insecure) {
            log::warn!(
                "net: --insecure: trusting ANY server certificate. The connection is \
                 encrypted but not authenticated; use this for development only"
            );
        }
        let runtime = runtime()?;
        let bind: SocketAddr = if addr.is_ipv6() {
            (Ipv6Addr::UNSPECIFIED, 0).into()
        } else {
            (Ipv4Addr::UNSPECIFIED, 0).into()
        };
        let mut config = cert::client_config(trust)?;
        config.transport_config(transport_config());
        let mut endpoint = {
            let _context = runtime.enter();
            Endpoint::client(bind).context("opening a UDP socket")?
        };
        endpoint.set_default_client_config(config);

        let (conn, send, recv) = block_on(runtime.handle(), async {
            // Connecting spawns quinn's driver task, so it runs on the runtime.
            let connecting = endpoint
                .connect(addr, SERVER_NAME)
                .with_context(|| format!("connecting to {addr}"))?;
            let conn = tokio::time::timeout(CONNECT_TIMEOUT, connecting)
                .await
                .map_err(|_| anyhow::anyhow!("no answer from {addr} within {CONNECT_TIMEOUT:?}"))?
                .with_context(|| format!("connecting to {addr}"))?;
            let (mut send, recv) = conn.open_bi().await.context("opening the stream")?;
            send.write_all(MAGIC).await.context("greeting the server")?;
            anyhow::Ok((conn, send, recv))
        })?;
        log::info!("net: connected to {addr} over QUIC");

        // The server is a peer from now on, so messages (a hello) can be
        // queued before the first poll, which reports it connected.
        let (tx, incoming) = mpsc::channel();
        let server = PeerId(0);
        let peers = BTreeMap::from([(server, link(runtime.handle(), conn.clone(), send))]);
        spawn_readers(runtime.handle(), server, conn, recv, tx);
        Ok(Self {
            endpoint,
            incoming,
            peers,
            unannounced: vec![server],
            cert: None,
            runtime: Some(runtime),
        })
    }

    /// The address this end's socket is bound to: on a server, where
    /// clients connect (useful after listening on port 0).
    pub fn local_addr(&self) -> anyhow::Result<SocketAddr> {
        Ok(self.endpoint.local_addr()?)
    }

    /// On a server, its self-signed certificate (DER), which a client can
    /// trust with [`ServerTrust::Cert`]; `None` on a client.
    pub fn cert_der(&self) -> Option<&[u8]> {
        self.cert.as_deref()
    }

    /// Queues `bytes` on `link`'s stream.
    fn send_reliable(link: &PeerLink, bytes: Vec<u8>) {
        // The writer is gone only when the connection is; the disconnect
        // event is on its way.
        let _ = link.reliable.send(bytes);
    }

    /// Sends `bytes` to `link` on `channel`.
    fn send_bytes(link: &PeerLink, bytes: Vec<u8>, channel: Channel) {
        let fits = |max: usize| bytes.len() <= max;
        match channel {
            Channel::Unreliable if link.conn.max_datagram_size().is_some_and(fits) => {
                if let Err(err) = link.conn.send_datagram(bytes.into()) {
                    log::debug!("net: dropping a datagram: {err}");
                }
            }
            // Reliable, or too big for a datagram: the stream.
            _ => Self::send_reliable(link, bytes),
        }
    }
}

impl Transport for QuicTransport {
    fn send(&mut self, to: PeerId, msg: &Message, channel: Channel) {
        match self.peers.get(&to) {
            Some(link) => Self::send_bytes(link, encode(msg), channel),
            None => log::debug!("net: dropping a message to unknown peer {to:?}"),
        }
    }

    fn broadcast(&mut self, msg: &Message, channel: Channel) {
        let bytes = encode(msg);
        for link in self.peers.values() {
            Self::send_bytes(link, bytes.clone(), channel);
        }
    }

    fn poll(&mut self) -> Vec<Event> {
        let mut events: Vec<Event> = self.unannounced.drain(..).map(Event::Connected).collect();
        for incoming in self.incoming.try_iter() {
            match incoming {
                Incoming::Connected(peer, link) => {
                    self.peers.insert(peer, link);
                    events.push(Event::Connected(peer));
                }
                Incoming::Message(peer, msg) => events.push(Event::Message(peer, msg)),
                Incoming::Disconnected(peer) => {
                    self.peers.remove(&peer);
                    events.push(Event::Disconnected(peer));
                }
            }
        }
        events
    }

    fn peers(&self) -> Vec<PeerId> {
        self.peers.keys().copied().collect()
    }
}

impl Drop for QuicTransport {
    /// Finishes every stream, waits up to a second for queued
    /// reliable messages to be acknowledged, then closes the endpoint.
    fn drop(&mut self) {
        let Some(runtime) = self.runtime.take() else {
            return;
        };
        // Peers that connected since the last poll have writers too.
        let mut links: Vec<PeerLink> = std::mem::take(&mut self.peers).into_values().collect();
        links.extend(
            self.incoming
                .try_iter()
                .filter_map(|incoming| match incoming {
                    Incoming::Connected(_, link) => Some(link),
                    _ => None,
                }),
        );
        let endpoint = self.endpoint.clone();
        block_on(runtime.handle(), async move {
            let writers: Vec<JoinHandle<()>> = links
                .into_iter()
                .map(|link| link.writer) // dropping `reliable` stops the writer
                .collect();
            let _ = tokio::time::timeout(CLOSE_GRACE, async {
                for writer in writers {
                    let _ = writer.await;
                }
            })
            .await;
            endpoint.close(0u32.into(), b"closed");
            let _ = tokio::time::timeout(CLOSE_GRACE, endpoint.wait_idle()).await;
        });
        runtime.shutdown_background();
    }
}

/// The runtime the transport's tasks run on: one worker thread is plenty.
fn runtime() -> anyhow::Result<Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("shin-net")
        .enable_all()
        .build()
        .context("starting the network runtime")
}

/// Keep-alives and an idle timeout, the same on both ends.
fn transport_config() -> Arc<quinn::TransportConfig> {
    let mut config = quinn::TransportConfig::default();
    config.keep_alive_interval(Some(KEEP_ALIVE));
    config.max_idle_timeout(Some(
        IDLE_TIMEOUT
            .try_into()
            .expect("the idle timeout fits a QUIC varint"),
    ));
    Arc::new(config)
}

/// Runs `fut` to completion on `handle`, from synchronous code. When the
/// caller is itself inside an async runtime (where blocking that thread is
/// not allowed), it blocks on a helper thread instead.
fn block_on<F>(handle: &Handle, fut: F) -> F::Output
where
    F: Future + Send,
    F::Output: Send,
{
    if Handle::try_current().is_err() {
        return handle.block_on(fut);
    }
    std::thread::scope(|scope| {
        scope
            .spawn(|| handle.block_on(fut))
            .join()
            .expect("the network runtime does not panic")
    })
}

/// Server: accepts connections until the endpoint closes, numbering
/// clients from 1.
async fn accept_loop(endpoint: Endpoint, tx: mpsc::Sender<Incoming>) {
    let mut next = 1u32;
    while let Some(connecting) = endpoint.accept().await {
        let peer = PeerId(next);
        next += 1;
        let tx = tx.clone();
        tokio::spawn(async move {
            if let Err(err) = accept_peer(peer, connecting, tx).await {
                log::warn!("net: a client failed to connect: {err:#}");
            }
        });
    }
}

/// Server: completes one client's handshake, checks its greeting, and
/// starts its tasks.
async fn accept_peer(
    peer: PeerId,
    connecting: quinn::Incoming,
    tx: mpsc::Sender<Incoming>,
) -> anyhow::Result<()> {
    let conn = connecting.await.context("handshake")?;
    let opened = tokio::time::timeout(STREAM_TIMEOUT, async {
        let (send, mut recv) = conn.accept_bi().await?;
        let mut magic = [0; MAGIC.len()];
        recv.read_exact(&mut magic).await?;
        anyhow::ensure!(&magic == MAGIC, "not a Shin client");
        anyhow::Ok((send, recv))
    })
    .await;
    let (send, recv) = match opened {
        Ok(Ok(streams)) => streams,
        Ok(Err(err)) => {
            conn.close(1u32.into(), b"not a Shin client");
            return Err(err);
        }
        Err(_) => {
            conn.close(1u32.into(), b"timed out");
            anyhow::bail!("no stream within {STREAM_TIMEOUT:?}");
        }
    };
    log::debug!(
        "net: peer {} connected from {}",
        peer.0,
        conn.remote_address()
    );
    let handle = Handle::current();
    let link = link(&handle, conn.clone(), send);
    if tx.send(Incoming::Connected(peer, link)).is_ok() {
        spawn_readers(&handle, peer, conn, recv, tx);
    }
    Ok(())
}

/// Starts the task that writes `send`'s frames and returns the link that
/// queues them.
fn link(handle: &Handle, conn: Connection, send: SendStream) -> PeerLink {
    let (reliable, frames) = async_mpsc::unbounded_channel();
    let writer = handle.spawn(write_frames(send, frames));
    PeerLink {
        conn,
        reliable,
        writer,
    }
}

/// Starts the task that reads `peer`'s stream and datagrams, reporting
/// each message and, once both stop, [`Incoming::Disconnected`].
fn spawn_readers(
    handle: &Handle,
    peer: PeerId,
    conn: Connection,
    recv: RecvStream,
    tx: mpsc::Sender<Incoming>,
) {
    handle.spawn(async move {
        let datagrams = tokio::spawn(read_datagrams(peer, conn.clone(), tx.clone()));
        read_frames(peer, recv, &tx).await;
        // The stream ended, so the peer is gone; this also stops the
        // datagram reader.
        conn.close(0u32.into(), b"stream closed");
        let _ = datagrams.await;
        let _ = tx.send(Incoming::Disconnected(peer));
    });
}

/// Writes every queued frame to the stream. When the queue closes (the
/// transport dropped the peer or itself), finishes the stream and waits
/// until the peer has acknowledged everything.
async fn write_frames(mut send: SendStream, mut frames: async_mpsc::UnboundedReceiver<Vec<u8>>) {
    while let Some(bytes) = frames.recv().await {
        let len = u32::try_from(bytes.len()).expect("a message under 4 GiB");
        let mut frame = Vec::with_capacity(4 + bytes.len());
        frame.extend_from_slice(&len.to_le_bytes());
        frame.extend_from_slice(&bytes);
        if let Err(err) = send.write_all(&frame).await {
            log::debug!("net: stream write failed: {err}");
            return;
        }
    }
    if send.finish().is_ok() {
        let _ = send.stopped().await;
    }
}

/// Reads frames from the stream until it ends or fails, reporting each.
async fn read_frames(peer: PeerId, mut recv: RecvStream, tx: &mpsc::Sender<Incoming>) {
    loop {
        let mut len = [0; 4];
        if recv.read_exact(&mut len).await.is_err() {
            return; // finished or lost
        }
        let len = u32::from_le_bytes(len) as usize;
        if len > MAX_FRAME {
            log::warn!(
                "net: peer {} sent a {len}-byte frame; disconnecting",
                peer.0
            );
            return;
        }
        let mut bytes = vec![0; len];
        if recv.read_exact(&mut bytes).await.is_err() {
            return;
        }
        if !report(peer, &bytes, tx) {
            return;
        }
    }
}

/// Reads datagrams until the connection ends, reporting each.
async fn read_datagrams(peer: PeerId, conn: Connection, tx: mpsc::Sender<Incoming>) {
    while let Ok(bytes) = conn.read_datagram().await {
        if !report(peer, &bytes, &tx) {
            return;
        }
    }
}

/// Decodes `bytes` and reports the message (a malformed one is logged and
/// skipped). Returns `false` once the transport is gone.
fn report(peer: PeerId, bytes: &[u8], tx: &mpsc::Sender<Incoming>) -> bool {
    match decode(bytes) {
        Ok(msg) => tx.send(Incoming::Message(peer, msg)).is_ok(),
        Err(err) => {
            log::warn!("net: dropping a message from peer {}: {err:#}", peer.0);
            true
        }
    }
}
