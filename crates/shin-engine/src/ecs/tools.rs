//! ECS tools (GE-015): `entity.spawn`, `entity.list`,
//! `entity.set_transform`, `entity.despawn`, `scene.clear`, `sim.step`;
//! (GE-023, for granular undo in the editor) `entity.set_name`,
//! `entity.set_color`, `entity.set_mesh`; (GE-032) `entity.set_texture`;
//! and (GE-031) `entity.set_parent`.
//!
//! Entities are identified by [`Entity::to_bits`]. Rotations are Euler
//! angles in degrees `[x, y, z]`, applied in intrinsic X, then Y, then Z
//! order ([`EulerRot::XYZ`]). Transforms are local to the entity's parent
//! (world space for an entity without one).

use anyhow::Context as _;
use glam::EulerRot;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    Entity, Name, Time, World, despawn_recursive, parent_of, set_parent, subtree, world_transform,
};
use crate::asset::{AssetSource, with_assets};
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
         scene.load_gltf) with a transform, a linear RGBA colour, an optional name, \
         optionally a base-colour texture (a handle from asset.load), and optionally under \
         a parent entity (the transform is then local to it). Returns its id.",
            entity_spawn,
        )
        .with_affected(spawn_affected),
    );
    reg.register(TypedTool::new(
        "entity.list",
        "Lists entities that have a transform, in id order: id, translation, rotation \
         (Euler degrees, XYZ), and scale (all local to the parent), world_translation, \
         parent id if it has one, name and script path if it has them, and mesh, colour, \
         and texture (a handle or null) if the entity draws one.",
        entity_list,
    ));
    reg.register(
        TypedTool::new(
            "entity.set_transform",
            "Changes an entity's translation, rotation (Euler degrees, XYZ), and/or scale, \
         local to its parent. Omitted fields keep their value. Returns the new transform.",
            entity_set_transform,
        )
        .with_affected(named_entity),
    );
    reg.register(
        TypedTool::new(
            "entity.set_parent",
            "Makes an entity the child of another (its transform becomes local to the \
         parent) or, with parent null or omitted, a root again. By default the entity's \
         world transform is kept and its local transform rewritten; with keep_world false \
         the local transform is kept and the entity moves with the new parent. Fails on a \
         cycle. Returns the entity, its parent, and its new local transform.",
            entity_set_parent,
        )
        .with_affected(set_parent_affected),
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
            "entity.set_texture",
            "Gives an entity that draws a mesh the base-colour texture named by a texture \
         asset source (e.g. \"assets/x.png\" or \"assets/x.gltf#image=0\"), loading it if \
         needed, or removes its texture when `source` is null. Fails if the entity draws \
         nothing. Returns the texture handle (null when removed).",
            entity_set_texture,
        )
        .with_affected(named_entity),
    );
    reg.register(
        TypedTool::new(
            "entity.despawn",
            "Removes an entity and, by default, its descendants; with recursive false its \
         children become roots keeping their world transforms. Fails if no entity has that \
         id. Returns how many entities were removed.",
            entity_despawn,
        )
        .with_affected(despawn_affected),
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
    /// Position `[x, y, z]`, in the parent's space when `parent` is given
    /// and world space otherwise; default the origin.
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
    /// Id of the entity to spawn under; the new entity's transform is then
    /// local to it. Default none: a root entity.
    parent: Option<u64>,
}

/// [`Affected`] of `entity.spawn`: the new entity, plus the parent whose
/// child list grows when `parent` is given.
fn spawn_affected(_: &Headless, args: &Value) -> Affected {
    match args
        .get("parent")
        .and_then(Value::as_u64)
        .and_then(Entity::from_bits)
    {
        Some(parent) => Affected::SpawnedWith(vec![parent]),
        None => Affected::Spawned,
    }
}

