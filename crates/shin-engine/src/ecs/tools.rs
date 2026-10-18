//! ECS tools (GE-015): `entity.spawn`, `entity.list`,
//! `entity.set_transform`, `entity.despawn`, `scene.clear`, `sim.step`;
//! and (GE-023, for granular undo in the editor) `entity.set_name`,
//! `entity.set_color`, and `entity.set_mesh`.
//!
//! Entities are identified by [`Entity::to_bits`]. Rotations are Euler
//! angles in degrees `[x, y, z]`, applied in intrinsic X, then Y, then Z
//! order ([`EulerRot::XYZ`]).

use anyhow::Context as _;
use glam::EulerRot;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Entity, Name, Time, World};
use crate::asset::{AssetSource, with_assets};
use crate::audio::{AudioSource, audio_json};
use crate::command::Affected;
use crate::headless::Headless;
use crate::math::{Quat, Transform, Vec3};
use crate::render::{MeshHandle, MeshInstance, RenderState, TextureHandle};
use crate::script::Script;
use crate::tool::{ToolOutput, ToolRegistry, TypedTool};

/// Registers the entity tools, `scene.clear`, and `sim.step`.
pub fn register_tools(reg: &mut ToolRegistry) {
    reg.register(
        TypedTool::new(
            "entity.spawn",
            "Spawns an entity that draws a mesh (a handle from mesh.list, mesh.builtin, or \
         scene.load_gltf) with a transform, a linear RGBA colour, an optional name, and \
         optionally a base-colour texture (a handle from asset.load). Returns its id.",
            entity_spawn,
        )
        .with_affected(|_, _| Affected::Spawned),
    );
    reg.register(TypedTool::new(
        "entity.list",
        "Lists entities that have a transform, in id order: id, translation, rotation \
         (Euler degrees, XYZ), scale, name, script path, and audio (source, volume, looping, \
         spatial, autoplay) if it has them, and mesh, colour, and texture (a handle or null) \
         if the entity draws one.",
        entity_list,
    ));
    reg.register(
        TypedTool::new(
            "entity.set_transform",
            "Changes an entity's translation, rotation (Euler degrees, XYZ), and/or scale. \
         Omitted fields keep their value. Returns the new transform.",
            entity_set_transform,
        )
        .with_affected(named_entity),
    );
    reg.register(
        TypedTool::new(
            "entity.set_name",
            "Renames an entity; an empty name removes its name. Returns the entity's id and \
         name (null when removed).",
            entity_set_name,
        )
        .with_affected(named_entity),
    );
    reg.register(
        TypedTool::new(
            "entity.set_color",
            "Changes the linear RGBA colour of an entity that draws a mesh. Fails if the \
         entity draws nothing.",
            entity_set_color,
        )
        .with_affected(named_entity),
    );
    reg.register(
        TypedTool::new(
            "entity.set_mesh",
            "Makes an entity draw the mesh named by an asset source (e.g. \"builtin://sphere\" \
         or \"assets/x.gltf#1\"), loading it if needed. Colour and texture are kept; an \
         entity that drew nothing gets a white one. Returns the mesh handle.",
            entity_set_mesh,
        )
        .with_affected(named_entity),
    );
    reg.register(
        TypedTool::new(
            "entity.despawn",
            "Removes an entity. Fails if no entity has that id.",
            entity_despawn,
        )
        .with_affected(named_entity),
    );
    reg.register(
        TypedTool::new(
            "scene.clear",
            "Despawns every entity. Uploaded meshes, the camera, and the light are kept. \
         Returns how many entities were removed.",
            scene_clear,
        )
        .with_affected(|_, _| Affected::AllEntities),
    );
    reg.register(TypedTool::new(
        "sim.step",
        "Runs fixed simulation steps (each `fixed_dt` seconds) and returns the Time \
         resource afterwards.",
        sim_step,
    ));
}

