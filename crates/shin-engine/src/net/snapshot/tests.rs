//! Tests of [`capture`] and [`apply`].

use std::path::PathBuf;

use glam::Vec3;
use serde_json::{Value, json};

use super::*;
use crate::ecs::Children;
use crate::headless::Headless;
use crate::net::{PlayerId, Snapshot};
use crate::project::{self, NewProjectOptions};
use crate::tool::{ToolOutput, ToolRegistry};

/// A world-less resources set for capture-only tests.
fn bare() -> (World, Resources) {
    (World::new(), Resources::new())
}

fn at(x: f32) -> Transform {
    Transform::from_translation(Vec3::new(x, 0.0, 0.0))
}

/// An entity snapshot with the given id, parent, x position, and mesh.
fn snap(id: u64, parent: Option<u64>, x: f32, mesh: Option<&str>) -> EntitySnap {
    EntitySnap {
        id: NetId(id),
        parent: parent.map(NetId),
        name: Some(format!("e{id}")),
        transform: TransformDesc::from_transform(&at(x)),
        mesh: mesh.map(str::to_string),
        color: mesh.map(|_| [0.5, 0.25, 1.0, 1.0]),
        texture: None,
        owner: None,
        mover: None,
    }
}

/// The client entity for `id`.
fn entity_of(engine: &Headless, id: u64) -> Option<Entity> {
    engine
        .resources
        .get::<NetIds>()
        .and_then(|ids| ids.entity(NetId(id)))
        .filter(|&e| engine.world.contains(e))
}

fn apply_to(engine: &mut Headless, snap: &Snapshot) -> anyhow::Result<()> {
    apply(
        &mut engine.world,
        &mut engine.resources,
        &engine.gpu.device,
        &engine.gpu.queue,
        snap,
    )
}

/// `entity.list` with the given row fields removed from every row.
fn list_without(reg: &ToolRegistry, engine: &mut Headless, drop: &[&str]) -> Vec<Value> {
    let Ok(ToolOutput::Json(Value::Array(rows))) = reg.call(engine, "entity.list", json!({}))
    else {
        panic!("entity.list failed");
    };
    rows.into_iter()
        .map(|mut row| {
            for field in drop {
                row.as_object_mut().unwrap().remove(*field);
            }
            row
        })
        .collect()
}

#[test]
fn capture_assigns_ids_to_replicated_entities_only() {
    let (mut world, mut res) = bare();
    let a = world.spawn((at(1.0), Replicated, Name("a".into()), Owner(PlayerId(4))));
    world.spawn((at(2.0),));
    let c = world.spawn((at(3.0), Replicated));
    world.spawn((Replicated,)); // no transform: not captured

    let snapshot = capture(&world, &mut res, 7);
    assert_eq!(snapshot.tick, 7);
    assert_eq!(snapshot.removed, []);
    let ids: Vec<_> = snapshot.entities.iter().map(|e| e.id).collect();
    assert_eq!(ids, [NetId(1), NetId(2)]);
    let net = res.expect::<NetIds>();
    assert_eq!((net.id(a), net.id(c)), (Some(NetId(1)), Some(NetId(2))));
    let first = &snapshot.entities[0];
    assert_eq!(first.name.as_deref(), Some("a"));
    assert_eq!(first.owner, Some(PlayerId(4)));
    assert_eq!(first.transform.translation, [1.0, 0.0, 0.0]);
    assert_eq!(first.mesh, None);

    let again = capture(&world, &mut res, 8);
    assert_eq!(again.entities, snapshot.entities, "ids are stable");
}

#[test]
fn capture_of_a_hierarchy_preserves_parent_links() -> anyhow::Result<()> {
    let (mut world, mut res) = bare();
    let root = world.spawn((at(1.0), Replicated));
    let child = world.spawn((at(2.0), Replicated));
    let grandchild = world.spawn((at(3.0), Replicated));
    set_parent(&mut world, child, Some(root), false)?;
    set_parent(&mut world, grandchild, Some(child), false)?;

    let snapshot = capture(&world, &mut res, 0);
    let ids = res.expect::<NetIds>();
    let id = |e| ids.id(e).unwrap();
    let parents: Vec<_> = snapshot.entities.iter().map(|e| (e.id, e.parent)).collect();
    assert_eq!(
        parents,
        [
            (id(root), None),
            (id(child), Some(id(root))),
            (id(grandchild), Some(id(child))),
        ]
    );
    let local: Vec<_> = snapshot
        .entities
        .iter()
        .map(|e| e.transform.translation[0])
        .collect();
    assert_eq!(
        local,
        [1.0, 2.0, 3.0],
        "transforms stay local to the parent"
    );
    Ok(())
}

