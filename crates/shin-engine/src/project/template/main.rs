//! A Shin starter game: a lit cube on a ground slab with a small moon
//! orbiting it and a stack of crates that physics drops onto the ground,
//! viewed through the free-fly camera. The window title and
//! size come from `shin.toml`; the scene is `scenes/main.scene.ron`.
//!
//! ```text
//! cargo run                         play in a window
//! cargo run -- --mcp                serve this game to an agent over MCP (stdio)
//! cargo run -- --serve [--port N]   run a dedicated multiplayer server, no window
//! cargo run -- --host [--port N]    play in a window and host a multiplayer game
//! cargo run -- --join <addr>        play in a window as a client of a server
//! ```
//!
//! Every mode builds the same world, systems ([`game_schedule`]), and
//! resources, so whatever an agent sees under `--mcp`, and whatever a
//! server simulates, is the game itself. The server's port defaults to
//! [`DEFAULT_PORT`].
//! The moon's script (`scripts/moon.rhai`) blips once per revolution.
//!
//! Controls: the arrow keys or a gamepad's left stick move the cube and
//! Space or its south button lifts it (the `move_x`, `move_z`, and `jump`
//! actions bound in `shin.toml`, read by `scripts/player.rhai`); W/S
//! forward and back, A/D strafe, E/Q up and down fly the camera; hold the
//! right mouse button to look around; Escape quits.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use shin::app::{self, AppConfig, EcsGame};
use shin::asset::Assets;
use shin::audio::audio_system;
use shin::ecs::{ExitRequest, Name, Resources, Schedule, Time, World};
use shin::gpu::GpuContext;
use shin::headless::Headless;
use shin::input::{Actions, FlyCamera, fly_camera_system};
use shin::math::{Camera, Projection, Transform, Vec3};
use shin::net::{
    LoopbackTransport, NetClient, NetServer, ServerOptions, Transport, net_client_system,
    net_interpolate_system, net_server_system,
};
use shin::physics::physics_system;
use shin::project::Project;
use shin::render::{RenderState, render_world};
use shin::scene;
use shin::script::script_system;
use shin::tool::{ToolOutput, ToolRegistry, TypedTool};

/// The project's root directory, where `shin.toml` and `assets/` live.
const ROOT: &str = env!("CARGO_MANIFEST_DIR");

/// The UDP port `--serve` and `--host` listen on unless `--port` says
/// otherwise.
const DEFAULT_PORT: u16 = 7777;

/// Linear clear colour: a dusky sky blue.
const SKY: wgpu::Color = wgpu::Color {
    r: 0.05,
    g: 0.08,
    b: 0.15,
    a: 1.0,
};

/// The name of the entity [`orbit_system`] moves; the starter scene has one.
const ORBITER: &str = "moon";
/// Distance of the moon from the cube's centre, in metres.
const ORBIT_RADIUS: f32 = 1.6;
/// Height of the orbit plane: the cube's centre.
const ORBIT_HEIGHT: f32 = 0.5;
/// Angular speed of the orbit, in radians per second.
const ORBIT_SPEED: f32 = 1.0;

/// Where the moon is `t` seconds into the simulation: on a circle around
/// the cube, starting at `(ORBIT_RADIUS, ORBIT_HEIGHT, 0)` and turning
/// counter-clockwise seen from above.
fn orbit_position(t: f32) -> Vec3 {
    let angle = t * ORBIT_SPEED;
    Vec3::new(
        ORBIT_RADIUS * angle.cos(),
        ORBIT_HEIGHT,
        -ORBIT_RADIUS * angle.sin(),
    )
}

/// Fixed system: puts every entity named [`ORBITER`] on its orbit for the
/// current simulation time. Scene entities have no game-specific
/// components, so the system finds them by [`Name`]; reloading the scene
/// keeps it working.
fn orbit_system(world: &mut World, res: &mut Resources) {
    let t = res.expect::<Time>().elapsed as f32;
    for (name, transform) in world.query_mut::<(&Name, &mut Transform)>() {
        if name.0 == ORBITER {
            transform.translation = orbit_position(t);
        }
    }
}

/// The game's own systems, plus the engine's script, physics, and audio
/// systems (so scene scripts run, rigid bodies fall and collide, and
/// spatial sounds follow the camera). Add yours here; they run in the
/// window and under `--mcp` alike. Under `--mcp`, scripts and physics
/// run once `sim.play` has started play mode. Keep `physics` after
/// systems that move kinematic bodies.
fn game_schedule() -> Schedule {
    let mut schedule = Schedule::new();
    schedule.add_fixed("orbit", orbit_system);
    schedule.add_fixed("script", script_system);
    schedule.add_fixed("physics", physics_system);
    schedule.add_frame("audio", audio_system);
    schedule
}

