//! The authority (GE-041): [`NetServer`] accepts players over a
//! [`Transport`] and broadcasts [`Snapshot`]s of the world every few ticks.
//!
//! The server is a resource plus a fixed system, so it runs inside any
//! engine:
//!
//! - **dedicated** (`cargo run -- --serve`): [`run_server`] drives a
//!   [`Headless`] at the fixed rate, with no window;
//! - **listen server** (`cargo run -- --host`): the game's window engine
//!   holds a [`NetServer`] and runs [`net_server_system`] too; the host is
//!   a local player ([`NetServer::add_local_player`]) that needs no
//!   transport.
//!
//! A server tick has two halves (GE-044):
//!
//! 1. [`net_server_input_system`], first in the fixed schedule: answers
//!    [`Message::Hello`] with [`Message::Welcome`] (or the
//!    [`reject_version`] goodbye), drops players that say
//!    [`Message::Goodbye`] or disconnect, and updates [`PlayerInputs`]
//!    from the [`Message::Input`] frames received, so this tick's game
//!    systems see them. Join and leave are logged.
//! 2. [`net_server_system`], last: on every `snapshot_every`-th tick
//!    (ticks 0, `n`, `2n`, ...) [`capture`]s the world and sends it to
//!    every player, then advances the tick. If the first half did not run
//!    this tick (it is not in the schedule), this one runs it first, so a
//!    server works with `net_server_system` alone, with input a tick later.
//!
//! Snapshots go out as deltas (GE-049): each client acknowledges the
//! newest snapshot it holds in its [`InputFrame::ack`], and the server
//! sends it only what changed since that one ([`diff`]). A player gets a
//! full snapshot instead when it joins, when its acknowledged snapshot is
//! older than the server keeps, and at least every
//! [`ServerOptions::full_snapshot_every`] ticks, so a client that lost
//! track always recovers. [`NetServer::bandwidth`] compares the bytes
//! sent with what full snapshots would have cost.
//!
//! [`NetServer::solo`] is a server with nobody to serve but the local
//! player: single-player games run the same scripts as multiplayer ones.
//!
//! [`Snapshot`]: super::Snapshot

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use super::players::PlayerInputs;
use super::protocol::{InputFrame, Message, encode, reject_version};
use super::snapshot::{capture, diff};
use super::transport::{Channel, Event, LoopbackTransport, PeerId, Transport};
use super::{PlayerId, Snapshot};
use crate::app::FixedTimestep;
use crate::ecs::{ExitRequest, Resources, Time, World};
use crate::headless::Headless;
use crate::input::Actions;

/// Resource: the server end of a multiplayer session. Add it to the
/// engine's resources and run [`net_server_input_system`] as the first
/// fixed system and [`net_server_system`] as the last (or let
/// [`run_server`] add the server and the last one).
pub struct NetServer {
    /// Where clients connect.
    transport: Box<dyn Transport + Send>,
    /// Remote players, by the peer they connect from.
    players: HashMap<PeerId, PlayerId>,
    /// The host's own player on a listen server.
    local: Option<PlayerId>,
    /// The next [`PlayerId`] handed out; ids count up from 1.
    next_player: u32,
    /// Ticks run so far; the tick the next [`net_server_system`] run is.
    tick: u64,
    /// A snapshot goes out on every tick divisible by this.
    snapshot_every: u32,
    /// A player gets a full snapshot at least every this many ticks.
    full_every: u64,
    /// The snapshots captured in the last `full_every` ticks, oldest
    /// first: the baselines deltas can be made against.
    history: VecDeque<Snapshot>,
    /// What each remote player acknowledged and when it last got a full
    /// snapshot.
    baselines: HashMap<PeerId, Baseline>,
    /// Snapshot bytes over the last second.
    traffic: Traffic,
    /// Input frames received since the last tick, by sender.
    frames: Vec<(PlayerId, InputFrame)>,
    /// Whether this tick's input half has run.
    received: bool,
    /// Made by [`Self::solo`]: nobody can join.
    solo: bool,
}