/// Arguments of `entity.spawn`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SpawnArgs {
    /// Mesh handle to draw, from mesh.list, mesh.builtin, or scene.load_gltf.
    mesh: u32,
    /// World-space position `[x, y, z]`; default the origin.
    translation: Option<[f32; 3]>,
    /// Rotation as Euler angles in degrees `[x, y, z]`, intrinsic XYZ
    /// order; default no rotation.
    rotation_euler_deg: Option<[f32; 3]>,
    /// Per-axis scale `[x, y, z]`; default `[1, 1, 1]`.
    scale: Option<[f32; 3]>,
    /// Linear RGBA colour `[r, g, b, a]`; default white.
    color: Option<[f32; 4]>,
    /// Base-colour texture handle from asset.load (an image source),
    /// multiplied into `color`; default none (untextured).
    texture: Option<u32>,
    /// Human-readable name, shown by entity.list and saved in scene files;
    /// default none.
    name: Option<String>,
}

fn entity_spawn(engine: &mut Headless, args: SpawnArgs) -> anyhow::Result<ToolOutput> {
    let mesh = MeshHandle(args.mesh);
    let texture = args.texture.map(TextureHandle);
    let state = engine
        .resources
        .get::<RenderState>()
        .context("no RenderState resource")?;
    anyhow::ensure!(
        state.registry.contains(mesh),
        "unknown mesh handle {}; {} meshes are uploaded (see mesh.list)",
        args.mesh,
        state.registry.len()
    );
    if let Some(handle) = texture {
        anyhow::ensure!(
            state.textures.contains(handle),
            "unknown texture handle {}; {} textures are uploaded (load one with asset.load)",
            handle.0,
            state.textures.len()
        );
    }
    let mut transform = Transform::IDENTITY;
    apply_transform(
        &mut transform,
        args.translation,
        args.rotation_euler_deg,
        args.scale,
    );
    let color = args.color.unwrap_or([1.0; 4]);
    let entity = engine.world.spawn((
        transform,
        MeshInstance {
            mesh,
            color,
            texture,
        },
    ));
    if let Some(name) = args.name {
        engine.world.insert_one(entity, Name(name))?;
    }
    Ok(ToolOutput::Json(json!({ "entity": entity_id(entity) })))
}

/// Arguments of `entity.list`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    /// Maximum number of entities to return; default 100.
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    100
}

fn entity_list(engine: &mut Headless, args: ListArgs) -> anyhow::Result<ToolOutput> {
    let mut rows: Vec<(u64, Value)> = engine
        .world
        .query::<(
            Entity,
            &Transform,
            Option<&MeshInstance>,
            Option<&Name>,
            Option<&Script>,
            Option<&AudioSource>,
        )>()
        .iter()
        .map(|(entity, transform, instance, name, script, audio)| {
            let mut row = transform_json(transform);
            row["entity"] = json!(entity_id(entity));
            if let Some(Name(name)) = name {
                row["name"] = json!(name);
            }
            if let Some(script) = script {
                row["script"] = json!(script.path.to_string_lossy());
            }
            if let Some(audio) = audio {
                row["audio"] = audio_json(audio);
            }
            if let Some(instance) = instance {
                row["mesh"] = json!(instance.mesh.0);
                row["color"] = json!(instance.color);
                row["texture"] = json!(instance.texture.map(|t| t.0));
            }
            (entity_id(entity), row)
        })
        .collect();
    rows.sort_by_key(|(id, _)| *id);
    let rows: Vec<Value> = rows
        .into_iter()
        .take(args.limit)
        .map(|(_, row)| row)
        .collect();
    Ok(ToolOutput::Json(Value::Array(rows)))
}

/// Arguments of `entity.set_transform`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetTransformArgs {
    /// Entity id, as returned by entity.spawn or entity.list.
    entity: u64,
    /// New world-space position `[x, y, z]`.
    translation: Option<[f32; 3]>,
    /// New rotation as Euler angles in degrees `[x, y, z]`, intrinsic XYZ
    /// order.
    rotation_euler_deg: Option<[f32; 3]>,
    /// New per-axis scale `[x, y, z]`.
    scale: Option<[f32; 3]>,
}

