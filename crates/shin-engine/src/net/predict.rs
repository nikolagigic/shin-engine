//! Local-player prediction and reconciliation (GE-048, ADR 0009 stage 4).
//!
//! Without prediction a client's own cube moves one round trip after the
//! key press: the input travels to the server, the server moves the cube,
//! and the snapshot travels back. With it, the client moves its own
//! entities at once and lets the server correct it.
//!
//! What is predicted: entities the local player owns ([`super::Owner`]
//! is the [`LocalPlayer`](super::LocalPlayer)) that walk with a
//! [`Mover`]. [`super::apply_pending`] marks them [`Predicted`] (unless
//! prediction is off, [`super::NetClient::set_prediction`]); every other
//! replicated entity stays interpolated. Movement that is not a
//! [`Mover`] (a script setting an owned entity's transform) runs on the
//! server only and reaches the client as a correction.
//!
//! Each client tick [`super::net_client_system`]:
//!
//! 1. **reconciles**: if a snapshot arrived, it says which of this
//!    client's input frames the server had applied
//!    ([`Snapshot::last_input_ticks`]) and where the server had the
//!    predicted entities then. If that differs from what this client
//!    predicted after the same frame by more than [`EPSILON`], the entity
//!    is put back at the server's state and the frames the server had not
//!    yet applied ([`InputHistory`]) are replayed on it with
//!    [`Mover::step`];
//! 2. **predicts**: steps every [`Predicted`] entity's [`Mover`] with this
//!    tick's [`InputFrame`] (the same one it sends the server) and keeps
//!    the frame and the result in the history.
//!
//! This works because [`Mover::step`] is a pure function of the input,
//! `dt`, the mover's state, and the static colliders, run identically on
//! both sides (see [`crate::physics::mover`]). The client lands movers on
//! its own static colliders (the ground it loaded from the scene), not on
//! other dynamic bodies.

use std::collections::VecDeque;

use super::protocol::{InputFrame, Snapshot};
use super::{NetId, NetIds, PlayerId};
use crate::ecs::{Entity, World};
use crate::math::{Transform, Vec3};
use crate::physics::{MoveInput, Mover, Statics};

/// Component on a client: this entity is one of the local player's own,
/// moved here ahead of the server and corrected by it. It is also a
/// [`super::RemoteEntity`] (physics and scripts leave it alone), but
/// [`super::net_interpolate_system`] does not move it and snapshots do
/// not overwrite its translation or [`Mover`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Predicted;

/// How far, in metres (and metres per second for the vertical speed), a
/// prediction may be from the server's state before it is corrected.
pub const EPSILON: f32 = 1e-4;

/// A predicted entity's state after one tick.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Pose {
    entity: Entity,
    at: Vec3,
    mover: Mover,
}

/// One tick of the history: the input sent and what it predicted.
#[derive(Clone, Debug)]
struct HistoryEntry {
    frame: InputFrame,
    poses: Vec<Pose>,
}

/// The input frames this client sent that the server has not yet
/// acknowledged, oldest first, each with the state it predicted for the
/// [`Predicted`] entities. At most [`Self::CAPACITY`] are kept.
#[derive(Clone, Debug, Default)]
pub struct InputHistory {
    frames: VecDeque<HistoryEntry>,
}

impl InputHistory {
    /// At most this many frames are kept (about four seconds at 60 Hz);
    /// the oldest go first.
    pub const CAPACITY: usize = 256;

    /// The number of frames held.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Whether no frame is held.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// The tick of the oldest frame held.
    pub fn oldest_tick(&self) -> Option<u64> {
        self.frames.front().map(|entry| entry.frame.tick)
    }

    /// Adds the newest frame.
    fn push(&mut self, entry: HistoryEntry) {
        self.frames.push_back(entry);
        while self.frames.len() > Self::CAPACITY {
            self.frames.pop_front();
        }
    }

    /// Forgets every frame up to and including `tick`; returns the one at
    /// `tick`, if it was held.
    fn acknowledge(&mut self, tick: u64) -> Option<HistoryEntry> {
        let mut acked = None;
        while self
            .frames
            .front()
            .is_some_and(|entry| entry.frame.tick <= tick)
        {
            acked = self.frames.pop_front().filter(|e| e.frame.tick == tick);
        }
        acked
    }
}

/// What the newest snapshot said about this player's own movers.
#[derive(Clone, Debug)]
struct Authority {
    /// The newest of this client's frames the server had applied.
    ack: Option<u64>,
    /// Each owned mover's translation and state on the server.
    states: Vec<(NetId, Vec3, Mover)>,
}

/// A client's prediction state, held by [`super::NetClient`] while
/// prediction is on.
#[derive(Debug, Default)]
pub(crate) struct Prediction {
    history: InputHistory,
    /// From the newest snapshot not yet reconciled with.
    authority: Option<Authority>,
    /// The tick of the newest snapshot observed; older ones are ignored.
    newest: Option<u64>,
    /// How many reconciliations corrected something.
    corrections: u64,
}

