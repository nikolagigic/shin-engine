//! Physics tools (GE-034): `physics.set_body`, `physics.set_collider`,
//! `physics.gravity`, and `physics.raycast`. The first three are recorded
//! by the command history (undoable); the raycast only reads.
//!
//! Entities are identified by [`Entity::to_bits`], as with the `entity.*`
//! tools.

use anyhow::Context as _;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{BodyKind, Collider, ColliderShape, PhysicsWorld, RigidBody, ShapeDesc, raycast};
use crate::command::Affected;
use crate::ecs::{Entity, Name, World};
use crate::headless::Headless;
use crate::math::Vec3;
use crate::tool::{ToolOutput, ToolRegistry, TypedTool};

/// Registers the `physics.*` tools.
pub fn register_tools(reg: &mut ToolRegistry) {
    reg.register(
        TypedTool::new(
            "physics.set_body",
            "Makes a root entity a rigid body: `kind` dynamic (falls and collides), kinematic \
             (follows its transform and pushes dynamic bodies), or fixed (never moves); null \
             removes the body. `mass` in kg applies to dynamic bodies (default: the current \
             mass, or 1). Bodies on child entities are ignored by the simulation. Undoable. \
             Returns the entity's body.",
            physics_set_body,
        )
        .with_affected(named_entity),
    );
    reg.register(
        TypedTool::new(
            "physics.set_collider",
            "Gives a root entity a collision shape, in its own units (multiplied by its \
             scale): {\"cuboid\": [hx, hy, hz]} half extents, {\"ball\": radius}, or \
             {\"capsule\": {\"half_height\": h, \"radius\": r}} along +Y; null removes the \
             collider. `friction` (0 or more, default 0.5) and `restitution` (0..1, default \
             0) keep their current values when omitted. A collider without a body is fixed in \
             place. Undoable. Returns the entity's collider.",
            physics_set_collider,
        )
        .with_affected(named_entity),
    );
    reg.register(
        TypedTool::new(
            "physics.gravity",
            "Sets the gravity vector in m/s² (default [0, -9.81, 0]); without `gravity`, \
             only reports it. Undoable. Returns `gravity`.",
            physics_gravity,
        )
        .with_affected(gravity_affected),
    );
    reg.register(TypedTool::new(
        "physics.raycast",
        "Casts a ray from `origin` along `dir` (need not be normalised) up to `max` metres \
         (default 1000) against the colliders of root entities at their current transforms, \
         in edit or play mode. Returns `hit` (bool) and, on a hit, the nearest `entity`, its \
         `name`, the world-space `point`, and the `distance`.",
        physics_raycast,
    ));
}

/// [`Affected`] for tools whose `entity` argument names the one entity
/// they change.
fn named_entity(_: &Headless, args: &Value) -> Affected {
    let entity = args
        .get("entity")
        .and_then(Value::as_u64)
        .and_then(Entity::from_bits);
    Affected::Entities(entity.into_iter().collect())
}

/// The live entity with tool id `id`, or an error naming the id.
fn find_entity(world: &World, id: u64) -> anyhow::Result<Entity> {
    Entity::from_bits(id)
        .filter(|&entity| world.contains(entity))
        .with_context(|| format!("no entity with id {id}"))
}

/// `body` as tools report it.
pub(crate) fn body_json(body: &RigidBody) -> Value {
    match body.kind {
        BodyKind::Dynamic => json!({ "kind": body.kind.label(), "mass": body.mass }),
        kind => json!({ "kind": kind.label() }),
    }
}

/// `collider` as tools report it.
pub(crate) fn collider_json(collider: &Collider) -> Value {
    json!({
        "shape": ShapeDesc::from(collider.shape),
        "friction": collider.friction,
        "restitution": collider.restitution,
    })
}

/// Arguments of `physics.set_body`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetBodyArgs {
    /// Entity id, as returned by entity.spawn or entity.list.
    entity: u64,
    /// `dynamic`, `kinematic`, or `fixed`; null (or omitted) removes the
    /// body.
    kind: Option<BodyKind>,
    /// Mass in kilograms (dynamic bodies), positive; default the current
    /// mass, or 1.
    mass: Option<f32>,
}