/// One remote player's delta state.
#[derive(Clone, Copy, Debug, Default)]
struct Baseline {
    /// The newest snapshot tick the client acknowledged.
    acked: Option<u64>,
    /// The tick of the last full snapshot sent to it.
    last_full: Option<u64>,
}

/// Snapshot bytes sent over a sliding window of one second of ticks.
#[derive(Debug, Default)]
struct Traffic {
    /// `(tick, full bytes, sent bytes)` per snapshot tick, oldest first.
    ticks: VecDeque<(u64, usize, usize)>,
}

impl Traffic {
    /// Records the bytes of one snapshot tick and forgets those a second
    /// (`ticks_per_second`) or more older.
    fn record(&mut self, tick: u64, full: usize, sent: usize, ticks_per_second: u64) {
        self.ticks.push_back((tick, full, sent));
        let window = ticks_per_second.max(1);
        while self
            .ticks
            .front()
            .is_some_and(|&(t, ..)| t + window <= tick)
        {
            self.ticks.pop_front();
        }
    }
}

/// Snapshot traffic of a [`NetServer`] over the last second of ticks, to
/// every remote player together ([`NetServer::bandwidth`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bandwidth {
    /// What sending every player a full snapshot each time would have
    /// cost, in bytes per second: the "before".
    pub full_bytes_per_s: f64,
    /// What was actually sent, deltas and full snapshots, in bytes per
    /// second: the "after".
    pub sent_bytes_per_s: f64,
}

impl Bandwidth {
    /// How many times smaller the traffic is than with full snapshots
    /// only (`full / sent`); `None` while nothing was sent.
    pub fn compression_ratio(&self) -> Option<f64> {
        (self.sent_bytes_per_s > 0.0).then(|| self.full_bytes_per_s / self.sent_bytes_per_s)
    }
}

impl NetServer {
    /// A server accepting clients on `transport`, sending a snapshot every
    /// `snapshot_every` ticks (`0` is treated as `1`: every tick), with
    /// the other [`ServerOptions`] at their defaults.
    pub fn new(transport: Box<dyn Transport + Send>, snapshot_every: u32) -> Self {
        Self::with_options(
            transport,
            ServerOptions {
                snapshot_every,
                ..ServerOptions::default()
            },
        )
    }

    /// A server accepting clients on `transport`, run as `opts` says.
    pub fn with_options(transport: Box<dyn Transport + Send>, opts: ServerOptions) -> Self {
        Self {
            transport,
            players: HashMap::new(),
            local: None,
            next_player: 1,
            tick: 0,
            snapshot_every: opts.snapshot_every.max(1),
            full_every: u64::from(opts.full_snapshot_every.max(1)),
            history: VecDeque::new(),
            baselines: HashMap::new(),
            traffic: Traffic::default(),
            frames: Vec::new(),
            received: false,
            solo: false,
        }
    }

    /// A server for a single-player game: no network (a
    /// [`LoopbackTransport`] nobody else holds) and one local player,
    /// `PlayerId(1)`. Run with the server systems, it makes a solo game
    /// behave like a listen server nobody joins: the same scripts greet
    /// the player and read its input.
    pub fn solo() -> Self {
        let (transport, _) = LoopbackTransport::pair(0);
        let mut server = Self::new(Box::new(transport), ServerOptions::default().snapshot_every);
        server.add_local_player();
        server.solo = true;
        server
    }

    /// Whether this is a [`Self::solo`] server, which nobody can join.
    pub fn is_solo(&self) -> bool {
        self.solo
    }

    /// Adds the host's own player on a listen server and returns its id
    /// (`PlayerId(1)` if called before anyone joins). Calling it again
    /// returns the same id. The local player is in [`Self::players`] but
    /// receives no messages: it shares the server's world.
    pub fn add_local_player(&mut self) -> PlayerId {
        if let Some(id) = self.local {
            return id;
        }
        let id = self.fresh_player();
        self.local = Some(id);
        log::info!("net: local player {} joined", id.0);
        id
    }

    /// The host's own player, if [`Self::add_local_player`] added one.
    pub fn local_player(&self) -> Option<PlayerId> {
        self.local
    }