#[test]
fn a_child_of_an_unreplicated_parent_is_sent_as_a_root_in_world_space() -> anyhow::Result<()> {
    let (mut world, mut res) = bare();
    let parent = world.spawn((at(10.0),));
    let child = world.spawn((at(1.0), Replicated));
    set_parent(&mut world, child, Some(parent), false)?;
    let snapshot = capture(&world, &mut res, 0);
    assert_eq!(snapshot.entities[0].parent, None);
    assert_eq!(snapshot.entities[0].transform.translation, [11.0, 0.0, 0.0]);
    Ok(())
}

#[test]
fn capture_lists_entities_that_stopped_being_replicated_once() -> anyhow::Result<()> {
    let (mut world, mut res) = bare();
    let a = world.spawn((at(1.0), Replicated));
    let b = world.spawn((at(2.0), Replicated));
    let c = world.spawn((at(3.0), Replicated));
    capture(&world, &mut res, 0);
    world.despawn(a)?;
    world.remove_one::<Replicated>(b)?;

    let snapshot = capture(&world, &mut res, 1);
    assert_eq!(snapshot.removed, [NetId(1), NetId(2)]);
    assert_eq!(snapshot.entities.len(), 1);
    assert_eq!(res.expect::<NetIds>().id(c), Some(NetId(3)));
    assert_eq!(capture(&world, &mut res, 2).removed, [], "reported once");

    world.insert_one(b, Replicated)?;
    let back = capture(&world, &mut res, 3);
    assert_eq!(
        res.expect::<NetIds>().id(b),
        Some(NetId(4)),
        "a returning entity gets a fresh id"
    );
    assert_eq!(back.entities.len(), 2);
    Ok(())
}

#[test]
fn apply_spawns_then_updates_then_despawns() -> anyhow::Result<()> {
    let mut client = Headless::new(8, 8)?;
    let mut first = Snapshot {
        tick: 1,
        entities: vec![
            snap(5, None, 1.0, Some("builtin://cube")),
            snap(6, Some(5), 2.0, None),
        ],
        removed: vec![],
        last_input_ticks: Vec::new(),
    };
    apply_to(&mut client, &first)?;
    let (root, child) = (
        entity_of(&client, 5).unwrap(),
        entity_of(&client, 6).unwrap(),
    );
    assert!(client.world.get::<&Replicated>(root).is_ok());
    assert_eq!(parent_of(&client.world, child), Some(root));
    assert_eq!(world_transform(&client.world, child).translation.x, 3.0);
    let instance = *client.world.get::<&MeshInstance>(root)?;
    assert_eq!(instance.color, [0.5, 0.25, 1.0, 1.0]);
    assert!(client.world.get::<&MeshInstance>(child).is_err());

    // Update: move the root, drop its name, give the child a mesh and owner.
    first.tick = 2;
    first.entities[0].transform.translation = [4.0, 0.0, 0.0];
    first.entities[0].name = None;
    first.entities[1].mesh = Some("builtin://sphere".into());
    first.entities[1].owner = Some(PlayerId(9));
    apply_to(&mut client, &first)?;
    assert_eq!(entity_of(&client, 5), Some(root), "updated in place");
    assert_eq!(client.world.get::<&Transform>(root)?.translation.x, 4.0);
    assert!(client.world.get::<&Name>(root).is_err());
    assert!(client.world.get::<&MeshInstance>(child).is_ok());
    assert_eq!(*client.world.get::<&Owner>(child)?, Owner(PlayerId(9)));
    assert_eq!(client.world.len(), 2);

    // A snapshot that does not mention an entity leaves it alone.
    let other = Snapshot {
        tick: 3,
        entities: vec![snap(7, None, 0.0, None)],
        removed: vec![],
        last_input_ticks: Vec::new(),
    };
    apply_to(&mut client, &other)?;
    assert_eq!(client.world.len(), 3);
    assert!(entity_of(&client, 5).is_some());
    Ok(())
}

