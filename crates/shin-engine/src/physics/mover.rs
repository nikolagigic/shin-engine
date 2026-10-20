//! Player movement (GE-048): the [`Mover`] component, which walks an
//! entity over the ground with its owner's input and lets it jump.
//!
//! A mover is a small kinematic character, not a rigid body: each fixed
//! step [`Mover::step`] slides it horizontally at [`Mover::speed`] times
//! the `move_x` and `move_z` axes, applies its own gravity, starts a jump
//! with [`Mover::jump_impulse`] when `jump` is held while standing, and
//! lands it on the highest **static** collider below it (a collider with
//! no [`RigidBody`] or a [`BodyKind::Fixed`] one, the ground). It does not
//! collide with walls, dynamic bodies, or other movers.
//!
//! # The movement contract
//!
//! [`Mover::step`] is a pure function of the mover's state (its
//! translation and [`Mover`] fields), the input, `dt`, and the static
//! colliders. Nothing else is read: no wall clock, no randomness, no other
//! entity. That is what lets a multiplayer client predict its own player
//! (`net::predict`): it runs the same function with the same input and
//! gets the same result the server will, and when the server disagrees it
//! can rewind to the server's state and replay its input from there.
//! Game code that moves an owned entity some other way (a script setting
//! its transform) runs on the server only and is not predicted: the client
//! sees it when the server's snapshot arrives, as a correction.
//!
//! [`mover_system`] runs movers on the server (and in a game that is not
//! networked), reading each [`Owner`]'s input from [`PlayerInputs`].

use rapier3d::geometry::{Ray, SharedShape};
use rapier3d::math::Pose;
use serde::{Deserialize, Serialize};

use super::{BodyKind, Collider, RigidBody, to_pose, to_vector};
use crate::ecs::{Entity, Resources, Time, World, parent_of};
use crate::headless::PlayState;
use crate::math::{Transform, Vec3};
use crate::net::{InputFrame, Owner, PlayerInputs, RemoteEntity};

/// Component: this entity walks with its [`Owner`]'s input and jumps.
///
/// See the [module docs](self) for what a step does and the movement
/// contract. The entity's [`Transform::translation`] is its centre;
/// [`Self::foot`] below it are its feet. Movers are replicated with their
/// entity, state included, so a client predicts with the server's numbers.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mover {
    /// Walking speed at full stick, in metres per second.
    pub speed: f32,
    /// Upward speed a jump starts with, in metres per second.
    pub jump_impulse: f32,
    /// Downward acceleration while airborne, in m/s² (positive).
    pub gravity: f32,
    /// From the entity's origin down to its feet, in metres: `0.5` for
    /// the unit cube.
    pub foot: f32,
    /// Current vertical speed, in metres per second (up is positive).
    /// State, not a setting: [`Self::step`] changes it.
    pub vertical_velocity: f32,
}

impl Mover {
    /// The gravity a mover gets from [`Self::new`], in m/s².
    pub const DEFAULT_GRAVITY: f32 = 9.81;
    /// The feet distance a mover gets from [`Self::new`]: the unit cube's.
    pub const DEFAULT_FOOT: f32 = 0.5;
    /// The action that walks along X (`-1..1`, +1 is +X).
    pub const MOVE_X: &str = "move_x";
    /// The action that walks along Z (`-1..1`, +1 is forward, -Z).
    pub const MOVE_Z: &str = "move_z";
    /// The action that jumps while held and standing.
    pub const JUMP: &str = "jump";
    /// How far above its feet a mover looks for ground, in metres: it
    /// steps up ledges lower than this.
    pub const STEP_UP: f32 = 0.05;
    /// A mover whose feet are within this of the ground stands on it.
    const GROUNDED: f32 = 1e-3;

    /// A mover with `speed` and `jump_impulse`, the default gravity and
    /// feet, at rest.
    pub fn new(speed: f32, jump_impulse: f32) -> Self {
        Self {
            speed,
            jump_impulse,
            gravity: Self::DEFAULT_GRAVITY,
            foot: Self::DEFAULT_FOOT,
            vertical_velocity: 0.0,
        }
    }

    /// Fails unless every setting is finite and none is negative.
    pub fn validate(&self) -> anyhow::Result<()> {
        let ok = [self.speed, self.jump_impulse, self.gravity, self.foot]
            .into_iter()
            .all(|x| x.is_finite() && x >= 0.0);
        anyhow::ensure!(ok, "mover settings must be 0 or more, got {self:?}");
        Ok(())
    }

