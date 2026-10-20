//! GE-018: a freshly scaffolded project builds against this checkout of the
//! engine, and its `shin.toml` loads. GE-044: its scripts run solo and as
//! a host.
//!
//! The tests sit in `mod project` so the ticket's filter
//! (`cargo test -p shin-engine project::`) selects them.

mod project {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use shin::ecs::{Resources, Schedule, World};
    use shin::headless::Headless;
    use shin::input::{ActionValue, Actions};
    use shin::math::{Transform, Vec3};
    use shin::net::{
        Channel, InputFrame, LoopbackTransport, Message, NetServer, Owner, PROTOCOL_VERSION,
        PeerId, PlayerId, Transport, net_server_input_system, net_server_system,
    };
    use shin::project::{self, EngineDep, NewProjectOptions, Project};
    use shin::render::MeshInstance;
    use shin::script::{ScriptEngine, script_system};

    /// `crates/shin-engine` of this checkout.
    fn engine_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    /// The workspace root (two levels above the engine crate).
    fn workspace_root() -> PathBuf {
        engine_dir().join("../..")
    }

    /// The cargo running this test, or `cargo` from PATH.
    fn cargo() -> String {
        std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
    }

    /// Scaffolds `name` into a fresh temp directory with a path dependency
    /// on this checkout's engine, and returns the project root.
    fn scaffold(name: &str) -> anyhow::Result<PathBuf> {
        let dir = std::env::temp_dir()
            .join(format!("shin-scaffold-{}", std::process::id()))
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        project::create(&NewProjectOptions {
            name: name.to_string(),
            dir: dir.clone(),
            engine: EngineDep::Path(engine_dir()),
        })?;
        Ok(dir)
    }