#[test]
fn removed_despawns_the_entity_and_its_children() -> anyhow::Result<()> {
    let mut client = Headless::new(8, 8)?;
    let snapshot = Snapshot {
        tick: 1,
        entities: vec![
            snap(1, None, 0.0, Some("builtin://cube")),
            snap(2, Some(1), 0.0, None),
            snap(3, Some(2), 0.0, None),
            snap(4, None, 0.0, None),
        ],
        removed: vec![],
        last_input_ticks: Vec::new(),
    };
    apply_to(&mut client, &snapshot)?;
    assert_eq!(client.world.len(), 4);

    let remove = Snapshot {
        tick: 2,
        entities: vec![],
        removed: vec![NetId(1), NetId(99)],
        last_input_ticks: Vec::new(),
    };
    apply_to(&mut client, &remove)?;
    assert_eq!(
        client.world.len(),
        1,
        "1, its child, and its grandchild are gone"
    );
    let ids = client.resources.expect::<NetIds>();
    assert_eq!(ids.len(), 1);
    assert!(ids.entity(NetId(4)).is_some());
    for id in 1..=3 {
        assert_eq!(ids.entity(NetId(id)), None, "{id} forgotten");
    }
    Ok(())
}

#[test]
fn applying_the_same_snapshot_twice_changes_nothing() -> anyhow::Result<()> {
    let mut client = Headless::new(8, 8)?;
    let snapshot = Snapshot {
        tick: 1,
        entities: vec![
            snap(3, Some(1), 2.0, None), // listed before its parent
            snap(1, None, 1.0, Some("builtin://cube")),
        ],
        removed: vec![NetId(50)],
        last_input_ticks: Vec::new(),
    };
    let reg = ToolRegistry::with_engine_tools();
    apply_to(&mut client, &snapshot)?;
    let once = list_without(&reg, &mut client, &[]);
    let meshes = client
        .resources
        .expect::<RenderState>()
        .registry
        .iter()
        .count();
    apply_to(&mut client, &snapshot)?;
    assert_eq!(list_without(&reg, &mut client, &[]), once);
    assert_eq!(
        client
            .resources
            .expect::<RenderState>()
            .registry
            .iter()
            .count(),
        meshes,
        "no mesh uploaded twice"
    );
    let child = entity_of(&client, 3).unwrap();
    let root = entity_of(&client, 1).unwrap();
    assert_eq!(client.world.get::<&Children>(root)?.0, [child]);
    Ok(())
}

#[test]
fn a_mesh_that_does_not_load_fails_and_changes_nothing() -> anyhow::Result<()> {
    let mut client = Headless::new(8, 8)?;
    let snapshot = Snapshot {
        tick: 1,
        entities: vec![
            snap(1, None, 0.0, Some("builtin://cube")),
            snap(2, None, 0.0, Some("builtin://teapot")),
        ],
        removed: vec![],
        last_input_ticks: Vec::new(),
    };
    let err = apply_to(&mut client, &snapshot).unwrap_err();
    assert!(format!("{err:#}").contains("builtin://teapot"), "{err:#}");
    assert_eq!(client.world.len(), 0);
    assert!(
        client.resources.get::<Assets>().is_some(),
        "Assets put back"
    );
    Ok(())
}

/// A fresh, missing directory under the system temp dir.
fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("shin-net-test-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// GE-040 acceptance: the starter scene, captured with every root
/// replicated and applied to an empty world, lists the same as the
/// original, apart from entity ids and what snapshots do not carry
/// (scripts, audio, physics).
#[test]
fn starter_scene_round_trips_through_a_snapshot() -> anyhow::Result<()> {
    let dir = temp_dir("starter");
    project::create(&NewProjectOptions {
        name: "net-starter".into(),
        dir: dir.clone(),
        engine: Default::default(),
    })?;
    let mut server = Headless::open_project(&dir)?;
    let roots: Vec<Entity> = server
        .world
        .query::<(Entity, &Transform)>()
        .iter()
        .map(|(e, _)| e)
        .filter(|&e| parent_of(&server.world, e).is_none())
        .collect();
    assert!(
        roots.len() > 5,
        "the starter has ground, cube, moon, crates"
    );
    for &root in &roots {
        server.world.insert_one(root, Replicated)?;
    }
    let snapshot = capture(&server.world, &mut server.resources, 0);
    let wire = crate::net::encode(&crate::net::Message::Snapshot(snapshot));
    let crate::net::Message::Snapshot(received) = crate::net::decode(&wire)? else {
        panic!("expected a snapshot");
    };

    let mut client = Headless::new(8, 8)?;
    client.resources.insert(Assets::new(dir.canonicalize()?));
    apply_to(&mut client, &received)?;

    let reg = ToolRegistry::with_engine_tools();
    let ignore = [
        "entity", "parent", "net_id", "script", "audio", "body", "collider",
    ];
    let want = list_without(&reg, &mut server, &ignore);
    let got = list_without(&reg, &mut client, &ignore);
    assert_eq!(got.len(), roots.len());
    assert_eq!(got, want);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
