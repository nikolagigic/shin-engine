//! GE-043: multiplayer end to end in one process (ADR 0009). Two
//! `Headless` engines, a server running the starter project's scene with
//! physics and a client joined to it over a `LoopbackTransport`, are
//! stepped side by side and compared through `entity.list` and their
//! rendered frames. Once the client joins, the starter's
//! `scripts/server.rhai` gives it a replicated player cube on the server
//! (GE-044), which the client must show too.
//!
//! - `client_matches_server_after_120_ticks`: a perfect loopback, a
//!   snapshot every tick, no interpolation delay; after 120 ticks the
//!   client's replicated entities list exactly as the server's.
//! - `golden_net_replicated`: in that state the client's frame equals the
//!   server's pixel for pixel (same project, so the same camera, light,
//!   and assets).
//! - `client_converges_under_latency_and_loss`: 100 ms one-way latency
//!   with jitter and 10 % snapshot loss, the default snapshot rate and
//!   interpolation delay; once the server's crates come to rest the
//!   client's positions agree within 1e-3.

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use common::{GoldenOpts, assert_golden_in, golden_dir};
use serde_json::{Value, json};
use shin::ecs::Entity;
use shin::headless::Headless;
use shin::math::Transform;
use shin::net::{
    LoopbackOptions, LoopbackTransport, NetClient, NetServer, Replicated, ServerOptions,
    net_client_system, net_interpolate_system, net_server_system,
};
use shin::project::{self, NewProjectOptions};
use shin::tool::{ToolOutput, ToolRegistry};

/// Both engines render at this size (the starter's 1280x720, scaled
/// down to keep software-GPU CI fast).
const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;

/// `entity.list` fields that differ between server and client by design:
/// local entity ids, and what snapshots do not carry (scripts, audio,
/// physics).
const NOT_REPLICATED: [&str; 5] = ["entity", "script", "audio", "body", "collider"];

/// A fresh starter project in the temp dir, removed on drop.
struct StarterProject(PathBuf);