    /// Every player in the session, local and remote, in id order.
    pub fn players(&self) -> Vec<PlayerId> {
        let mut players: Vec<PlayerId> = self.players.values().copied().chain(self.local).collect();
        players.sort();
        players
    }

    /// The tick the server is at: how many ticks it has run.
    pub fn tick(&self) -> u64 {
        self.tick
    }

    /// Snapshot bytes sent to remote players over the last second of
    /// ticks, and what full snapshots would have cost.
    pub fn bandwidth(&self) -> Bandwidth {
        let (full, sent) = self
            .traffic
            .ticks
            .iter()
            .fold((0, 0), |(f, s), &(_, full, sent)| (f + full, s + sent));
        Bandwidth {
            full_bytes_per_s: full as f64,
            sent_bytes_per_s: sent as f64,
        }
    }

    /// Tells every remote player the session is ending with a
    /// [`Message::Goodbye`] carrying `reason`, and forgets them.
    pub fn shutdown(&mut self, reason: &str) {
        let bye = Message::Goodbye {
            reason: reason.to_string(),
        };
        for &peer in self.players.keys() {
            self.transport.send(peer, &bye, Channel::Reliable);
        }
        self.players.clear();
        self.baselines.clear();
    }

    fn fresh_player(&mut self) -> PlayerId {
        let id = PlayerId(self.next_player);
        self.next_player += 1;
        id
    }

    /// Handles everything the transport received since the last tick.
    fn handle_events(&mut self, fixed_dt: f64) {
        for event in self.transport.poll() {
            match event {
                Event::Connected(peer) => log::debug!("net: peer {} connected", peer.0),
                Event::Message(peer, Message::Hello { protocol, name }) => {
                    self.hello(peer, protocol, &name, fixed_dt);
                }
                Event::Message(peer, Message::Goodbye { reason }) => {
                    self.leave(peer, &format!("said goodbye: {reason}"));
                }
                Event::Message(peer, Message::Input(frame)) => match self.players.get(&peer) {
                    Some(&player) => {
                        if let Some(ack) = frame.ack {
                            let baseline = self.baselines.entry(peer).or_default();
                            // Frames can arrive out of order; keep the newest.
                            baseline.acked = baseline.acked.max(Some(ack));
                        }
                        self.frames.push((player, frame));
                    }
                    None => log::debug!("net: input from peer {} before joining", peer.0),
                },
                Event::Message(peer, other) => {
                    log::warn!(
                        "net: ignoring an unexpected message from peer {}: {other:?}",
                        peer.0
                    );
                }
                Event::Disconnected(peer) => self.leave(peer, "disconnected"),
            }
        }
    }

    /// Answers a [`Message::Hello`]: a [`Message::Welcome`] with a new
    /// player id, or a goodbye if the protocol differs. A repeated hello
    /// is welcomed again with the same id.
    fn hello(&mut self, peer: PeerId, protocol: u32, name: &str, fixed_dt: f64) {
        if let Some(bye) = reject_version(protocol) {
            log::warn!(
                "net: turning away `{name}` (peer {}): protocol {protocol}",
                peer.0
            );
            self.transport.send(peer, &bye, Channel::Reliable);
            return;
        }
        let player = match self.players.get(&peer) {
            Some(&player) => player,
            None => {
                let player = self.fresh_player();
                self.players.insert(peer, player);
                log::info!(
                    "net: `{name}` joined as player {} (peer {})",
                    player.0,
                    peer.0
                );
                player
            }
        };
        let welcome = Message::Welcome {
            player,
            tick: self.tick,
            fixed_dt,
        };
        self.transport.send(peer, &welcome, Channel::Reliable);
    }

    /// Forgets the player connected from `peer`, if any.
    fn leave(&mut self, peer: PeerId, why: &str) {
        self.baselines.remove(&peer);
        match self.players.remove(&peer) {
            Some(player) => log::info!("net: player {} left ({why})", player.0),
            None => log::debug!("net: peer {} {why} before joining", peer.0),
        }
    }

