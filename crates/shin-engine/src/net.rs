//! Multiplayer core (GE-040): server-authoritative snapshot replication
//! without any real network (ADR 0009, ADR 0011).
//!
//! - Components: [`Replicated`] marks an entity that snapshots carry,
//!   [`Owner`] says which [`PlayerId`] controls it.
//! - [`NetIds`] (a resource) gives every replicated entity a [`NetId`] that
//!   server and clients agree on. Ids are assigned lazily by [`capture`],
//!   live only for the session, and are never reused; scene files do not
//!   store them.
//! - [`protocol`]: the [`Message`]s peers exchange and their binary codec,
//!   [`encode`] and [`decode`] (postcard), checked by [`PROTOCOL_VERSION`].
//! - [`snapshot`]: [`capture`] turns the replicated entities into a
//!   [`Snapshot`]; [`apply`] spawns, updates, and despawns entities from
//!   one.
//! - [`server`]: the authority (GE-041): [`NetServer`],
//!   [`net_server_input_system`] and [`net_server_system`] (the two
//!   halves of a tick), and [`run_server`] for a dedicated server.
//! - [`client`]: joining one (GE-042): [`NetClient`],
//!   [`net_client_system`], [`apply_pending`], and
//!   [`net_interpolate_system`]; replicated entities on a client are
//!   [`RemoteEntity`]s.
//! - [`players`]: players and their input (GE-044): [`InputFrame`]s from
//!   clients become the server's [`PlayerInputs`]; a client knows itself
//!   as [`LocalPlayer`].
//! - [`predict`]: local-player prediction and reconciliation (GE-048):
//!   the local player's own [`Mover`](crate::physics::Mover)s are
//!   [`Predicted`], moved at once with its input, and corrected by the
//!   server's snapshots ([`Snapshot::last_input_ticks`]).
//! - [`transport`]: the synchronous, polled [`Transport`] trait and
//!   [`LoopbackTransport`], an in-process implementation for tests, with
//!   simulated latency, jitter, and loss ([`LoopbackOptions`]).
//!
//! Tools: [`register_tools`] adds `net.status`.

pub mod client;
pub mod players;
pub mod predict;
pub mod protocol;
pub mod server;
pub mod snapshot;
pub mod transport;

mod tools;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::ecs::Entity;

pub use client::{
    ClientState, NetClient, RemoteEntity, SnapshotBuffer, apply_pending, net_client_system,
    net_interpolate_system,
};
pub use players::{LocalPlayer, PlayerInputs};
pub use predict::{InputHistory, Predicted};
pub use protocol::{
    EntitySnap, InputFrame, Message, PROTOCOL_VERSION, Snapshot, decode, encode, reject_version,
};
pub use server::{
    NetServer, ServerOptions, net_server_input_system, net_server_system, run_server,
};
pub use snapshot::{apply, capture};
pub use tools::register_tools;
pub use transport::{Channel, Event, LoopbackOptions, LoopbackTransport, PeerId, Transport};

/// An entity's identity on the network: the same on the server and every
/// client, and never reused within a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NetId(pub u64);

/// A connected player, as the server numbers them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PlayerId(pub u32);

/// Component: this entity is in snapshots. Each replicated entity carries
/// it; children of a replicated entity are not implied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Replicated;

/// Component: the player who controls this entity. Optional; replicated
/// with the entity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Owner(pub PlayerId);

/// Resource: the two-way map between local entities and [`NetId`]s.
///
/// On the server, [`capture`] fills it; on a client, [`apply`] does, with
/// the server's ids. Fresh ids count up from 1 and are never handed out
/// twice, even after their entity is gone.
#[derive(Debug)]
pub struct NetIds {
    /// The next id [`Self::assign`] hands out.
    next: u64,
    by_entity: HashMap<Entity, NetId>,
    by_id: HashMap<NetId, Entity>,
}

impl Default for NetIds {
    fn default() -> Self {
        Self {
            next: 1,
            by_entity: HashMap::new(),
            by_id: HashMap::new(),
        }
    }
}

impl NetIds {
    /// An empty map; the first id assigned is `NetId(1)`.
    pub fn new() -> Self {
        Self::default()
    }

    /// `entity`'s id, if it has one.
    pub fn id(&self, entity: Entity) -> Option<NetId> {
        self.by_entity.get(&entity).copied()
    }

    /// The entity with `id`, if any.
    pub fn entity(&self, id: NetId) -> Option<Entity> {
        self.by_id.get(&id).copied()
    }

    /// Number of mapped entities.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Whether no entity is mapped.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// `entity`'s id, giving it a fresh one first if it has none.
    pub(crate) fn assign(&mut self, entity: Entity) -> NetId {
        if let Some(id) = self.id(entity) {
            return id;
        }
        let id = NetId(self.next);
        self.next += 1;
        self.insert(entity, id);
        id
    }

    /// Maps `entity` to `id` (an id received from the server), replacing
    /// any previous mapping of either. Fresh ids stay above `id`.
    pub(crate) fn insert(&mut self, entity: Entity, id: NetId) {
        self.remove_id(id);
        if let Some(old) = self.by_entity.remove(&entity) {
            self.by_id.remove(&old);
        }
        self.by_entity.insert(entity, id);
        self.by_id.insert(id, entity);
        self.next = self.next.max(id.0 + 1);
    }

    /// Forgets `id` and its entity; returns the entity it mapped to.
    pub(crate) fn remove_id(&mut self, id: NetId) -> Option<Entity> {
        let entity = self.by_id.remove(&id)?;
        self.by_entity.remove(&entity);
        Some(entity)
    }

    /// Every `(entity, id)` pair, in no particular order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (Entity, NetId)> + '_ {
        self.by_entity.iter().map(|(&e, &id)| (e, id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ecs::World;

    #[test]
    fn ids_count_up_and_are_never_reused() {
        let mut world = World::new();
        let (a, b) = (world.spawn(()), world.spawn(()));
        let mut ids = NetIds::new();
        assert_eq!(ids.assign(a), NetId(1));
        assert_eq!(ids.assign(a), NetId(1), "assign is stable");
        assert_eq!(ids.assign(b), NetId(2));
        assert_eq!(ids.remove_id(NetId(1)), Some(a));
        let c = world.spawn(());
        assert_eq!(
            ids.assign(c),
            NetId(3),
            "a freed id is not handed out again"
        );
        assert_eq!((ids.entity(NetId(2)), ids.id(b)), (Some(b), Some(NetId(2))));
    }

    #[test]
    fn received_ids_keep_fresh_ids_above_them() {
        let mut world = World::new();
        let (a, b) = (world.spawn(()), world.spawn(()));
        let mut ids = NetIds::new();
        ids.insert(a, NetId(41));
        assert_eq!(ids.assign(b), NetId(42));
        ids.insert(a, NetId(7));
        assert_eq!(ids.entity(NetId(41)), None, "remapping drops the old id");
        assert_eq!(ids.len(), 2);
    }
}
