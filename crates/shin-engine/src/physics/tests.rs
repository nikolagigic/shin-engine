use serde_json::{Value, json};

use super::*;
use crate::command;
use crate::ecs::{Name, set_parent};
use crate::headless::Headless;
use crate::scene::SceneFile;
use crate::tool::{ToolError, ToolOutput, ToolRegistry};

const DT: f64 = 1.0 / 60.0;

/// Runs `steps` fixed steps of [`DT`] with only [`physics_system`], in a
/// world with no `PlayState` (so physics always runs).
fn run(world: &mut World, res: &mut Resources, steps: u32) {
    for _ in 0..steps {
        if res.get::<Time>().is_none() {
            res.insert(Time::default());
        }
        res.expect_mut::<Time>().advance_fixed(DT);
        physics_system(world, res);
    }
}

fn cuboid(half: f32) -> Collider {
    Collider::new(ColliderShape::Cuboid(Vec3::splat(half)))
}

/// A fixed ground slab whose top face is y = 0: the unit cube scaled to
/// 20 x 0.5 x 20, with the unit cube's collider.
fn spawn_ground(world: &mut World) -> Entity {
    world.spawn((
        Transform::from_trs(
            Vec3::new(0.0, -0.25, 0.0),
            Quat::IDENTITY,
            Vec3::new(20.0, 0.5, 20.0),
        ),
        RigidBody::new(BodyKind::Fixed),
        cuboid(0.5),
    ))
}

fn translation(world: &World, entity: Entity) -> Vec3 {
    world.get::<&Transform>(entity).unwrap().translation
}

/// The ticket's settle test: a dynamic unit cube dropped onto a fixed
/// ground comes to rest with its centre at y = 0.5 ± 0.01 within 2 s.
#[test]
fn dynamic_cube_settles_on_fixed_ground() {
    let mut world = World::new();
    let mut res = Resources::new();
    spawn_ground(&mut world);
    let cube = world.spawn((
        Transform::from_translation(Vec3::new(0.0, 2.0, 0.0)),
        RigidBody::new(BodyKind::Dynamic),
        cuboid(0.5),
    ));

    run(&mut world, &mut res, 30);
    let falling = translation(&world, cube).y;
    assert!(falling < 2.0, "the cube falls: y = {falling}");

    run(&mut world, &mut res, 90);
    let settled = translation(&world, cube);
    assert!(
        (settled.y - 0.5).abs() <= 0.01,
        "the cube rests on the ground: {settled}"
    );
    assert!(
        settled.with_y(0.0).length() < 0.01,
        "and straight down: {settled}"
    );
    assert_eq!(res.expect::<PhysicsWorld>().len(), 2);
}

/// Scale multiplies collider sizes: a cube scaled 2x with the unit-cube
/// collider rests at y = 1.
#[test]
fn collider_size_follows_the_entity_scale() {
    let mut world = World::new();
    let mut res = Resources::new();
    spawn_ground(&mut world);
    let big = world.spawn((
        Transform::from_trs(Vec3::new(0.0, 3.0, 0.0), Quat::IDENTITY, Vec3::splat(2.0)),
        RigidBody::new(BodyKind::Dynamic),
        cuboid(0.5),
    ));
    run(&mut world, &mut res, 150);
    let y = translation(&world, big).y;
    assert!((y - 1.0).abs() <= 0.01, "y = {y}");
    let transform = *world.get::<&Transform>(big).unwrap();
    assert_eq!(transform.scale, Vec3::splat(2.0), "scale is kept");
}