    /// One step of `dt` seconds from `at` (the entity's centre) with
    /// `input`, landing on `statics`; returns the new centre and updates
    /// [`Self::vertical_velocity`].
    ///
    /// Horizontal first: `x += input.x * speed * dt`, `z -= input.z *
    /// speed * dt`. Then the ground under the new spot is the highest
    /// static surface at most [`Self::STEP_UP`] above the feet. Standing
    /// on it (feet within a millimetre, not rising), a held jump sets the
    /// vertical speed to [`Self::jump_impulse`]. Gravity then pulls, and a
    /// mover that would sink below the ground stands on it, at rest.
    /// Without ground it falls forever.
    pub fn step(&mut self, at: Vec3, input: MoveInput, dt: f32, statics: &Statics) -> Vec3 {
        let mut next = at;
        next.x += input.x * self.speed * dt;
        next.z -= input.z * self.speed * dt;

        let feet = at.y - self.foot;
        let ground = statics.ground(Vec3::new(next.x, feet + Self::STEP_UP, next.z));
        let standing =
            self.vertical_velocity <= 0.0 && ground.is_some_and(|g| feet - g <= Self::GROUNDED);
        if standing {
            self.vertical_velocity = if input.jump { self.jump_impulse } else { 0.0 };
        }
        self.vertical_velocity -= self.gravity * dt;
        next.y += self.vertical_velocity * dt;
        if let Some(ground) = ground
            && next.y - self.foot <= ground
        {
            next.y = ground + self.foot;
            self.vertical_velocity = 0.0;
        }
        next
    }
}

/// What a [`Mover`] does this step: the `move_x` and `move_z` axes and
/// whether `jump` is held.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MoveInput {
    /// `move_x`, `-1..1`.
    pub x: f32,
    /// `move_z`, `-1..1`; +1 is forward (-Z).
    pub z: f32,
    /// `jump` is held.
    pub jump: bool,
}

impl MoveInput {
    /// `player`'s input this tick, as the server holds it.
    pub fn from_players(inputs: &PlayerInputs, player: crate::net::PlayerId) -> Self {
        Self {
            x: inputs.axis(player, Mover::MOVE_X),
            z: inputs.axis(player, Mover::MOVE_Z),
            jump: inputs.pressed(player, Mover::JUMP),
        }
    }

    /// The input `frame` carries, as the client sent it.
    pub fn from_frame(frame: &InputFrame) -> Self {
        let value = |name: &str| {
            frame
                .actions
                .iter()
                .find(|(action, _)| action == name)
                .map(|(_, value)| *value)
                .unwrap_or_default()
        };
        Self {
            x: value(Mover::MOVE_X).axis,
            z: value(Mover::MOVE_Z).axis,
            jump: value(Mover::JUMP).pressed,
        }
    }
}

/// The static colliders of a world, gathered once for many [`Mover`]
/// steps: every root entity's [`Collider`] with no [`RigidBody`] or a
/// [`BodyKind::Fixed`] one, and no [`Mover`].
pub struct Statics {
    shapes: Vec<(SharedShape, Pose)>,
}

impl Statics {
    /// Gathers `world`'s static colliders at their current transforms.
    pub fn collect(world: &World) -> Self {
        let mut found: Vec<(Entity, Collider, Transform)> = world
            .query::<(Entity, &Transform, &Collider, Option<&RigidBody>)>()
            .without::<&Mover>()
            .iter()
            .filter(|(_, _, _, body)| body.is_none_or(|b| b.kind == BodyKind::Fixed))
            .map(|(entity, transform, collider, _)| (entity, *collider, *transform))
            .collect();
        found.retain(|(entity, ..)| parent_of(world, *entity).is_none());
        found.sort_by_key(|(entity, ..)| entity.to_bits());
        let shapes = found
            .into_iter()
            .map(|(_, collider, transform)| {
                (
                    collider.shape.scaled(transform.scale).shared(),
                    to_pose(&transform),
                )
            })
            .collect();
        Self { shapes }
    }

    /// The height of the highest surface straight below `from`, if any.
    /// A collider `from` is inside does not count.
    fn ground(&self, from: Vec3) -> Option<f32> {
        let ray = Ray::new(to_vector(from), to_vector(Vec3::NEG_Y));
        self.shapes
            .iter()
            .filter_map(|(shape, pose)| shape.cast_ray(pose, &ray, GROUND_RANGE, true))
            .filter(|&distance| distance > 0.0)
            .min_by(f32::total_cmp)
            .map(|distance| from.y - distance)
    }
}

/// How far below a mover's feet ground is looked for, in metres.
const GROUND_RANGE: f32 = 1000.0;

