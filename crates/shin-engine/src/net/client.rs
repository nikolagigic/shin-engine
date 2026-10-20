//! The client (GE-042): [`NetClient`] joins a server over a [`Transport`],
//! keeps a replica of the replicated entities, and shows them smoothly
//! although snapshots arrive less often than the client ticks, and not
//! always on time.
//!
//! Three steps, split by what they need:
//!
//! 1. [`net_client_system`] (fixed, no GPU): polls the transport, says
//!    [`Message::Hello`] when connected, tracks the [`ClientState`]
//!    (inserting [`LocalPlayer`] once welcomed), sends the player's
//!    [`Actions`] as an [`InputFrame`](super::InputFrame) (GE-044), and
//!    files incoming snapshots in the [`SnapshotBuffer`]. The snapshot that
//!    is due (the newest one at least [`SnapshotBuffer::delay`] ticks older
//!    than the newest received) becomes pending.
//! 2. [`apply_pending`] (after the fixed steps, with the GPU): applies the
//!    pending snapshot to the world with [`super::apply`], which may upload
//!    meshes, and marks every entity it lists as a [`RemoteEntity`].
//!    [`Headless::step`](crate::headless::Headless::step) and
//!    [`EcsGame`](crate::app::EcsGame) call it; systems have no GPU access.
//! 3. [`net_interpolate_system`] (frame): moves every remote entity
//!    between the applied snapshot and the next one, by how far the client
//!    has played past the applied tick.
//!
//! Remote entities belong to the server: physics does not simulate them
//! and scripts do not run on them, so they show exactly what the server
//! sent.

use std::collections::VecDeque;

use anyhow::Context as _;

use super::players::{LocalPlayer, input_frame};
use super::protocol::{Message, PROTOCOL_VERSION, Snapshot};
use super::snapshot::apply;
use super::transport::{Channel, Event, PeerId, Transport};
use super::{NetId, NetIds, PlayerId};
use crate::ecs::{Entity, Resources, Time, World};
use crate::input::Actions;
use crate::math::Transform;
use crate::render::RenderState;

/// Component: this entity is the server's, replicated to this client.
/// [`apply_pending`] adds it to every entity a snapshot spawns or updates.
/// The physics system skips such entities and the script system does not
/// run their scripts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RemoteEntity;

/// Where a [`NetClient`] is in its session.
#[derive(Clone, Debug, PartialEq)]
pub enum ClientState {
    /// Waiting for the transport to connect and the server to answer
    /// [`Message::Hello`].
    Connecting,
    /// The server welcomed this client.
    Connected {
        /// The id the server gave this player.
        player: PlayerId,
        /// The newest server tick heard of: the welcome's, then each
        /// snapshot's.
        server_tick: u64,
    },
    /// The session is over, and why (the server's goodbye, or the
    /// connection dropping). Remote entities stay where they were.
    Disconnected(String),
}

/// The last few snapshots received, and which one the world shows.
///
/// Snapshots are kept in tick order. Each client tick
/// ([`net_client_system`]) the newest snapshot whose tick is at most
/// `newest - delay` becomes due (if it is newer than the one already
/// applied); waiting `delay` ticks means the next snapshot has usually
/// arrived by the time the world reaches it, so there is something to
/// interpolate towards. Snapshots older than the applied one are dropped.
#[derive(Debug)]
pub struct SnapshotBuffer {
    /// Received snapshots, oldest first; the front is the applied one
    /// once there is one.
    snapshots: VecDeque<Snapshot>,
    /// Interpolation delay in ticks.
    delay: u64,
    /// The tick of the snapshot last made due.
    applied: Option<u64>,
    /// Client ticks run since that snapshot was made due.
    since_applied: u64,
    /// Due but not yet applied to the world (that needs the GPU).
    pending: Option<Snapshot>,
}

impl Default for SnapshotBuffer {
    fn default() -> Self {
        Self::new(Self::DEFAULT_DELAY)
    }
}

