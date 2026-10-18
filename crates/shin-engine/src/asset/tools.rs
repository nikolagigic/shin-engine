//! Asset tools (GE-015, GE-019, GE-025): `asset.load`, `scene.load_gltf`,
//! and `mesh.builtin`. All three resolve [`AssetSource`]s through the
//! [`Assets`] resource, which uploads each source once into the renderer's
//! [`crate::render::MeshRegistry`] or [`crate::render::TextureRegistry`].
//! `asset.list` (GE-032, in [`super::scan`]) lists the project's assets.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Context as _;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use super::registry::sphere_name;
use super::{AssetSource, Assets};
use crate::gpu::GpuContext;
use crate::headless::Headless;
use crate::render::{MeshInstance, RenderState};
use crate::tool::{ToolOutput, ToolRegistry, TypedTool};

/// Registers `asset.load`, `asset.list`, `scene.load_gltf`, and
/// `mesh.builtin`.
pub fn register_tools(reg: &mut ToolRegistry) {
    reg.register(TypedTool::new(
        "asset.load",
        "Uploads the mesh or texture named by an asset source, or decodes a sound. A mesh \
         returns {\"handle\": n} and a texture (PNG/JPEG file or glTF image) returns \
         {\"texture\": n}; pass them to entity.spawn as `mesh` and `texture`. A sound \
         (.ogg/.wav) returns {\"sound\": source, \"duration_s\": seconds}; play it with \
         audio.play or entity.set_audio. Loading the same source again returns the same \
         handle.",
        asset_load,
    ));
    reg.register(TypedTool::new(
        "asset.list",
        "Lists what the open project contains, from its assets/, scenes/, and scripts/ \
         directories: one entry per glTF triangle primitive (kind \"mesh\"), per image \
         file or glTF image (\"texture\"), per .ogg or .wav file (\"audio\"), per \
         .scene.ron file (\"scene\"), and per .rhai file (\"script\"), each with the \
         `source` string the matching tool takes (asset.load, entity.set_mesh, \
         entity.set_texture, audio.play, entity.set_audio, scene.load, script.attach) \
         and the file's path relative to the project.",
        super::scan::asset_list,
    ));
    reg.register(TypedTool::new(
        "scene.load_gltf",
        "Loads a glTF 2.0 file (.gltf or .glb), uploads its meshes and base-colour \
         textures, and spawns one entity per node with a mesh, at the node's world \
         transform, tinted by its material's base colour and textured by its \
         base-colour texture. Returns the new mesh handles, texture handles (one per \
         glTF image used as a base colour), and entity ids.",
        load_gltf,
    ));
    reg.register(TypedTool::new(
        "mesh.builtin",
        "Uploads a built-in mesh: a unit cube (extent -0.5..0.5) or a unit-radius UV \
         sphere. Returns its handle for entity.spawn.",
        mesh_builtin,
    ));
}

/// Runs `f` with the [`Assets`] resource, the GPU context, and the
/// [`RenderState`] (mesh and texture registries) borrowed together.
///
/// [`crate::ecs::Resources`] hands out one mutable borrow at a time, so the
/// `Assets` value is moved out for the call and put back afterwards.
pub(crate) fn with_assets<R>(
    engine: &mut Headless,
    f: impl FnOnce(&mut Assets, &GpuContext, &mut RenderState) -> R,
) -> anyhow::Result<R> {
    let slot = engine
        .resources
        .get_mut::<Assets>()
        .context("no Assets resource")?;
    let mut assets = std::mem::replace(slot, Assets::new(PathBuf::new()));
    let result = match engine.resources.get_mut::<RenderState>() {
        Some(state) => Ok(f(&mut assets, &engine.gpu, state)),
        None => Err(anyhow::anyhow!("no RenderState resource")),
    };
    engine.resources.insert(assets);
    result
}

/// Arguments of `asset.load`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AssetLoadArgs {
    /// Asset source. Meshes: "builtin://cube", "builtin://sphere" (32x16),
    /// "builtin://sphere-<segments>x<rings>", or a glTF path with an optional
    /// triangle-primitive index, e.g. "assets/x.gltf" or "assets/x.gltf#2".
    /// Textures: a .png/.jpg/.jpeg path, e.g. "assets/checker.png", or a
    /// glTF image, e.g. "assets/x.gltf#image=0". Sounds: an .ogg/.wav path,
    /// e.g. "assets/sounds/blip.wav". Relative paths are resolved against
    /// the project directory (the engine's working directory).
    source: String,
}

fn asset_load(engine: &mut Headless, args: AssetLoadArgs) -> anyhow::Result<ToolOutput> {
    let source = AssetSource::parse(&args.source)?;
    if source.is_audio() {
        let sound = engine
            .resources
            .get_mut::<Assets>()
            .context("no Assets resource")?
            .sound(&source)?;
        return Ok(ToolOutput::Json(json!({
            "sound": source.to_string(),
            "duration_s": sound.duration().as_secs_f64(),
        })));
    }
    if source.is_texture() {
        let texture = with_assets(engine, |assets, gpu, state| {
            assets.texture(&gpu.device, &gpu.queue, &mut state.textures, &source)
        })??;
        return Ok(ToolOutput::Json(json!({ "texture": texture.0 })));
    }
    let handle = with_assets(engine, |assets, gpu, state| {
        assets.mesh(&gpu.device, &mut state.registry, &source)
    })??;
    Ok(ToolOutput::Json(json!({ "handle": handle.0 })))
}

