//! GE-018: a freshly scaffolded project builds against this checkout of the
//! engine, and its `shin.toml` loads.
//!
//! The tests sit in `mod project` so the ticket's filter
//! (`cargo test -p shin-engine project::`) selects them.

mod project {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use shin::project::{self, EngineDep, NewProjectOptions, Project};

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

    /// Best-effort cleanup; a leftover temp dir is not a test failure.
    fn remove(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }
}
