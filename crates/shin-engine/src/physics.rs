//! Rigid-body physics with rapier3d (GE-034): things fall, stack, and
//! collide.
//!
//! Two components describe physics on an entity:
//!
//! - [`RigidBody`]: how the entity moves. [`BodyKind::Dynamic`] bodies are
//!   moved by gravity and contacts; [`BodyKind::Kinematic`] bodies follow
//!   their [`Transform`] (set by scripts or systems) and push dynamic
//!   bodies out of the way; [`BodyKind::Fixed`] bodies never move.
//! - [`Collider`]: the entity's shape for contacts and [`raycast`], with
//!   its friction and restitution (bounciness). A collider without a body
//!   is fixed in place.
//!
//! Units are metres, kilograms, and seconds; gravity defaults to
//! `(0, -9.81, 0)`. Collider sizes are in the entity's own units and are
//! multiplied by its [`Transform::scale`], so `Cuboid(0.5, 0.5, 0.5)` fits
//! the unit cube mesh at any scale. A ball takes the largest scale axis,
//! and a capsule (upright, along local +Y) its Y scale for the height and
//! the larger of X and Z for the radius.
//!
//! [`physics_system`] runs in the fixed step. It mirrors every root
//! entity that has a `RigidBody` or a `Collider` into the [`PhysicsWorld`]
//! resource (creating bodies the first time it sees them, rebuilding them
//! when their components or scale change, and teleporting them when
//! something else moved their transform), steps the simulation by the
//! fixed `dt`, and writes the new position and rotation of each dynamic
//! body back into its `Transform`. Bodies attach to **root entities
//! only**: a `RigidBody` or `Collider` on a child is ignored, with a
//! warning logged once per entity.
//!
//! Like scripts, physics runs only while the
//! [`PlayState`] resource is playing and not paused, and always in a world
//! with no `PlayState` (a game outside the editor). Bodies are created
//! from the components when play starts and torn down when it stops
//! ([`PhysicsWorld::clear`]); the play snapshot restores the transforms.
//!
//! Not supported (yet): joints, character controllers, trigger events,
//! continuous collision detection, and colliders on children (compound
//! shapes).
//!
//! rapier3d works in its own math types (glam 0.33 through `glamx`), which
//! this module converts at the boundary; none of them appear in its public
//! API.
//!
//! Tools: [`register_tools`] adds `physics.set_body`,
//! `physics.set_collider`, `physics.gravity`, and `physics.raycast`.

mod tools;

use std::collections::{HashMap, HashSet};

use rapier3d::dynamics::{RigidBodyBuilder, RigidBodyHandle};
use rapier3d::geometry::{ColliderBuilder, ColliderHandle, Ray, SharedShape};
use rapier3d::math::{Pose, Rotation, Vector};
use rapier3d::pipeline::PhysicsWorld as RapierWorld;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use tools::register_tools;
pub(crate) use tools::{body_json, collider_json};

use crate::ecs::{Entity, Resources, Time, World, parent_of};
use crate::headless::PlayState;
use crate::math::{Quat, Transform, Vec3};

/// The gravity a new [`PhysicsWorld`] starts with, in m/s².
pub const DEFAULT_GRAVITY: Vec3 = Vec3::new(0.0, -9.81, 0.0);

/// How a [`RigidBody`] moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum BodyKind {
    /// Moved by gravity and contacts; its transform is written back every
    /// step.
    Dynamic,
    /// Follows its transform (moved by scripts or systems) and pushes
    /// dynamic bodies; not affected by gravity or contacts.
    Kinematic,
    /// Never moves.
    Fixed,
}

impl BodyKind {
    /// `"dynamic"`, `"kinematic"`, or `"fixed"`, as scene files and tools
    /// write it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Dynamic => "dynamic",
            Self::Kinematic => "kinematic",
            Self::Fixed => "fixed",
        }
    }
}

/// Component: this root entity is a rigid body.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RigidBody {
    /// How it moves.
    pub kind: BodyKind,
    /// Mass in kilograms; used by [`BodyKind::Dynamic`] bodies only. Must
    /// be positive.
    pub mass: f32,
}