/// Arguments of `scene.load_gltf`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LoadGltfArgs {
    /// Path to a .gltf or .glb file, absolute or relative to the engine's
    /// working directory.
    path: String,
}

/// `scene.load_gltf` reads the file once and caches each triangle primitive
/// under its own `path#index` source and each base-colour image under
/// `path#image=index`, so it shares meshes and textures with `asset.load`.
fn load_gltf(engine: &mut Headless, args: LoadGltfArgs) -> anyhow::Result<ToolOutput> {
    let path = PathBuf::from(&args.path);
    let resolved = engine
        .resources
        .get::<Assets>()
        .context("no Assets resource")?
        .resolve(&path);
    let scene = super::load_gltf(resolved)?;

    // Base-colour image index of each mesh's material, if it has one.
    let mesh_images: Vec<Option<usize>> = scene
        .meshes
        .iter()
        .map(|mesh| {
            let material = scene.materials.get(mesh.material?)?;
            material.base_color_texture
        })
        .collect();

    let (handles, textures) = with_assets(engine, |assets, gpu, state| {
        let handles: Vec<_> = scene
            .meshes
            .iter()
            .enumerate()
            .map(|(index, mesh)| {
                let source = AssetSource::File {
                    path: path.clone(),
                    index,
                };
                assets.mesh_from_data(&gpu.device, &mut state.registry, &source, mesh)
            })
            .collect();
        // glTF image index -> texture handle, uploading each image once.
        let mut textures = HashMap::new();
        for &index in mesh_images.iter().flatten() {
            if textures.contains_key(&index) {
                continue;
            }
            let image = scene
                .images
                .get(index)
                .with_context(|| format!("{} has no image {index}", path.display()))?;
            let source = AssetSource::GltfImage {
                path: path.clone(),
                index,
            };
            let handle = assets.texture_from_data(
                &gpu.device,
                &gpu.queue,
                &mut state.textures,
                &source,
                image,
            )?;
            textures.insert(index, handle);
        }
        anyhow::Ok((handles, textures))
    })??;

    let mut entities = Vec::new();
    for node in &scene.nodes {
        let Some(index) = node.mesh else { continue };
        let color = scene.meshes[index]
            .material
            .and_then(|m| scene.materials.get(m))
            .map_or([1.0; 4], |m| m.base_color);
        let mesh = handles[index];
        let texture = mesh_images[index].map(|image| textures[&image]);
        let entity = engine.world.spawn((
            node.transform,
            MeshInstance {
                mesh,
                color,
                texture,
            },
        ));
        entities.push(entity.to_bits().get());
    }
    let meshes: Vec<u32> = handles.iter().map(|h| h.0).collect();
    let mut textures: Vec<u32> = textures.values().map(|h| h.0).collect();
    textures.sort_unstable();
    Ok(ToolOutput::Json(json!({
        "meshes": meshes,
        "textures": textures,
        "entities": entities,
    })))
}

/// Which built-in mesh to create.
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum BuiltinKind {
    /// Unit cube centred at the origin.
    Cube,
    /// Unit-radius UV sphere centred at the origin.
    Sphere,
}

/// Arguments of `mesh.builtin`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct BuiltinArgs {
    /// Which mesh: "cube" or "sphere".
    kind: BuiltinKind,
    /// Sphere only: divisions around the equator (at least 3); default 32.
    segments: Option<u32>,
    /// Sphere only: divisions from pole to pole (at least 2); default 16.
    rings: Option<u32>,
}

