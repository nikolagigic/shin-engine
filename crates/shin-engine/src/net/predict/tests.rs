//! Tests of local-player prediction (GE-048): a server and clients on a
//! loopback with 100 ms of one-way latency, stepped side by side.
//!
//! The server's world has the ground and one walking cube per player; a
//! client has only its own copy of the ground (as a game's client loads
//! the scene's static colliders) and gets the cubes from snapshots.

use super::*;
use crate::ecs::{Resources, Schedule};
use crate::headless::Headless;
use crate::input::{Actions, InputConfig};
use crate::net::{
    LocalPlayer, LoopbackOptions, LoopbackTransport, NetClient, NetServer, Owner, RemoteEntity,
    Replicated, net_client_system, net_interpolate_system, net_server_input_system,
    net_server_system,
};
use crate::physics::{Collider, ColliderShape, mover_system};

/// 100 ms one-way at 60 ticks per second.
const LATENCY: LoopbackOptions = LoopbackOptions {
    latency_ticks: 6,
    jitter_ticks: 0,
    drop_unreliable: 0.0,
    seed: 48,
};

/// The players' walking speed, metres per second.
const SPEED: f32 = 3.0;

/// The fixed step both sides run at.
const DT: f32 = 1.0 / 60.0;

/// Spawns the starter's ground slab (top at y = 0), a static collider.
fn spawn_ground(world: &mut World) {
    world.spawn((
        Transform {
            translation: Vec3::new(0.0, -0.25, 0.0),
            scale: Vec3::new(20.0, 0.5, 20.0),
            ..Default::default()
        },
        Collider::new(ColliderShape::Cuboid(Vec3::splat(0.5))),
    ));
}

/// Where player `n`'s cube starts.
fn spot(n: u32) -> Vec3 {
    Vec3::new(1.5 * n as f32, 0.5, 2.5)
}

/// The starter's input actions.
fn actions() -> Actions {
    let config: InputConfig = toml::from_str(
        "[actions]\njump = [\"Space\"]\n\
         move_x = { axis = [\"ArrowLeft\", \"ArrowRight\"] }\n\
         move_z = { axis = [\"ArrowDown\", \"ArrowUp\"] }\n",
    )
    .expect("valid input config");
    Actions::from_config(&config)
}

/// A server on `transport` sending the default three ticks per snapshot,
/// with the ground and a replicated walking cube for each of `players`
/// (who join in that order, so the server numbers them 1, 2, ...).
fn server(transport: LoopbackTransport, players: u32) -> anyhow::Result<Headless> {
    let mut schedule = Schedule::new();
    schedule.add_fixed("net_server_input", net_server_input_system);
    schedule.add_fixed("mover", mover_system);
    schedule.add_fixed("net_server", net_server_system);
    let mut res = Resources::new();
    res.insert(NetServer::new(Box::new(transport), 3));
    let mut engine = Headless::from_parts(World::new(), res, schedule, (8, 8))?;
    spawn_ground(&mut engine.world);
    for n in 1..=players {
        engine.world.spawn((
            Transform::from_translation(spot(n)),
            Replicated,
            Owner(PlayerId(n)),
            Mover::new(SPEED, 5.0),
        ));
    }
    engine.play()?;
    Ok(engine)
}

/// A client on `transport` with the ground, the starter's actions, and
/// prediction `on` or off.
fn client(transport: LoopbackTransport, on: bool) -> anyhow::Result<Headless> {
    let mut schedule = Schedule::new();
    schedule.add_fixed("net_client", net_client_system);
    let mut res = Resources::new();
    res.insert(actions());
    let mut net = NetClient::new(Box::new(transport), "player");
    net.set_prediction(on);
    res.insert(net);
    let mut engine = Headless::from_parts(World::new(), res, schedule, (8, 8))?;
    spawn_ground(&mut engine.world);
    engine.play()?;
    Ok(engine)
}

/// A server and `n` clients on one loopback with [`LATENCY`] (or `opts`).
struct Session {
    server: Headless,
    clients: Vec<Headless>,
}

impl Session {
    /// Starts the session and runs it for a second, so every client has
    /// joined and shows every cube.
    fn start(n: u32, opts: LoopbackOptions, predict: bool) -> anyhow::Result<Self> {
        let (server_end, client_ends) = LoopbackTransport::pair_with(n as usize, opts);
        let mut session = Self {
            server: server(server_end, n)?,
            clients: client_ends
                .into_iter()
                .map(|end| client(end, predict))
                .collect::<anyhow::Result<_>>()?,
        };
        session.tick(60);
        for client in &session.clients {
            assert!(client.resources.get::<LocalPlayer>().is_some(), "joined");
        }
        Ok(session)
    }