/// The ticket's collision test: with gravity off, a kinematic ball driven
/// along +X runs into a dynamic ball; they touch, the dynamic one is pushed
/// away along +X, and once the kinematic ball stops they separate.
#[test]
fn two_spheres_collide_and_separate() {
    let mut world = World::new();
    let mut res = Resources::new();
    let mut physics = PhysicsWorld::default();
    physics.set_gravity(Vec3::ZERO);
    res.insert(physics);
    let ball = Collider::new(ColliderShape::Ball(0.5));
    let target = world.spawn((
        Transform::default(),
        RigidBody::new(BodyKind::Dynamic),
        ball,
    ));
    let pusher = world.spawn((
        Transform::from_translation(Vec3::new(-3.0, 0.0, 0.0)),
        RigidBody::new(BodyKind::Kinematic),
        ball,
    ));

    // 3 m/s for one second: the pusher would end at x = 0 if nothing
    // were in the way.
    for _ in 0..60 {
        world.get::<&mut Transform>(pusher).unwrap().translation.x += 3.0 / 60.0;
        run(&mut world, &mut res, 1);
        let gap = translation(&world, target).x - translation(&world, pusher).x;
        assert!(gap > 0.95, "the balls never pass through each other: {gap}");
    }
    let hit = translation(&world, target);
    assert!(hit.x > 0.05, "the dynamic ball was pushed along +X: {hit}");
    assert!(hit.y.abs() < 1e-3 && hit.z.abs() < 1e-3, "head-on: {hit}");

    // The pusher stops; the pushed ball keeps going.
    run(&mut world, &mut res, 60);
    let gap = translation(&world, target).x - translation(&world, pusher).x;
    assert!(gap > 1.5, "they separate: centres {gap} apart");
}

/// Something other than the simulation moving a dynamic body (a tool, a
/// script) teleports it there.
#[test]
fn moving_a_body_teleports_it() {
    let mut world = World::new();
    let mut res = Resources::new();
    spawn_ground(&mut world);
    let cube = world.spawn((
        Transform::from_translation(Vec3::new(0.0, 0.5, 0.0)),
        RigidBody::new(BodyKind::Dynamic),
        cuboid(0.5),
    ));
    run(&mut world, &mut res, 10);
    world.get::<&mut Transform>(cube).unwrap().translation = Vec3::new(3.0, 5.0, 0.0);
    run(&mut world, &mut res, 1);
    let moved = translation(&world, cube);
    assert!(
        (moved.x - 3.0).abs() < 1e-3 && moved.y > 4.9,
        "it is where it was put, starting to fall: {moved}"
    );
}

/// Changing a component rebuilds the body: removing the body makes it stop
/// being simulated, and a fixed body does not fall.
#[test]
fn component_changes_rebuild_the_body() {
    let mut world = World::new();
    let mut res = Resources::new();
    let cube = world.spawn((
        Transform::from_translation(Vec3::new(0.0, 5.0, 0.0)),
        RigidBody::new(BodyKind::Dynamic),
        cuboid(0.5),
    ));
    run(&mut world, &mut res, 10);
    let y = translation(&world, cube).y;
    assert!(y < 5.0);

    world
        .insert_one(cube, RigidBody::new(BodyKind::Fixed))
        .unwrap();
    run(&mut world, &mut res, 10);
    assert_eq!(translation(&world, cube).y, y, "fixed bodies do not move");

    world.remove_one::<RigidBody>(cube).unwrap();
    world.remove_one::<Collider>(cube).unwrap();
    run(&mut world, &mut res, 1);
    assert!(res.expect::<PhysicsWorld>().is_empty());
}

/// Bodies on children are ignored: the child stays where its parent puts
/// it.
#[test]
fn bodies_on_children_are_ignored() {
    let mut world = World::new();
    let mut res = Resources::new();
    let parent = world.spawn((Transform::from_translation(Vec3::new(0.0, 5.0, 0.0)),));
    let child = world.spawn((
        Transform::from_translation(Vec3::X),
        RigidBody::new(BodyKind::Dynamic),
        cuboid(0.5),
    ));
    set_parent(&mut world, child, Some(parent), false).unwrap();
    run(&mut world, &mut res, 30);
    assert_eq!(translation(&world, child), Vec3::X);
    assert!(res.expect::<PhysicsWorld>().is_empty());
    assert!(res.expect::<PhysicsWorld>().warned.contains(&child));
}