impl SnapshotBuffer {
    /// The interpolation delay a client starts with, in ticks.
    pub const DEFAULT_DELAY: u64 = 2;
    /// At most this many snapshots are kept; the oldest go first.
    pub const CAPACITY: usize = 32;

    /// An empty buffer with an interpolation delay of `delay` ticks.
    pub fn new(delay: u64) -> Self {
        Self {
            snapshots: VecDeque::new(),
            delay,
            applied: None,
            since_applied: 0,
            pending: None,
        }
    }

    /// The interpolation delay in ticks.
    pub fn delay(&self) -> u64 {
        self.delay
    }

    /// The tick of the newest snapshot received, if any is kept.
    pub fn newest_tick(&self) -> Option<u64> {
        self.snapshots.back().map(|snap| snap.tick)
    }

    /// The tick of the snapshot the world shows (or is about to, until
    /// [`apply_pending`] runs).
    pub fn applied_tick(&self) -> Option<u64> {
        self.applied
    }

    /// Where the world is in server ticks: the applied tick, plus the
    /// client ticks since, plus `alpha` (the fraction of a tick since the
    /// last one). `None` before a snapshot is applied.
    pub fn playhead(&self, alpha: f64) -> Option<f64> {
        self.applied
            .map(|tick| tick as f64 + self.since_applied as f64 + alpha)
    }

    /// Files `snap` in tick order. A snapshot no newer than the applied
    /// one, or one already held, is dropped.
    pub(crate) fn push(&mut self, snap: Snapshot) {
        if self.applied.is_some_and(|tick| snap.tick <= tick) {
            log::debug!("net: dropping late snapshot {}", snap.tick);
            return;
        }
        match self.snapshots.binary_search_by_key(&snap.tick, |s| s.tick) {
            Ok(_) => log::debug!("net: dropping duplicate snapshot {}", snap.tick),
            Err(at) => self.snapshots.insert(at, snap),
        }
        while self.snapshots.len() > Self::CAPACITY {
            self.snapshots.pop_front();
        }
    }

    /// One client tick: counts it, then makes the due snapshot pending.
    ///
    /// Skipped snapshots (ones between the applied and the due) are not
    /// shown, but entities they removed are, so a despawn is never lost
    /// by skipping. A pending snapshot not yet applied is replaced the
    /// same way.
    pub(crate) fn advance(&mut self) {
        if self.applied.is_some() {
            self.since_applied += 1;
        }
        let Some(newest) = self.newest_tick() else {
            return;
        };
        let target = newest.saturating_sub(self.delay);
        let Some(due) = self
            .snapshots
            .iter()
            .rposition(|snap| snap.tick <= target && self.applied.is_none_or(|t| snap.tick > t))
        else {
            return;
        };

        let mut snap = self.snapshots[due].clone();
        let skipped = self.snapshots.range(..due).filter(|s| {
            // Older than the applied one: already shown.
            self.applied.is_none_or(|t| s.tick > t)
        });
        let mut removed: Vec<NetId> = skipped
            .flat_map(|s| s.removed.iter().copied())
            .chain(self.pending.take().into_iter().flat_map(|p| p.removed))
            .chain(snap.removed.iter().copied())
            .collect();
        removed.sort();
        removed.dedup();
        snap.removed = removed;

        self.snapshots.drain(..due);
        self.applied = Some(snap.tick);
        self.since_applied = 0;
        self.pending = Some(snap);
    }

    /// The applied snapshot and the next one, if both are held.
    fn bracket(&self) -> Option<(&Snapshot, &Snapshot)> {
        let from = self.snapshots.front()?;
        let to = self.snapshots.get(1)?;
        (Some(from.tick) == self.applied).then_some((from, to))
    }
}