    /// `n` ticks: the server steps, then each client steps and
    /// interpolates (a frame per tick).
    fn tick(&mut self, n: u32) {
        for _ in 0..n {
            self.server.step(1);
            for client in &mut self.clients {
                client.step(1);
                net_interpolate_system(&mut client.world, &mut client.resources);
            }
        }
    }

    /// Sets client `i`'s action `name` to `axis`.
    fn input(&mut self, i: usize, name: &str, axis: f32) {
        self.clients[i]
            .resources
            .expect_mut::<Actions>()
            .set_value(name, axis)
            .expect("a starter action");
    }

    /// Player `n`'s cube on the server.
    fn on_server(&self, n: u32) -> Vec3 {
        cube(&self.server, n).1
    }

    /// Player `n`'s cube as client `i` shows it.
    fn on_client(&self, i: usize, n: u32) -> Vec3 {
        cube(&self.clients[i], n).1
    }
}

/// Player `n`'s cube in `engine`: the entity and its translation.
fn cube(engine: &Headless, n: u32) -> (Entity, Vec3) {
    engine
        .world
        .query::<(Entity, &Owner, &Transform)>()
        .iter()
        .find(|(_, owner, _)| owner.0 == PlayerId(n))
        .map(|(entity, _, transform)| (entity, transform.translation))
        .unwrap_or_else(|| panic!("player {n} has a cube"))
}

/// Whether `entity` in `engine` has component `T`.
fn has<T: hecs::Component>(engine: &Headless, entity: Entity) -> bool {
    engine.world.get::<&T>(entity).is_ok()
}

fn client_net(engine: &Headless) -> &NetClient {
    engine.resources.expect::<NetClient>()
}

#[test]
fn the_owned_cube_moves_on_the_client_within_one_tick_of_input() -> anyhow::Result<()> {
    let mut session = Session::start(1, LATENCY, true)?;
    let (own, start) = cube(&session.clients[0], 1);
    assert!(has::<Predicted>(&session.clients[0], own), "own cube");
    assert!(has::<RemoteEntity>(&session.clients[0], own));
    assert_eq!(start, spot(1), "at rest where the server put it");

    session.input(0, "move_x", 1.0);
    session.tick(1);
    let moved = session.on_client(0, 1) - start;
    assert!(
        (moved.x - SPEED * DT).abs() < 1e-5,
        "one tick of walking at once: {moved}"
    );
    assert_eq!(session.on_server(1), start, "the server has not heard yet");

    // Walk for a second, stop, and let the server catch up: both agree,
    // and the steady stream of input needed no correction.
    let corrections = client_net(&session.clients[0]).corrections();
    session.tick(59);
    session.input(0, "move_x", 0.0);
    session.tick(30);
    let (ours, theirs) = (session.on_client(0, 1), session.on_server(1));
    assert!(ours.distance(theirs) <= EPSILON, "{ours} vs {theirs}");
    assert!((theirs.x - start.x - SPEED).abs() < 1e-3, "{theirs}");
    assert_eq!(client_net(&session.clients[0]).corrections(), corrections);
    let history = client_net(&session.clients[0])
        .input_history()
        .expect("predicting");
    assert!(
        history.len() <= 16,
        "acknowledged input is forgotten: {} held",
        history.len()
    );
    Ok(())
}

#[test]
fn a_jump_is_predicted_and_agrees_with_the_server() -> anyhow::Result<()> {
    let mut session = Session::start(1, LATENCY, true)?;
    let start = session.on_client(0, 1);
    session.input(0, "jump", 1.0);
    session.tick(1);
    assert!(session.on_client(0, 1).y > start.y, "rises at once");
    session.input(0, "jump", 0.0);
    session.tick(90);
    assert_eq!(session.on_client(0, 1), start, "landed");
    assert_eq!(session.on_server(1), start);
    Ok(())
}

#[test]
fn the_client_converges_within_ten_ticks_of_a_server_teleport() -> anyhow::Result<()> {
    let mut session = Session::start(1, LATENCY, true)?;
    let corrections = client_net(&session.clients[0]).corrections();
    let target = Vec3::new(-4.0, 0.5, -3.0);
    let (entity, _) = cube(&session.server, 1);
    session
        .server
        .world
        .get::<&mut Transform>(entity)?
        .translation = target;

    let mut ticks = 0;
    while session.on_client(0, 1).distance(target) > EPSILON {
        session.tick(1);
        ticks += 1;
        assert!(ticks <= 10, "not converged after {ticks} ticks");
    }
    assert!(
        client_net(&session.clients[0]).corrections() > corrections,
        "a correction, not a coincidence"
    );
    // And it stays there.
    session.tick(10);
    assert!(session.on_client(0, 1).distance(target) <= EPSILON);
    Ok(())
}