impl RigidBody {
    /// The mass a body gets when none is given, in kilograms.
    pub const DEFAULT_MASS: f32 = 1.0;

    /// A body of `kind` with [`Self::DEFAULT_MASS`].
    pub fn new(kind: BodyKind) -> Self {
        Self {
            kind,
            mass: Self::DEFAULT_MASS,
        }
    }

    /// Fails unless the mass is positive and finite.
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.mass.is_finite() && self.mass > 0.0,
            "body mass must be positive, got {}",
            self.mass
        );
        Ok(())
    }
}

/// The shape of a [`Collider`], in the entity's own units (see the
/// [module docs](self) for how scale applies).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ColliderShape {
    /// A box with these half extents.
    Cuboid(Vec3),
    /// A sphere with this radius.
    Ball(f32),
    /// A capsule along local +Y: a cylinder of `2 * half_height` capped by
    /// hemispheres of `radius`.
    Capsule {
        /// Half the length of the cylindrical part.
        half_height: f32,
        /// Radius of the cylinder and its caps.
        radius: f32,
    },
}

impl ColliderShape {
    /// Fails unless every dimension is positive and finite.
    pub fn validate(&self) -> anyhow::Result<()> {
        let positive = |x: f32| x.is_finite() && x > 0.0;
        let ok = match *self {
            Self::Cuboid(half) => half.to_array().into_iter().all(positive),
            Self::Ball(radius) => positive(radius),
            Self::Capsule {
                half_height,
                radius,
            } => positive(half_height) && positive(radius),
        };
        anyhow::ensure!(ok, "collider dimensions must be positive, got {self:?}");
        Ok(())
    }

    /// This shape with `scale` (an entity's transform scale) applied, as
    /// the simulation sees it.
    pub fn scaled(&self, scale: Vec3) -> Self {
        let scale = scale.abs();
        match *self {
            Self::Cuboid(half) => Self::Cuboid(half * scale),
            Self::Ball(radius) => Self::Ball(radius * scale.max_element()),
            Self::Capsule {
                half_height,
                radius,
            } => Self::Capsule {
                half_height: half_height * scale.y,
                radius: radius * scale.x.max(scale.z),
            },
        }
    }

    /// The full size of the shape's bounding box along each axis.
    pub fn extents(&self) -> Vec3 {
        match *self {
            Self::Cuboid(half) => half * 2.0,
            Self::Ball(radius) => Vec3::splat(radius * 2.0),
            Self::Capsule {
                half_height,
                radius,
            } => Vec3::new(radius, half_height + radius, radius) * 2.0,
        }
    }

    /// The rapier shape for `self` (already scaled).
    fn shared(&self) -> SharedShape {
        match *self {
            Self::Cuboid(half) => SharedShape::cuboid(half.x, half.y, half.z),
            Self::Ball(radius) => SharedShape::ball(radius),
            Self::Capsule {
                half_height,
                radius,
            } => SharedShape::capsule_y(half_height, radius),
        }
    }
}

/// A [`ColliderShape`] as scene files and tools write it: `cuboid((hx,
/// hy, hz))`, `ball(r)`, or `capsule(half_height: h, radius: r)` in RON;
/// `{"cuboid": [hx, hy, hz]}`, `{"ball": r}`, or `{"capsule":
/// {"half_height": h, "radius": r}}` in JSON.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ShapeDesc {
    /// A box with these half extents `[x, y, z]`.
    Cuboid([f32; 3]),
    /// A sphere with this radius.
    Ball(f32),
    /// A capsule along local +Y.
    Capsule {
        /// Half the length of the cylindrical part.
        half_height: f32,
        /// Radius of the cylinder and its caps.
        radius: f32,
    },
}

impl From<ShapeDesc> for ColliderShape {
    fn from(desc: ShapeDesc) -> Self {
        match desc {
            ShapeDesc::Cuboid(half) => Self::Cuboid(Vec3::from(half)),
            ShapeDesc::Ball(radius) => Self::Ball(radius),
            ShapeDesc::Capsule {
                half_height,
                radius,
            } => Self::Capsule {
                half_height,
                radius,
            },
        }
    }
}

