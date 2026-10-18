//! GE-027: a generated project serves its own world with `--mcp`.
//!
//! Scaffolds a project (path dependencies on this checkout's engine and
//! `shin-mcp`), builds it with `cargo build`, spawns its binary with
//! `--mcp`, and over stdio checks that the project's own tool
//! (`game.hello`) is listed and callable, and that the project's own
//! system (the starter scene's orbiting `moon`) runs under `sim.step`.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use shin::project::{self, EngineDep, NewProjectOptions, Project};

use common::{Server, first_text, parse_json_line};

/// The generated project's name, and so its binary's.
const NAME: &str = "hosted-game";

/// The workspace root of this checkout.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The cargo running this test, or `cargo` from PATH.
fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

/// Scaffolds [`NAME`] with a 64x36 window into a fresh temp directory and
/// builds it. Returns the project root and its binary.
fn scaffold_and_build() -> anyhow::Result<(PathBuf, PathBuf)> {
    let dir = std::env::temp_dir()
        .join(format!("shin-mcp-hosted-{}", std::process::id()))
        .join(NAME);
    let _ = fs::remove_dir_all(&dir);
    project::create(&NewProjectOptions {
        name: NAME.to_string(),
        dir: dir.clone(),
        engine: EngineDep::Path(workspace_root().join("crates/shin-engine")),
    })?;
    let mut manifest = Project::load(&dir)?;
    manifest.window.width = 64;
    manifest.window.height = 36;
    manifest.save(&dir)?;

    // Resolve the versions this workspace is tested with, and build into a
    // directory reserved for this test inside this checkout's `target/`,
    // so reruns are incremental and nothing is shared with other checkouts.
    fs::copy(workspace_root().join("Cargo.lock"), dir.join("Cargo.lock"))?;
    let target_dir = workspace_root().join("target/project-hosted-mcp");
    let status = Command::new(cargo())
        .args(["build", "--quiet", "--target-dir"])
        .arg(&target_dir)
        .current_dir(&dir)
        .env_remove("CARGO_TARGET_DIR")
        .status()?;
    anyhow::ensure!(
        status.success(),
        "`cargo build` failed in {}",
        dir.display()
    );

    let binary = target_dir
        .join("debug")
        .join(format!("{NAME}{}", std::env::consts::EXE_SUFFIX));
    anyhow::ensure!(binary.is_file(), "{} was not built", binary.display());
    Ok((dir, binary))
}

/// The translation of the entity named `name` in an `entity.list` result.
fn translation_of(entities: &Value, name: &str) -> [f64; 3] {
    let entity = entities
        .as_array()
        .expect("entity.list returns an array")
        .iter()
        .find(|e| e["name"] == name)
        .unwrap_or_else(|| panic!("no entity named {name} in {entities}"));
    serde_json::from_value(entity["translation"].clone())
        .unwrap_or_else(|err| panic!("bad translation ({err}): {entity}"))
}

/// Asserts `a` and `b` agree to within 1e-4 on every axis.
fn assert_near(a: [f64; 3], b: [f64; 3]) {
    let close = a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-4);
    assert!(close, "{a:?} != {b:?}");
}

#[test]
fn generated_project_serves_its_own_tools_and_systems() -> anyhow::Result<()> {
    let (dir, binary) = scaffold_and_build()?;
    let mut server = Server::spawn_program(&binary, &["--mcp"]);
    server.handshake("project_hosted");

    // The project's tool is listed next to the engine's.
    let list = server.request(2, "tools/list", json!({}));
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("no tools in {list}"))
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    for name in ["game.hello", "sim.step", "entity.list", "history.undo"] {
        assert!(names.contains(&name), "{name} missing from {names:?}");
    }

    // The game serves the engine's skills as prompts too (GE-039).
    let prompts = server.request(3, "prompts/list", json!({}));
    let prompts = prompts["result"]["prompts"]
        .as_array()
        .unwrap_or_else(|| panic!("no prompts in {prompts}"));
    assert_eq!(prompts.len(), shin::project::skills::SKILLS.len());

    // ...and callable.
    let hello = server.call("game.hello", json!({}));
    assert_ne!(hello["isError"], json!(true), "{hello}");
    assert_eq!(first_text(&hello), NAME);

    // The project is open: its entry scene, at its window size.
    let info = server.call_json("engine.info", json!({}));
    assert_eq!(
        info["target"],
        json!({ "width": 64, "height": 36 }),
        "{info}"
    );
    let before = server.call_json("entity.list", json!({}));
    assert_near(translation_of(&before, "moon"), [1.6, 0.5, 0.0]);

    // The project's orbit system runs under sim.step: after one simulated
    // second (60 steps of 1/60 s) the moon is one radian round its orbit.
    let time = server.call_json("sim.step", json!({ "steps": 60 }));
    assert_eq!(time["fixed_steps"], 60, "{time}");
    let after = server.call_json("entity.list", json!({}));
    let (sin, cos) = 1.0f64.sin_cos();
    assert_near(translation_of(&after, "moon"), [1.6 * cos, 0.5, -1.6 * sin]);
    // Only the moon moves.
    assert_near(
        translation_of(&after, "cube"),
        translation_of(&before, "cube"),
    );

    for line in server.shutdown() {
        parse_json_line(&line);
    }
    remove(&dir);
    Ok(())
}

/// Best-effort cleanup; a leftover temp dir is not a test failure.
fn remove(dir: &Path) {
    let _ = fs::remove_dir_all(dir);
}