    /// Whether this tick sends a snapshot.
    fn snapshot_due(&self) -> bool {
        self.tick.is_multiple_of(u64::from(self.snapshot_every))
    }

    /// Sends the full `snapshot` to every remote player, as a delta
    /// against the snapshot it acknowledged where it can (see the module
    /// docs), then keeps it as a baseline for later deltas.
    /// `ticks_per_second` sizes the bandwidth window.
    fn send_snapshot(&mut self, snapshot: Snapshot, ticks_per_second: u64) {
        let tick = snapshot.tick;
        let full = Message::Snapshot(snapshot);
        let full_bytes = encode(&full).len();
        let Message::Snapshot(snapshot) = &full else {
            unreachable!("built above")
        };
        let mut peers: Vec<PeerId> = self.players.keys().copied().collect();
        peers.sort();
        let mut sent_bytes = 0;
        for &peer in &peers {
            let baseline = self.baselines.entry(peer).or_default();
            let full_due = baseline
                .last_full
                .is_none_or(|last| tick - last >= self.full_every);
            let base = baseline.acked.and_then(|acked| {
                self.history
                    .binary_search_by_key(&acked, |s| s.tick)
                    .ok()
                    .map(|at| &self.history[at])
            });
            match base.filter(|_| !full_due) {
                Some(base) => {
                    let delta = Message::Snapshot(diff(base, snapshot));
                    sent_bytes += encode(&delta).len();
                    self.transport.send(peer, &delta, Channel::Unreliable);
                }
                None => {
                    baseline.last_full = Some(tick);
                    sent_bytes += full_bytes;
                    self.transport.send(peer, &full, Channel::Unreliable);
                }
            }
        }
        self.traffic
            .record(tick, full_bytes * peers.len(), sent_bytes, ticks_per_second);

        let Message::Snapshot(snapshot) = full else {
            unreachable!("built above")
        };
        self.history.push_back(snapshot);
        let oldest = tick.saturating_sub(self.full_every);
        while self.history.front().is_some_and(|s| s.tick < oldest) {
            self.history.pop_front();
        }
    }
}

/// Fixed system: the first half of a server tick. Run it first in the
/// fixed schedule, so the game systems after it see this tick's input.
/// Does nothing without a [`NetServer`] resource.
///
/// Polls the transport (hello → welcome, goodbye or disconnect → leave,
/// input frames kept), then updates the [`PlayerInputs`] resource
/// (inserting one if missing): players who left are dropped, each remote
/// player's newest frame is applied, and the local player's input is
/// copied from the [`Actions`] resource.
pub fn net_server_input_system(_: &mut World, res: &mut Resources) {
    let fixed_dt = res.get::<Time>().map_or(1.0 / 60.0, |time| time.fixed_dt);
    let Some(server) = res.get_mut::<NetServer>() else {
        return;
    };
    server.handle_events(fixed_dt);
    server.received = true;
    let frames = std::mem::take(&mut server.frames);
    let players = server.players();
    let local = server.local;
    let mut inputs = res
        .get_mut::<PlayerInputs>()
        .map(std::mem::take)
        .unwrap_or_default();
    inputs.begin_tick(&players, frames, local.zip(res.get::<Actions>()));
    res.insert(inputs);
}

/// Fixed system: the second half of a server tick. Run it last in the
/// fixed schedule, so snapshots carry what this step's systems did. Does
/// nothing without a [`NetServer`] resource.
///
/// Runs [`net_server_input_system`] first if that has not run this tick,
/// then on ticks divisible by `snapshot_every` captures the replicated
/// world ([`capture`], at the server's tick) and sends it to every remote
/// player, then advances the tick.
pub fn net_server_system(world: &mut World, res: &mut Resources) {
    let Some(server) = res.get::<NetServer>() else {
        return;
    };
    if !server.received {
        net_server_input_system(world, res);
    }
    let fixed_dt = res.get::<Time>().map_or(1.0 / 60.0, |time| time.fixed_dt);
    let ticks_per_second = (1.0 / fixed_dt).round() as u64;
    let server = res.expect_mut::<NetServer>();
    server.received = false;
    let tick = server.tick;
    if server.snapshot_due() {
        let snapshot = capture(world, res, tick);
        res.expect_mut::<NetServer>()
            .send_snapshot(snapshot, ticks_per_second);
    }
    res.expect_mut::<NetServer>().tick += 1;
}