/// Paused, nothing moves; editing tears the simulation down.
#[test]
fn physics_follows_the_play_state() -> anyhow::Result<()> {
    let mut engine = Headless::new(4, 4)?;
    let cube = engine.world.spawn((
        Transform::from_translation(Vec3::new(0.0, 5.0, 0.0)),
        RigidBody::new(BodyKind::Dynamic),
        cuboid(0.5),
    ));
    engine.step(10);
    assert_eq!(
        translation(&engine.world, cube).y,
        5.0,
        "editing: no physics"
    );

    engine.play()?;
    engine.step(10);
    let y = translation(&engine.world, cube).y;
    assert!(y < 5.0, "playing: it falls");
    assert_eq!(engine.resources.expect::<PhysicsWorld>().len(), 1);

    engine.pause()?;
    engine.step(10);
    assert_eq!(translation(&engine.world, cube).y, y, "paused: no physics");

    engine.stop()?;
    assert!(engine.resources.expect::<PhysicsWorld>().is_empty());
    assert_eq!(
        translation(&engine.world, cube).y,
        5.0,
        "stop restores the transform"
    );
    Ok(())
}

/// The ticket's raycast test: the nearest collider along the ray is hit,
/// at the right point; misses and children report nothing.
#[test]
fn raycast_hits_the_right_entity() -> anyhow::Result<()> {
    let mut world = World::new();
    let near = world.spawn((
        Transform::from_translation(Vec3::new(0.0, 0.0, -3.0)),
        cuboid(0.5),
    ));
    let far = world.spawn((
        Transform::from_translation(Vec3::new(0.0, 0.0, -6.0)),
        Collider::new(ColliderShape::Ball(1.0)),
    ));
    let side = world.spawn((
        Transform::from_trs(Vec3::new(4.0, 0.0, -3.0), Quat::IDENTITY, Vec3::splat(2.0)),
        cuboid(0.5),
    ));

    let hit = raycast(&world, Vec3::ZERO, Vec3::NEG_Z, 100.0)?.expect("a hit");
    assert_eq!(hit.entity, near);
    assert!((hit.distance - 2.5).abs() < 1e-5, "{hit:?}");
    assert!(hit.point.distance(Vec3::new(0.0, 0.0, -2.5)) < 1e-5);

    // Remove the near one: the ball behind it is hit at its surface.
    world.despawn(near)?;
    let hit = raycast(&world, Vec3::ZERO, Vec3::new(0.0, 0.0, -10.0), 100.0)?.expect("a hit");
    assert_eq!(hit.entity, far);
    assert!((hit.distance - 5.0).abs() < 1e-5, "{hit:?}");

    // The scaled side cube spans x = 3..5.
    let hit = raycast(&world, Vec3::new(10.0, 0.0, -3.0), Vec3::NEG_X, 100.0)?.expect("a hit");
    assert_eq!(hit.entity, side);
    assert!((hit.distance - 5.0).abs() < 1e-5, "{hit:?}");

    assert_eq!(raycast(&world, Vec3::ZERO, Vec3::Y, 100.0)?, None, "a miss");
    assert_eq!(
        raycast(&world, Vec3::ZERO, Vec3::NEG_Z, 4.0)?,
        None,
        "out of range"
    );
    let parent = world.spawn((Transform::default(),));
    set_parent(&mut world, far, Some(parent), false)?;
    assert_eq!(
        raycast(&world, Vec3::ZERO, Vec3::NEG_Z, 100.0)?,
        None,
        "children are not simulated, so not hit"
    );
    assert!(raycast(&world, Vec3::ZERO, Vec3::ZERO, 1.0).is_err());
    Ok(())
}