fn entity_set_transform(
    engine: &mut Headless,
    args: SetTransformArgs,
) -> anyhow::Result<ToolOutput> {
    let entity = find_entity(&engine.world, args.entity)?;
    if !engine.world.satisfies::<&Transform>(entity) {
        engine.world.insert_one(entity, Transform::IDENTITY)?;
    }
    let transform = engine.world.query_one_mut::<&mut Transform>(entity)?;
    apply_transform(
        transform,
        args.translation,
        args.rotation_euler_deg,
        args.scale,
    );
    Ok(ToolOutput::Json(transform_json(transform)))
}

/// Arguments of `entity.set_name`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetNameArgs {
    /// Entity id, as returned by entity.spawn or entity.list.
    entity: u64,
    /// The new name; empty removes the name.
    name: String,
}

fn entity_set_name(engine: &mut Headless, args: SetNameArgs) -> anyhow::Result<ToolOutput> {
    let entity = find_entity(&engine.world, args.entity)?;
    let name = if args.name.is_empty() {
        // Removing a name the entity does not have is not an error.
        let _ = engine.world.remove_one::<Name>(entity);
        None
    } else {
        engine.world.insert_one(entity, Name(args.name.clone()))?;
        Some(args.name)
    };
    Ok(ToolOutput::Json(
        json!({ "entity": args.entity, "name": name }),
    ))
}

/// Arguments of `entity.set_color`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetColorArgs {
    /// Entity id, as returned by entity.spawn or entity.list.
    entity: u64,
    /// New linear RGBA colour `[r, g, b, a]`.
    color: [f32; 4],
}

fn entity_set_color(engine: &mut Headless, args: SetColorArgs) -> anyhow::Result<ToolOutput> {
    let entity = find_entity(&engine.world, args.entity)?;
    let instance = engine
        .world
        .query_one_mut::<&mut MeshInstance>(entity)
        .map_err(|_| {
            anyhow::anyhow!("entity {} draws no mesh, so it has no colour", args.entity)
        })?;
    instance.color = args.color;
    Ok(ToolOutput::Json(
        json!({ "entity": args.entity, "color": args.color }),
    ))
}

/// Arguments of `entity.set_mesh`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetMeshArgs {
    /// Entity id, as returned by entity.spawn or entity.list.
    entity: u64,
    /// Mesh asset source: "builtin://cube", "builtin://sphere", or a glTF
    /// path with an optional triangle-primitive index, e.g.
    /// "assets/x.gltf#2". Relative paths are resolved against the project.
    source: String,
}

fn entity_set_mesh(engine: &mut Headless, args: SetMeshArgs) -> anyhow::Result<ToolOutput> {
    let entity = find_entity(&engine.world, args.entity)?;
    let source = AssetSource::parse(&args.source)?;
    anyhow::ensure!(
        !source.is_texture(),
        "{source} is a texture source; entity.set_mesh needs a mesh source"
    );
    let mesh = with_assets(engine, |assets, gpu, state| {
        assets.mesh(&gpu.device, &mut state.registry, &source)
    })??;
    match engine.world.query_one_mut::<&mut MeshInstance>(entity) {
        Ok(instance) => instance.mesh = mesh,
        Err(_) => engine.world.insert_one(
            entity,
            MeshInstance {
                mesh,
                color: [1.0; 4],
                texture: None,
            },
        )?,
    }
    Ok(ToolOutput::Json(
        json!({ "entity": args.entity, "mesh": mesh.0 }),
    ))
}

/// Arguments of `entity.despawn`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DespawnArgs {
    /// Entity id, as returned by entity.spawn or entity.list.
    entity: u64,
}