/// How a [`NetServer`] (and [`run_server`]) runs the authority.
#[derive(Clone, Copy, Debug)]
pub struct ServerOptions {
    /// A snapshot goes out every this many ticks. Default 3: 20 per
    /// second at the default 60 Hz tick.
    pub snapshot_every: u32,
    /// Each player gets a full snapshot rather than a delta at least
    /// every this many ticks. Default 60: once a second at 60 Hz. `1`
    /// sends only full snapshots (deltas off); `0` is treated as `1`.
    pub full_snapshot_every: u32,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            snapshot_every: 3,
            full_snapshot_every: 60,
        }
    }
}

/// Runs `engine` as a dedicated server until a system sets
/// [`ExitRequest`] to `true` (or the process is interrupted, e.g. by
/// Ctrl-C).
///
/// Inserts a [`NetServer`] on `transport`, appends [`net_server_system`]
/// to the fixed schedule (so it runs last; do not add it yourself; do add
/// [`net_server_input_system`] first in the schedule, or input reaches
/// the game a tick later),
/// starts play mode ([`Headless::play`]), and then steps the engine at
/// [`Headless::fixed_dt`] by the wall clock, sleeping between ticks and
/// catching up after a slow one. Every 10 seconds it logs the tick and
/// the number of players. On exit it sends every player a goodbye.
///
/// `sim.*` and the other tools keep working on the engine between calls
/// to this function; while it runs, it owns the engine.
///
/// Fails if play mode cannot start (see [`Headless::play`]).
pub fn run_server(
    engine: &mut Headless,
    transport: Box<dyn Transport + Send>,
    opts: ServerOptions,
) -> anyhow::Result<()> {
    engine
        .resources
        .insert(NetServer::with_options(transport, opts));
    engine.resources.insert(ExitRequest(false));
    engine.schedule.add_fixed("net_server", net_server_system);
    engine.play()?;
    log::info!(
        "net: server running at {:.0} ticks per second, a snapshot every {} tick(s)",
        1.0 / engine.fixed_dt,
        opts.snapshot_every
    );

    let mut timestep = FixedTimestep::new(engine.fixed_dt);
    let mut last = Instant::now();
    let mut last_status = last;
    loop {
        let now = Instant::now();
        let steps = timestep.advance((now - last).as_secs_f64()).steps;
        last = now;
        for _ in 0..steps {
            engine.step(1);
            if exit_requested(engine) {
                engine
                    .resources
                    .expect_mut::<NetServer>()
                    .shutdown("the server is shutting down");
                log::info!("net: server stopped");
                return Ok(());
            }
        }
        if now - last_status >= STATUS_EVERY {
            last_status = now;
            let server = engine.resources.expect::<NetServer>();
            log::info!(
                "net: tick {}, {} player(s)",
                server.tick(),
                server.players().len()
            );
        }
        // Wake twice per tick, so a tick is late by at most half a step.
        std::thread::sleep(Duration::from_secs_f64(engine.fixed_dt / 2.0));
    }
}

/// How often [`run_server`] logs the tick and player count.
const STATUS_EVERY: Duration = Duration::from_secs(10);

