//! Scene file tools (GE-021): `scene.save` and `scene.load`.
//!
//! With a project open (GE-026), relative paths are resolved against its
//! [`ProjectRoot`] and a path that escapes the root is rejected. Otherwise
//! they are resolved against the [`Assets`] root, the same directory
//! relative mesh sources use.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::asset::Assets;
use crate::command::Affected;
use crate::headless::Headless;
use crate::math::Transform;
use crate::project::ProjectRoot;
use crate::tool::{ToolOutput, ToolRegistry, TypedTool};

/// Registers `scene.save` and `scene.load`.
pub fn register_tools(reg: &mut ToolRegistry) {
    reg.register(TypedTool::new(
        "scene.save",
        "Saves every entity (name, transform, mesh source, colour, texture, script, with \
         children nested under their parent), the camera, and the light to a .scene.ron \
         file, creating missing directories. Returns the path written and the number of \
         entities.",
        scene_save,
    ));
    reg.register(
        TypedTool::new(
            "scene.load",
            "Replaces every entity with those in a .scene.ron file and applies its camera and \
         light, loading meshes by asset source. On error the scene is left unchanged. \
         Returns the number of entities spawned.",
            scene_load,
        )
        .with_affected(|_, _| Affected::Scene),
    );
}

/// Arguments of `scene.save`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SaveArgs {
    /// File to write, ending in ".scene.ron", e.g. "scenes/main.scene.ron".
    /// Relative paths are resolved against the open project's directory
    /// (paths leaving it are rejected), or the engine's working directory
    /// when no project is open. An existing file is overwritten.
    path: String,
}

fn scene_save(engine: &mut Headless, args: SaveArgs) -> anyhow::Result<ToolOutput> {
    let path = resolve(engine, &args.path)?;
    super::save(&engine.world, &engine.resources, &path)?;
    let entities = engine.world.query::<&Transform>().iter().count();
    Ok(ToolOutput::Json(
        json!({ "path": path, "entities": entities }),
    ))
}

/// Arguments of `scene.load`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LoadArgs {
    /// Scene file to read, e.g. "scenes/main.scene.ron". Relative paths are
    /// resolved against the open project's directory (paths leaving it are
    /// rejected), or the engine's working directory when no project is open.
    path: String,
}

fn scene_load(engine: &mut Headless, args: LoadArgs) -> anyhow::Result<ToolOutput> {
    let path = resolve(engine, &args.path)?;
    let entities = super::load(
        &mut engine.world,
        &mut engine.resources,
        &engine.gpu.device,
        &engine.gpu.queue,
        &path,
    )?;
    Ok(ToolOutput::Json(json!({ "entities": entities })))
}

/// `path` resolved against the open project's [`ProjectRoot`] (rejecting
/// paths that escape it), or against the [`Assets`] root when no project
/// is open.
fn resolve(engine: &Headless, path: &str) -> anyhow::Result<PathBuf> {
    if let Some(root) = engine.resources.get::<ProjectRoot>() {
        return root.resolve(Path::new(path));
    }
    let assets = engine
        .resources
        .get::<Assets>()
        .context("no Assets resource")?;
    Ok(assets.resolve(Path::new(path)))
}