    #[test]
    fn scaffolded_manifest_round_trips() -> anyhow::Result<()> {
        let dir = scaffold("round-trip")?;
        let loaded = Project::load(&dir)?;
        // The starter binds jump, move_x, and move_z (GE-033).
        let actions: Vec<&str> = loaded.input.actions.keys().map(String::as_str).collect();
        assert_eq!(actions, ["jump", "move_x", "move_z"]);
        let mut expected = Project::new("round-trip");
        expected.entry_scene = Some(PathBuf::from(project::STARTER_SCENE));
        expected.input = loaded.input.clone();
        assert_eq!(loaded, expected);
        assert_eq!(loaded.window.title, "round-trip");
        loaded.save(&dir)?;
        assert_eq!(Project::load(&dir)?, loaded);
        fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn scaffolded_project_passes_cargo_check() -> anyhow::Result<()> {
        if Command::new(cargo()).arg("--version").output().is_err() {
            eprintln!("SKIPPED: `cargo` is not on PATH; cannot check the scaffolded project");
            return Ok(());
        }
        let dir = scaffold("check-game")?;

        // Start from this workspace's lock file so the project resolves the
        // same dependency versions the engine is tested with, without
        // needing the network.
        fs::copy(workspace_root().join("Cargo.lock"), dir.join("Cargo.lock"))?;
        // A target directory reserved for this test inside this checkout's
        // `target/`, so reruns are incremental and nothing is shared with
        // other checkouts.
        let target_dir = workspace_root().join("target/project-scaffold-check");

        let status = Command::new(cargo())
            .arg("check")
            .arg("--quiet")
            .arg("--target-dir")
            .arg(&target_dir)
            .current_dir(&dir)
            .env_remove("CARGO_TARGET_DIR")
            .status()?;
        assert!(
            status.success(),
            "`cargo check` failed in {} ({status})",
            dir.display()
        );
        remove(&dir);
        Ok(())
    }

    /// A headless engine set up as the starter's `src/main.rs` sets up a
    /// solo game (and `--mcp`): the game schedule's script system between
    /// the two halves of the server tick, `server` as the server, and the
    /// scaffolded project open, playing.
    fn starter_engine(dir: &Path, server: NetServer) -> anyhow::Result<Headless> {
        let mut schedule = Schedule::new();
        schedule.add_fixed("net_server_input", net_server_input_system);
        schedule.add_fixed("script", script_system);
        schedule.add_fixed("net_server", net_server_system);
        let mut resources = Resources::new();
        resources.insert(server);
        let mut engine = Headless::from_parts(World::new(), resources, schedule, (16, 16))?;
        engine.load_project(dir)?;
        engine.play()?;
        Ok(engine)
    }

    /// Every player's cube: `(owner, translation)`, by owner.
    fn player_cubes(engine: &Headless) -> Vec<(PlayerId, Vec3)> {
        let mut cubes: Vec<_> = engine
            .world
            .query::<(&Owner, &Transform, &MeshInstance)>()
            .iter()
            .map(|(owner, transform, _)| (owner.0, transform.translation))
            .collect();
        cubes.sort_by_key(|(owner, _)| *owner);
        cubes
    }

    fn script_errors(engine: &Headless) -> Vec<String> {
        engine
            .resources
            .get::<ScriptEngine>()
            .map(|scripts| scripts.errors().map(ToString::to_string).collect())
            .unwrap_or_default()
    }

    /// GE-044: a fresh project plays solo with its own `server.rhai` and
    /// `player.rhai`: the one local player gets a cube, which the local
    /// input actions move.
    #[test]
    fn scaffolded_project_runs_solo_with_its_scripts() -> anyhow::Result<()> {
        let dir = scaffold("solo-game")?;
        let mut engine = starter_engine(&dir, NetServer::solo())?;
        engine.step(1);
        let start = player_cubes(&engine);
        assert_eq!(start.len(), 1, "one cube for the one player: {start:?}");
        assert_eq!(start[0].0, PlayerId(1));

        engine
            .resources
            .expect_mut::<Actions>()
            .set_value("move_x", 1.0)?;
        engine.step(30);
        let moved = player_cubes(&engine)[0].1 - start[0].1;
        assert!(moved.x > 1.0, "the arrow keys move the cube: {moved}");
        assert_eq!(script_errors(&engine), Vec::<String>::new());
        remove(&dir);
        Ok(())
    }

    /// GE-044: the same project as a host: a client that joins gets its
    /// own cube and moves only that one; the host's cube stays put.
    #[test]
    fn scaffolded_project_hosts_a_client_with_its_scripts() -> anyhow::Result<()> {
        let dir = scaffold("host-game")?;
        let (transport, mut clients) = LoopbackTransport::pair(1);
        let mut server = NetServer::new(Box::new(transport), 3);
        server.add_local_player();
        let mut engine = starter_engine(&dir, server)?;
        let guest = &mut clients[0];
        let hello = Message::Hello {
            protocol: PROTOCOL_VERSION,
            name: "guest".into(),
        };
        guest.send(PeerId(0), &hello, Channel::Reliable);
        engine.step(1);
        let start = player_cubes(&engine);
        let owners: Vec<PlayerId> = start.iter().map(|(owner, _)| *owner).collect();
        assert_eq!(owners, [PlayerId(1), PlayerId(2)]);

        for tick in 0..30 {
            let frame = InputFrame {
                tick,
                actions: vec![(
                    "move_z".to_string(),
                    ActionValue {
                        pressed: true,
                        just_pressed: tick == 0,
                        axis: 1.0,
                    },
                )],
            };
            guest.send(PeerId(0), &Message::Input(frame), Channel::Unreliable);
            engine.step(1);
        }
        let end = player_cubes(&engine);
        assert_eq!(end[0].1, start[0].1, "the host did not move");
        assert!(end[1].1.z < start[1].1.z - 1.0, "the guest went forward");
        assert_eq!(script_errors(&engine), Vec::<String>::new());
        remove(&dir);
        Ok(())
    }

    /// Best-effort cleanup; a leftover temp dir is not a test failure.
    fn remove(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }
}