fn entity_despawn(engine: &mut Headless, args: DespawnArgs) -> anyhow::Result<ToolOutput> {
    let entity = find_entity(&engine.world, args.entity)?;
    engine.world.despawn(entity)?;
    Ok(ToolOutput::Json(json!({ "ok": true })))
}

/// `scene.clear` takes no arguments.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ClearArgs {}

fn scene_clear(engine: &mut Headless, _: ClearArgs) -> anyhow::Result<ToolOutput> {
    let removed = engine.world.len();
    engine.world.clear();
    Ok(ToolOutput::Json(json!({ "removed": removed })))
}

/// Arguments of `sim.step`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct StepArgs {
    /// Number of fixed steps to run; default 1.
    #[serde(default = "default_steps")]
    steps: u32,
}

fn default_steps() -> u32 {
    1
}

fn sim_step(engine: &mut Headless, args: StepArgs) -> anyhow::Result<ToolOutput> {
    engine.step(args.steps);
    let time = engine.resources.get::<Time>().copied().unwrap_or_default();
    Ok(ToolOutput::Json(json!({
        "fixed_dt": time.fixed_dt,
        "elapsed": time.elapsed,
        "fixed_steps": time.fixed_steps,
        "frame_dt": time.frame_dt,
        "alpha": time.alpha,
    })))
}

/// [`Affected`] for tools whose `entity` argument names the one entity
/// they change. An unknown id names no entity; the call then fails anyway.
fn named_entity(_: &Headless, args: &Value) -> Affected {
    let entity = args
        .get("entity")
        .and_then(Value::as_u64)
        .and_then(Entity::from_bits);
    Affected::Entities(entity.into_iter().collect())
}

/// The id tools use for `entity`.
fn entity_id(entity: Entity) -> u64 {
    entity.to_bits().get()
}

/// The live entity with tool id `id`, or an error naming the id.
fn find_entity(world: &World, id: u64) -> anyhow::Result<Entity> {
    Entity::from_bits(id)
        .filter(|&entity| world.contains(entity))
        .with_context(|| format!("no entity with id {id}"))
}

/// Overwrites the parts of `t` that are given.
fn apply_transform(
    t: &mut Transform,
    translation: Option<[f32; 3]>,
    rotation_euler_deg: Option<[f32; 3]>,
    scale: Option<[f32; 3]>,
) {
    if let Some(translation) = translation {
        t.translation = Vec3::from(translation);
    }
    if let Some([x, y, z]) = rotation_euler_deg {
        t.rotation = Quat::from_euler(
            EulerRot::XYZ,
            x.to_radians(),
            y.to_radians(),
            z.to_radians(),
        );
    }
    if let Some(scale) = scale {
        t.scale = Vec3::from(scale);
    }
}