/// Resource: the client end of a multiplayer session. Add it with
/// [`net_client_system`] in the fixed schedule and
/// [`net_interpolate_system`] in the frame schedule; the engine applies
/// snapshots with [`apply_pending`].
pub struct NetClient {
    /// The connection to the server.
    transport: Box<dyn Transport + Send>,
    /// The player name sent in [`Message::Hello`].
    name: String,
    /// Where the session is.
    state: ClientState,
    /// Received snapshots.
    buffer: SnapshotBuffer,
    /// The server, once the transport has connected.
    server: Option<PeerId>,
    /// Client ticks run so far; stamps each [`InputFrame`](super::InputFrame).
    ticks: u64,
}

impl NetClient {
    /// A client that joins over `transport` as `name`, with the default
    /// interpolation delay. It says hello once the transport connects.
    pub fn new(transport: Box<dyn Transport + Send>, name: &str) -> Self {
        Self::with_delay(transport, name, SnapshotBuffer::DEFAULT_DELAY)
    }

    /// [`Self::new`] with an interpolation delay of `delay` ticks. `0`
    /// shows each snapshot as soon as it arrives, with nothing to
    /// interpolate towards.
    pub fn with_delay(transport: Box<dyn Transport + Send>, name: &str, delay: u64) -> Self {
        Self {
            transport,
            name: name.to_string(),
            state: ClientState::Connecting,
            buffer: SnapshotBuffer::new(delay),
            server: None,
            ticks: 0,
        }
    }

    /// Where the session is.
    pub fn state(&self) -> &ClientState {
        &self.state
    }

    /// The received snapshots.
    pub fn buffer(&self) -> &SnapshotBuffer {
        &self.buffer
    }

    /// Handles everything the transport received since the last tick.
    fn handle_events(&mut self) {
        for event in self.transport.poll() {
            match event {
                Event::Connected(peer) => {
                    self.server = Some(peer);
                    if self.state == ClientState::Connecting {
                        let hello = Message::Hello {
                            protocol: PROTOCOL_VERSION,
                            name: self.name.clone(),
                        };
                        self.transport.send(peer, &hello, Channel::Reliable);
                    }
                }
                Event::Message(_, Message::Welcome { player, tick, .. }) => {
                    log::info!("net: joined as player {} at server tick {tick}", player.0);
                    self.state = ClientState::Connected {
                        player,
                        server_tick: tick,
                    };
                }
                Event::Message(_, Message::Snapshot(snap)) => match &mut self.state {
                    ClientState::Connected { server_tick, .. } => {
                        *server_tick = (*server_tick).max(snap.tick);
                        self.buffer.push(snap);
                    }
                    _ => log::debug!("net: ignoring snapshot {} while not joined", snap.tick),
                },
                Event::Message(_, Message::Goodbye { reason }) => {
                    log::info!("net: the server said goodbye: {reason}");
                    self.state = ClientState::Disconnected(reason);
                }
                Event::Message(_, other) => {
                    log::warn!("net: ignoring an unexpected message from the server: {other:?}");
                }
                Event::Disconnected(_) => {
                    if !matches!(self.state, ClientState::Disconnected(_)) {
                        log::info!("net: the connection to the server closed");
                        self.state =
                            ClientState::Disconnected("the connection to the server closed".into());
                    }
                }
            }
        }
    }
}

/// Fixed system: one client tick. Does nothing without a [`NetClient`]
/// resource.
///
/// Polls the transport (sending [`Message::Hello`] on connect, taking
/// [`Message::Welcome`], filing snapshots, and moving to
/// [`ClientState::Disconnected`] on a goodbye or a dropped connection).
/// Once welcomed, it keeps a [`LocalPlayer`] resource naming this client's
/// player and sends the [`Actions`] resource (no actions without one) to
/// the server as an unreliable [`Message::Input`] every tick. Then, unless
/// disconnected, it advances the [`SnapshotBuffer`], which may make a
/// snapshot pending for [`apply_pending`].
pub fn net_client_system(_: &mut World, res: &mut Resources) {
    let Some(client) = res.get::<NetClient>() else {
        return;
    };
    let frame = input_frame(client.ticks, res.get::<Actions>());
    let client = res.expect_mut::<NetClient>();
    client.handle_events();
    client.ticks += 1;
    if let (ClientState::Connected { player, .. }, Some(server)) = (&client.state, client.server) {
        let player = *player;
        client
            .transport
            .send(server, &Message::Input(frame), Channel::Unreliable);
        if res.get::<LocalPlayer>() != Some(&LocalPlayer(player)) {
            res.insert(LocalPlayer(player));
        }
    }
    let client = res.expect_mut::<NetClient>();
    if !matches!(client.state, ClientState::Disconnected(_)) {
        client.buffer.advance();
    }
}

