//! A small multiplayer arena over QUIC: the manual smoke test for
//! multiplayer (GE-047).
//!
//! ```text
//! cargo run -p shin-net --example net_arena -- --serve [--port N]
//! cargo run -p shin-net --example net_arena -- --join 127.0.0.1:7777 --insecure
//! cargo run -p shin-net --example net_arena -- --join 127.0.0.1:7777 --cert <path>
//! cargo run -p shin-net --example net_arena -- --join 127.0.0.1:7777 --insecure --latency 100
//! ```
//!
//! `--serve` runs a dedicated server (no window) on UDP port 7777: a
//! floor, a gold coin, and one cube per player who joins. Each `--join`
//! opens a window as a client. Move your cube with the arrow keys (or a
//! gamepad's left stick); touch the coin to score. Your cube's name is
//! `player <id>: <score>`, a replicated name, so the score reaches every
//! clients with the snapshots, and each client logs its own score when it
//! changes. Start two clients to see each other's cubes move. Walk off
//! the floor and you fall; the server puts you back at your start.
//!
//! Cubes walk with a [`Mover`] (GE-048), so each client predicts its own
//! cube: it moves the moment you press a key, while other players' cubes
//! are interpolated. `--latency <ms>` delays everything this client sends
//! and receives by that many milliseconds each way, to feel the
//! difference on one machine: your cube stays immediate, the others stay
//! smooth (and a fall shows the server's correction).
//!
//! Everything the clients see comes from the server: they spawn nothing
//! visible themselves, only the floor's collider, which a mover needs to
//! stand on and snapshots do not carry. The server saves its development certificate to the temp
//! directory and logs the exact `--join ... --cert` command.

use std::collections::{BTreeSet, VecDeque};
use std::net::{Ipv4Addr, ToSocketAddrs};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use shin::app::{self, AppConfig, EcsGame};
use shin::asset::{self, AssetSource, Assets};
use shin::ecs::{Entity, Name, Resources, Schedule, World};
use shin::gpu::GpuContext;
use shin::headless::Headless;
use shin::input::{ActionBinding, Actions, Binding, GamepadAxis, KeyCode};
use shin::math::{Camera, Projection, Quat, Transform, Vec3};
use shin::net::{
    Channel, Event, LocalPlayer, Message, NetClient, NetServer, Owner, PeerId, PlayerId,
    Replicated, ServerOptions, Transport, net_client_system, net_interpolate_system,
    net_server_input_system,
};
use shin::physics::{Collider, ColliderShape, Mover, mover_system};
use shin::render::{LightUniform, MeshHandle, MeshInstance, RenderState, render_world};
use shin_net::{QuicTransport, ServerTrust};

/// The UDP port `--serve` listens on unless `--port` says otherwise.
const DEFAULT_PORT: u16 = 7777;
/// Half the floor's walkable width, in metres; the floor reaches half a
/// metre further on every side.
const ARENA: f32 = 5.0;
/// A cube that falls below this height goes back to its start.
const FALLEN: f32 = -5.0;
/// How fast a player's cube moves, in metres per second.
const SPEED: f32 = 4.0;
/// A cube scores when its centre is this close to the coin's (on x and z).
const PICKUP_RADIUS: f32 = 0.7;
/// Where the coin goes after each pickup, in turn.
const COIN_SPOTS: [[f32; 2]; 5] = [
    [2.5, -2.0],
    [-3.0, 1.5],
    [0.5, 3.0],
    [-1.5, -3.5],
    [3.5, 2.5],
];
/// Linear clear colour: a dark blue.
const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.02,
    g: 0.03,
    b: 0.08,
    a: 1.0,
};

/// Server resource: the cube mesh every entity draws with, and how many
/// coins have been picked up (which picks the coin's next spot).
struct Arena {
    cube: MeshHandle,
    pickups: usize,
}

/// Server component: a player's score, shown in the cube's name.
struct Score(u32);

/// The name a player's cube carries: replicated, so clients read the
/// score from it.
fn player_name(player: PlayerId, score: u32) -> String {
    format!("player {}: {score}", player.0)
}

/// A distinct colour per player.
fn player_color(player: PlayerId) -> [f32; 4] {
    const COLORS: [[f32; 4]; 4] = [
        [0.8, 0.15, 0.1, 1.0],
        [0.1, 0.35, 0.85, 1.0],
        [0.15, 0.7, 0.2, 1.0],
        [0.75, 0.2, 0.75, 1.0],
    ];
    COLORS[(player.0 as usize).saturating_sub(1) % COLORS.len()]
}

/// The coin's position after `pickups` pickups.
fn coin_spot(pickups: usize) -> Vec3 {
    let [x, z] = COIN_SPOTS[pickups % COIN_SPOTS.len()];
    Vec3::new(x, 0.3, z)
}

