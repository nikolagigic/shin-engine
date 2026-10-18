//! GE-026: `shin-mcp --project <dir>` serves a generated project.
//!
//! Scaffolds a project with `project::create` (path dependency on this
//! checkout's engine), shrinks its window in `shin.toml`, spawns the binary
//! with `--project`, and over stdio: lists `project.open`/`project.info`,
//! calls `project.info`, loads the entry scene, lists entities, renders a
//! frame at the project's window size, saves a scene into the project, and
//! checks that a path escaping the project root is rejected.

mod common;

use std::fs;
use std::path::PathBuf;

use base64::Engine as _;
use serde_json::{Value, json};
use shin::project::{self, EngineDep, NewProjectOptions, Project};

use common::{Server, first_text, parse_json_line};

/// A fresh project called `name` in the system temp dir, with a 96x54
/// window.
fn scaffold(name: &str) -> anyhow::Result<PathBuf> {
    let dir = std::env::temp_dir()
        .join(format!("shin-mcp-project-{}", std::process::id()))
        .join(name);
    let _ = fs::remove_dir_all(&dir);
    project::create(&NewProjectOptions {
        name: name.to_string(),
        dir: dir.clone(),
        engine: EngineDep::Path(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../shin-engine")),
    })?;
    let mut manifest = Project::load(&dir)?;
    manifest.window.width = 96;
    manifest.window.height = 54;
    manifest.save(&dir)?;
    Ok(dir)
}

#[test]
fn serves_a_generated_project() -> anyhow::Result<()> {
    let dir = scaffold("mcp-game")?;
    let mut server = Server::spawn(&["--project", dir.to_str().unwrap()]);
    server.handshake("project_server");

    // Both project tools are listed, with descriptions.
    let list = server.request(2, "tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().expect("tools");
    for name in ["project.open", "project.info"] {
        let tool = tools
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name} not listed"));
        let description = tool["description"].as_str().unwrap_or_default();
        assert!(!description.is_empty(), "{name} has no description");
    }

    let info = server.call_json("project.info", json!({}));
    assert_eq!(info["name"], "mcp-game", "{info}");
    assert_eq!(info["dir"], json!(dir.canonicalize()?), "{info}");
    assert_eq!(info["entry_scene"], project::STARTER_SCENE, "{info}");
    assert_eq!(info["window"]["width"], 96, "{info}");
    assert_eq!(info["window"]["height"], 54, "{info}");

    // The entry scene is already loaded; loading it again by its relative
    // path resolves against the project root.
    let entities = server.call_json("entity.list", json!({}));
    // Ground, cube, moon, and the crate stack (GE-034).
    let starter = 3 + project::STARTER_CRATES;
    assert_eq!(
        entities.as_array().map(Vec::len),
        Some(starter),
        "{entities}"
    );
    let loaded = server.call_json("scene.load", json!({ "path": project::STARTER_SCENE }));
    assert_eq!(loaded["entities"], starter, "{loaded}");
    let entities = server.call_json("entity.list", json!({}));
    let entities = entities.as_array().expect("entity.list returns an array");
    assert!(!entities.is_empty());
    let names: Vec<&str> = entities.iter().filter_map(|e| e["name"].as_str()).collect();
    let crates: Vec<String> = (1..=project::STARTER_CRATES)
        .map(|i| format!("crate {i}"))
        .collect();
    let expected: Vec<&str> = ["ground", "cube", "moon"]
        .into_iter()
        .chain(crates.iter().map(String::as_str))
        .collect();
    assert_eq!(names, expected);

    // The frame defaults to the project's window size and decodes as PNG.
    let frame = server.call("render.frame", json!({}));
    let block = &frame["content"][0];
    assert_eq!(block["type"], "image", "{frame}");
    assert_eq!(block["mimeType"], "image/png", "{frame}");
    let png = base64::prelude::BASE64_STANDARD.decode(block["data"].as_str().unwrap())?;
    let image = image::load_from_memory(&png)?;
    assert_eq!((image.width(), image.height()), (96, 54));

    // Saving with a relative path writes under the project directory.
    let saved = server.call_json("scene.save", json!({ "path": "scenes/out.scene.ron" }));
    assert_eq!(saved["entities"], starter, "{saved}");
    let out = dir.join("scenes/out.scene.ron");
    assert!(out.is_file(), "{} was not written", out.display());

    // A path that escapes the project root is a tool error naming it.
    let escaped = server.call("scene.save", json!({ "path": "../escape.scene.ron" }));
    assert_eq!(escaped["isError"], json!(true), "{escaped}");
    assert!(
        first_text(&escaped).contains("../escape.scene.ron"),
        "{escaped}"
    );
    assert!(!dir.join("../escape.scene.ron").exists());

    for line in server.shutdown() {
        let _: Value = parse_json_line(&line);
    }
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn width_and_height_override_the_project_window() -> anyhow::Result<()> {
    let dir = scaffold("mcp-override")?;
    let mut server = Server::spawn(&["--project", dir.to_str().unwrap(), "--width", "40"]);
    server.handshake("project_server");
    let info = server.call_json("engine.info", json!({}));
    assert_eq!(
        info["target"],
        json!({ "width": 40, "height": 54 }),
        "{info}"
    );
    server.shutdown();
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}
