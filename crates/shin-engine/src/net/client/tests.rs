//! Tests of the client: a loopback "server" that sends hand-built
//! snapshots, and one real server engine for physics.

use glam::Vec3;

use super::*;
use crate::headless::Headless;
use crate::net::{EntitySnap, LoopbackTransport, NetServer, PeerId, Replicated, net_server_system};
use crate::physics::{BodyKind, Collider, ColliderShape, PhysicsWorld, RigidBody};
use crate::scene::TransformDesc;
use crate::script::Script;

/// The server end of a loopback, driven by hand.
struct FakeServer(LoopbackTransport);

impl FakeServer {
    fn send(&mut self, msg: Message) {
        self.0.send(PeerId(1), &msg, Channel::Reliable);
    }

    /// Everything the client sent, dropping connection events.
    fn received(&mut self) -> Vec<Message> {
        self.0
            .poll()
            .into_iter()
            .filter_map(|event| match event {
                Event::Message(_, msg) => Some(msg),
                _ => None,
            })
            .collect()
    }

    fn welcome(&mut self, tick: u64) {
        self.send(Message::Welcome {
            player: PlayerId(3),
            tick,
            fixed_dt: 1.0 / 60.0,
        });
    }

    /// Sends a snapshot at `tick` of entities `(net id, x)`, removing
    /// `removed`.
    fn snapshot(&mut self, tick: u64, entities: &[(u64, f32)], removed: &[u64]) {
        self.send(Message::Snapshot(Snapshot {
            tick,
            entities: entities.iter().map(|&(id, x)| at(id, x)).collect(),
            removed: removed.iter().copied().map(NetId).collect(),
            last_input_ticks: Vec::new(),
        }));
    }
}

/// Entity `id` drawing a cube at `(x, 0, 0)`.
fn at(id: u64, x: f32) -> EntitySnap {
    EntitySnap {
        id: NetId(id),
        parent: None,
        name: Some(format!("e{id}")),
        transform: TransformDesc::from_transform(&Transform::from_translation(Vec3::X * x)),
        mesh: Some("builtin://cube".into()),
        color: Some([1.0, 0.5, 0.25, 1.0]),
        texture: None,
        owner: None,
        mover: None,
    }
}

/// A client engine joined to a fake server, with the client systems and
/// an interpolation delay of `delay` ticks. The hello and welcome are
/// already exchanged.
fn joined(delay: u64) -> anyhow::Result<(Headless, FakeServer)> {
    let (server, mut clients) = LoopbackTransport::pair(1);
    let mut engine = Headless::new(8, 8)?;
    let client = NetClient::with_delay(Box::new(clients.remove(0)), "ada", delay);
    engine.resources.insert(client);
    engine.schedule.add_fixed("net_client", net_client_system);
    engine
        .schedule
        .add_frame("net_interpolate", net_interpolate_system);
    let mut server = FakeServer(server);
    engine.step(1);
    let hello = server.received();
    assert_eq!(
        hello,
        [Message::Hello {
            protocol: PROTOCOL_VERSION,
            name: "ada".into()
        }]
    );
    server.welcome(9);
    engine.step(1);
    Ok((engine, server))
}

fn state(engine: &Headless) -> &ClientState {
    engine.resources.expect::<NetClient>().state()
}

/// The client entity for net id `id`, if it exists.
fn entity(engine: &Headless, id: u64) -> Option<Entity> {
    engine
        .resources
        .get::<NetIds>()
        .and_then(|ids| ids.entity(NetId(id)))
        .filter(|&e| engine.world.contains(e))
}

/// The x of net id `id`'s transform.
fn x_of(engine: &Headless, id: u64) -> f32 {
    let entity = entity(engine, id).expect("entity exists");
    engine
        .world
        .get::<&Transform>(entity)
        .unwrap()
        .translation
        .x
}

/// One client tick, then a frame (which interpolates); returns entity 1's x.
fn tick_and_render(engine: &mut Headless) -> f32 {
    engine.step(1);
    engine.render_to_target();
    x_of(engine, 1)
}

#[test]
fn hello_then_welcome_connects() -> anyhow::Result<()> {
    let (engine, _server) = joined(2)?;
    assert_eq!(
        state(&engine),
        &ClientState::Connected {
            player: PlayerId(3),
            server_tick: 9
        }
    );
    Ok(())
}