fn exit_requested(engine: &Headless) -> bool {
    engine
        .resources
        .get::<ExitRequest>()
        .is_some_and(|exit| exit.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::Transform;
    use crate::net::{LoopbackTransport, PROTOCOL_VERSION, Replicated};

    fn hello(name: &str) -> Message {
        Message::Hello {
            protocol: PROTOCOL_VERSION,
            name: name.into(),
        }
    }

    /// The messages in `client`'s inbox, dropping connection events.
    fn received(client: &mut LoopbackTransport) -> Vec<Message> {
        client
            .poll()
            .into_iter()
            .filter_map(|event| match event {
                Event::Message(_, msg) => Some(msg),
                _ => None,
            })
            .collect()
    }

    /// The ticks of the snapshots in `msgs`.
    fn snapshot_ticks(msgs: &[Message]) -> Vec<u64> {
        msgs.iter()
            .filter_map(|msg| match msg {
                Message::Snapshot(snap) => Some(snap.tick),
                _ => None,
            })
            .collect()
    }

    /// A world with one replicated entity and resources holding a server
    /// on a loopback with `clients` client ends.
    fn setup(clients: usize, every: u32) -> (World, Resources, Vec<LoopbackTransport>) {
        let opts = ServerOptions {
            snapshot_every: every,
            ..ServerOptions::default()
        };
        setup_with(clients, opts)
    }

    /// [`setup`] with a server run as `opts` says.
    fn setup_with(
        clients: usize,
        opts: ServerOptions,
    ) -> (World, Resources, Vec<LoopbackTransport>) {
        let (server, clients) = LoopbackTransport::pair(clients);
        let mut world = World::new();
        world.spawn((Transform::default(), Replicated));
        let mut res = Resources::new();
        let mut time = Time::default();
        time.advance_fixed(1.0 / 60.0);
        res.insert(time);
        res.insert(NetServer::with_options(Box::new(server), opts));
        (world, res, clients)
    }

    fn tick(world: &mut World, res: &mut Resources, n: u32) {
        for _ in 0..n {
            net_server_system(world, res);
        }
    }

    #[test]
    fn hello_gets_welcome_with_the_current_tick() {
        let (mut world, mut res, mut clients) = setup(1, 100);
        tick(&mut world, &mut res, 5);
        clients[0].send(PeerId(0), &hello("ada"), Channel::Reliable);
        tick(&mut world, &mut res, 1);
        let msgs = received(&mut clients[0]);
        assert_eq!(
            msgs,
            [Message::Welcome {
                player: PlayerId(1),
                tick: 5,
                fixed_dt: 1.0 / 60.0,
            }]
        );
        assert_eq!(res.expect::<NetServer>().players(), [PlayerId(1)]);
    }

    #[test]
    fn a_bad_protocol_is_turned_away() {
        let (mut world, mut res, mut clients) = setup(1, 1);
        let old = Message::Hello {
            protocol: PROTOCOL_VERSION + 1,
            name: "old".into(),
        };
        clients[0].send(PeerId(0), &old, Channel::Reliable);
        tick(&mut world, &mut res, 2);
        let msgs = received(&mut clients[0]);
        assert!(
            matches!(msgs.as_slice(), [Message::Goodbye { .. }]),
            "no welcome and no snapshots: {msgs:?}"
        );
        assert!(res.expect::<NetServer>().players().is_empty());
    }

    #[test]
    fn both_clients_receive_each_snapshot() {
        let (mut world, mut res, mut clients) = setup(2, 1);
        for (i, client) in clients.iter_mut().enumerate() {
            client.send(PeerId(0), &hello(&format!("p{i}")), Channel::Reliable);
        }
        tick(&mut world, &mut res, 3);
        for client in &mut clients {
            let msgs = received(client);
            assert!(matches!(msgs[0], Message::Welcome { .. }), "{msgs:?}");
            assert_eq!(snapshot_ticks(&msgs), [0, 1, 2]);
            let Message::Snapshot(snap) = &msgs[1] else {
                unreachable!()
            };
            assert_eq!(snap.entities.len(), 1, "the replicated entity");
        }
        assert_eq!(
            res.expect::<NetServer>().players(),
            [PlayerId(1), PlayerId(2)]
        );
    }

    #[test]
    fn snapshots_go_out_every_n_ticks_from_zero() {
        let (mut world, mut res, mut clients) = setup(1, 3);
        clients[0].send(PeerId(0), &hello("ada"), Channel::Reliable);
        tick(&mut world, &mut res, 8);
        assert_eq!(snapshot_ticks(&received(&mut clients[0])), [0, 3, 6]);
        assert_eq!(res.expect::<NetServer>().tick(), 8);
    }

    #[test]
    fn disconnect_and_goodbye_remove_players() {
        let (mut world, mut res, mut clients) = setup(3, 1);
        for client in &mut clients {
            client.send(PeerId(0), &hello("p"), Channel::Reliable);
        }
        tick(&mut world, &mut res, 1);
        assert_eq!(res.expect::<NetServer>().players().len(), 3);

        drop(clients.remove(0));
        let bye = Message::Goodbye {
            reason: "bed time".into(),
        };
        clients[0].send(PeerId(0), &bye, Channel::Reliable);
        tick(&mut world, &mut res, 1);
        assert_eq!(res.expect::<NetServer>().players(), [PlayerId(3)]);
        // The one left still gets snapshots; the one that said goodbye does not.
        received(&mut clients[0]);
        tick(&mut world, &mut res, 1);
        assert!(snapshot_ticks(&received(&mut clients[0])).is_empty());
        assert_eq!(snapshot_ticks(&received(&mut clients[1])), [0, 1, 2]);
    }

    #[test]
    fn shutdown_says_goodbye_to_every_player() {
        let (mut world, mut res, mut clients) = setup(1, 100);
        clients[0].send(PeerId(0), &hello("ada"), Channel::Reliable);
        tick(&mut world, &mut res, 1);
        received(&mut clients[0]);
        res.expect_mut::<NetServer>().shutdown("done");
        assert_eq!(
            received(&mut clients[0]),
            [Message::Goodbye {
                reason: "done".into()
            }]
        );
        assert!(res.expect::<NetServer>().players().is_empty());
    }

    /// Frame system for the listen-server test: counts frames.
    fn count_frames(_: &mut World, res: &mut Resources) {
        *res.expect_mut::<u32>() += 1;
    }

    /// A `--host`-style engine: the game's own engine holds the server and
    /// a local player, and keeps rendering frames while a client joins.
    #[test]
    fn a_listen_server_accepts_a_client_while_its_frame_loop_runs() -> anyhow::Result<()> {
        let (server, mut clients) = LoopbackTransport::pair(1);
        let mut engine = Headless::new(16, 16)?;
        engine.resources.insert(0_u32);
        engine.schedule.add_frame("count_frames", count_frames);
        engine.schedule.add_fixed("net_server", net_server_system);
        let mut net = NetServer::new(Box::new(server), 2);
        assert_eq!(net.add_local_player(), PlayerId(1));
        assert_eq!(net.add_local_player(), PlayerId(1), "stable");
        engine.resources.insert(net);
        engine.world.spawn((Transform::default(), Replicated));
        engine.play()?;

        // The host's loop: a frame, then the fixed steps due.
        let mut msgs = Vec::new();
        for frame in 0..6 {
            if frame == 2 {
                clients[0].send(PeerId(0), &hello("guest"), Channel::Reliable);
            }
            engine.render_to_target();
            engine.step(1);
            msgs.extend(received(&mut clients[0]));
        }
        assert_eq!(engine.resources.get::<u32>(), Some(&6), "frames ran");
        assert_eq!(
            msgs[0],
            Message::Welcome {
                player: PlayerId(2),
                tick: 2,
                fixed_dt: engine.fixed_dt,
            }
        );
        assert_eq!(snapshot_ticks(&msgs), [2, 4]);
        let net = engine.resources.expect::<NetServer>();
        assert_eq!(net.players(), [PlayerId(1), PlayerId(2)]);
        Ok(())
    }

    /// Fixed system for the dedicated-server test: asks to exit at tick 4.
    fn exit_at_four(_: &mut World, res: &mut Resources) {
        if res.expect::<Time>().fixed_steps >= 4 {
            res.insert(ExitRequest(true));
        }
    }

    #[test]
    fn run_server_steps_until_exit_and_says_goodbye() -> anyhow::Result<()> {
        let (server, mut clients) = LoopbackTransport::pair(1);
        clients[0].send(PeerId(0), &hello("ada"), Channel::Reliable);
        let mut engine = Headless::new(8, 8)?;
        engine.fixed_dt = 1.0 / 240.0;
        engine.schedule.add_fixed("exit_at_four", exit_at_four);
        run_server(
            &mut engine,
            Box::new(server),
            ServerOptions {
                snapshot_every: 2,
                ..ServerOptions::default()
            },
        )?;
        assert!(engine.play_state().is_playing());
        assert_eq!(engine.resources.expect::<Time>().fixed_steps, 4);
        let msgs = received(&mut clients[0]);
        assert!(matches!(
            msgs.first(),
            Some(Message::Welcome { tick: 0, .. })
        ));
        assert_eq!(snapshot_ticks(&msgs), [0, 2]);
        assert!(matches!(msgs.last(), Some(Message::Goodbye { .. })));
        // sim.* keeps working on the server engine afterwards.
        let reg = crate::tool::ToolRegistry::with_engine_tools();
        reg.call(&mut engine, "sim.stop", serde_json::json!({}))?;
        assert!(!engine.play_state().is_playing());
        Ok(())
    }

    /// An input frame acknowledging snapshot `ack`.
    fn ack(ack: u64) -> Message {
        Message::Input(InputFrame {
            ack: Some(ack),
            ..InputFrame::default()
        })
    }

    /// `(tick, baseline)` of each snapshot in `msgs`.
    fn kinds(msgs: &[Message]) -> Vec<(u64, Option<u64>)> {
        msgs.iter()
            .filter_map(|msg| match msg {
                Message::Snapshot(snap) => Some((snap.tick, snap.baseline)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn deltas_follow_the_acknowledged_snapshot_with_a_full_one_every_n_ticks() {
        let opts = ServerOptions {
            snapshot_every: 1,
            full_snapshot_every: 4,
        };
        let (mut world, mut res, mut clients) = setup_with(1, opts);
        let client = &mut clients[0];
        client.send(PeerId(0), &hello("ada"), Channel::Reliable);

        // Join: a full snapshot; nothing acknowledged, so still full.
        tick(&mut world, &mut res, 2);
        assert_eq!(kinds(&received(client)), [(0, None), (1, None)]);
        // Each tick is a delta against the newest acknowledged snapshot.
        client.send(PeerId(0), &ack(0), Channel::Unreliable);
        tick(&mut world, &mut res, 1);
        client.send(PeerId(0), &ack(2), Channel::Unreliable);
        client.send(PeerId(0), &ack(1), Channel::Unreliable); // late: ignored
        tick(&mut world, &mut res, 1);
        let msgs = received(client);
        assert_eq!(kinds(&msgs), [(2, Some(0)), (3, Some(2))]);
        let Message::Snapshot(delta) = &msgs[1] else {
            unreachable!()
        };
        assert!(delta.entities.is_empty(), "the one entity did not change");
        // Four ticks after the last full snapshot (1), another one.
        tick(&mut world, &mut res, 2);
        assert_eq!(kinds(&received(client)), [(4, Some(2)), (5, None)]);
        // An acknowledgement of a snapshot the server does not hold gets
        // full snapshots, which always recover.
        client.send(PeerId(0), &ack(999), Channel::Unreliable);
        tick(&mut world, &mut res, 1);
        assert_eq!(kinds(&received(client)), [(6, None)]);
    }

    #[test]
    fn bandwidth_compares_sent_bytes_with_full_snapshots() {
        let (mut world, mut res, mut clients) = setup(1, 1);
        clients[0].send(PeerId(0), &hello("ada"), Channel::Reliable);
        tick(&mut world, &mut res, 1);
        let full = res.expect::<NetServer>().bandwidth();
        assert!(full.full_bytes_per_s > 0.0);
        assert_eq!(full.compression_ratio(), Some(1.0), "a full snapshot");
        for t in 0..60 {
            clients[0].send(PeerId(0), &ack(t), Channel::Unreliable);
            tick(&mut world, &mut res, 1);
        }
        let bandwidth = res.expect::<NetServer>().bandwidth();
        assert!(
            bandwidth.sent_bytes_per_s < bandwidth.full_bytes_per_s,
            "{bandwidth:?}"
        );
        assert!(bandwidth.compression_ratio().unwrap() > 1.0);
        let none = Bandwidth::default();
        assert_eq!(none.compression_ratio(), None);
    }
}