impl From<ColliderShape> for ShapeDesc {
    fn from(shape: ColliderShape) -> Self {
        match shape {
            ColliderShape::Cuboid(half) => Self::Cuboid(half.to_array()),
            ColliderShape::Ball(radius) => Self::Ball(radius),
            ColliderShape::Capsule {
                half_height,
                radius,
            } => Self::Capsule {
                half_height,
                radius,
            },
        }
    }
}

/// Component: this root entity collides, with this shape.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Collider {
    /// The shape, in the entity's own units.
    pub shape: ColliderShape,
    /// Friction coefficient, 0 or more; 0 is ice.
    pub friction: f32,
    /// Restitution (bounciness), 0 to 1; 0 does not bounce, 1 bounces
    /// back to its starting height.
    pub restitution: f32,
}

impl Collider {
    /// The friction a collider gets when none is given.
    pub const DEFAULT_FRICTION: f32 = 0.5;
    /// The restitution a collider gets when none is given.
    pub const DEFAULT_RESTITUTION: f32 = 0.0;

    /// A collider of `shape` with the default friction and restitution.
    pub fn new(shape: ColliderShape) -> Self {
        Self {
            shape,
            friction: Self::DEFAULT_FRICTION,
            restitution: Self::DEFAULT_RESTITUTION,
        }
    }

    /// Fails unless the shape is valid, the friction is 0 or more, and the
    /// restitution is between 0 and 1.
    pub fn validate(&self) -> anyhow::Result<()> {
        self.shape.validate()?;
        anyhow::ensure!(
            self.friction.is_finite() && self.friction >= 0.0,
            "collider friction must be 0 or more, got {}",
            self.friction
        );
        anyhow::ensure!(
            (0.0..=1.0).contains(&self.restitution),
            "collider restitution must be between 0 and 1, got {}",
            self.restitution
        );
        Ok(())
    }
}

/// What the simulation holds for one entity, and what it was built from.
struct Tracked {
    body: Option<RigidBodyHandle>,
    collider: Option<ColliderHandle>,
    source: Source,
    /// The transform the simulation last put the entity at (or found it
    /// at); a different transform means something else moved it.
    written: Transform,
}

/// The components and scale a [`Tracked`] entry was built from; any change
/// rebuilds it.
#[derive(Clone, Copy, PartialEq)]
struct Source {
    body: Option<RigidBody>,
    collider: Option<Collider>,
    scale: Vec3,
}

/// Resource: the rapier simulation and which entity each of its bodies
/// belongs to. [`physics_system`] keeps it in step with the world;
/// [`crate::headless::Headless`] installs one.
pub struct PhysicsWorld {
    rapier: RapierWorld,
    tracked: HashMap<Entity, Tracked>,
    /// Children already warned about (bodies on children are ignored).
    warned: HashSet<Entity>,
}

impl Default for PhysicsWorld {
    /// An empty simulation with [`DEFAULT_GRAVITY`].
    fn default() -> Self {
        let mut rapier = RapierWorld::default();
        rapier.gravity = to_vector(DEFAULT_GRAVITY);
        Self {
            rapier,
            tracked: HashMap::new(),
            warned: HashSet::new(),
        }
    }
}

impl PhysicsWorld {
    /// The gravity applied to dynamic bodies, in m/s².
    pub fn gravity(&self) -> Vec3 {
        from_vector(self.rapier.gravity)
    }

    /// Sets the gravity applied to dynamic bodies, in m/s².
    pub fn set_gravity(&mut self, gravity: Vec3) {
        self.rapier.gravity = to_vector(gravity);
    }

    /// The number of entities currently simulated (with a body, a
    /// collider, or both).
    pub fn len(&self) -> usize {
        self.tracked.len()
    }

    /// Whether nothing is simulated.
    pub fn is_empty(&self) -> bool {
        self.tracked.is_empty()
    }

    /// Removes every body and collider, keeping the gravity. The next
    /// [`physics_system`] run creates them again from the components.
    pub fn clear(&mut self) {
        let gravity = self.rapier.gravity;
        self.rapier = RapierWorld::default();
        self.rapier.gravity = gravity;
        self.tracked.clear();
        self.warned.clear();
    }