#[test]
fn a_client_applies_spawn_update_and_despawn() -> anyhow::Result<()> {
    let (mut engine, mut server) = joined(0)?;
    server.snapshot(10, &[(1, 0.0), (2, 5.0)], &[]);
    engine.step(1);
    let one = entity(&engine, 1).expect("spawned");
    assert_eq!((x_of(&engine, 1), x_of(&engine, 2)), (0.0, 5.0));
    for id in [1, 2] {
        let e = entity(&engine, id).unwrap();
        assert!(
            engine.world.get::<&RemoteEntity>(e).is_ok(),
            "{id} is remote"
        );
        assert!(engine.world.get::<&Replicated>(e).is_ok());
        assert!(engine.world.get::<&crate::render::MeshInstance>(e).is_ok());
    }
    assert_eq!(
        state(&engine),
        &ClientState::Connected {
            player: PlayerId(3),
            server_tick: 10
        }
    );

    server.snapshot(11, &[(1, 3.0)], &[2]);
    engine.step(1);
    assert_eq!(entity(&engine, 1), Some(one), "updated in place");
    assert_eq!(x_of(&engine, 1), 3.0);
    assert_eq!(entity(&engine, 2), None, "despawned");
    assert_eq!(engine.world.len(), 1);
    Ok(())
}

#[test]
fn the_delay_waits_for_newer_snapshots() -> anyhow::Result<()> {
    let (mut engine, mut server) = joined(2)?;
    server.snapshot(10, &[(1, 0.0)], &[]);
    engine.step(1);
    assert_eq!(entity(&engine, 1), None, "10 is not due until 12 arrives");
    server.snapshot(12, &[(1, 2.0)], &[]);
    engine.step(1);
    assert_eq!(x_of(&engine, 1), 0.0, "10 applied");
    let buffer = engine.resources.expect::<NetClient>().buffer();
    assert_eq!(
        (buffer.applied_tick(), buffer.newest_tick()),
        (Some(10), Some(12))
    );
    Ok(())
}

#[test]
fn interpolation_reaches_the_midpoint_halfway() -> anyhow::Result<()> {
    let (mut engine, mut server) = joined(2)?;
    server.snapshot(10, &[(1, 0.0)], &[]);
    server.snapshot(12, &[(1, 2.0)], &[]);
    assert_eq!(tick_and_render(&mut engine), 0.0, "at tick 10");
    // Half a tick on: the render alpha counts.
    engine.resources.expect_mut::<Time>().alpha = 0.5;
    net_interpolate_system(&mut engine.world, &mut engine.resources);
    assert_eq!(x_of(&engine, 1), 0.5);
    // One tick on: tick 11, the midpoint between 10 and 12.
    assert_eq!(tick_and_render(&mut engine), 1.0);
    assert_eq!(tick_and_render(&mut engine), 2.0, "at tick 12");
    Ok(())
}

#[test]
fn a_snapshot_gap_holds_the_last_pose() -> anyhow::Result<()> {
    let (mut engine, mut server) = joined(2)?;
    server.snapshot(10, &[(1, 0.0)], &[]);
    server.snapshot(12, &[(1, 2.0)], &[]);
    let mut xs = vec![tick_and_render(&mut engine)];
    // Tick 14's snapshot is lost: nothing arrives for four ticks.
    for _ in 0..4 {
        xs.push(tick_and_render(&mut engine));
    }
    assert_eq!(xs, [0.0, 1.0, 2.0, 2.0, 2.0], "holds at 12's pose");
    // 16 arrives; 12 becomes due and the entity moves on from where it was.
    server.snapshot(16, &[(1, 6.0)], &[]);
    for _ in 0..3 {
        xs.push(tick_and_render(&mut engine));
    }
    assert_eq!(&xs[5..], [2.0, 3.0, 4.0]);
    assert!(
        xs.windows(2).all(|w| (w[1] - w[0]).abs() <= 1.0),
        "no jumps: {xs:?}"
    );
    Ok(())
}

#[test]
fn skipped_snapshots_still_despawn() -> anyhow::Result<()> {
    let (mut engine, mut server) = joined(0)?;
    server.snapshot(10, &[(1, 0.0), (2, 0.0)], &[]);
    engine.step(1);
    assert!(entity(&engine, 2).is_some());
    // 11 removes 2, but 12 arrives in the same poll and is the one shown.
    server.snapshot(11, &[(1, 1.0)], &[2]);
    server.snapshot(12, &[(1, 2.0)], &[]);
    engine.step(1);
    assert_eq!(x_of(&engine, 1), 2.0);
    assert_eq!(entity(&engine, 2), None);
    Ok(())
}

