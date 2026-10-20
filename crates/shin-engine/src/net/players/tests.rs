//! Tests of players and input: [`PlayerInputs`] on its own, then a server
//! engine running the starter project's scripts with loopback clients.

use std::path::{Path, PathBuf};

use super::*;
use crate::asset::Assets;
use crate::ecs::{Name, Resources, Schedule, World};
use crate::headless::Headless;
use crate::input::{ActionBinding, InputConfig};
use crate::math::{Transform, Vec3};
use crate::net::{
    Channel, Event, LoopbackTransport, Message, NetClient, NetServer, Owner, PROTOCOL_VERSION,
    PeerId, RemoteEntity, Transport, net_client_system, net_server_input_system, net_server_system,
};
use crate::physics::{Collider, ColliderShape, mover_system};
use crate::script::script_system;

/// The starter project's scripts, as `shin new` writes them.
const SERVER_RHAI: &str = include_str!("../../project/template/server.rhai");
const PLAYER_RHAI: &str = include_str!("../../project/template/player.rhai");

/// The starter's movement speed (`mover(3.0, 5.0)` in `server.rhai`),
/// metres per second.
const SPEED: f32 = 3.0;

fn value(pressed: bool, just_pressed: bool, axis: f32) -> ActionValue {
    ActionValue {
        pressed,
        just_pressed,
        axis,
    }
}

fn frame(tick: u64, actions: &[(&str, ActionValue)]) -> InputFrame {
    InputFrame {
        tick,
        actions: actions
            .iter()
            .map(|(name, value)| (name.to_string(), *value))
            .collect(),
        ack: None,
    }
}

const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

#[test]
fn the_newest_frame_wins_and_late_frames_are_dropped() {
    let mut inputs = PlayerInputs::new();
    let frames = vec![
        (P1, frame(5, &[("move_x", value(true, false, 1.0))])),
        (P1, frame(7, &[("move_x", value(false, false, -0.25))])),
        (P1, frame(6, &[("move_x", value(true, false, 0.5))])),
    ];
    inputs.begin_tick(&[P1], frames, None);
    assert_eq!(inputs.axis(P1, "move_x"), -0.25, "tick 7 is the newest");

    inputs.begin_tick(&[P1], vec![(P1, frame(6, &[]))], None);
    assert_eq!(inputs.axis(P1, "move_x"), -0.25, "a late frame is dropped");

    inputs.begin_tick(&[P1], Vec::new(), None);
    assert_eq!(inputs.axis(P1, "move_x"), -0.25, "held without a new frame");

    inputs.begin_tick(&[P1], vec![(P1, frame(8, &[]))], None);
    assert_eq!(
        inputs.value(P1, "move_x"),
        ActionValue::default(),
        "missing from a frame: at rest"
    );
    assert_eq!(
        inputs.value(P2, "move_x"),
        ActionValue::default(),
        "unknown"
    );
}

#[test]
fn a_press_lasts_one_tick_and_is_not_lost() {
    let mut inputs = PlayerInputs::new();
    let held = value(true, false, 1.0);
    let pressed = value(true, true, 1.0);

    // Two frames in one tick: the press rides on the older one.
    let frames = vec![
        (P1, frame(1, &[("jump", pressed)])),
        (P1, frame(2, &[("jump", held)])),
    ];
    inputs.begin_tick(&[P1], frames, None);
    assert!(inputs.just_pressed(P1, "jump") && inputs.pressed(P1, "jump"));
    inputs.begin_tick(&[P1], vec![(P1, frame(3, &[("jump", held)]))], None);
    assert!(!inputs.just_pressed(P1, "jump"), "one tick only");
    assert!(inputs.pressed(P1, "jump"), "still held");

    // Released, then the frame with the press is lost: held again in the
    // next frame still counts as a press.
    inputs.begin_tick(&[P1], vec![(P1, frame(4, &[]))], None);
    inputs.begin_tick(&[P1], vec![(P1, frame(6, &[("jump", held)]))], None);
    assert!(inputs.just_pressed(P1, "jump"));

    // A tap: pressed and released again between two ticks.
    inputs.begin_tick(&[P1], vec![(P1, frame(7, &[]))], None);
    let tap = vec![(P1, frame(8, &[("jump", pressed)])), (P1, frame(9, &[]))];
    inputs.begin_tick(&[P1], tap, None);
    assert!(inputs.just_pressed(P1, "jump"), "the tap is seen");
    assert!(!inputs.pressed(P1, "jump"), "but not held");
    inputs.begin_tick(&[P1], Vec::new(), None);
    assert!(!inputs.just_pressed(P1, "jump"));
}