    /// Makes the simulation match `world`'s physics components (see the
    /// [module docs](self)).
    fn sync(&mut self, world: &World) {
        let mut wanted: HashMap<Entity, (Source, Transform)> = HashMap::new();
        for (entity, transform, body, collider) in world
            .query::<(Entity, &Transform, Option<&RigidBody>, Option<&Collider>)>()
            .iter()
        {
            if body.is_none() && collider.is_none() {
                continue;
            }
            if parent_of(world, entity).is_some() {
                if self.warned.insert(entity) {
                    log::warn!(
                        "entity {} has a rigid body or collider but is a child; physics only \
                         simulates root entities",
                        entity.to_bits()
                    );
                }
                continue;
            }
            let source = Source {
                body: body.copied(),
                collider: collider.copied(),
                scale: transform.scale,
            };
            wanted.insert(entity, (source, *transform));
        }

        let stale: Vec<Entity> = self
            .tracked
            .iter()
            .filter(|(entity, tracked)| {
                wanted
                    .get(entity)
                    .is_none_or(|(source, _)| *source != tracked.source)
            })
            .map(|(entity, _)| *entity)
            .collect();
        for entity in stale {
            self.remove(entity);
        }

        for (entity, (source, transform)) in wanted {
            match self.tracked.get_mut(&entity) {
                None => {
                    let tracked = self.create(source, &transform);
                    self.tracked.insert(entity, tracked);
                }
                Some(tracked) => {
                    let moved = tracked.written.translation != transform.translation
                        || tracked.written.rotation != transform.rotation;
                    if !moved {
                        continue;
                    }
                    tracked.written = transform;
                    let pose = to_pose(&transform);
                    if let Some(body) = tracked.body.and_then(|h| self.rapier.bodies.get_mut(h)) {
                        if body.is_kinematic() {
                            body.set_next_kinematic_position(pose);
                        } else {
                            body.set_position(pose, true);
                        }
                    } else if let Some(collider) = tracked
                        .collider
                        .and_then(|h| self.rapier.colliders.get_mut(h))
                    {
                        collider.set_position(pose);
                    }
                }
            }
        }
    }

    /// Builds the rapier body and collider for `source` at `transform`.
    fn create(&mut self, source: Source, transform: &Transform) -> Tracked {
        let pose = to_pose(transform);
        let dynamic_mass = source
            .body
            .filter(|b| b.kind == BodyKind::Dynamic)
            .map(|b| b.mass);
        let body = source.body.map(|body| {
            let builder = match body.kind {
                BodyKind::Dynamic => RigidBodyBuilder::dynamic(),
                BodyKind::Kinematic => RigidBodyBuilder::kinematic_position_based(),
                BodyKind::Fixed => RigidBodyBuilder::fixed(),
            }
            .pose(pose);
            // With a collider the collider carries the mass (and the
            // rotational inertia that goes with its shape).
            let builder = match (dynamic_mass, source.collider) {
                (Some(mass), None) => builder.additional_mass(mass),
                _ => builder,
            };
            self.rapier.insert_body(builder)
        });
        let collider = source.collider.map(|collider| {
            let mut builder = ColliderBuilder::new(collider.shape.scaled(source.scale).shared())
                .friction(collider.friction)
                .restitution(collider.restitution);
            if let Some(mass) = dynamic_mass {
                builder = builder.mass(mass);
            }
            if body.is_none() {
                builder = builder.position(pose);
            }
            self.rapier.insert_collider(builder, body)
        });
        Tracked {
            body,
            collider,
            source,
            written: *transform,
        }
    }

    /// Removes `entity`'s body and collider from the simulation.
    fn remove(&mut self, entity: Entity) {
        let Some(tracked) = self.tracked.remove(&entity) else {
            return;
        };
        if let Some(body) = tracked.body {
            // Also removes the attached collider.
            self.rapier.remove_body(body);
        } else if let Some(collider) = tracked.collider {
            self.rapier.remove_collider(collider);
        }
    }

    /// Advances the simulation by `dt` seconds.
    fn step(&mut self, dt: f32) {
        self.rapier.integration_parameters.dt = dt;
        self.rapier.step();
    }