/// Applies the [`NetClient`]'s pending snapshot, if any, to `world` with
/// [`super::apply`], and marks every entity it lists as a
/// [`RemoteEntity`]. Call it after the fixed steps of each frame (or
/// [`crate::headless::Headless::step`]), with the GPU the world renders
/// on.
///
/// Does nothing without a [`NetClient`], or before a [`RenderState`]
/// exists (a window creates it on its first frame): the snapshot stays
/// pending until then. A snapshot that fails to apply (a mesh that does
/// not load) is dropped and the error returned; the next one is tried
/// as usual.
pub fn apply_pending(
    world: &mut World,
    res: &mut Resources,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> anyhow::Result<()> {
    if res.get::<RenderState>().is_none() {
        return Ok(());
    }
    let Some(snap) = res
        .get_mut::<NetClient>()
        .and_then(|client| client.buffer.pending.take())
    else {
        return Ok(());
    };
    apply(world, res, device, queue, &snap)
        .with_context(|| format!("failed to apply the snapshot of server tick {}", snap.tick))?;
    let ids = res.expect::<NetIds>();
    let listed: Vec<Entity> = snap
        .entities
        .iter()
        .filter_map(|desc| ids.entity(desc.id))
        .collect();
    for entity in listed {
        if world.get::<&RemoteEntity>(entity).is_err() {
            world.insert_one(entity, RemoteEntity)?;
        }
    }
    Ok(())
}

/// Frame system: moves every [`RemoteEntity`] between the applied
/// snapshot and the next one received.
///
/// The fraction is how far the [`SnapshotBuffer::playhead`] (with
/// `Time::alpha`) is from the applied tick to the next snapshot's, held
/// at the next snapshot once the playhead passes it: when a snapshot is
/// late or lost, entities wait at the newest pose rather than jumping or
/// guessing. With no next snapshot they stay at the applied pose.
/// Translation and scale are interpolated linearly, rotation spherically.
/// Entities missing from either snapshot are left alone. Does nothing
/// without a [`NetClient`].
pub fn net_interpolate_system(world: &mut World, res: &mut Resources) {
    let alpha = res.get::<Time>().map_or(0.0, |time| time.alpha);
    let (Some(client), Some(ids)) = (res.get::<NetClient>(), res.get::<NetIds>()) else {
        return;
    };
    let buffer = &client.buffer;
    let (Some((from, to)), Some(playhead)) = (buffer.bracket(), buffer.playhead(alpha)) else {
        return;
    };
    let span = (to.tick - from.tick) as f64;
    let t = ((playhead - from.tick as f64) / span).clamp(0.0, 1.0) as f32;
    for a in &from.entities {
        let Ok(at) = to.entities.binary_search_by_key(&a.id, |b| b.id) else {
            continue;
        };
        let Some(entity) = ids.entity(a.id) else {
            continue;
        };
        let (a, b) = (
            a.transform.to_transform(),
            to.entities[at].transform.to_transform(),
        );
        if let Ok((transform, _)) = world.query_one_mut::<(&mut Transform, &RemoteEntity)>(entity) {
            *transform = lerp(&a, &b, t);
        }
    }
}

/// The transform `t` of the way from `a` to `b`.
fn lerp(a: &Transform, b: &Transform, t: f32) -> Transform {
    Transform {
        translation: a.translation.lerp(b.translation, t),
        rotation: a.rotation.slerp(b.rotation, t),
        scale: a.scale.lerp(b.scale, t),
    }
}

#[cfg(test)]
mod tests;