#[test]
fn players_who_leave_are_forgotten_and_strangers_ignored() {
    let mut inputs = PlayerInputs::new();
    let frames = vec![
        (P1, frame(1, &[("jump", value(true, true, 1.0))])),
        (P2, frame(1, &[("jump", value(true, true, 1.0))])),
        (PlayerId(9), frame(1, &[("jump", value(true, true, 1.0))])),
    ];
    inputs.begin_tick(&[P1, P2], frames, None);
    assert_eq!(inputs.players(), [P1, P2], "no input for a non-player");
    inputs.begin_tick(&[P2], Vec::new(), None);
    assert_eq!(inputs.players(), [P2]);
    assert!(!inputs.pressed(P1, "jump"));
}

fn starter_actions() -> Actions {
    let config: InputConfig = toml::from_str(
        "[actions]\njump = [\"Space\"]\n\
         move_x = { axis = [\"ArrowLeft\", \"ArrowRight\"] }\n\
         move_z = { axis = [\"ArrowDown\", \"ArrowUp\"] }\n",
    )
    .expect("valid input config");
    Actions::from_config(&config)
}

#[test]
fn the_local_player_reads_the_local_actions() -> anyhow::Result<()> {
    let mut actions = starter_actions();
    actions.set_value("move_x", -1.0)?;
    let mut inputs = PlayerInputs::new();
    // A remote frame for the local player is ignored in favour of Actions.
    let frames = vec![(P1, frame(1, &[("move_x", value(true, false, 1.0))]))];
    inputs.begin_tick(&[P1, P2], frames, Some((P1, &actions)));
    assert_eq!(inputs.axis(P1, "move_x"), -1.0);
    assert!(inputs.just_pressed(P1, "move_x"));
    assert_eq!(inputs.axis(P2, "move_x"), 0.0);
    Ok(())
}

#[test]
fn an_input_frame_lists_only_actions_not_at_rest() -> anyhow::Result<()> {
    let mut actions = starter_actions();
    actions.define("fire", ActionBinding::Buttons(Vec::new()));
    assert_eq!(input_frame(3, Some(&actions)), frame(3, &[]));
    actions.set_value("move_z", 0.25)?;
    actions.set_pressed("jump", true)?;
    assert_eq!(
        input_frame(4, Some(&actions)),
        frame(
            4,
            &[
                ("jump", value(true, true, 1.0)),
                ("move_z", value(false, false, 0.25)),
            ]
        )
    );
    assert_eq!(input_frame(5, None), frame(5, &[]), "no Actions resource");
    Ok(())
}

// --- A server engine running scripts -------------------------------------