#[test]
fn late_and_duplicate_snapshots_are_dropped() {
    let mut buffer = SnapshotBuffer::new(0);
    let snap = |tick| Snapshot {
        tick,
        ..Snapshot::default()
    };
    buffer.push(snap(5));
    buffer.push(snap(3));
    buffer.push(snap(5));
    assert_eq!(buffer.snapshots.len(), 2, "sorted, one copy each");
    buffer.advance();
    assert_eq!(buffer.applied_tick(), Some(5));
    buffer.push(snap(4));
    assert_eq!(buffer.newest_tick(), Some(5), "older than applied: dropped");
    assert_eq!(buffer.playhead(0.25), Some(5.25));
}

#[test]
fn goodbye_disconnects() -> anyhow::Result<()> {
    let (mut engine, mut server) = joined(2)?;
    server.send(Message::Goodbye {
        reason: "server full".into(),
    });
    engine.step(1);
    assert_eq!(
        state(&engine),
        &ClientState::Disconnected("server full".into())
    );
    // Snapshots after the goodbye are ignored.
    server.snapshot(20, &[(1, 0.0)], &[]);
    engine.step(1);
    assert_eq!(entity(&engine, 1), None);
    Ok(())
}

#[test]
fn a_dropped_connection_disconnects() -> anyhow::Result<()> {
    let (mut engine, server) = joined(2)?;
    drop(server);
    engine.step(1);
    assert!(matches!(state(&engine), ClientState::Disconnected(_)));
    Ok(())
}

/// The real server simulates a falling dynamic body; the client, which
/// even has a body on its copy and is playing, shows exactly the
/// server's transforms and never simulates it.
#[test]
fn a_replicated_body_is_never_simulated_on_the_client() -> anyhow::Result<()> {
    let (server_end, mut clients) = LoopbackTransport::pair(1);
    let mut server = Headless::new(8, 8)?;
    server.schedule.add_fixed("net_server", net_server_system);
    server
        .resources
        .insert(NetServer::new(Box::new(server_end), 1));
    let body = server.world.spawn((
        Transform::from_translation(Vec3::new(0.0, 5.0, 0.0)),
        RigidBody::new(BodyKind::Dynamic),
        Collider::new(ColliderShape::Cuboid(Vec3::splat(0.5))),
        Replicated,
    ));
    server.play()?;

    let mut client = Headless::new(8, 8)?;
    let net = NetClient::with_delay(Box::new(clients.remove(0)), "ada", 0);
    client.resources.insert(net);
    client.schedule.add_fixed("net_client", net_client_system);
    client.play()?;
    client.step(1); // hello

    let mut checked = 0;
    for _ in 0..30 {
        server.step(1);
        client.step(1);
        let Some(copy) = entity(&client, 1) else {
            continue;
        };
        if client.world.get::<&RigidBody>(copy).is_err() {
            client.world.insert(
                copy,
                (
                    RigidBody::new(BodyKind::Dynamic),
                    Collider::new(ColliderShape::Cuboid(Vec3::splat(0.5))),
                ),
            )?;
        }
        let theirs = *server.world.get::<&Transform>(body)?;
        let ours = *client.world.get::<&Transform>(copy)?;
        assert_eq!(ours.translation, theirs.translation, "tick {checked}");
        assert_eq!(ours.rotation, theirs.rotation);
        checked += 1;
    }
    assert!(checked >= 25, "{checked} ticks compared");
    let y = server.world.get::<&Transform>(body)?.translation.y;
    assert!(y < 4.0, "the body fell on the server: y = {y}");
    assert!(
        client.resources.expect::<PhysicsWorld>().is_empty(),
        "the client tracks no bodies"
    );
    Ok(())
}

/// Scripts run on the client's own entities but not on remote ones.
#[test]
fn scripts_do_not_run_on_remote_entities() -> anyhow::Result<()> {
    let dir = std::env::temp_dir().join(format!("shin-net-client-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("lift.rhai");
    std::fs::write(
        &path,
        "fn update(entity, dt) {\n    entity.transform.translation.y = 7.0;\n}\n",
    )?;

    let (mut engine, mut server) = joined(0)?;
    server.snapshot(10, &[(1, 0.0)], &[]);
    engine.step(1);
    let remote = entity(&engine, 1).expect("spawned");
    engine.world.insert_one(remote, Script::new(&path))?;
    let local = engine
        .world
        .spawn((Transform::default(), Script::new(&path)));
    engine.play()?;
    engine.step(1);
    let y = |e| engine.world.get::<&Transform>(e).unwrap().translation.y;
    assert_eq!((y(local), y(remote)), (7.0, 0.0));
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