/// `mesh.builtin` is `asset.load` with a `builtin://` source built from its
/// arguments.
fn mesh_builtin(engine: &mut Headless, args: BuiltinArgs) -> anyhow::Result<ToolOutput> {
    let name = match args.kind {
        BuiltinKind::Cube => "cube".to_string(),
        BuiltinKind::Sphere => {
            // Values below the minimum are raised to it, as `uv_sphere` does.
            let segments = args.segments.unwrap_or(32).max(3);
            let rings = args.rings.unwrap_or(16).max(2);
            sphere_name(segments, rings)
        }
    };
    let source = AssetSource::Builtin(name);
    let handle = with_assets(engine, |assets, gpu, state| {
        assets.mesh(&gpu.device, &mut state.registry, &source)
    })??;
    Ok(ToolOutput::Json(json!({ "handle": handle.0 })))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::Value;

    use super::*;
    use crate::tool::ToolError;

    fn call_json(
        reg: &ToolRegistry,
        engine: &mut Headless,
        name: &str,
        args: Value,
    ) -> anyhow::Result<Value> {
        match reg.call(engine, name, args)? {
            ToolOutput::Json(value) => Ok(value),
            other => anyhow::bail!("expected JSON from {name}, got {other:?}"),
        }
    }

    #[test]
    fn builtin_cube_and_sphere_get_new_handles() -> anyhow::Result<()> {
        let reg = ToolRegistry::with_engine_tools();
        let mut engine = Headless::new(8, 8)?;
        let cube = call_json(&reg, &mut engine, "mesh.builtin", json!({ "kind": "cube" }))?;
        let sphere = call_json(
            &reg,
            &mut engine,
            "mesh.builtin",
            json!({ "kind": "sphere", "segments": 8, "rings": 4 }),
        )?;
        assert_eq!(cube, json!({ "handle": 0 }));
        assert_eq!(sphere, json!({ "handle": 1 }));
        let registry = &engine.resources.expect::<RenderState>().registry;
        assert_eq!(registry.len(), 2);
        assert_eq!(
            registry.get(crate::render::MeshHandle(1)).vertex_count,
            9 * 5
        );

        let err = reg
            .call(&mut engine, "mesh.builtin", json!({ "kind": "torus" }))
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs { .. }), "{err:?}");
        Ok(())
    }

    #[test]
    fn asset_load_caches_by_source_and_lists_it() -> anyhow::Result<()> {
        let reg = ToolRegistry::with_engine_tools();
        let info = reg
            .list()
            .into_iter()
            .find(|t| t.name == "asset.load")
            .expect("asset.load is registered");
        let field = &info.input_schema["properties"]["source"];
        assert!(
            field["description"]
                .as_str()
                .is_some_and(|d| d.contains("builtin://cube")),
            "{field}"
        );

        let mut engine = Headless::new(8, 8)?;
        let load = |engine: &mut Headless, source: &str| {
            call_json(&reg, engine, "asset.load", json!({ "source": source }))
        };
        assert_eq!(load(&mut engine, "builtin://cube")?, json!({ "handle": 0 }));
        assert_eq!(load(&mut engine, "builtin://cube")?, json!({ "handle": 0 }));
        // mesh.builtin goes through the same cache.
        assert_eq!(
            call_json(&reg, &mut engine, "mesh.builtin", json!({ "kind": "cube" }))?,
            json!({ "handle": 0 })
        );
        let gltf = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/test/cube.gltf");
        let gltf = gltf.to_str().expect("UTF-8 path");
        assert_eq!(load(&mut engine, gltf)?, json!({ "handle": 1 }));
        assert_eq!(
            load(&mut engine, &format!("{gltf}#0"))?,
            json!({ "handle": 1 })
        );
        assert_eq!(engine.resources.expect::<RenderState>().registry.len(), 2);

        let list = call_json(&reg, &mut engine, "mesh.list", json!({}))?;
        assert_eq!(list[0]["source"], "builtin://cube");
        assert_eq!(list[1]["source"], gltf);

        for bad in ["builtin://torus", "", "x.gltf#nope"] {
            let err = reg
                .call(&mut engine, "asset.load", json!({ "source": bad }))
                .unwrap_err();
            assert!(matches!(err, ToolError::Failed(_)), "{bad:?}: {err:?}");
        }
        // A failed load leaves the cache in place.
        assert_eq!(load(&mut engine, "builtin://cube")?, json!({ "handle": 0 }));
        Ok(())
    }

    #[test]
    fn load_gltf_spawns_one_entity_per_mesh_node() -> anyhow::Result<()> {
        let reg = ToolRegistry::with_engine_tools();
        let mut engine = Headless::new(8, 8)?;
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/test/cube.gltf");
        let out = call_json(
            &reg,
            &mut engine,
            "scene.load_gltf",
            json!({ "path": path }),
        )?;
        let meshes = out["meshes"].as_array().expect("meshes");
        let entities = out["entities"].as_array().expect("entities");
        assert!(!meshes.is_empty(), "{out}");
        assert!(!entities.is_empty(), "{out}");
        assert_eq!(engine.world.len() as usize, entities.len());

        // Loading the file again spawns again but reuses the meshes.
        let again = call_json(
            &reg,
            &mut engine,
            "scene.load_gltf",
            json!({ "path": path }),
        )?;
        assert_eq!(again["meshes"], out["meshes"]);
        assert_eq!(engine.world.len() as usize, 2 * entities.len());
        let list = call_json(&reg, &mut engine, "mesh.list", json!({}))?;
        assert_eq!(list.as_array().map(Vec::len), Some(meshes.len()));
        assert_eq!(list[0]["source"], path.to_str().expect("UTF-8 path"));
        Ok(())
    }

    #[test]
    fn load_gltf_missing_path_reports_the_path() -> anyhow::Result<()> {
        let reg = ToolRegistry::with_engine_tools();
        let mut engine = Headless::new(8, 8)?;
        let path = "/definitely/not/here/scene.gltf";
        let err = reg
            .call(&mut engine, "scene.load_gltf", json!({ "path": path }))
            .unwrap_err();
        assert!(matches!(err, ToolError::Failed(_)), "{err:?}");
        // `{:#}` is how the MCP server and agents see the full error chain.
        let ToolError::Failed(inner) = &err else {
            unreachable!()
        };
        assert!(format!("{inner:#}").contains(path), "{inner:#}");
        Ok(())
    }
}