/// `{translation, rotation_euler_deg, scale}` for `t`.
fn transform_json(t: &Transform) -> Value {
    let (x, y, z) = t.rotation.to_euler(EulerRot::XYZ);
    json!({
        "translation": t.translation.to_array(),
        "rotation_euler_deg": [x.to_degrees(), y.to_degrees(), z.to_degrees()],
        "scale": t.scale.to_array(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolError;

    fn setup() -> anyhow::Result<(ToolRegistry, Headless, u32)> {
        let reg = ToolRegistry::with_engine_tools();
        let mut engine = Headless::new(16, 16)?;
        let state = engine.resources.expect_mut::<RenderState>();
        let cube = state
            .registry
            .add(&engine.gpu.device, &crate::asset::cube());
        Ok((reg, engine, cube.0))
    }

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

    fn assert_close(value: &Value, expected: [f64; 3]) {
        let got: Vec<f64> = serde_json::from_value(value.clone()).unwrap();
        for (g, e) in got.iter().zip(expected) {
            assert!((g - e).abs() < 1e-3, "{got:?} != {expected:?}");
        }
    }

    #[test]
    fn spawn_then_list_round_trips() -> anyhow::Result<()> {
        let (reg, mut engine, cube) = setup()?;
        let spawned = call_json(
            &reg,
            &mut engine,
            "entity.spawn",
            json!({
                "mesh": cube,
                "translation": [1, 2, 3],
                "rotation_euler_deg": [0, 90, 0],
                "scale": [2, 2, 2],
                "color": [1, 0, 0, 1],
                "name": "red cube",
            }),
        )?;
        let id = spawned["entity"].as_u64().expect("entity id");

        let list = call_json(&reg, &mut engine, "entity.list", json!({}))?;
        assert_eq!(list.as_array().unwrap().len(), 1);
        let row = &list[0];
        assert_eq!(row["entity"], id);
        assert_eq!(row["mesh"], cube);
        assert_eq!(row["name"], "red cube");
        assert_eq!(row["color"], json!([1.0, 0.0, 0.0, 1.0]));
        assert_close(&row["translation"], [1.0, 2.0, 3.0]);
        assert_close(&row["rotation_euler_deg"], [0.0, 90.0, 0.0]);
        assert_close(&row["scale"], [2.0, 2.0, 2.0]);
        Ok(())
    }

    #[test]
    fn spawn_rejects_unknown_mesh() -> anyhow::Result<()> {
        let (reg, mut engine, _) = setup()?;
        let err = reg
            .call(&mut engine, "entity.spawn", json!({ "mesh": 7 }))
            .unwrap_err();
        assert!(matches!(err, ToolError::Failed(_)), "{err:?}");
        assert!(err.to_string().contains("unknown mesh handle 7"), "{err}");
        Ok(())
    }

    #[test]
    fn spawn_takes_a_texture_and_rejects_unknown_ones() -> anyhow::Result<()> {
        let (reg, mut engine, cube) = setup()?;
        let err = reg
            .call(
                &mut engine,
                "entity.spawn",
                json!({ "mesh": cube, "texture": 0 }),
            )
            .unwrap_err();
        assert!(matches!(err, ToolError::Failed(_)), "{err:?}");
        assert!(
            err.to_string().contains("unknown texture handle 0"),
            "{err}"
        );
        assert_eq!(engine.world.len(), 0);

        let state = engine.resources.expect_mut::<RenderState>();
        let image = crate::asset::ImageData {
            name: "grey".into(),
            width: 1,
            height: 1,
            rgba: vec![128, 128, 128, 255],
        };
        let texture = state
            .textures
            .add(&engine.gpu.device, &engine.gpu.queue, &image);
        call_json(
            &reg,
            &mut engine,
            "entity.spawn",
            json!({ "mesh": cube, "texture": texture.0 }),
        )?;
        call_json(&reg, &mut engine, "entity.spawn", json!({ "mesh": cube }))?;
        let list = call_json(&reg, &mut engine, "entity.list", json!({}))?;
        let textures: Vec<&Value> = list
            .as_array()
            .unwrap()
            .iter()
            .map(|row| &row["texture"])
            .collect();
        assert_eq!(textures, [&json!(texture.0), &Value::Null]);
        Ok(())
    }

    #[test]
    fn list_respects_limit() -> anyhow::Result<()> {
        let (reg, mut engine, cube) = setup()?;
        for _ in 0..5 {
            call_json(&reg, &mut engine, "entity.spawn", json!({ "mesh": cube }))?;
        }
        let all = call_json(&reg, &mut engine, "entity.list", json!({}))?;
        assert_eq!(all.as_array().unwrap().len(), 5);
        let two = call_json(&reg, &mut engine, "entity.list", json!({ "limit": 2 }))?;
        assert_eq!(two.as_array().unwrap().len(), 2);
        assert_eq!(two[0], all[0]);
        Ok(())
    }

    #[test]
    fn set_transform_changes_only_given_fields() -> anyhow::Result<()> {
        let (reg, mut engine, cube) = setup()?;
        let id = call_json(
            &reg,
            &mut engine,
            "entity.spawn",
            json!({ "mesh": cube, "translation": [1, 1, 1] }),
        )?["entity"]
            .clone();
        let t = call_json(
            &reg,
            &mut engine,
            "entity.set_transform",
            json!({ "entity": id, "scale": [3, 3, 3] }),
        )?;
        assert_close(&t["translation"], [1.0, 1.0, 1.0]);
        assert_close(&t["scale"], [3.0, 3.0, 3.0]);
        Ok(())
    }

    #[test]
    fn set_transform_unknown_entity_names_the_id() -> anyhow::Result<()> {
        let (reg, mut engine, _) = setup()?;
        let id = 0x1_0000_002a_u64;
        let err = reg
            .call(
                &mut engine,
                "entity.set_transform",
                json!({ "entity": id, "translation": [0, 0, 0] }),
            )
            .unwrap_err();
        assert!(matches!(err, ToolError::Failed(_)), "{err:?}");
        assert!(err.to_string().contains(&id.to_string()), "{err}");
        Ok(())
    }

    #[test]
    fn new_setters_are_listed() -> anyhow::Result<()> {
        let (reg, mut engine, _) = setup()?;
        let tools = call_json(&reg, &mut engine, "engine.list_tools", json!({}))?;
        let names: Vec<&str> = tools
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        for name in ["entity.set_name", "entity.set_color", "entity.set_mesh"] {
            assert!(names.contains(&name), "{name} is listed: {names:?}");
        }
        Ok(())
    }

    #[test]
    fn set_name_renames_and_empty_removes() -> anyhow::Result<()> {
        let (reg, mut engine, cube) = setup()?;
        let id = call_json(&reg, &mut engine, "entity.spawn", json!({ "mesh": cube }))?["entity"]
            .clone();
        let out = call_json(
            &reg,
            &mut engine,
            "entity.set_name",
            json!({ "entity": id, "name": "crate" }),
        )?;
        assert_eq!(out["name"], "crate");
        let list = call_json(&reg, &mut engine, "entity.list", json!({}))?;
        assert_eq!(list[0]["name"], "crate");

        let out = call_json(
            &reg,
            &mut engine,
            "entity.set_name",
            json!({ "entity": id, "name": "" }),
        )?;
        assert_eq!(out["name"], Value::Null);
        let list = call_json(&reg, &mut engine, "entity.list", json!({}))?;
        assert!(list[0].get("name").is_none(), "{list}");
        Ok(())
    }

    #[test]
    fn set_color_changes_colour_and_needs_a_mesh() -> anyhow::Result<()> {
        let (reg, mut engine, cube) = setup()?;
        let id = call_json(&reg, &mut engine, "entity.spawn", json!({ "mesh": cube }))?["entity"]
            .clone();
        call_json(
            &reg,
            &mut engine,
            "entity.set_color",
            json!({ "entity": id, "color": [0, 0.5, 1, 1] }),
        )?;
        let list = call_json(&reg, &mut engine, "entity.list", json!({}))?;
        assert_eq!(list[0]["color"], json!([0.0, 0.5, 1.0, 1.0]));

        let bare = engine.world.spawn((Transform::IDENTITY,));
        let err = reg
            .call(
                &mut engine,
                "entity.set_color",
                json!({ "entity": entity_id(bare), "color": [1, 1, 1, 1] }),
            )
            .unwrap_err();
        assert!(err.to_string().contains("draws no mesh"), "{err}");
        Ok(())
    }

    #[test]
    fn set_mesh_loads_the_source_and_keeps_colour() -> anyhow::Result<()> {
        let (reg, mut engine, cube) = setup()?;
        let id = call_json(
            &reg,
            &mut engine,
            "entity.spawn",
            json!({ "mesh": cube, "color": [1, 0, 0, 1] }),
        )?["entity"]
            .clone();
        let out = call_json(
            &reg,
            &mut engine,
            "entity.set_mesh",
            json!({ "entity": id, "source": "builtin://sphere" }),
        )?;
        let sphere = out["mesh"].as_u64().unwrap();
        assert_ne!(sphere, u64::from(cube));
        let list = call_json(&reg, &mut engine, "entity.list", json!({}))?;
        assert_eq!(list[0]["mesh"], sphere);
        assert_eq!(list[0]["color"], json!([1.0, 0.0, 0.0, 1.0]));

        for bad in ["builtin://torus", "assets/x.png"] {
            let err = reg
                .call(
                    &mut engine,
                    "entity.set_mesh",
                    json!({ "entity": id, "source": bad }),
                )
                .unwrap_err();
            assert!(matches!(err, ToolError::Failed(_)), "{bad}: {err:?}");
        }
        Ok(())
    }

    #[test]
    fn setters_are_undoable() -> anyhow::Result<()> {
        let (reg, mut engine, cube) = setup()?;
        let id = call_json(&reg, &mut engine, "entity.spawn", json!({ "mesh": cube }))?["entity"]
            .clone();
        let before = call_json(&reg, &mut engine, "entity.list", json!({}))?;
        for (tool, args) in [
            ("entity.set_name", json!({ "entity": id, "name": "n" })),
            (
                "entity.set_color",
                json!({ "entity": id, "color": [0, 1, 0, 1] }),
            ),
            (
                "entity.set_mesh",
                json!({ "entity": id, "source": "builtin://sphere" }),
            ),
        ] {
            crate::command::apply(&reg, &mut engine, tool, args)?;
        }
        let after = call_json(&reg, &mut engine, "entity.list", json!({}))?;
        assert_ne!(before, after);
        for _ in 0..3 {
            assert!(crate::command::undo(&mut engine)?);
        }
        assert_eq!(
            call_json(&reg, &mut engine, "entity.list", json!({}))?,
            before
        );
        for _ in 0..3 {
            assert!(crate::command::redo(&mut engine)?);
        }
        assert_eq!(
            call_json(&reg, &mut engine, "entity.list", json!({}))?,
            after
        );
        Ok(())
    }

    #[test]
    fn despawn_removes_and_rejects_unknown() -> anyhow::Result<()> {
        let (reg, mut engine, cube) = setup()?;
        let id = call_json(&reg, &mut engine, "entity.spawn", json!({ "mesh": cube }))?["entity"]
            .clone();
        let out = call_json(&reg, &mut engine, "entity.despawn", json!({ "entity": id }))?;
        assert_eq!(out, json!({ "ok": true }));
        assert_eq!(engine.world.len(), 0);
        let err = reg
            .call(&mut engine, "entity.despawn", json!({ "entity": id }))
            .unwrap_err();
        assert!(err.to_string().contains(&id.to_string()), "{err}");
        Ok(())
    }

    #[test]
    fn scene_clear_counts_removed() -> anyhow::Result<()> {
        let (reg, mut engine, cube) = setup()?;
        for _ in 0..3 {
            call_json(&reg, &mut engine, "entity.spawn", json!({ "mesh": cube }))?;
        }
        let out = call_json(&reg, &mut engine, "scene.clear", json!({}))?;
        assert_eq!(out, json!({ "removed": 3 }));
        assert_eq!(engine.world.len(), 0);
        assert_eq!(
            engine.resources.expect::<RenderState>().registry.len(),
            1,
            "meshes are kept"
        );
        Ok(())
    }

    #[test]
    fn sim_step_advances_time() -> anyhow::Result<()> {
        let (reg, mut engine, _) = setup()?;
        let time = call_json(&reg, &mut engine, "sim.step", json!({}))?;
        assert_eq!(time["fixed_steps"], 1);
        let time = call_json(&reg, &mut engine, "sim.step", json!({ "steps": 9 }))?;
        assert_eq!(time["fixed_steps"], 10);
        assert!((time["elapsed"].as_f64().unwrap() - 10.0 / 60.0).abs() < 1e-9);
        Ok(())
    }
}