/// Resources both modes start with: the camera's lens. The entry scene
/// sets where the camera is and what it looks at.
fn add_game_resources(res: &mut Resources) {
    res.insert(Camera {
        transform: Transform::default(),
        projection: Projection::Perspective {
            fov_y_radians: 60f32.to_radians(),
            near: 0.1,
            far: 200.0,
        },
    });
}

/// `game.hello` takes no arguments.
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct HelloArgs {}

/// Registers the game's own tools, next to the engine's. Add yours here:
/// a name `<subsystem>.<verb>`, a description for agents, an args struct
/// (its doc comments become the JSON schema), and a function of the
/// engine.
fn register_game_tools(registry: &mut ToolRegistry, project_name: String) {
    registry.register(TypedTool::new(
        "game.hello",
        "Returns the name of this game. An example of a project-defined tool.",
        move |_: &mut Headless, _: HelloArgs| Ok(ToolOutput::Text(project_name.clone())),
    ));
}

/// The entry scene's path, if `shin.toml` names one.
fn entry_scene(project: &Project) -> Option<PathBuf> {
    project
        .entry_scene
        .as_ref()
        .map(|scene| Path::new(ROOT).join(scene))
}

/// Serves the game over MCP on stdio: the engine's tools plus
/// [`register_game_tools`], on a headless engine running [`game_schedule`]
/// with the project (and its entry scene) open.
fn serve_mcp(project: Project) -> anyhow::Result<()> {
    let mut registry = ToolRegistry::with_engine_tools();
    register_game_tools(&mut registry, project.name.clone());
    let size = (project.window.width, project.window.height);
    shin_mcp::serve_stdio(registry, move || {
        let mut resources = Resources::new();
        add_game_resources(&mut resources);
        let mut engine = Headless::from_parts(World::new(), resources, game_schedule(), size)?;
        engine.load_project(Path::new(ROOT))?;
        Ok(engine)
    })
}

/// The network transport a server listens on.
///
/// No network transport is built into the engine yet, so this is an
/// in-process loopback with no clients: the server runs and simulates,
/// but nobody can connect from another process. The real transport
/// replaces this function's body.
fn server_transport(port: u16) -> Box<LoopbackTransport> {
    log::warn!(
        "no network transport is built yet: serving on an in-process loopback, \
         so port {port} is not opened and no remote client can join"
    );
    Box::new(LoopbackTransport::pair(0).0)
}

/// The network transport a client joins `addr` over.
///
/// No network transport is built into the engine yet, so joining another
/// process is not possible: this fails, saying so. The real transport
/// replaces this function's body.
fn client_transport(addr: &str) -> anyhow::Result<Box<dyn Transport + Send>> {
    anyhow::bail!(
        "cannot join {addr}: no network transport is built into the engine yet, \
         so a game cannot connect to another process. Joining over the network \
         arrives with the QUIC transport."
    )
}

/// The name this player joins with: the user's login name, or `player`.
fn player_name() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "player".to_string())
}

/// Runs a dedicated server: the game's schedule on a headless engine with
/// the project open, stepped at the fixed rate until Ctrl-C.
fn serve(project: Project, port: u16) -> anyhow::Result<()> {
    let size = (project.window.width, project.window.height);
    let mut resources = Resources::new();
    add_game_resources(&mut resources);
    let mut engine = Headless::from_parts(World::new(), resources, game_schedule(), size)?;
    engine.load_project(Path::new(ROOT))?;
    log::info!(
        "serving `{}` on port {port}; press Ctrl-C to stop",
        project.name
    );
    shin::net::run_server(&mut engine, server_transport(port), ServerOptions::default())
}

/// First-frame setup in the window: the renderer for the window's colour
/// format, the project's assets, the entry scene, and a fly camera facing
/// the way the scene's camera does.
fn load_scene(
    world: &mut World,
    res: &mut Resources,
    gpu: &GpuContext,
    format: wgpu::TextureFormat,
    scene: Option<&Path>,
) -> anyhow::Result<()> {
    res.insert(RenderState::new(&gpu.device, &gpu.queue, format));
    res.insert(Assets::new(PathBuf::from(ROOT)));
    if let Some(path) = scene {
        scene::load(world, res, &gpu.device, &gpu.queue, path)?;
    }
    let forward = res.expect::<Camera>().transform.forward();
    let fly = res.expect_mut::<FlyCamera>();
    fly.pitch = forward.y.asin();
    fly.yaw = forward.x.atan2(-forward.z);
    Ok(())
}