#[test]
fn a_correction_replays_the_input_the_server_has_not_seen() -> anyhow::Result<()> {
    let mut session = Session::start(1, LATENCY, true)?;
    session.input(0, "move_x", 1.0);
    session.tick(30);
    // Mid-walk, the server moves the cube one metre back.
    let (entity, at) = cube(&session.server, 1);
    session
        .server
        .world
        .get::<&mut Transform>(entity)?
        .translation = at - Vec3::X;
    session.tick(20);
    // The client is a round trip of input ahead of the server, and still
    // walking, on top of the correction.
    let (ours, theirs) = (session.on_client(0, 1), session.on_server(1));
    let ahead = ours.x - theirs.x;
    assert!(
        ahead > 0.0 && ahead < SPEED * DT * 20.0,
        "ahead by {ahead}: {ours} vs {theirs}"
    );
    session.input(0, "move_x", 0.0);
    session.tick(30);
    let (ours, theirs) = (session.on_client(0, 1), session.on_server(1));
    assert!(ours.distance(theirs) <= EPSILON, "{ours} vs {theirs}");
    Ok(())
}

#[test]
fn remote_cubes_stay_interpolated() -> anyhow::Result<()> {
    let mut session = Session::start(2, LATENCY, true)?;
    // Client 0 is player 1; player 2 (client 1) walks.
    let (theirs, _) = cube(&session.clients[0], 2);
    assert!(has::<RemoteEntity>(&session.clients[0], theirs));
    assert!(!has::<Predicted>(&session.clients[0], theirs), "not ours");
    let (own, _) = cube(&session.clients[1], 2);
    assert!(has::<Predicted>(&session.clients[1], own), "theirs to them");

    session.input(1, "move_x", 1.0);
    let mut shown = vec![session.on_client(0, 2).x];
    for _ in 0..60 {
        session.tick(1);
        shown.push(session.on_client(0, 2).x);
        assert!(
            session.on_client(0, 2).x <= session.on_server(2).x,
            "a remote cube is shown behind the server"
        );
    }
    let steps: Vec<f32> = shown.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(
        steps
            .iter()
            .all(|&d| (-1e-5..=SPEED * DT + 1e-4).contains(&d)),
        "smooth: never back, never more than a tick's walk at once: {steps:?}"
    );
    let moving = steps.iter().filter(|&&d| d > 0.0).count();
    assert!(moving >= 30, "it moved on most ticks: {steps:?}");
    Ok(())
}

#[test]
fn prediction_can_be_turned_off() -> anyhow::Result<()> {
    let mut session = Session::start(1, LATENCY, false)?;
    let (own, start) = cube(&session.clients[0], 1);
    assert!(!client_net(&session.clients[0]).predicting());
    assert!(!has::<Predicted>(&session.clients[0], own), "interpolated");
    session.input(0, "move_x", 1.0);
    let mut ticks = 0;
    while session.on_client(0, 1) == start {
        session.tick(1);
        ticks += 1;
        assert!(ticks < 60, "never moved");
    }
    assert!(
        ticks > 12,
        "without prediction the cube moves a round trip later, not after {ticks} ticks"
    );
    assert!(client_net(&session.clients[0]).input_history().is_none());

    // Turned on mid-session, the next snapshot makes the cube predicted.
    session.clients[0]
        .resources
        .expect_mut::<NetClient>()
        .set_prediction(true);
    session.tick(10);
    assert!(has::<Predicted>(&session.clients[0], own));
    Ok(())
}

#[test]
fn jitter_and_loss_are_corrected() -> anyhow::Result<()> {
    let lossy = LoopbackOptions {
        latency_ticks: 6,
        jitter_ticks: 2,
        drop_unreliable: 0.1,
        seed: 7,
    };
    let mut session = Session::start(1, lossy, true)?;
    for (axis, ticks) in [(1.0, 40), (-0.5, 25), (0.0, 1)] {
        session.input(0, "move_z", axis);
        session.tick(ticks);
    }
    session.tick(60);
    let (ours, theirs) = (session.on_client(0, 1), session.on_server(1));
    assert!(ours.distance(theirs) <= EPSILON, "{ours} vs {theirs}");
    Ok(())
}

#[test]
fn history_acknowledges_through_a_tick() {
    let entry = |tick| HistoryEntry {
        frame: InputFrame {
            tick,
            actions: Vec::new(),
            ack: None,
        },
        poses: Vec::new(),
    };
    let mut history = InputHistory::default();
    for tick in 3..8 {
        history.push(entry(tick));
    }
    assert_eq!(history.acknowledge(5).map(|e| e.frame.tick), Some(5));
    assert_eq!((history.len(), history.oldest_tick()), (2, Some(6)));
    assert!(history.acknowledge(1).is_none(), "already forgotten");
    for tick in 8..8 + InputHistory::CAPACITY as u64 {
        history.push(entry(tick));
    }
    assert_eq!(history.len(), InputHistory::CAPACITY);
    assert_eq!(history.oldest_tick(), Some(8));
}
