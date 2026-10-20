//! Hosting and joining on a running engine (GE-046): what `net.host`,
//! `net.join`, and `net.disconnect` do, and what `net.status` reports.
//!
//! The engine library has no network of its own (ADR 0011): the
//! [`Networking`] resource holds a [`Network`] that makes transports.
//! `shin_net::QuicNetwork` is the real one, which the editor and games
//! install; [`LoopbackNetwork`] is an in-process one for tests.
//!
//! A session replaces whatever [`NetServer`] the engine had (a solo game's
//! [`NetServer::solo`]) and puts it back when it ends. It ends with
//! [`end`]: `net.disconnect`, or stopping play ([`Headless::stop`]), so
//! Stop in the editor ends hosting or leaves the server.

use std::collections::VecDeque;
use std::path::PathBuf;

use anyhow::Context as _;

use super::transport::{LoopbackNetwork, Traffic, Transport};
use super::{
    ClientState, LocalPlayer, NetClient, NetIds, NetServer, RemoteEntity, net_client_system,
    net_interpolate_system, net_server_input_system, net_server_system,
};
use crate::ecs::{Entity, Resources, World};
use crate::headless::Headless;
use crate::project::ProjectRoot;

/// The UDP port `net.host` listens on unless told otherwise.
pub const DEFAULT_PORT: u16 = 7777;

/// Where a host saves its certificate, relative to the project root, so a
/// client on the same machine can trust exactly that server.
pub const CERT_FILE: &str = "target/shin-server-cert.der";

/// How many ticks of traffic a session remembers: one second at 60 Hz.
const HISTORY: usize = 60;

/// Something that makes [`Transport`]s: what `net.host` and `net.join` use,
/// from the [`Networking`] resource.
pub trait Network {
    /// A server end listening on `port` (`0` picks a free port).
    fn listen(&mut self, port: u16) -> anyhow::Result<Listening>;

    /// A client end connected to the server at `addr` (`host:port`),
    /// trusting exactly the certificate `cert` (DER bytes), or any server
    /// when `cert` is `None`.
    fn connect(
        &mut self,
        addr: &str,
        cert: Option<&[u8]>,
    ) -> anyhow::Result<Box<dyn Transport + Send>>;
}

/// A server end from [`Network::listen`].
pub struct Listening {
    /// The transport clients connect to.
    pub transport: Box<dyn Transport + Send>,
    /// The port it listens on (the one picked, if asked for `0`).
    pub port: u16,
    /// The server's certificate (DER), if the network uses one; a host
    /// saves it to [`CERT_FILE`].
    pub cert_der: Option<Vec<u8>>,
}

/// Resource: the [`Network`] `net.host` and `net.join` use. Without one,
/// they fail.
pub struct Networking(pub Box<dyn Network>);

impl Network for LoopbackNetwork {
    fn listen(&mut self, port: u16) -> anyhow::Result<Listening> {
        let (transport, port) = LoopbackNetwork::listen(self, port)?;
        Ok(Listening {
            transport: Box::new(transport),
            port,
            cert_der: None,
        })
    }

    /// Connects to the port after the last `:` of `addr`; the host part
    /// and `cert` are ignored.
    fn connect(
        &mut self,
        addr: &str,
        _cert: Option<&[u8]>,
    ) -> anyhow::Result<Box<dyn Transport + Send>> {
        let port = addr
            .rsplit(':')
            .next()
            .and_then(|port| port.parse().ok())
            .with_context(|| format!("`{addr}` is not a host:port address"))?;
        Ok(Box::new(LoopbackNetwork::connect(self, port)?))
    }
}

/// Resource: the multiplayer session `net.host` or `net.join` started.
pub(crate) struct NetSession {
    pub(crate) role: Role,
    /// The server the session replaced, put back when it ends.
    previous: Option<NetServer>,
    /// Traffic totals at the end of each of the last ticks, oldest first.
    samples: VecDeque<Traffic>,
}

/// What this engine is in a session.
pub(crate) enum Role {
    /// A listen server.
    Host {
        /// The port clients join.
        port: u16,
        /// Where the certificate was saved, if it was.
        cert_path: Option<PathBuf>,
    },
    /// A client of the server at `addr`.
    Client {
        /// The address joined.
        addr: String,
    },
}

impl NetSession {
    /// Bytes per tick over the remembered ticks, oldest first:
    /// `(received, sent)`.
    pub(crate) fn per_tick(&self) -> Vec<(u64, u64)> {
        self.samples
            .iter()
            .zip(self.samples.iter().skip(1))
            .map(|(a, b)| {
                (
                    b.received.saturating_sub(a.received),
                    b.sent.saturating_sub(a.sent),
                )
            })
            .collect()
    }
}

/// Fixed system: records the session's traffic once per tick, for
/// `net.status`. Does nothing without a session.
fn net_session_system(_: &mut World, res: &mut Resources) {
    let traffic = match (res.get::<NetServer>(), res.get::<NetClient>()) {
        (Some(server), _) => server.traffic(),
        (None, Some(client)) => client.traffic(),
        (None, None) => Traffic::default(),
    };
    if let Some(session) = res.get_mut::<NetSession>() {
        session.samples.push_back(traffic);
        while session.samples.len() > HISTORY + 1 {
            session.samples.pop_front();
        }
    }
}

/// The engine's [`Network`], or an error saying how to get one.
fn network(engine: &mut Headless) -> anyhow::Result<&mut dyn Network> {
    let networking = engine.resources.get_mut::<Networking>().context(
        "this engine has no network to host or join on (the editor and games \
         built from `shin new` install one)",
    )?;
    Ok(networking.0.as_mut())
}