/// Plays the game in a window. The project's input actions (`shin.toml`
/// `[input.actions]`) are installed as the [`Actions`] resource, which
/// the engine fills from the keyboard, mouse, and gamepads every frame;
/// scripts read them with `action_pressed` and friends, and so can your
/// systems with `res.expect::<Actions>()`. Under `--mcp`,
/// [`Headless::load_project`] installs them for `input.set`.
///
/// With [`Net::Host`], the window is also a listen server: a
/// [`NetServer`] runs in the same engine, last in the fixed schedule, and
/// the person at the window is its local player one. With [`Net::Join`],
/// it is a client: a [`NetClient`] joins the server, applies its
/// snapshots, and interpolates the entities it replicates; the server's
/// entities are not simulated here.
fn run_window(project: Project, net: Net) -> anyhow::Result<()> {
    let mut schedule = game_schedule();
    schedule.add_frame("fly_camera", fly_camera_system);
    match &net {
        Net::Solo => {}
        Net::Host(_) => {
            schedule.add_fixed("net_server", net_server_system);
        }
        Net::Join(_) => {
            schedule.add_fixed("net_client", net_client_system);
            schedule.add_frame("net_interpolate", net_interpolate_system);
        }
    }
    let mut game = EcsGame::new(World::new(), schedule);
    match net {
        Net::Solo => {}
        Net::Host(port) => {
            let opts = ServerOptions::default();
            let mut server = NetServer::new(server_transport(port), opts.snapshot_every);
            server.add_local_player();
            game.resources.insert(server);
            log::info!("hosting `{}` on port {port}", project.name);
        }
        Net::Join(addr) => {
            let client = NetClient::new(client_transport(&addr)?, &player_name());
            game.resources.insert(client);
            log::info!("joining {addr}");
        }
    }
    game.title = project.window.title.clone();
    add_game_resources(&mut game.resources);
    game.resources.insert(FlyCamera::default());
    game.resources.insert(Actions::from_config(&project.input));

    let scene = entry_scene(&project);
    game.render_fn = Some(Box::new(move |world, res, gpu, color, depth| {
        let texture = color.texture();
        // The first frame loads the scene; on failure the game exits.
        if res.get::<RenderState>().is_none()
            && let Err(err) = load_scene(world, res, gpu, texture.format(), scene.as_deref())
        {
            log::error!("{err:#}");
            res.insert(ExitRequest(true));
            return;
        }
        let aspect = texture.width() as f32 / texture.height() as f32;
        render_world(world, res, gpu, color, depth, aspect);
    }));

    let config = AppConfig {
        title: project.window.title,
        width: project.window.width,
        height: project.window.height,
        clear_color: SKY,
        ..Default::default()
    };
    app::run(config, game)
}

fn main() -> anyhow::Result<()> {
    // Logs go to stderr, which keeps stdout free for MCP under `--mcp`.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let project = Project::load(Path::new(ROOT))?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse_mode(&args)? {
        Mode::Window => run_window(project, Net::Solo),
        Mode::Mcp => serve_mcp(project),
        Mode::Serve(port) => serve(project, port),
        Mode::Host(port) => run_window(project, Net::Host(port)),
        Mode::Join(addr) => run_window(project, Net::Join(addr)),
    }
}

/// The window's part in a multiplayer game.
enum Net {
    /// Not networked.
    Solo,
    /// A listen server on this port.
    Host(u16),
    /// A client of the server at this address.
    Join(String),
}

/// How the game was asked to run.
enum Mode {
    /// Play in a window (no arguments).
    Window,
    /// `--mcp`: serve to an agent over stdio.
    Mcp,
    /// `--serve [--port N]`: a dedicated server on port `N`.
    Serve(u16),
    /// `--host [--port N]`: a window that is also a server on port `N`.
    Host(u16),
    /// `--join <addr>`: a window that is a client of the server at `addr`
    /// (`host:port`).
    Join(String),
}

/// Reads the command line (without the program name).
fn parse_mode(args: &[String]) -> anyhow::Result<Mode> {
    const USAGE: &str =
        "usage: cargo run [-- --mcp | --serve [--port N] | --host [--port N] | --join <host:port>]";
    let port = |rest: &[String]| -> anyhow::Result<u16> {
        match rest {
            [] => Ok(DEFAULT_PORT),
            [flag, n] if flag == "--port" => n
                .parse()
                .map_err(|_| anyhow::anyhow!("--port needs a number from 0 to 65535, got `{n}`")),
            _ => anyhow::bail!(USAGE),
        }
    };
    match args {
        [] => Ok(Mode::Window),
        [flag] if flag == "--mcp" => Ok(Mode::Mcp),
        [flag, rest @ ..] if flag == "--serve" => Ok(Mode::Serve(port(rest)?)),
        [flag, rest @ ..] if flag == "--host" => Ok(Mode::Host(port(rest)?)),
        [flag, addr] if flag == "--join" && !addr.starts_with("--") => Ok(Mode::Join(addr.clone())),
        _ => anyhow::bail!(USAGE),
    }
}