fn entity_spawn(engine: &mut Headless, args: SpawnArgs) -> anyhow::Result<ToolOutput> {
    let mesh = MeshHandle(args.mesh);
    let texture = args.texture.map(TextureHandle);
    let parent = args
        .parent
        .map(|id| find_entity(&engine.world, id))
        .transpose()?;
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
    if let Some(parent) = parent {
        set_parent(&mut engine.world, entity, Some(parent), false)?;
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
    let world = &engine.world;
    let mut rows: Vec<(u64, Value)> = world
        .query::<(
            Entity,
            &Transform,
            Option<&MeshInstance>,
            Option<&Name>,
            Option<&Script>,
        )>()
        .iter()
        .map(|(entity, transform, instance, name, script)| {
            let mut row = transform_json(transform);
            row["entity"] = json!(entity_id(entity));
            row["world_translation"] = json!(world_transform(world, entity).translation.to_array());
            if let Some(parent) = parent_of(world, entity) {
                row["parent"] = json!(entity_id(parent));
            }
            if let Some(Name(name)) = name {
                row["name"] = json!(name);
            }
            if let Some(script) = script {
                row["script"] = json!(script.path.to_string_lossy());
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
    /// New position `[x, y, z]`, local to the parent (world space for a
    /// root entity).
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

/// Arguments of `entity.set_parent`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetParentArgs {
    /// Entity id, as returned by entity.spawn or entity.list.
    entity: u64,
    /// Id of the new parent, or null (or omitted) to make the entity a
    /// root.
    #[serde(default)]
    parent: Option<u64>,
    /// Keep the entity where it is in the world by rewriting its local
    /// transform (default true); false keeps the local transform instead,
    /// so the entity moves with its new parent.
    #[serde(default = "default_true")]
    keep_world: bool,
}

fn default_true() -> bool {
    true
}

/// [`Affected`] of `entity.set_parent`: the entity, its old parent, and
/// its new parent (their child lists change).
fn set_parent_affected(engine: &Headless, args: &Value) -> Affected {
    let entity = args
        .get("entity")
        .and_then(Value::as_u64)
        .and_then(Entity::from_bits);
    let new_parent = args
        .get("parent")
        .and_then(Value::as_u64)
        .and_then(Entity::from_bits);
    let old_parent = entity.and_then(|entity| parent_of(&engine.world, entity));
    let mut ids: Vec<Entity> = [entity, old_parent, new_parent]
        .into_iter()
        .flatten()
        .collect();
    ids.sort();
    ids.dedup();
    Affected::Entities(ids)
}

fn entity_set_parent(engine: &mut Headless, args: SetParentArgs) -> anyhow::Result<ToolOutput> {
    let entity = find_entity(&engine.world, args.entity)?;
    let parent = args
        .parent
        .map(|id| find_entity(&engine.world, id))
        .transpose()?;
    set_parent(&mut engine.world, entity, parent, args.keep_world)?;
    let transform = engine
        .world
        .get::<&Transform>(entity)
        .map(|t| *t)
        .unwrap_or_default();
    Ok(ToolOutput::Json(json!({
        "entity": args.entity,
        "parent": args.parent,
        "transform": transform_json(&transform),
    })))
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

/// Arguments of `entity.set_texture`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetTextureArgs {
    /// Entity id, as returned by entity.spawn or entity.list.
    entity: u64,
    /// Texture asset source: a .png/.jpg/.jpeg path such as
    /// "assets/checker.png" or a glTF image such as "assets/x.gltf#image=0",
    /// relative to the project; null removes the entity's texture.
    source: Option<String>,
}

fn entity_set_texture(engine: &mut Headless, args: SetTextureArgs) -> anyhow::Result<ToolOutput> {
    let entity = find_entity(&engine.world, args.entity)?;
    anyhow::ensure!(
        engine.world.satisfies::<&MeshInstance>(entity),
        "entity {} draws no mesh, so it cannot be textured",
        args.entity
    );
    let texture = match &args.source {
        Some(source) => {
            let source = AssetSource::parse(source)?;
            anyhow::ensure!(
                source.is_texture(),
                "{source} is a mesh source; entity.set_texture needs a texture source"
            );
            Some(with_assets(engine, |assets, gpu, state| {
                assets.texture(&gpu.device, &gpu.queue, &mut state.textures, &source)
            })??)
        }
        None => None,
    };
    let instance = engine.world.query_one_mut::<&mut MeshInstance>(entity)?;
    instance.texture = texture;
    Ok(ToolOutput::Json(
        json!({ "entity": args.entity, "texture": texture.map(|t| t.0) }),
    ))
}

/// Arguments of `entity.despawn`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DespawnArgs {
    /// Entity id, as returned by entity.spawn or entity.list.
    entity: u64,
    /// Also remove every descendant (default true). With false the
    /// entity's children become root entities, keeping their world
    /// transforms.
    #[serde(default = "default_true")]
    recursive: bool,
}

/// [`Affected`] of `entity.despawn`: the entity, its parent (whose child
/// list shrinks), and its subtree (removed) or children (made roots).
fn despawn_affected(engine: &Headless, args: &Value) -> Affected {
    let Some(entity) = args
        .get("entity")
        .and_then(Value::as_u64)
        .and_then(Entity::from_bits)
        .filter(|&entity| engine.world.contains(entity))
    else {
        return Affected::Entities(Vec::new());
    };
    let recursive = args
        .get("recursive")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let world = &engine.world;
    let mut ids = if recursive {
        subtree(world, entity)
    } else {
        let mut ids = vec![entity];
        ids.extend(super::child_map(world).remove(&entity).unwrap_or_default());
        ids
    };
    ids.extend(parent_of(world, entity));
    ids.sort();
    ids.dedup();
    Affected::Entities(ids)
}

fn entity_despawn(engine: &mut Headless, args: DespawnArgs) -> anyhow::Result<ToolOutput> {
    let entity = find_entity(&engine.world, args.entity)?;
    let world = &mut engine.world;
    if !args.recursive {
        for child in super::child_map(world).remove(&entity).unwrap_or_default() {
            set_parent(world, child, None, true)?;
        }
    }
    let removed = despawn_recursive(world, entity)?;
    Ok(ToolOutput::Json(json!({ "ok": true, "removed": removed })))
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
    use crate::ecs::Parent;
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
        for name in [
            "entity.set_name",
            "entity.set_color",
            "entity.set_mesh",
            "entity.set_texture",
        ] {
            assert!(names.contains(&name), "{name} is listed: {names:?}");
        }
        Ok(())
    }

    #[test]
    fn set_texture_loads_removes_and_is_undoable() -> anyhow::Result<()> {
        let (reg, mut engine, cube) = setup()?;
        let id = call_json(&reg, &mut engine, "entity.spawn", json!({ "mesh": cube }))?["entity"]
            .clone();
        let png = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/test/checker.png");
        let png = png.to_str().expect("UTF-8 path");
        let out = call_json(
            &reg,
            &mut engine,
            "entity.set_texture",
            json!({ "entity": id, "source": png }),
        )?;
        let handle = out["texture"].as_u64().expect("a texture handle");
        let list = call_json(&reg, &mut engine, "entity.list", json!({}))?;
        assert_eq!(list[0]["texture"], handle);
        // The same source is not uploaded twice.
        assert_eq!(
            call_json(&reg, &mut engine, "asset.load", json!({ "source": png }))?,
            json!({ "texture": handle })
        );

        let out = call_json(
            &reg,
            &mut engine,
            "entity.set_texture",
            json!({ "entity": id, "source": null }),
        )?;
        assert_eq!(out, json!({ "entity": id, "texture": null }));
        let list = call_json(&reg, &mut engine, "entity.list", json!({}))?;
        assert_eq!(list[0]["texture"], Value::Null);

        // Mesh sources, unknown files, and entities without a mesh fail.
        for bad in ["builtin://cube", "no/such.png"] {
            let err = reg
                .call(
                    &mut engine,
                    "entity.set_texture",
                    json!({ "entity": id, "source": bad }),
                )
                .unwrap_err();
            assert!(matches!(err, ToolError::Failed(_)), "{bad}: {err:?}");
        }
        let bare = engine.world.spawn((Transform::IDENTITY,));
        let err = reg
            .call(
                &mut engine,
                "entity.set_texture",
                json!({ "entity": entity_id(bare), "source": png }),
            )
            .unwrap_err();
        assert!(err.to_string().contains("draws no mesh"), "{err}");

        // Through the history: set, undo (back to none), redo.
        crate::command::apply(
            &reg,
            &mut engine,
            "entity.set_texture",
            json!({ "entity": id, "source": png }),
        )?;
        let list = call_json(&reg, &mut engine, "entity.list", json!({}))?;
        assert_eq!(list[0]["texture"], handle);
        assert!(crate::command::undo(&mut engine)?);
        let list = call_json(&reg, &mut engine, "entity.list", json!({}))?;
        assert_eq!(list[0]["texture"], Value::Null, "undone");
        assert!(crate::command::redo(&mut engine)?);
        let list = call_json(&reg, &mut engine, "entity.list", json!({}))?;
        assert_eq!(list[0]["texture"], handle, "redone");
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
        assert_eq!(out, json!({ "ok": true, "removed": 1 }));
        assert_eq!(engine.world.len(), 0);
        let err = reg
            .call(&mut engine, "entity.despawn", json!({ "entity": id }))
            .unwrap_err();
        assert!(err.to_string().contains(&id.to_string()), "{err}");
        Ok(())
    }

    /// Spawns a cube at `translation` (optionally under `parent`) and
    /// returns its id.
    fn spawn_at(
        reg: &ToolRegistry,
        engine: &mut Headless,
        cube: u32,
        translation: [f32; 3],
        parent: Option<u64>,
    ) -> anyhow::Result<u64> {
        let mut args = json!({ "mesh": cube, "translation": translation });
        if let Some(parent) = parent {
            args["parent"] = json!(parent);
        }
        Ok(call_json(reg, engine, "entity.spawn", args)?["entity"]
            .as_u64()
            .expect("entity id"))
    }

    /// The `entity.list` row of `id`.
    fn row(reg: &ToolRegistry, engine: &mut Headless, id: u64) -> anyhow::Result<Value> {
        let list = call_json(reg, engine, "entity.list", json!({}))?;
        list.as_array()
            .unwrap()
            .iter()
            .find(|row| row["entity"] == id)
            .cloned()
            .context("entity not listed")
    }

    #[test]
    fn hierarchy_set_parent_keeps_world_and_lists_parent() -> anyhow::Result<()> {
        let (reg, mut engine, cube) = setup()?;
        let parent = spawn_at(&reg, &mut engine, cube, [1.0, 0.0, 0.0], None)?;
        call_json(
            &reg,
            &mut engine,
            "entity.set_transform",
            json!({ "entity": parent, "rotation_euler_deg": [0, 90, 0] }),
        )?;
        let child = spawn_at(&reg, &mut engine, cube, [1.0, 0.0, -1.0], None)?;
        assert!(row(&reg, &mut engine, child)?.get("parent").is_none());

        // keep_world (the default): the world position stays, the local
        // position becomes (1, 0, 0) in the rotated parent's space.
        let out = call_json(
            &reg,
            &mut engine,
            "entity.set_parent",
            json!({ "entity": child, "parent": parent }),
        )?;
        assert_eq!(out["parent"], parent);
        assert_close(&out["transform"]["translation"], [1.0, 0.0, 0.0]);
        let listed = row(&reg, &mut engine, child)?;
        assert_eq!(listed["parent"], parent);
        assert_close(&listed["translation"], [1.0, 0.0, 0.0]);
        assert_close(&listed["world_translation"], [1.0, 0.0, -1.0]);

        // Without keep_world the local transform is kept.
        let out = call_json(
            &reg,
            &mut engine,
            "entity.set_parent",
            json!({ "entity": child, "parent": null, "keep_world": false }),
        )?;
        assert_eq!(out["parent"], Value::Null);
        let listed = row(&reg, &mut engine, child)?;
        assert!(listed.get("parent").is_none(), "{listed}");
        assert_close(&listed["world_translation"], [1.0, 0.0, 0.0]);

        // Cycles and unknown ids are refused.
        call_json(
            &reg,
            &mut engine,
            "entity.set_parent",
            json!({ "entity": child, "parent": parent }),
        )?;
        let err = reg
            .call(
                &mut engine,
                "entity.set_parent",
                json!({ "entity": parent, "parent": child }),
            )
            .unwrap_err();
        assert!(err.to_string().contains("descendant"), "{err}");
        let err = reg
            .call(
                &mut engine,
                "entity.set_parent",
                json!({ "entity": parent, "parent": 0x1_0000_002a_u64 }),
            )
            .unwrap_err();
        assert!(err.to_string().contains("no entity"), "{err}");
        Ok(())
    }

    #[test]
    fn hierarchy_spawn_under_a_parent_is_local_and_undoable() -> anyhow::Result<()> {
        let (reg, mut engine, cube) = setup()?;
        let parent = spawn_at(&reg, &mut engine, cube, [0.0, 5.0, 0.0], None)?;
        let before = call_json(&reg, &mut engine, "entity.list", json!({}))?;

        let child_out = crate::command::apply(
            &reg,
            &mut engine,
            "entity.spawn",
            json!({ "mesh": cube, "translation": [1, 0, 0], "parent": parent }),
        )?;
        let ToolOutput::Json(child_out) = child_out else {
            anyhow::bail!("no JSON");
        };
        let child = child_out["entity"].as_u64().unwrap();
        let listed = row(&reg, &mut engine, child)?;
        assert_eq!(listed["parent"], parent);
        assert_close(&listed["world_translation"], [1.0, 5.0, 0.0]);
        let parent_entity = Entity::from_bits(parent).unwrap();
        assert_eq!(
            engine
                .world
                .get::<&super::super::Children>(parent_entity)?
                .0,
            vec![Entity::from_bits(child).unwrap()]
        );

        assert!(crate::command::undo(&mut engine)?);
        assert_eq!(
            call_json(&reg, &mut engine, "entity.list", json!({}))?,
            before
        );
        assert!(
            engine
                .world
                .get::<&super::super::Children>(parent_entity)
                .is_err(),
            "the parent's child list is restored too"
        );
        assert!(crate::command::redo(&mut engine)?);
        assert_eq!(row(&reg, &mut engine, child)?["parent"], parent);
        assert_eq!(
            engine
                .world
                .get::<&super::super::Children>(parent_entity)?
                .0,
            vec![Entity::from_bits(child).unwrap()]
        );

        let err = reg
            .call(
                &mut engine,
                "entity.spawn",
                json!({ "mesh": cube, "parent": 0x1_0000_002a_u64 }),
            )
            .unwrap_err();
        assert!(err.to_string().contains("no entity"), "{err}");
        assert_eq!(engine.world.len(), 2, "nothing spawned");
        Ok(())
    }

    #[test]
    fn hierarchy_set_parent_and_recursive_despawn_are_undoable() -> anyhow::Result<()> {
        let (reg, mut engine, cube) = setup()?;
        let root = spawn_at(&reg, &mut engine, cube, [0.0, 0.0, 0.0], None)?;
        let mid = spawn_at(&reg, &mut engine, cube, [2.0, 0.0, 0.0], Some(root))?;
        let leaf = spawn_at(&reg, &mut engine, cube, [0.0, 3.0, 0.0], Some(mid))?;
        let other = spawn_at(&reg, &mut engine, cube, [0.0, 0.0, 9.0], None)?;
        let snapshot = |engine: &mut Headless| call_json(&reg, engine, "entity.list", json!({}));
        let start = snapshot(&mut engine)?;

        // Reparent the leaf onto `other`, keeping its world position.
        crate::command::apply(
            &reg,
            &mut engine,
            "entity.set_parent",
            json!({ "entity": leaf, "parent": other }),
        )?;
        let moved = snapshot(&mut engine)?;
        assert_ne!(moved, start);
        assert_eq!(row(&reg, &mut engine, leaf)?["parent"], other);
        assert_close(
            &row(&reg, &mut engine, leaf)?["world_translation"],
            [2.0, 3.0, 0.0],
        );
        assert!(crate::command::undo(&mut engine)?);
        assert_eq!(snapshot(&mut engine)?, start, "set_parent undone");
        assert_eq!(
            engine
                .world
                .get::<&super::super::Children>(Entity::from_bits(mid).unwrap())?
                .0,
            vec![Entity::from_bits(leaf).unwrap()]
        );
        assert!(crate::command::redo(&mut engine)?);
        assert_eq!(snapshot(&mut engine)?, moved, "set_parent redone");
        assert!(crate::command::undo(&mut engine)?);

        // Despawning the root takes mid and leaf with it; undo brings all
        // three back under their ids with their links.
        let out = crate::command::apply(
            &reg,
            &mut engine,
            "entity.despawn",
            json!({ "entity": root }),
        )?;
        let ToolOutput::Json(out) = out else {
            anyhow::bail!("no JSON");
        };
        assert_eq!(out["removed"], 3);
        assert_eq!(engine.world.len(), 1);
        assert!(crate::command::undo(&mut engine)?);
        assert_eq!(snapshot(&mut engine)?, start, "recursive despawn undone");
        assert_eq!(row(&reg, &mut engine, leaf)?["parent"], mid);
        assert!(crate::command::redo(&mut engine)?);
        assert_eq!(engine.world.len(), 1);
        assert!(crate::command::undo(&mut engine)?);
        assert_eq!(snapshot(&mut engine)?, start);
        Ok(())
    }

    #[test]
    fn hierarchy_despawn_non_recursive_frees_the_children() -> anyhow::Result<()> {
        let (reg, mut engine, cube) = setup()?;
        let root = spawn_at(&reg, &mut engine, cube, [0.0, 4.0, 0.0], None)?;
        let child = spawn_at(&reg, &mut engine, cube, [1.0, 0.0, 0.0], Some(root))?;
        let start = call_json(&reg, &mut engine, "entity.list", json!({}))?;

        let out = crate::command::apply(
            &reg,
            &mut engine,
            "entity.despawn",
            json!({ "entity": root, "recursive": false }),
        )?;
        let ToolOutput::Json(out) = out else {
            anyhow::bail!("no JSON");
        };
        assert_eq!(out["removed"], 1);
        let listed = row(&reg, &mut engine, child)?;
        assert!(listed.get("parent").is_none(), "{listed}");
        assert_close(&listed["translation"], [1.0, 4.0, 0.0]);
        assert_close(&listed["world_translation"], [1.0, 4.0, 0.0]);

        assert!(crate::command::undo(&mut engine)?);
        assert_eq!(
            call_json(&reg, &mut engine, "entity.list", json!({}))?,
            start
        );
        assert_eq!(
            engine
                .world
                .get::<&Parent>(Entity::from_bits(child).unwrap())?
                .0,
            Entity::from_bits(root).unwrap()
        );
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