#[test]
fn shapes_validate_and_scale() {
    assert!(ColliderShape::Ball(0.0).validate().is_err());
    assert!(
        ColliderShape::Cuboid(Vec3::new(1.0, -1.0, 1.0))
            .validate()
            .is_err()
    );
    assert!(
        ColliderShape::Capsule {
            half_height: 0.5,
            radius: f32::NAN
        }
        .validate()
        .is_err()
    );
    assert!(
        Collider {
            restitution: 1.5,
            ..cuboid(0.5)
        }
        .validate()
        .is_err()
    );
    assert!(
        RigidBody {
            kind: BodyKind::Dynamic,
            mass: 0.0
        }
        .validate()
        .is_err()
    );

    let scale = Vec3::new(2.0, 3.0, -4.0);
    assert_eq!(
        ColliderShape::Ball(0.5).scaled(scale),
        ColliderShape::Ball(2.0)
    );
    let capsule = ColliderShape::Capsule {
        half_height: 1.0,
        radius: 0.5,
    };
    assert_eq!(
        capsule.scaled(scale),
        ColliderShape::Capsule {
            half_height: 3.0,
            radius: 2.0
        }
    );
    assert_eq!(capsule.extents(), Vec3::new(1.0, 3.0, 1.0));
    assert_eq!(
        ColliderShape::Cuboid(Vec3::splat(0.5))
            .scaled(scale)
            .extents(),
        Vec3::new(2.0, 3.0, 4.0)
    );
}

// Tools, undo, scene files, and play mode, through the registry.

fn call(reg: &ToolRegistry, engine: &mut Headless, tool: &str, args: Value) -> Value {
    match command::apply(reg, engine, tool, args) {
        Ok(ToolOutput::Json(value)) => value,
        other => panic!("`{tool}` did not return JSON: {other:?}"),
    }
}

fn call_err(reg: &ToolRegistry, engine: &mut Headless, tool: &str, args: Value) -> String {
    match command::apply(reg, engine, tool, args) {
        Err(ToolError::Failed(err)) => format!("{err:#}"),
        Err(err) => err.to_string(),
        other => panic!("`{tool}` should have failed: {other:?}"),
    }
}

/// An engine with a cube mesh, a `ground` and a `crate` (both cubes), and
/// their tool ids.
fn tool_engine(reg: &ToolRegistry) -> anyhow::Result<(Headless, u64, u64)> {
    let mut engine = Headless::new(8, 8)?;
    reg.call(&mut engine, "mesh.builtin", json!({ "kind": "cube" }))?;
    let ground = call(
        reg,
        &mut engine,
        "entity.spawn",
        json!({ "mesh": 0, "name": "ground", "translation": [0, -0.25, 0], "scale": [20, 0.5, 20] }),
    )["entity"]
        .as_u64()
        .unwrap();
    let crate_ = call(
        reg,
        &mut engine,
        "entity.spawn",
        json!({ "mesh": 0, "name": "crate", "translation": [0, 3, 0] }),
    )["entity"]
        .as_u64()
        .unwrap();
    Ok((engine, ground, crate_))
}

fn entity(id: u64) -> Entity {
    Entity::from_bits(id).unwrap()
}

#[test]
fn physics_tools_are_listed_with_schemas() {
    let reg = ToolRegistry::with_engine_tools();
    for name in [
        "physics.set_body",
        "physics.set_collider",
        "physics.gravity",
        "physics.raycast",
    ] {
        let info = reg
            .list()
            .into_iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("{name} listed"));
        assert!(!info.description.is_empty());
        assert_eq!(info.input_schema["type"], "object", "{name}");
    }
}