    /// Writes each dynamic body's position and rotation into its entity's
    /// [`Transform`] (scale is kept).
    fn write_back(&mut self, world: &mut World) {
        for (&entity, tracked) in &mut self.tracked {
            let Some(body) = tracked.body.and_then(|h| self.rapier.bodies.get(h)) else {
                continue;
            };
            if !body.is_dynamic() {
                continue;
            }
            let Ok(mut transform) = world.get::<&mut Transform>(entity) else {
                continue;
            };
            let (translation, rotation) = from_pose(body.position());
            transform.translation = translation;
            transform.rotation = rotation;
            tracked.written = *transform;
        }
    }
}

/// Fixed system: syncs the [`PhysicsWorld`] resource (inserting one if
/// missing) with the world's [`RigidBody`] and [`Collider`] components,
/// steps it by `Time::fixed_dt`, and writes dynamic bodies' transforms
/// back.
///
/// Does nothing while paused. While editing it tears the simulation down
/// ([`PhysicsWorld::clear`]) instead, so the next play starts from the
/// components. See the [module docs](self).
pub fn physics_system(world: &mut World, res: &mut Resources) {
    match res.get::<PlayState>() {
        None | Some(PlayState::Playing { paused: false, .. }) => {}
        Some(PlayState::Edit) => {
            if let Some(physics) = res.get_mut::<PhysicsWorld>() {
                physics.clear();
            }
            return;
        }
        Some(PlayState::Playing { paused: true, .. }) => return,
    }
    let dt = res
        .get::<Time>()
        .map_or(1.0 / 60.0, |time| time.fixed_dt as f32);
    if res.get::<PhysicsWorld>().is_none() {
        res.insert(PhysicsWorld::default());
    }
    let physics = res.expect_mut::<PhysicsWorld>();
    physics.sync(world);
    physics.step(dt);
    physics.write_back(world);
}

/// Where a ray hit a collider.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayHit {
    /// The entity whose collider was hit.
    pub entity: Entity,
    /// The world-space point hit.
    pub point: Vec3,
    /// Distance from the ray's origin to `point`.
    pub distance: f32,
}

/// The nearest collider (on a root entity) that a ray from `origin` along
/// `dir` hits within `max` metres, using the colliders' current
/// transforms. Works whether or not the simulation is running. A ray that
/// starts inside a collider hits it at distance 0. Ties go to the lower
/// entity id.
///
/// Fails if `dir` is zero or not finite, or `max` is negative.
pub fn raycast(world: &World, origin: Vec3, dir: Vec3, max: f32) -> anyhow::Result<Option<RayHit>> {
    let dir = dir
        .try_normalize()
        .ok_or_else(|| anyhow::anyhow!("ray direction must be a non-zero vector, got {dir}"))?;
    anyhow::ensure!(max >= 0.0, "ray length must be 0 or more, got {max}");
    let ray = Ray::new(to_vector(origin), to_vector(dir));
    let mut best: Option<RayHit> = None;
    for (entity, transform, collider) in world.query::<(Entity, &Transform, &Collider)>().iter() {
        if parent_of(world, entity).is_some() {
            continue;
        }
        let shape = collider.shape.scaled(transform.scale).shared();
        let Some(distance) = shape.cast_ray(&to_pose(transform), &ray, max, true) else {
            continue;
        };
        let closer = best.is_none_or(|b| {
            distance < b.distance
                || (distance == b.distance && entity.to_bits() < b.entity.to_bits())
        });
        if closer {
            best = Some(RayHit {
                entity,
                point: origin + dir * distance,
                distance,
            });
        }
    }
    Ok(best)
}

// Conversions between the engine's glam and rapier's (an older glam).

fn to_vector(v: Vec3) -> Vector {
    Vector::from_array(v.to_array())
}

fn from_vector(v: Vector) -> Vec3 {
    Vec3::from_array(v.to_array())
}

fn to_pose(t: &Transform) -> Pose {
    let rotation = Rotation::from_array(t.rotation.normalize().to_array());
    Pose::from_parts(to_vector(t.translation), rotation)
}

fn from_pose(pose: &Pose) -> (Vec3, Quat) {
    (
        from_vector(pose.translation),
        Quat::from_array(pose.rotation.to_array()),
    )
}

#[cfg(test)]
mod tests;