/// The floor's transform: a slab whose top is at y = 0.
fn floor() -> Transform {
    Transform::from_trs(
        Vec3::new(0.0, -0.25, 0.0),
        Quat::IDENTITY,
        Vec3::new(2.0 * ARENA + 1.0, 0.5, 2.0 * ARENA + 1.0),
    )
}

/// The floor's collider: the unit cube's, sized by [`floor`]'s scale.
fn floor_collider() -> Collider {
    Collider::new(ColliderShape::Cuboid(Vec3::splat(0.5)))
}

/// Where `player`'s cube starts: in a row along the near edge.
fn start_spot(player: PlayerId) -> Vec3 {
    Vec3::new(-ARENA + 1.5 * player.0 as f32, 0.5, ARENA - 1.0)
}

/// Spawns the replicated floor (with its collider) and coin, drawn with
/// `cube`.
fn spawn_level(world: &mut World, cube: MeshHandle) {
    world.spawn((
        Name("floor".into()),
        floor(),
        floor_collider(),
        MeshInstance {
            mesh: cube,
            color: [0.18, 0.2, 0.22, 1.0],
            texture: None,
        },
        Replicated,
    ));
    world.spawn((
        Name("coin".into()),
        Transform::from_trs(coin_spot(0), Quat::IDENTITY, Vec3::splat(0.4)),
        MeshInstance {
            mesh: cube,
            color: [1.0, 0.65, 0.05, 1.0],
            texture: None,
        },
        Replicated,
    ));
}

/// Fixed system on the server, after `net_server_input_system` and
/// `mover_system`: gives each player who joined a walking cube (a
/// [`Mover`], which `mover_system` moves by its owner's input), removes
/// the cubes of players who left, puts cubes that fell off the floor back
/// at their start, and scores a cube that touches the coin (which then
/// moves on).
fn arena_system(world: &mut World, res: &mut Resources) {
    let dt = res.expect::<shin::ecs::Time>().fixed_dt as f32;
    let players: BTreeSet<PlayerId> = res.expect::<NetServer>().players().into_iter().collect();

    // Players who left: despawn their cubes.
    let owned: Vec<(Entity, PlayerId)> = world
        .query::<(Entity, &Owner)>()
        .iter()
        .map(|(entity, owner)| (entity, owner.0))
        .collect();
    for &(entity, player) in &owned {
        if !players.contains(&player) {
            log::info!("arena: player {} left", player.0);
            let _ = world.despawn(entity);
        }
    }
    // Players who joined: spawn a walking cube in a row along the near
    // edge.
    let have: BTreeSet<PlayerId> = owned.iter().map(|(_, p)| *p).collect();
    let cube = res.expect::<Arena>().cube;
    for &player in players.difference(&have) {
        log::info!("arena: player {} gets a cube", player.0);
        world.spawn((
            Name(player_name(player, 0)),
            Transform::from_translation(start_spot(player)),
            MeshInstance {
                mesh: cube,
                color: player_color(player),
                texture: None,
            },
            Owner(player),
            Mover::new(SPEED, 0.0),
            Score(0),
            Replicated,
        ));
    }

    // Fell off the floor: back to the start. Only the server does this, so
    // the falling player's client sees it as a correction.
    for (owner, transform, mover) in world.query_mut::<(&Owner, &mut Transform, &mut Mover)>() {
        if transform.translation.y < FALLEN {
            transform.translation = start_spot(owner.0);
            mover.vertical_velocity = 0.0;
        }
    }

    // Pick up the coin.
    let arena = res.expect_mut::<Arena>();
    let coin = coin_spot(arena.pickups);
    for (owner, transform, score, name) in
        world.query_mut::<(&Owner, &Transform, &mut Score, &mut Name)>()
    {
        let d = transform.translation - coin;
        if d.x.hypot(d.z) < PICKUP_RADIUS {
            score.0 += 1;
            name.0 = player_name(owner.0, score.0);
            arena.pickups += 1;
            log::info!("arena: {}", name.0);
            break;
        }
    }
    let spot = coin_spot(arena.pickups);
    for (name, transform) in world.query_mut::<(&Name, &mut Transform)>() {
        if name.0 == "coin" {
            transform.translation = spot;
            transform.rotation *= Quat::from_rotation_y(2.0 * dt);
        }
    }
}

/// Where the server saves its certificate for `--join --cert`.
fn cert_path() -> PathBuf {
    std::env::temp_dir().join("shin-net-arena-cert.der")
}