impl StarterProject {
    fn create(label: &str) -> anyhow::Result<Self> {
        let dir =
            std::env::temp_dir().join(format!("shin-net-loopback-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        project::create(&NewProjectOptions {
            name: "net-loopback".into(),
            dir: dir.clone(),
            engine: Default::default(),
        })?;
        Ok(Self(dir))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for StarterProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The server: the starter project open, every entity replicated, a
/// [`NetServer`] on `transport` sending a snapshot every `snapshot_every`
/// ticks after the default schedule's script and physics systems, and
/// playing (so the crates fall).
fn server(
    project: &StarterProject,
    transport: LoopbackTransport,
    snapshot_every: u32,
) -> anyhow::Result<Headless> {
    let mut engine = Headless::open_project(project.path())?;
    engine.resize(WIDTH, HEIGHT);
    let all: Vec<Entity> = engine
        .world
        .query::<(Entity, &Transform)>()
        .iter()
        .map(|(e, _)| e)
        .collect();
    for entity in all {
        engine.world.insert_one(entity, Replicated)?;
    }
    engine.schedule.add_fixed("net_server", net_server_system);
    engine
        .resources
        .insert(NetServer::new(Box::new(transport), snapshot_every));
    engine.play()?;
    Ok(engine)
}

/// The client: the same project open (its camera, light, and assets) but
/// with no entities of its own, a [`NetClient`] on `transport` with an
/// interpolation delay of `delay` ticks, and playing.
fn client(
    project: &StarterProject,
    transport: LoopbackTransport,
    delay: u64,
) -> anyhow::Result<Headless> {
    let mut engine = Headless::open_project(project.path())?;
    engine.resize(WIDTH, HEIGHT);
    engine.world.clear();
    engine.schedule.add_fixed("net_client", net_client_system);
    engine
        .schedule
        .add_frame("net_interpolate", net_interpolate_system);
    engine
        .resources
        .insert(NetClient::with_delay(Box::new(transport), "ada", delay));
    engine.play()?;
    Ok(engine)
}

/// `entity.list` with the [`NOT_REPLICATED`] fields removed.
fn replicated_list(reg: &ToolRegistry, engine: &mut Headless) -> anyhow::Result<Vec<Value>> {
    let ToolOutput::Json(Value::Array(rows)) = reg.call(engine, "entity.list", json!({}))? else {
        anyhow::bail!("entity.list returned no array");
    };
    Ok(rows
        .into_iter()
        .map(|mut row| {
            let fields = row.as_object_mut().expect("rows are objects");
            for field in NOT_REPLICATED {
                fields.remove(field);
            }
            row
        })
        .collect())
}

/// A server and a client on a perfect loopback, a snapshot every tick and
/// no interpolation delay, stepped together for 120 ticks.
fn lockstep_pair(project: &StarterProject) -> anyhow::Result<(Headless, Headless)> {
    let (server_end, mut client_ends) = LoopbackTransport::pair(1);
    let mut server = server(project, server_end, 1)?;
    let mut client = client(project, client_ends.remove(0), 0)?;
    for _ in 0..120 {
        server.step(1);
        client.step(1);
    }
    Ok((server, client))
}

#[test]
fn client_matches_server_after_120_ticks() -> anyhow::Result<()> {
    let project = StarterProject::create("list")?;
    let (mut server, mut client) = lockstep_pair(&project)?;
    let reg = ToolRegistry::with_engine_tools();
    let want = replicated_list(&reg, &mut server)?;
    let got = replicated_list(&reg, &mut client)?;
    assert!(want.len() > 5, "ground, cube, moon, crates: {want:?}");
    assert!(
        want.iter().all(|row| row["net_id"].is_u64()),
        "every server entity has been captured"
    );
    // Snapshots carry rotations as Euler degrees rounded to 1e-4
    // (`TransformDesc`), so the client's rotation is the server's to
    // within that; every other field must match exactly.
    assert_eq!(got.len(), want.len());
    for (ours, theirs) in got.iter().zip(&want) {
        let (mut ours, mut theirs) = (ours.clone(), theirs.clone());
        let a = take_rotation(&mut ours);
        let b = take_rotation(&mut theirs);
        assert_eq!(ours, theirs);
        for (x, y) in a.iter().zip(b) {
            assert!(
                (x - y).abs() <= ROTATION_QUANTUM,
                "{}: rotation {a:?} vs {b:?}",
                theirs["name"]
            );
        }
    }
    Ok(())
}

/// How finely snapshots round rotations, in degrees, with a little slack
/// for `f32`.
const ROTATION_QUANTUM: f64 = 1.1e-4;

/// Removes `rotation_euler_deg` from an `entity.list` row and returns it.
fn take_rotation(row: &mut Value) -> [f64; 3] {
    let rotation = row
        .as_object_mut()
        .and_then(|fields| fields.remove("rotation_euler_deg"))
        .expect("a rotation");
    [0, 1, 2].map(|i| rotation[i].as_f64().expect("a number"))
}

#[test]
fn golden_net_replicated() -> anyhow::Result<()> {
    let project = StarterProject::create("golden")?;
    let (mut server, mut client) = lockstep_pair(&project)?;
    let theirs = server.render()?;
    let ours = client.render()?;
    // The server's frame is the reference. Comparing through the golden
    // harness writes both frames and a diff under `tests/golden/_diff/`
    // (gitignored) on failure.
    let dir = golden_dir().join("_diff").join("net_replicated");
    let exact = GoldenOpts {
        max_channel_diff: 0,
        max_diff_fraction: 0.0,
    };
    assert_golden_in(&dir, "server", &theirs, exact, true);
    assert_golden_in(&dir, "server", &ours, exact, false);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// The world translation of every entity in `list`, by name (the starter
/// scene's names are unique; net ids are null before the first capture).
fn positions(list: &[Value]) -> HashMap<String, [f64; 3]> {
    list.iter()
        .map(|row| {
            let id = row["name"].as_str().expect("a name").to_string();
            let p = &row["world_translation"];
            let xyz = [0, 1, 2].map(|i| p[i].as_f64().expect("a number"));
            (id, xyz)
        })
        .collect()
}

/// Whether two position sets list the same entities. The server's set
/// grows when the client joins: the starter's `scripts/server.rhai` gives
/// the new player a cube (GE-044).
fn same_entities(a: &HashMap<String, [f64; 3]>, b: &HashMap<String, [f64; 3]>) -> bool {
    a.len() == b.len() && a.keys().all(|id| b.contains_key(id))
}

/// The largest coordinate difference between two position sets with the
/// same ids.
fn max_difference(a: &HashMap<String, [f64; 3]>, b: &HashMap<String, [f64; 3]>) -> f64 {
    assert_eq!(
        a.keys().collect::<std::collections::BTreeSet<_>>(),
        b.keys().collect(),
        "the same entities"
    );
    a.iter()
        .flat_map(|(id, p)| (0..3).map(move |i| (p[i] - b[id][i]).abs()))
        .fold(0.0, f64::max)
}

#[test]
fn client_converges_under_latency_and_loss() -> anyhow::Result<()> {
    let project = StarterProject::create("lossy")?;
    // 100 ms one-way at 60 ticks per second, a little jitter, and one
    // snapshot in ten lost.
    let network = LoopbackOptions {
        latency_ticks: 6,
        jitter_ticks: 2,
        drop_unreliable: 0.1,
        seed: 43,
    };
    let (server_end, mut client_ends) = LoopbackTransport::pair_with(1, network);
    let every = ServerOptions::default().snapshot_every;
    let mut server = server(&project, server_end, every)?;
    let mut client = client(&project, client_ends.remove(0), 2)?;
    let reg = ToolRegistry::with_engine_tools();

    // Step both until the server has been still for a second.
    let tick = |server: &mut Headless, client: &mut Headless| {
        server.step(1);
        client.step(1);
        net_interpolate_system(&mut client.world, &mut client.resources);
    };
    let mut last = positions(&replicated_list(&reg, &mut server)?);
    let mut still = 0;
    let mut ticks = 0;
    let mut worst_lag: f64 = 0.0;
    while still < 60 {
        tick(&mut server, &mut client);
        ticks += 1;
        assert!(ticks < 1200, "the server's crates never came to rest");
        let now = positions(&replicated_list(&reg, &mut server)?);
        let shown = positions(&replicated_list(&reg, &mut client)?);
        if same_entities(&now, &shown) {
            worst_lag = worst_lag.max(max_difference(&now, &shown));
        }
        // An entity appearing (the client's player cube) is not still.
        still = if same_entities(&now, &last) && max_difference(&now, &last) == 0.0 {
            still + 1
        } else {
            0
        };
        last = now;
    }

    // The client caught up while the server stood still: latency, jitter,
    // the snapshot interval, and the interpolation delay add up to well
    // under a second, and losing a snapshot only waits for the next one.
    assert!(
        worst_lag > 0.05,
        "the client showed the falling crates late: at most {worst_lag} behind"
    );
    let theirs = positions(&replicated_list(&reg, &mut server)?);
    let ours = positions(&replicated_list(&reg, &mut client)?);
    let diff = max_difference(&theirs, &ours);
    assert!(
        diff <= 1e-3,
        "the client converged after {ticks} ticks: off by {diff}"
    );
    Ok(())
}