/// The ticket's undo test, plus the tools' replies and errors.
#[test]
fn set_body_and_collider_are_undoable() -> anyhow::Result<()> {
    let reg = ToolRegistry::with_engine_tools();
    let (mut engine, _, crate_) = tool_engine(&reg)?;

    let reply = call(
        &reg,
        &mut engine,
        "physics.set_body",
        json!({ "entity": crate_, "kind": "dynamic", "mass": 2.5 }),
    );
    assert_eq!(reply["body"], json!({ "kind": "dynamic", "mass": 2.5 }));
    let reply = call(
        &reg,
        &mut engine,
        "physics.set_collider",
        json!({ "entity": crate_, "shape": { "cuboid": [0.5, 0.5, 0.5] }, "restitution": 0.25 }),
    );
    assert_eq!(
        reply["collider"],
        json!({ "shape": { "cuboid": [0.5, 0.5, 0.5] }, "friction": 0.5, "restitution": 0.25 })
    );
    // Omitted fields keep their values.
    call(
        &reg,
        &mut engine,
        "physics.set_body",
        json!({ "entity": crate_, "kind": "kinematic" }),
    );
    let body = *engine.world.get::<&RigidBody>(entity(crate_))?;
    assert_eq!(
        body,
        RigidBody {
            kind: BodyKind::Kinematic,
            mass: 2.5
        }
    );

    // Undo, one call at a time.
    command::undo(&mut engine)?;
    assert_eq!(
        engine.world.get::<&RigidBody>(entity(crate_))?.kind,
        BodyKind::Dynamic
    );
    command::undo(&mut engine)?;
    assert!(engine.world.get::<&Collider>(entity(crate_)).is_err());
    command::undo(&mut engine)?;
    assert!(
        engine.world.get::<&RigidBody>(entity(crate_)).is_err(),
        "undoing set_body removes the body"
    );
    command::redo(&mut engine)?;
    assert_eq!(
        *engine.world.get::<&RigidBody>(entity(crate_))?,
        RigidBody {
            kind: BodyKind::Dynamic,
            mass: 2.5
        }
    );

    // null removes, and is undoable too.
    let reply = call(
        &reg,
        &mut engine,
        "physics.set_body",
        json!({ "entity": crate_, "kind": null }),
    );
    assert_eq!(reply["body"], Value::Null);
    assert!(engine.world.get::<&RigidBody>(entity(crate_)).is_err());
    command::undo(&mut engine)?;
    assert!(engine.world.get::<&RigidBody>(entity(crate_)).is_ok());

    let err = call_err(
        &reg,
        &mut engine,
        "physics.set_body",
        json!({ "entity": crate_, "kind": "dynamic", "mass": -1 }),
    );
    assert!(err.contains("mass must be positive"), "{err}");
    let err = call_err(
        &reg,
        &mut engine,
        "physics.set_collider",
        json!({ "entity": crate_, "shape": { "ball": 0 } }),
    );
    assert!(err.contains("positive"), "{err}");
    let err = call_err(
        &reg,
        &mut engine,
        "physics.set_body",
        json!({ "entity": 999_999, "kind": "fixed" }),
    );
    assert!(err.contains("999999"), "{err}");
    let err = call_err(
        &reg,
        &mut engine,
        "physics.set_body",
        json!({ "entity": crate_, "kind": "floaty" }),
    );
    assert!(err.contains("floaty"), "{err}");
    Ok(())
}

#[test]
fn gravity_is_set_read_and_undone() -> anyhow::Result<()> {
    let reg = ToolRegistry::with_engine_tools();
    let mut engine = Headless::new(4, 4)?;
    assert_eq!(
        call(&reg, &mut engine, "physics.gravity", json!({}))["gravity"],
        json!([0.0, -9.81_f32, 0.0])
    );
    call(
        &reg,
        &mut engine,
        "physics.gravity",
        json!({ "gravity": [0, -1.62, 0] }),
    );
    assert_eq!(
        engine.resources.expect::<PhysicsWorld>().gravity(),
        Vec3::new(0.0, -1.62, 0.0)
    );
    command::undo(&mut engine)?;
    assert_eq!(
        engine.resources.expect::<PhysicsWorld>().gravity(),
        DEFAULT_GRAVITY
    );
    let history = engine.resources.expect::<command::History>();
    assert_eq!(history.undo_len(), 0, "reading gravity is not recorded");
    Ok(())
}

#[test]
fn raycast_tool_reports_the_hit() -> anyhow::Result<()> {
    let reg = ToolRegistry::with_engine_tools();
    let (mut engine, ground, _) = tool_engine(&reg)?;
    call(
        &reg,
        &mut engine,
        "physics.set_collider",
        json!({ "entity": ground, "shape": { "cuboid": [0.5, 0.5, 0.5] } }),
    );
    let hit = call(
        &reg,
        &mut engine,
        "physics.raycast",
        json!({ "origin": [1, 10, 2], "dir": [0, -2, 0] }),
    );
    assert_eq!(hit["hit"], true);
    assert_eq!(hit["entity"], ground);
    assert_eq!(hit["name"], "ground");
    assert_eq!(hit["point"], json!([1.0, 0.0, 2.0]));
    assert_eq!(hit["distance"], 10.0);
    let miss = call(
        &reg,
        &mut engine,
        "physics.raycast",
        json!({ "origin": [1, 10, 2], "dir": [0, 1, 0] }),
    );
    assert_eq!(miss, json!({ "hit": false }));
    Ok(())
}