/// Runs the dedicated server on UDP `port` until Ctrl-C.
fn serve(port: u16) -> anyhow::Result<()> {
    let mut schedule = Schedule::new();
    schedule.add_fixed("net_server_input", net_server_input_system);
    schedule.add_fixed("mover", mover_system);
    schedule.add_fixed("arena", arena_system);
    let mut engine = Headless::from_parts(World::new(), Resources::new(), schedule, (64, 64))?;

    // Registered under its built-in source, so snapshots name it and
    // clients load the same mesh.
    let cube = engine
        .resources
        .expect_mut::<RenderState>()
        .registry
        .add_with_source(
            &engine.gpu.device,
            &asset::cube(),
            AssetSource::Builtin("cube".into()),
        );
    engine.resources.insert(Arena { cube, pickups: 0 });
    spawn_level(&mut engine.world, cube);

    let transport = QuicTransport::listen((Ipv4Addr::UNSPECIFIED, port).into())?;
    let der = transport.cert_der().expect("a server has a certificate");
    std::fs::write(cert_path(), der)?;
    log::info!(
        "arena: join with `cargo run -p shin-net --example net_arena -- --join 127.0.0.1:{port} \
         --cert {}` (or `--insecure` instead of `--cert <path>`)",
        cert_path().display()
    );
    shin::net::run_server(&mut engine, Box::new(transport), ServerOptions::default())
}

/// The actions a client sends: `move_x` and `move_z` on the arrow keys
/// and the left stick.
fn actions() -> Actions {
    let axis = |neg, pos, stick| ActionBinding::Axis {
        axis: Some([Binding::Key(neg), Binding::Key(pos)]),
        gamepad_axis: Some(stick),
    };
    let mut actions = Actions::new();
    actions.define(
        "move_x",
        axis(
            KeyCode::ArrowLeft,
            KeyCode::ArrowRight,
            GamepadAxis::LeftStickX,
        ),
    );
    actions.define(
        "move_z",
        axis(
            KeyCode::ArrowDown,
            KeyCode::ArrowUp,
            GamepadAxis::LeftStickY,
        ),
    );
    actions
}

/// Client resource: the score this client last logged.
#[derive(Default)]
struct ShownScore(Option<String>);

/// Frame system on the client: logs this player's cube name (and so its
/// score) whenever it changes.
fn score_log_system(world: &mut World, res: &mut Resources) {
    let Some(me) = res.get::<LocalPlayer>().map(|p| p.0) else {
        return;
    };
    let name = world
        .query::<(&Owner, &Name)>()
        .iter()
        .find(|(owner, _)| owner.0 == me)
        .map(|(_, name)| name.0.clone());
    let shown = res.expect_mut::<ShownScore>();
    if name.is_some() && shown.0 != name {
        log::info!("arena: you are {}", name.as_deref().unwrap_or_default());
        shown.0 = name;
    }
}

/// First-frame setup in the client's window: the renderer, a light, and
/// an asset cache for the meshes snapshots name.
fn setup_client(res: &mut Resources, gpu: &GpuContext, format: wgpu::TextureFormat) {
    let mut state = RenderState::new(&gpu.device, &gpu.queue, format);
    state.light = LightUniform {
        direction: [-0.6, -2.0, -1.0, 0.0],
        color: [1.0, 1.0, 1.0, 1.0],
        ambient: [0.15, 0.15, 0.18, 1.0],
    };
    res.insert(state);
    res.insert(Assets::new(std::env::temp_dir()));
}

/// A transport that holds everything sent and received for `delay`
/// each way: simulated latency, for `--join ... --latency <ms>`. Sends go
/// out, and arrivals are handed on, at the first poll after they are due
/// (a client polls once per tick).
struct Delayed {
    inner: Box<dyn Transport + Send>,
    delay: Duration,
    /// Received, and when to hand each on.
    inbox: VecDeque<(Instant, Event)>,
    /// To send, and when.
    outbox: VecDeque<(Instant, PeerId, Message, Channel)>,
}

impl Transport for Delayed {
    fn send(&mut self, to: PeerId, msg: &Message, channel: Channel) {
        let due = Instant::now() + self.delay;
        self.outbox.push_back((due, to, msg.clone(), channel));
    }

    fn broadcast(&mut self, msg: &Message, channel: Channel) {
        for peer in self.inner.peers() {
            self.send(peer, msg, channel);
        }
    }

    fn poll(&mut self) -> Vec<Event> {
        let now = Instant::now();
        while self.outbox.front().is_some_and(|(due, ..)| *due <= now) {
            let (_, to, msg, channel) = self.outbox.pop_front().expect("checked");
            self.inner.send(to, &msg, channel);
        }
        let due = now + self.delay;
        self.inbox
            .extend(self.inner.poll().into_iter().map(|event| (due, event)));
        let mut ready = Vec::new();
        while self.inbox.front().is_some_and(|(due, _)| *due <= now) {
            ready.push(self.inbox.pop_front().expect("checked").1);
        }
        ready
    }