/// A fresh project directory with the starter cube asset and `server`
/// and `player` as `scripts/server.rhai` and `scripts/player.rhai`.
fn project_dir(test: &str, server: &str, player: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("shin-net-players-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("scripts")).expect("create temp dir");
    std::fs::create_dir_all(dir.join("assets")).expect("create temp dir");
    let test_assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/test");
    for file in ["cube.gltf", "cube.bin"] {
        std::fs::copy(test_assets.join(file), dir.join("assets").join(file)).expect("copy asset");
    }
    std::fs::write(dir.join("scripts/server.rhai"), server).expect("write script");
    std::fs::write(dir.join("scripts/player.rhai"), player).expect("write script");
    dir
}

/// A playing server engine for the project in `dir`, serving on
/// `transport` with a snapshot every tick, its schedule in the order a
/// game uses: the input half, the scripts, the movers, the snapshot half.
/// Its world has the starter's ground slab (top at y = 0) for the
/// players' cubes to stand on.
fn server_engine(dir: &Path, server: NetServer) -> anyhow::Result<Headless> {
    let mut res = Resources::new();
    res.insert(Assets::new(dir.to_path_buf()));
    res.insert(starter_actions());
    res.insert(server);
    let mut schedule = Schedule::new();
    schedule.add_fixed("net_server_input", net_server_input_system);
    schedule.add_fixed("script", script_system);
    schedule.add_fixed("mover", mover_system);
    schedule.add_fixed("net_server", net_server_system);
    let mut engine = Headless::from_parts(World::new(), res, schedule, (8, 8))?;
    engine.world.spawn((
        Transform {
            translation: Vec3::new(0.0, -0.25, 0.0),
            scale: Vec3::new(20.0, 0.5, 20.0),
            ..Default::default()
        },
        Collider::new(ColliderShape::Cuboid(Vec3::splat(0.5))),
    ));
    engine.play()?;
    Ok(engine)
}

fn hello(client: &mut LoopbackTransport, name: &str) {
    let msg = Message::Hello {
        protocol: PROTOCOL_VERSION,
        name: name.into(),
    };
    client.send(PeerId(0), &msg, Channel::Reliable);
}

fn send_input(client: &mut LoopbackTransport, tick: u64, actions: &[(&str, ActionValue)]) {
    let msg = Message::Input(frame(tick, actions));
    client.send(PeerId(0), &msg, Channel::Unreliable);
}

/// Every entity a player owns, as `(owner, name, translation)`, by owner.
fn owned(engine: &Headless) -> Vec<(PlayerId, String, Vec3)> {
    let mut owned: Vec<_> = engine
        .world
        .query::<(&Owner, &Name, &Transform)>()
        .iter()
        .map(|(owner, name, t)| (owner.0, name.0.clone(), t.translation))
        .collect();
    owned.sort_by_key(|(owner, ..)| *owner);
    owned
}

/// How many entities are called `name`.
fn named(engine: &Headless, name: &str) -> usize {
    engine
        .world
        .query::<&Name>()
        .iter()
        .filter(|n| n.0 == name)
        .count()
}

fn errors(engine: &Headless) -> Vec<String> {
    engine
        .resources
        .get::<crate::script::ScriptEngine>()
        .map(|scripts| scripts.errors().map(ToString::to_string).collect())
        .unwrap_or_default()
}

/// AC: two loopback clients control two distinct cubes, with the starter
/// project's own scripts; a client that leaves loses its cube.
#[test]
fn two_loopback_clients_control_two_distinct_cubes() -> anyhow::Result<()> {
    let dir = project_dir("two-clients", SERVER_RHAI, PLAYER_RHAI);
    let (server, mut clients) = LoopbackTransport::pair(2);
    let mut engine = server_engine(&dir, NetServer::new(Box::new(server), 1))?;
    hello(&mut clients[0], "ada");
    hello(&mut clients[1], "bob");
    engine.step(1);

    let start = owned(&engine);
    assert_eq!(start.len(), 2, "one cube per player: {start:?}");
    assert_eq!((start[0].0, start[0].1.as_str()), (P1, "player 1"));
    assert_eq!((start[1].0, start[1].1.as_str()), (P2, "player 2"));
    assert_ne!(start[0].2, start[1].2, "distinct spots");

    // Ada holds right for 30 ticks; Bob holds jump for the last 10.
    let right = value(true, false, 1.0);
    for tick in 0..30 {
        send_input(&mut clients[0], tick, &[("move_x", right)]);
        let bob: &[(&str, ActionValue)] = if tick >= 20 {
            &[("jump", value(true, tick == 20, 1.0))]
        } else {
            &[]
        };
        send_input(&mut clients[1], tick, bob);
        engine.step(1);
    }
    let end = owned(&engine);
    let dt = engine.fixed_dt as f32;
    let ada = end[0].2 - start[0].2;
    let bob = end[1].2 - start[1].2;
    assert!(
        (ada.x - SPEED * 30.0 * dt).abs() < 1e-4,
        "ada moved right: {ada}"
    );
    assert!(
        ada.y.abs() < 1e-6 && ada.z.abs() < 1e-6,
        "only right: {ada}"
    );
    assert!(
        bob.x.abs() < 1e-6 && bob.z.abs() < 1e-6,
        "bob did not move: {bob}"
    );
    assert!(bob.y > 0.5, "bob's jump lifted only bob's cube: {bob}");
    assert_eq!(errors(&engine), Vec::<String>::new());

    // Both cubes are replicated, with their owners, to both clients.
    for client in &mut clients {
        let last = client
            .poll()
            .into_iter()
            .filter_map(|event| match event {
                Event::Message(_, Message::Snapshot(snap)) => Some(snap),
                _ => None,
            })
            .next_back()
            .expect("snapshots arrive");
        let owners: Vec<_> = last.entities.iter().map(|e| e.owner).collect();
        assert_eq!(owners, [Some(P1), Some(P2)]);
    }

    // Ada leaves: her cube goes, Bob's stays.
    drop(clients.remove(0));
    engine.step(1);
    let left = owned(&engine);
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(left[0].0, P2);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// Leaves a marker entity per hook call, so calls can be counted.
const COUNTING_SERVER: &str = r#"
    fn on_player_join(id) { spawn_entity().name(`joined ${id}`); }
    fn on_player_leave(id) { spawn_entity().name(`left ${id}`); }
"#;

/// AC: `on_player_join` and `on_player_leave` fire exactly once per
/// event, also across pauses; stopping play greets the players again.
#[test]
fn join_and_leave_hooks_fire_exactly_once_per_event() -> anyhow::Result<()> {
    let dir = project_dir("hooks", COUNTING_SERVER, PLAYER_RHAI);
    let (server, mut clients) = LoopbackTransport::pair(2);
    let mut server = NetServer::new(Box::new(server), 1);
    assert_eq!(server.add_local_player(), P1);
    let mut engine = server_engine(&dir, server)?;
    hello(&mut clients[0], "ada");
    engine.step(5);
    hello(&mut clients[1], "bob");
    hello(&mut clients[1], "bob again");
    engine.step(5);
    for player in 1..=3 {
        assert_eq!(named(&engine, &format!("joined {player}")), 1, "{player}");
        assert_eq!(named(&engine, &format!("left {player}")), 0, "{player}");
    }

    let bye = Message::Goodbye {
        reason: "bed time".into(),
    };
    clients[0].send(PeerId(0), &bye, Channel::Reliable);
    engine.step(5);
    assert_eq!(named(&engine, "left 2"), 1);

    // Bob drops while paused: seen off once play resumes, and only once.
    engine.pause()?;
    drop(clients.remove(1));
    engine.step(3);
    assert_eq!(named(&engine, "left 3"), 0, "scripts do not run paused");
    engine.pause()?;
    engine.step(5);
    assert_eq!(named(&engine, "left 3"), 1);
    assert_eq!(named(&engine, "joined 3"), 1);

    // Stop puts the world back; the next play greets the host again.
    engine.stop()?;
    assert_eq!(named(&engine, "joined 1"), 0, "restored");
    engine.play()?;
    engine.step(5);
    assert_eq!(named(&engine, "joined 1"), 1);
    assert_eq!(errors(&engine), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// A real [`NetClient`] sends its [`Actions`] every tick, learns its
/// [`LocalPlayer`], and sees its own cube, replicated, move.
#[test]
fn a_net_client_drives_its_cube_with_its_actions() -> anyhow::Result<()> {
    let dir = project_dir("client", SERVER_RHAI, PLAYER_RHAI);
    let (server, mut clients) = LoopbackTransport::pair(1);
    let mut host = server_engine(&dir, NetServer::new(Box::new(server), 1))?;

    let mut client = Headless::new(8, 8)?;
    client.resources.insert(Assets::new(dir.clone()));
    client.resources.insert(starter_actions());
    let net = NetClient::with_delay(Box::new(clients.remove(0)), "ada", 0);
    client.resources.insert(net);
    client.schedule.add_fixed("net_client", net_client_system);
    let tick = |host: &mut Headless, client: &mut Headless| {
        client.step(1);
        host.step(1);
    };
    for _ in 0..3 {
        tick(&mut host, &mut client);
    }
    assert_eq!(
        client.resources.get::<LocalPlayer>(),
        Some(&LocalPlayer(P1))
    );

    let start = owned(&host)[0].2;
    client
        .resources
        .expect_mut::<Actions>()
        .set_value("move_z", 1.0)?;
    for _ in 0..20 {
        tick(&mut host, &mut client);
    }
    let moved = owned(&host)[0].2 - start;
    assert!(moved.z < -0.5, "forward is -Z: {moved}");
    assert!(moved.x.abs() < 1e-6, "{moved}");

    // The client shows the server's cube, at the server's position.
    tick(&mut host, &mut client);
    let (name, z) = client
        .world
        .query::<(&Name, &Transform, &RemoteEntity)>()
        .iter()
        .map(|(name, t, _)| (name.0.clone(), t.translation.z))
        .next()
        .expect("the client has the replicated cube");
    assert_eq!(name, "player 1");
    assert!(z < start.z - 0.5, "{z}");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// A solo game is a server whose only player is the local one: the same
/// scripts give it a cube and move it with the local [`Actions`].
#[test]
fn a_solo_server_drives_the_local_cube_with_local_actions() -> anyhow::Result<()> {
    let dir = project_dir("solo", SERVER_RHAI, PLAYER_RHAI);
    let mut engine = server_engine(&dir, NetServer::solo())?;
    engine.step(1);
    let start = owned(&engine);
    assert_eq!(start.len(), 1);
    assert_eq!(start[0].0, P1);
    engine
        .resources
        .expect_mut::<Actions>()
        .set_value("move_x", -1.0)?;
    engine.step(10);
    let moved = owned(&engine)[0].2 - start[0].2;
    let dt = engine.fixed_dt as f32;
    assert!((moved.x + SPEED * 10.0 * dt).abs() < 1e-4, "{moved}");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// An agent drives the solo player through the tool registry, as under
/// `--mcp`: `input.set` moves its cube under `sim.step`, and `script.eval`
/// reads the same player input scripts do.
#[test]
fn tools_drive_the_solo_player() -> anyhow::Result<()> {
    use serde_json::json;

    use crate::tool::{ToolOutput, ToolRegistry};

    let dir = project_dir("tools", SERVER_RHAI, PLAYER_RHAI);
    let mut engine = server_engine(&dir, NetServer::solo())?;
    let reg = ToolRegistry::with_engine_tools();
    reg.call(&mut engine, "sim.step", json!({ "steps": 1 }))?;
    let start = owned(&engine)[0].2;
    let set = json!({ "action": "jump", "pressed": true });
    reg.call(&mut engine, "input.set", set)?;
    reg.call(&mut engine, "sim.step", json!({ "steps": 15 }))?;
    let lifted = owned(&engine)[0].2 - start;
    assert!(lifted.y > 0.5, "jump lifts the cube: {lifted}");

    let source = r#"[player_input_pressed(1, "jump"), player_input(1, "move_x")]"#;
    let ToolOutput::Json(out) =
        reg.call(&mut engine, "script.eval", json!({ "source": source }))?
    else {
        panic!("expected JSON");
    };
    assert_eq!(out["result"], json!([true, 0.0]));
    let unknown = json!({ "source": r#"player_input(1, "fly")"# });
    let err = reg.call(&mut engine, "script.eval", unknown).unwrap_err();
    assert!(
        format!("{err:#}").contains("unknown action `fly`"),
        "{err:#}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// GE-048: `.mover(speed, jump)` gives a spawned entity a [`Mover`], as
/// the starter's server script does for each player's cube; settings
/// that make no sense are a script error.
#[test]
fn a_spawn_can_be_a_mover() -> anyhow::Result<()> {
    use serde_json::json;

    use crate::physics::Mover;
    use crate::tool::ToolRegistry;

    let dir = project_dir("mover", SERVER_RHAI, PLAYER_RHAI);
    let mut engine = server_engine(&dir, NetServer::solo())?;
    engine.step(1);
    let movers: Vec<(PlayerId, Mover)> = engine
        .world
        .query::<(&Owner, &Mover)>()
        .iter()
        .map(|(owner, mover)| (owner.0, *mover))
        .collect();
    assert_eq!(movers, [(P1, Mover::new(SPEED, 5.0))]);

    let reg = ToolRegistry::with_engine_tools();
    let bad = json!({ "source": "spawn_entity().mover(-3.0, 5.0)" });
    let err = reg.call(&mut engine, "script.eval", bad).unwrap_err();
    assert!(format!("{err:#}").contains("mover settings"), "{err:#}");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