fn physics_set_body(engine: &mut Headless, args: SetBodyArgs) -> anyhow::Result<ToolOutput> {
    let entity = find_entity(&engine.world, args.entity)?;
    let current = engine.world.get::<&RigidBody>(entity).ok().map(|b| *b);
    let body = match args.kind {
        None => {
            anyhow::ensure!(
                args.mass.is_none(),
                "`mass` needs a `kind`; to remove the body, pass kind null alone"
            );
            let _ = engine.world.remove_one::<RigidBody>(entity);
            Value::Null
        }
        Some(kind) => {
            let body = RigidBody {
                kind,
                mass: args
                    .mass
                    .or(current.map(|b| b.mass))
                    .unwrap_or(RigidBody::DEFAULT_MASS),
            };
            body.validate()?;
            engine.world.insert_one(entity, body)?;
            body_json(&body)
        }
    };
    Ok(ToolOutput::Json(
        json!({ "entity": args.entity, "body": body }),
    ))
}

/// Arguments of `physics.set_collider`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetColliderArgs {
    /// Entity id, as returned by entity.spawn or entity.list.
    entity: u64,
    /// The shape in the entity's own units; null (or omitted) removes the
    /// collider.
    shape: Option<ShapeDesc>,
    /// Friction, 0 or more; default the current friction, or 0.5.
    friction: Option<f32>,
    /// Restitution (bounciness), 0 to 1; default the current restitution,
    /// or 0.
    restitution: Option<f32>,
}

fn physics_set_collider(
    engine: &mut Headless,
    args: SetColliderArgs,
) -> anyhow::Result<ToolOutput> {
    let entity = find_entity(&engine.world, args.entity)?;
    let current = engine.world.get::<&Collider>(entity).ok().map(|c| *c);
    let collider = match args.shape {
        None => {
            anyhow::ensure!(
                args.friction.is_none() && args.restitution.is_none(),
                "`friction` and `restitution` need a `shape`; to remove the collider, pass \
                 shape null alone"
            );
            let _ = engine.world.remove_one::<Collider>(entity);
            Value::Null
        }
        Some(shape) => {
            let collider = Collider {
                shape: ColliderShape::from(shape),
                friction: args
                    .friction
                    .or(current.map(|c| c.friction))
                    .unwrap_or(Collider::DEFAULT_FRICTION),
                restitution: args
                    .restitution
                    .or(current.map(|c| c.restitution))
                    .unwrap_or(Collider::DEFAULT_RESTITUTION),
            };
            collider.validate()?;
            engine.world.insert_one(entity, collider)?;
            collider_json(&collider)
        }
    };
    Ok(ToolOutput::Json(
        json!({ "entity": args.entity, "collider": collider }),
    ))
}

/// Arguments of `physics.gravity`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GravityArgs {
    /// New gravity `[x, y, z]` in m/s²; omit to only read it.
    gravity: Option<[f32; 3]>,
}

/// `physics.gravity` changes the gravity only when given one.
fn gravity_affected(_: &Headless, args: &Value) -> Affected {
    match args.get("gravity") {
        Some(value) if !value.is_null() => Affected::Gravity,
        _ => Affected::None,
    }
}

fn physics_gravity(engine: &mut Headless, args: GravityArgs) -> anyhow::Result<ToolOutput> {
    if engine.resources.get::<PhysicsWorld>().is_none() {
        engine.resources.insert(PhysicsWorld::default());
    }
    let physics = engine.resources.expect_mut::<PhysicsWorld>();
    if let Some(gravity) = args.gravity {
        let gravity = Vec3::from(gravity);
        anyhow::ensure!(gravity.is_finite(), "gravity must be finite, got {gravity}");
        physics.set_gravity(gravity);
    }
    Ok(ToolOutput::Json(
        json!({ "gravity": physics.gravity().to_array() }),
    ))
}

/// Arguments of `physics.raycast`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RaycastArgs {
    /// World-space start of the ray `[x, y, z]`.
    origin: [f32; 3],
    /// Direction `[x, y, z]`; any non-zero length.
    dir: [f32; 3],
    /// Longest distance to look, in metres; default 1000.
    #[serde(default = "default_max")]
    max: f32,
}

fn default_max() -> f32 {
    1000.0
}

fn physics_raycast(engine: &mut Headless, args: RaycastArgs) -> anyhow::Result<ToolOutput> {
    let hit = raycast(
        &engine.world,
        Vec3::from(args.origin),
        Vec3::from(args.dir),
        args.max,
    )?;
    let Some(hit) = hit else {
        return Ok(ToolOutput::Json(json!({ "hit": false })));
    };
    let name = engine
        .world
        .get::<&Name>(hit.entity)
        .ok()
        .map(|name| name.0.clone());
    Ok(ToolOutput::Json(json!({
        "hit": true,
        "entity": hit.entity.to_bits().get(),
        "name": name,
        "point": hit.point.to_array(),
        "distance": hit.distance,
    })))
}