impl Prediction {
    /// The unacknowledged input.
    pub(crate) fn history(&self) -> &InputHistory {
        &self.history
    }

    /// How many times a snapshot disagreed with the prediction.
    pub(crate) fn corrections(&self) -> u64 {
        self.corrections
    }

    /// Notes what `snap` says about `player`'s own movers, for the next
    /// [`Self::tick`]. A snapshot older than one already observed is
    /// ignored.
    pub(crate) fn observe(&mut self, snap: &Snapshot, player: PlayerId) {
        if self.newest.is_some_and(|tick| snap.tick <= tick) {
            return;
        }
        self.newest = Some(snap.tick);
        let ack = snap
            .last_input_ticks
            .iter()
            .find(|(p, _)| *p == player)
            .map(|&(_, tick)| tick);
        let states = snap
            .entities
            .iter()
            .filter(|e| e.owner == Some(player))
            .filter_map(|e| {
                let mover = e.mover?;
                Some((e.id, Vec3::from(e.transform.translation), mover))
            })
            .collect();
        self.authority = Some(Authority { ack, states });
    }

    /// One client tick: reconciles with the newest snapshot observed (if
    /// any), then predicts `frame` on every [`Predicted`] entity.
    pub(crate) fn tick(&mut self, world: &mut World, ids: &NetIds, frame: &InputFrame, dt: f32) {
        let statics = Statics::collect(world);
        self.reconcile(world, ids, &statics, dt);
        self.predict(world, frame, &statics, dt);
    }

    /// Puts every predicted entity the server disagrees with back at the
    /// server's state and replays the unacknowledged frames on it.
    fn reconcile(&mut self, world: &mut World, ids: &NetIds, statics: &Statics, dt: f32) {
        let Some(Authority { ack, states }) = self.authority.take() else {
            return;
        };
        // Until the server has applied a frame of ours, its state says
        // nothing about our input.
        let Some(ack) = ack else {
            return;
        };
        let acked = self.history.acknowledge(ack);
        let mut wrong = Vec::new();
        for (id, at, mover) in states {
            let Some(entity) = ids.entity(id) else {
                continue;
            };
            if world.get::<&Predicted>(entity).is_err() {
                continue;
            }
            let predicted = acked
                .as_ref()
                .and_then(|entry| entry.poses.iter().find(|pose| pose.entity == entity));
            if predicted.is_some_and(|pose| agrees(pose, at, &mover)) {
                continue;
            }
            if let Ok((transform, current)) =
                world.query_one_mut::<(&mut Transform, &mut Mover)>(entity)
            {
                log::debug!(
                    "net: correcting predicted {id:?} from {} to the server's {at}",
                    transform.translation
                );
                transform.translation = at;
                *current = mover;
                wrong.push(entity);
            }
        }
        if wrong.is_empty() {
            return;
        }
        self.corrections += 1;
        for entry in &mut self.history.frames {
            let input = MoveInput::from_frame(&entry.frame);
            for &entity in &wrong {
                let Some(pose) = step(world, entity, input, dt, statics) else {
                    continue;
                };
                match entry.poses.iter_mut().find(|p| p.entity == entity) {
                    Some(slot) => *slot = pose,
                    None => entry.poses.push(pose),
                }
            }
        }
    }

    /// Steps every [`Predicted`] mover with `frame` and records it.
    fn predict(&mut self, world: &mut World, frame: &InputFrame, statics: &Statics, dt: f32) {
        let mut entities: Vec<Entity> = world
            .query::<(Entity, &Predicted, &Transform, &Mover)>()
            .iter()
            .map(|(entity, ..)| entity)
            .collect();
        entities.sort_by_key(|entity| entity.to_bits());
        let input = MoveInput::from_frame(frame);
        let poses = entities
            .into_iter()
            .filter_map(|entity| step(world, entity, input, dt, statics))
            .collect();
        self.history.push(HistoryEntry {
            frame: frame.clone(),
            poses,
        });
    }
}

/// Steps `entity`'s [`Mover`] once with `input`; its new state, if it has
/// a mover and a transform.
fn step(
    world: &mut World,
    entity: Entity,
    input: MoveInput,
    dt: f32,
    statics: &Statics,
) -> Option<Pose> {
    let (transform, mover) = world
        .query_one_mut::<(&mut Transform, &mut Mover)>(entity)
        .ok()?;
    transform.translation = mover.step(transform.translation, input, dt, statics);
    Some(Pose {
        entity,
        at: transform.translation,
        mover: *mover,
    })
}

/// Whether a predicted `pose` matches the server's `at` and `mover`
/// within [`EPSILON`] (settings must match exactly).
fn agrees(pose: &Pose, at: Vec3, mover: &Mover) -> bool {
    let settings = |m: &Mover| (m.speed, m.jump_impulse, m.gravity, m.foot);
    pose.at.distance(at) <= EPSILON
        && (pose.mover.vertical_velocity - mover.vertical_velocity).abs() <= EPSILON
        && settings(&pose.mover) == settings(mover)
}

#[cfg(test)]
mod tests;