/// Fails if a session is already running.
fn ensure_no_session(engine: &Headless) -> anyhow::Result<()> {
    match engine.resources.get::<NetSession>().map(|s| &s.role) {
        None => Ok(()),
        Some(Role::Host { port, .. }) => {
            anyhow::bail!("already hosting on port {port}; call net.disconnect first")
        }
        Some(Role::Client { addr }) => {
            anyhow::bail!("already joined to {addr}; call net.disconnect first")
        }
    }
}

/// Adds the fixed system `name` at the end unless the schedule has it.
fn ensure_fixed(engine: &mut Headless, name: &'static str, system: crate::ecs::System) {
    if !engine.schedule.has_fixed(name) {
        engine.schedule.add_fixed(name, system);
    }
}

/// Starts hosting: a [`NetServer`] on the [`Networking`] network's `port`,
/// with this engine's own player, sending a snapshot every
/// `snapshot_every` ticks; then starts play mode. The server's systems
/// are added to the schedule if missing (`net_server_input` first,
/// `net_server` last), and the certificate is saved to [`CERT_FILE`] in
/// the open project. Returns the port.
///
/// Fails if a session is running, there is no network, the port cannot
/// be listened on, or play cannot start; the engine is then as it was.
pub(crate) fn host(engine: &mut Headless, port: u16, snapshot_every: u32) -> anyhow::Result<u16> {
    ensure_no_session(engine)?;
    let listening = network(engine)?.listen(port)?;
    let cert_path = match (&listening.cert_der, engine.resources.get::<ProjectRoot>()) {
        (Some(der), Some(root)) => {
            let path = root.0.join(CERT_FILE);
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("creating {}", dir.display()))?;
            }
            std::fs::write(&path, der).with_context(|| format!("writing {}", path.display()))?;
            Some(path)
        }
        _ => None,
    };
    let mut server = NetServer::new(listening.transport, snapshot_every);
    server.add_local_player();
    let previous = engine.resources.remove::<NetServer>();
    engine.resources.insert(server);
    if !engine.schedule.has_fixed("net_server_input") {
        engine
            .schedule
            .prepend_fixed("net_server_input", net_server_input_system);
    }
    ensure_fixed(engine, "net_server", net_server_system);
    ensure_fixed(engine, "net_session", net_session_system);
    let port = listening.port;
    engine.resources.insert(NetSession {
        role: Role::Host { port, cert_path },
        previous,
        samples: VecDeque::new(),
    });
    if let Err(err) = engine.play() {
        end(engine);
        return Err(err);
    }
    log::info!("net: hosting on port {port}");
    Ok(port)
}

/// Joins the server at `addr` as `name`, trusting `cert` (or any server
/// if `None`): a [`NetClient`] on the [`Networking`] network, with the
/// client's systems added to the schedule if missing; then starts play
/// mode. The engine's own [`NetServer`] (a solo one) is set aside, so its
/// server script does not run here.
///
/// Fails if a session is running, there is no network, the server cannot
/// be reached, or play cannot start; the engine is then as it was.
pub(crate) fn join(
    engine: &mut Headless,
    addr: &str,
    cert: Option<&[u8]>,
    name: &str,
) -> anyhow::Result<()> {
    ensure_no_session(engine)?;
    let transport = network(engine)?.connect(addr, cert)?;
    let previous = engine.resources.remove::<NetServer>();
    engine.resources.insert(NetClient::new(transport, name));
    ensure_fixed(engine, "net_client", net_client_system);
    if !engine.schedule.has_frame("net_interpolate") {
        engine
            .schedule
            .add_frame("net_interpolate", net_interpolate_system);
    }
    ensure_fixed(engine, "net_session", net_session_system);
    engine.resources.insert(NetSession {
        role: Role::Client {
            addr: addr.to_string(),
        },
        previous,
        samples: VecDeque::new(),
    });
    if let Err(err) = engine.play() {
        end(engine);
        return Err(err);
    }
    log::info!("net: joining {addr}");
    Ok(())
}

/// Ends the session, if any, and returns whether there was one.
///
/// A host says goodbye to every player and drops its server; a client says
/// goodbye to the server and removes what it replicated (every
/// [`RemoteEntity`], [`LocalPlayer`], and the [`NetIds`]). Either way the
/// server the session replaced comes back. Play mode is left as it is.
pub(crate) fn end(engine: &mut Headless) -> bool {
    let Some(session) = engine.resources.remove::<NetSession>() else {
        return false;
    };
    match session.role {
        Role::Host { port, .. } => {
            if let Some(mut server) = engine.resources.remove::<NetServer>() {
                server.shutdown("the host ended the session");
            }
            log::info!("net: stopped hosting on port {port}");
        }
        Role::Client { addr } => {
            if let Some(mut client) = engine.resources.remove::<NetClient>() {
                client.leave("left the session");
            }
            engine.resources.remove::<LocalPlayer>();
            engine.resources.remove::<NetIds>();
            let remote: Vec<Entity> = engine
                .world
                .query::<(Entity, &RemoteEntity)>()
                .iter()
                .map(|(entity, _)| entity)
                .collect();
            for entity in remote {
                let _ = engine.world.despawn(entity);
            }
            log::info!("net: left {addr}");
        }
    }
    if let Some(previous) = session.previous {
        engine.resources.insert(previous);
    }
    true
}

/// The client's state as `net.status` names it, and the reason if it is
/// disconnected.
pub(crate) fn client_state(client: &NetClient) -> (&'static str, Option<&str>) {
    match client.state() {
        ClientState::Connecting => ("connecting", None),
        ClientState::Connected { .. } => ("connected", None),
        ClientState::Disconnected(reason) => ("disconnected", Some(reason)),
    }
}