/// Fixed system: steps every [`Mover`] (except on entities replicated
/// from a server, [`RemoteEntity`]) by `Time::fixed_dt` with its
/// [`Owner`]'s input from the [`PlayerInputs`] resource; a mover with no
/// owner, or no such resource, gets no input (it still falls).
///
/// Runs only while playing and not paused, like [`super::physics_system`];
/// put it after the systems that read input and before physics. Static
/// colliders are gathered once per run ([`Statics`]).
pub fn mover_system(world: &mut World, res: &mut Resources) {
    if !matches!(
        res.get::<PlayState>(),
        None | Some(PlayState::Playing { paused: false, .. })
    ) {
        return;
    }
    let dt = res
        .get::<Time>()
        .map_or(1.0 / 60.0, |time| time.fixed_dt as f32);
    let statics = Statics::collect(world);
    let inputs = res.get::<PlayerInputs>();
    for (transform, mover, owner) in world
        .query_mut::<(&mut Transform, &mut Mover, Option<&Owner>)>()
        .without::<&RemoteEntity>()
    {
        let input = match (owner, inputs) {
            (Some(owner), Some(inputs)) => MoveInput::from_players(inputs, owner.0),
            _ => MoveInput::default(),
        };
        transform.translation = mover.step(transform.translation, input, dt, &statics);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::ActionValue;
    use crate::physics::ColliderShape;

    const DT: f32 = 1.0 / 60.0;

    /// A world with the starter's ground: a 20 x 0.5 x 20 slab whose top
    /// is at y = 0.
    fn ground() -> World {
        let mut world = World::new();
        world.spawn((
            Transform {
                translation: Vec3::new(0.0, -0.25, 0.0),
                scale: Vec3::new(20.0, 0.5, 20.0),
                ..Default::default()
            },
            Collider::new(ColliderShape::Cuboid(Vec3::splat(0.5))),
        ));
        world
    }

    #[test]
    fn walks_at_its_speed_and_stays_on_the_ground() {
        let statics = Statics::collect(&ground());
        let mut mover = Mover::new(3.0, 5.0);
        let mut at = Vec3::new(0.0, 0.5, 0.0);
        let input = MoveInput {
            x: 1.0,
            z: 1.0,
            jump: false,
        };
        for _ in 0..60 {
            at = mover.step(at, input, DT, &statics);
        }
        assert!((at.x - 3.0).abs() < 1e-4, "{at}");
        assert!((at.z + 3.0).abs() < 1e-4, "+z input is forward, -Z: {at}");
        assert_eq!(at.y, 0.5, "standing exactly on the ground");
        assert_eq!(mover.vertical_velocity, 0.0);
    }

    #[test]
    fn a_jump_rises_and_lands_again() {
        let statics = Statics::collect(&ground());
        let mut mover = Mover::new(3.0, 5.0);
        let jump = MoveInput {
            jump: true,
            ..Default::default()
        };
        let mut at = mover.step(Vec3::new(0.0, 0.5, 0.0), jump, DT, &statics);
        assert!(at.y > 0.5 && mover.vertical_velocity > 0.0, "{at}");
        // Holding jump in the air does nothing more.
        let mut top: f32 = at.y;
        for _ in 0..120 {
            at = mover.step(at, jump, DT, &statics);
            top = top.max(at.y);
            if at.y == 0.5 {
                break;
            }
        }
        // v²/2g = 25 / 19.62 ≈ 1.27 m above the start.
        assert!((top - 0.5 - 1.27).abs() < 0.1, "apex {top}");
        assert_eq!(at.y, 0.5, "landed");
    }

    #[test]
    fn falls_onto_the_ground_and_off_its_edge() {
        let statics = Statics::collect(&ground());
        let mut mover = Mover::new(3.0, 5.0);
        let mut at = Vec3::new(0.0, 3.0, 0.0);
        for _ in 0..120 {
            at = mover.step(at, MoveInput::default(), DT, &statics);
        }
        assert_eq!(at.y, 0.5, "landed from a drop");
        let mut off = Vec3::new(10.5, 0.5, 0.0);
        for _ in 0..30 {
            off = mover.step(off, MoveInput::default(), DT, &statics);
        }
        assert!(off.y < 0.0, "beyond the slab there is no ground: {off}");
    }

    #[test]
    fn only_static_colliders_are_ground() {
        let mut world = ground();
        // A dynamic crate and another mover under the start: not ground.
        world.spawn((
            Transform::from_translation(Vec3::new(0.0, 1.0, 0.0)),
            Collider::new(ColliderShape::Cuboid(Vec3::splat(0.5))),
            RigidBody::new(BodyKind::Dynamic),
        ));
        world.spawn((
            Transform::from_translation(Vec3::new(0.0, 2.0, 0.0)),
            Collider::new(ColliderShape::Cuboid(Vec3::splat(0.4))),
            Mover::new(1.0, 1.0),
        ));
        let statics = Statics::collect(&world);
        let mut mover = Mover::new(3.0, 5.0);
        let mut at = Vec3::new(0.0, 3.0, 0.0);
        for _ in 0..120 {
            at = mover.step(at, MoveInput::default(), DT, &statics);
        }
        assert_eq!(at.y, 0.5, "fell through both onto the ground");
    }

    #[test]
    fn input_reads_the_same_from_a_frame_and_from_players() {
        let held = |axis| ActionValue {
            pressed: true,
            just_pressed: false,
            axis,
        };
        let frame = InputFrame {
            tick: 3,
            actions: vec![("jump".into(), held(1.0)), ("move_x".into(), held(-0.5))],
        };
        let want = MoveInput {
            x: -0.5,
            z: 0.0,
            jump: true,
        };
        assert_eq!(MoveInput::from_frame(&frame), want);
        let player = crate::net::PlayerId(1);
        let mut inputs = PlayerInputs::new();
        inputs.begin_tick(&[player], vec![(player, frame)], None);
        assert_eq!(MoveInput::from_players(&inputs, player), want);
    }
}