/// The ticket's scene round trip: bodies and colliders are saved and
/// loaded, and a save of the loaded scene is byte for byte the same.
#[test]
fn scene_round_trip_keeps_bodies_and_colliders() -> anyhow::Result<()> {
    let reg = ToolRegistry::with_engine_tools();
    let (mut engine, ground, crate_) = tool_engine(&reg)?;
    for args in [
        json!({ "entity": ground, "kind": "fixed" }),
        json!({ "entity": crate_, "kind": "dynamic", "mass": 3 }),
    ] {
        call(&reg, &mut engine, "physics.set_body", args);
    }
    for args in [
        json!({ "entity": ground, "shape": { "cuboid": [0.5, 0.5, 0.5] }, "friction": 0.9 }),
        json!({ "entity": crate_, "shape": { "capsule": { "half_height": 0.25, "radius": 0.5 } },
                "restitution": 0.5 }),
    ] {
        call(&reg, &mut engine, "physics.set_collider", args);
    }
    let first = SceneFile::capture(&engine.world, &engine.resources)?.to_ron()?;
    assert!(first.contains("body: (\n"), "{first}");
    assert!(first.contains("kind: fixed"), "{first}");
    assert!(first.contains("mass: 3.0"), "{first}");
    assert!(
        first.contains("capsule(\n") && first.contains("half_height: 0.25"),
        "{first}"
    );

    let scene = SceneFile::from_ron(&first)?;
    crate::scene::apply(
        &mut engine.world,
        &mut engine.resources,
        &engine.gpu.device,
        &engine.gpu.queue,
        &scene,
    )?;
    let second = SceneFile::capture(&engine.world, &engine.resources)?.to_ron()?;
    assert_eq!(second, first);
    let (_, body, collider) = engine
        .world
        .query::<(&Name, &RigidBody, &Collider)>()
        .iter()
        .find(|(name, ..)| name.0 == "crate")
        .map(|(n, b, c)| (n.clone(), *b, *c))
        .expect("the crate is loaded");
    assert_eq!(
        body,
        RigidBody {
            kind: BodyKind::Dynamic,
            mass: 3.0
        }
    );
    assert_eq!(collider.restitution, 0.5);
    Ok(())
}

/// Hand-written physics fields: defaults fill in, and an invalid value is
/// an error naming the entity, leaving the world as it was.
#[test]
fn scene_physics_defaults_and_errors() -> anyhow::Result<()> {
    let mut engine = Headless::new(4, 4)?;
    let scene = SceneFile::from_ron(
        "(version: 1, entities: [(body: (kind: dynamic), collider: (shape: ball(0.5)))])",
    )?;
    crate::scene::apply(
        &mut engine.world,
        &mut engine.resources,
        &engine.gpu.device,
        &engine.gpu.queue,
        &scene,
    )?;
    let (body, collider) = engine
        .world
        .query::<(&RigidBody, &Collider)>()
        .iter()
        .map(|(b, c)| (*b, *c))
        .next()
        .expect("one entity");
    assert_eq!(body, RigidBody::new(BodyKind::Dynamic));
    assert_eq!(collider, Collider::new(ColliderShape::Ball(0.5)));

    let bad = SceneFile::from_ron(
        "(version: 1, entities: [(), (collider: (shape: cuboid((1, 0, 1))))])",
    )?;
    let err = crate::scene::apply(
        &mut engine.world,
        &mut engine.resources,
        &engine.gpu.device,
        &engine.gpu.queue,
        &bad,
    )
    .expect_err("a flat cuboid is invalid");
    assert!(format!("{err:#}").contains("entity 1"), "{err:#}");
    assert_eq!(engine.world.len(), 1, "the world is untouched");
    Ok(())
}