    fn peers(&self) -> Vec<PeerId> {
        self.inner.peers()
    }
}

/// Joins the server at `addr` (`host:port`) in a window, with `latency`
/// added each way.
fn join(addr: &str, trust: &ServerTrust, latency: Duration) -> anyhow::Result<()> {
    let resolved = addr
        .to_socket_addrs()
        .with_context(|| format!("`{addr}` is not a host:port address"))?
        .next()
        .with_context(|| format!("`{addr}` resolves to no address"))?;
    let mut transport: Box<dyn Transport + Send> =
        Box::new(QuicTransport::connect(resolved, trust)?);
    if !latency.is_zero() {
        log::info!("arena: adding {} ms each way", latency.as_millis());
        transport = Box::new(Delayed {
            inner: transport,
            delay: latency,
            inbox: VecDeque::new(),
            outbox: VecDeque::new(),
        });
    }
    let who = std::env::var("USER").unwrap_or_else(|_| "player".into());

    let mut schedule = Schedule::new();
    schedule.add_fixed("net_client", net_client_system);
    schedule.add_frame("net_interpolate", net_interpolate_system);
    schedule.add_frame("score_log", score_log_system);
    let mut world = World::new();
    // The floor's collider, invisible and local: the client's own cube
    // (predicted) stands on it. The floor you see comes from the server.
    world.spawn((floor(), floor_collider()));
    let mut game = EcsGame::new(world, schedule);
    game.resources.insert(NetClient::new(transport, &who));
    game.resources.insert(actions());
    game.resources.insert(ShownScore::default());
    let mut transform = Transform::from_translation(Vec3::new(0.0, 9.0, 10.0));
    transform.looking_at(Vec3::ZERO, Vec3::Y);
    game.resources.insert(Camera {
        transform,
        projection: Projection::Perspective {
            fov_y_radians: 50f32.to_radians(),
            near: 0.1,
            far: 100.0,
        },
    });
    game.render_fn = Some(Box::new(|world, res, gpu, color, depth| {
        let texture = color.texture();
        if res.get::<RenderState>().is_none() {
            setup_client(res, gpu, texture.format());
        }
        let aspect = texture.width() as f32 / texture.height() as f32;
        render_world(world, res, gpu, color, depth, aspect);
    }));
    log::info!("arena: joining {addr}");

    let config = AppConfig {
        title: format!("net_arena: {addr}"),
        clear_color: CLEAR,
        ..Default::default()
    };
    game.title = config.title.clone();
    app::run(config, game)
}

/// How the example was asked to run.
enum Mode {
    /// `--serve [--port N]`.
    Serve(u16),
    /// `--join <addr> (--insecure | --cert <path>) [--latency <ms>]`.
    Join(String, ServerTrust, Duration),
}

/// Reads the command line (without the program name).
fn parse_mode(args: &[String]) -> anyhow::Result<Mode> {
    const USAGE: &str = "usage: net_arena --serve [--port N] \
                         | --join <host:port> (--insecure | --cert <path>) [--latency <ms>]";
    // `--latency <ms>` may end a `--join`.
    let (args, latency) = match args {
        [rest @ .., flag, ms] if flag == "--latency" => {
            let ms: u64 = ms
                .parse()
                .with_context(|| format!("--latency needs milliseconds, got `{ms}`"))?;
            (rest, Duration::from_millis(ms))
        }
        _ => (args, Duration::ZERO),
    };
    if !latency.is_zero() && args.first().is_none_or(|flag| flag != "--join") {
        anyhow::bail!(USAGE);
    }
    match args {
        [flag] if flag == "--serve" => Ok(Mode::Serve(DEFAULT_PORT)),
        [flag, port_flag, n] if flag == "--serve" && port_flag == "--port" => {
            Ok(Mode::Serve(n.parse().with_context(|| {
                format!("--port needs a number, got `{n}`")
            })?))
        }
        [flag, addr, insecure] if flag == "--join" && insecure == "--insecure" => {
            Ok(Mode::Join(addr.clone(), ServerTrust::Insecure, latency))
        }
        [flag, addr, cert, path] if flag == "--join" && cert == "--cert" => {
            let der = std::fs::read(path).with_context(|| format!("reading certificate {path}"))?;
            Ok(Mode::Join(addr.clone(), ServerTrust::Cert(der), latency))
        }
        _ => anyhow::bail!(USAGE),
    }
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse_mode(&args)? {
        Mode::Serve(port) => serve(port),
        Mode::Join(addr, trust, latency) => join(&addr, &trust, latency),
    }
}
