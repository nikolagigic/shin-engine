//! The editor's edits as plain functions (GE-023), so they can be tested
//! without a window; the panels only call them.
//!
//! Every `commit_*`, [`spawn`], and [`despawn`] is one tool call through
//! the engine's command history ([`shin::command::apply`]), so it is
//! undoable and an agent can reproduce it with the same tool call. The
//! `preview_*` functions write a component directly for live feedback while
//! a value is being dragged; they are not recorded. Before committing a
//! previewed value, put the original back with the same `preview_*`
//! function so the history records the change from the original.

use anyhow::Context as _;
use serde_json::{Value, json};
use shin::command;
use shin::ecs::{Entity, world_transform};
use shin::headless::Headless;
use shin::math::{Transform, Vec3};
use shin::physics::{Collider, RigidBody, ShapeDesc};
use shin::render::MeshInstance;
use shin::scene::{AudioDesc, TransformDesc};
use shin::tool::{ToolOutput, ToolRegistry};

/// The id tools use for `entity` (`entity.list`, `entity.spawn`, ...).
pub fn entity_id(entity: Entity) -> u64 {
    entity.to_bits().get()
}

/// Runs `tool` through the engine's command history and returns its JSON
/// output (`null` for non-JSON output).
fn apply(
    reg: &ToolRegistry,
    engine: &mut Headless,
    tool: &str,
    args: Value,
) -> anyhow::Result<Value> {
    let output =
        command::apply(reg, engine, tool, args).with_context(|| format!("{tool} failed"))?;
    Ok(match output {
        ToolOutput::Json(value) => value,
        _ => Value::Null,
    })
}

/// Spawns an entity drawing the mesh at asset `source` (for example
/// `builtin://cube`) at world position `at`, named `name` if given, and
/// returns it. With a `parent` (GE-031) the entity is spawned as its child:
/// `at` is converted into the parent's space, so it still appears at `at`.
///
/// The mesh is loaded with `asset.load` (not recorded: loading changes no
/// entity) and the entity created with one recorded `entity.spawn`.
pub fn spawn(
    reg: &ToolRegistry,
    engine: &mut Headless,
    source: &str,
    at: Vec3,
    name: Option<&str>,
    parent: Option<Entity>,
) -> anyhow::Result<Entity> {
    let loaded = apply(reg, engine, "asset.load", json!({ "source": source }))?;
    let mesh = loaded["handle"]
        .as_u64()
        .with_context(|| format!("{source} is not a mesh source"))?;
    let translation = match parent {
        Some(parent) => {
            let local = Transform::from_translation(at)
                .relative_to(&world_transform(&engine.world, parent))
                .translation;
            anyhow::ensure!(
                local.is_finite(),
                "cannot add under entity #{}: it has a zero scale component",
                entity_id(parent)
            );
            local
        }
        None => at,
    };
    let mut args = json!({ "mesh": mesh, "translation": translation.to_array() });
    if let Some(name) = name {
        args["name"] = json!(name);
    }
    if let Some(parent) = parent {
        args["parent"] = json!(entity_id(parent));
    }
    let spawned = apply(reg, engine, "entity.spawn", args)?;
    spawned["entity"]
        .as_u64()
        .and_then(Entity::from_bits)
        .context("entity.spawn returned no entity id")
}

/// Removes `entity` with one recorded `entity.despawn`.
pub fn despawn(reg: &ToolRegistry, engine: &mut Headless, entity: Entity) -> anyhow::Result<()> {
    apply(
        reg,
        engine,
        "entity.despawn",
        json!({ "entity": entity_id(entity) }),
    )?;
    Ok(())
}

/// Makes `parent` the parent of `entity` (or makes `entity` a root with
/// `None`), keeping its world transform, with one recorded
/// `entity.set_parent` (GE-031).
pub fn set_parent(
    reg: &ToolRegistry,
    engine: &mut Headless,
    entity: Entity,
    parent: Option<Entity>,
) -> anyhow::Result<()> {
    apply(
        reg,
        engine,
        "entity.set_parent",
        json!({
            "entity": entity_id(entity),
            "parent": parent.map(entity_id),
            "keep_world": true,
        }),
    )?;
    Ok(())
}

/// Renames `entity` (empty removes the name) with one recorded
/// `entity.set_name`.
pub fn commit_name(
    reg: &ToolRegistry,
    engine: &mut Headless,
    entity: Entity,
    name: &str,
) -> anyhow::Result<()> {
    apply(
        reg,
        engine,
        "entity.set_name",
        json!({ "entity": entity_id(entity), "name": name }),
    )?;
    Ok(())
}

/// Sets `entity`'s translation, rotation, and scale with one recorded
/// `entity.set_transform`.
pub fn commit_transform(
    reg: &ToolRegistry,
    engine: &mut Headless,
    entity: Entity,
    desc: &TransformDesc,
) -> anyhow::Result<()> {
    apply(
        reg,
        engine,
        "entity.set_transform",
        json!({
            "entity": entity_id(entity),
            "translation": desc.translation,
            "rotation_euler_deg": desc.rotation_euler_deg,
            "scale": desc.scale,
        }),
    )?;
    Ok(())
}

/// Sets `entity`'s linear RGBA colour with one recorded `entity.set_color`.
pub fn commit_color(
    reg: &ToolRegistry,
    engine: &mut Headless,
    entity: Entity,
    color: [f32; 4],
) -> anyhow::Result<()> {
    apply(
        reg,
        engine,
        "entity.set_color",
        json!({ "entity": entity_id(entity), "color": color }),
    )?;
    Ok(())
}

/// Makes `entity` draw the mesh at asset `source` with one recorded
/// `entity.set_mesh`.
pub fn commit_mesh(
    reg: &ToolRegistry,
    engine: &mut Headless,
    entity: Entity,
    source: &str,
) -> anyhow::Result<()> {
    apply(
        reg,
        engine,
        "entity.set_mesh",
        json!({ "entity": entity_id(entity), "source": source }),
    )?;
    Ok(())
}

/// Gives `entity` the base-colour texture at texture asset `source`, or
/// removes its texture when `source` is `None`, with one recorded
/// `entity.set_texture`.
pub fn commit_texture(
    reg: &ToolRegistry,
    engine: &mut Headless,
    entity: Entity,
    source: Option<&str>,
) -> anyhow::Result<()> {
    apply(
        reg,
        engine,
        "entity.set_texture",
        json!({ "entity": entity_id(entity), "source": source }),
    )?;
    Ok(())
}

/// Attaches the Rhai script at `path` (relative to the project directory)
/// to `entity`, or removes its script when `path` is empty, with one
/// recorded `script.attach`. Returns the compile error, if any; the script
/// is attached either way, so it runs once the file is fixed.
pub fn commit_script(
    reg: &ToolRegistry,
    engine: &mut Headless,
    entity: Entity,
    path: &str,
) -> anyhow::Result<Option<String>> {
    let out = apply(
        reg,
        engine,
        "script.attach",
        json!({ "entity": entity_id(entity), "path": path }),
    )?;
    Ok(out["error"].as_str().map(str::to_string))
}

/// Sets `entity`'s sound to `audio` (source, volume, looping, spatial,
/// autoplay), or removes it when `audio` is `None`, with one recorded
/// `entity.set_audio` (GE-036).
pub fn commit_audio(
    reg: &ToolRegistry,
    engine: &mut Headless,
    entity: Entity,
    audio: Option<&AudioDesc>,
) -> anyhow::Result<()> {
    let args = match audio {
        Some(audio) => json!({
            "entity": entity_id(entity),
            "source": audio.source,
            "volume": audio.volume,
            "looping": audio.looping,
            "spatial": audio.spatial,
            "autoplay": audio.autoplay,
        }),
        None => json!({ "entity": entity_id(entity), "source": null }),
    };
    apply(reg, engine, "entity.set_audio", args)?;
    Ok(())
}

/// Plays the sound at asset `source` once, at full volume, with
/// `audio.play` (not recorded: it changes no entity). The Inspector's
/// preview button.
pub fn preview_sound(
    reg: &ToolRegistry,
    engine: &mut Headless,
    source: &str,
) -> anyhow::Result<()> {
    apply(reg, engine, "audio.play", json!({ "source": source }))?;
    Ok(())
}

/// Moves the scene's camera to `position`, aimed at `look_at` with +Y up
/// and keeping its field of view, with one recorded `camera.set` (GE-030:
/// the Inspector's "Set scene camera from view").
pub fn commit_camera(
    reg: &ToolRegistry,
    engine: &mut Headless,
    position: Vec3,
    look_at: Vec3,
) -> anyhow::Result<()> {
    apply(
        reg,
        engine,
        "camera.set",
        json!({ "position": position.to_array(), "look_at": look_at.to_array() }),
    )?;
    Ok(())
}

/// Gives `entity` the rigid `body` (GE-034), or removes its body with
/// `None`, with one recorded `physics.set_body`.
pub fn commit_body(
    reg: &ToolRegistry,
    engine: &mut Headless,
    entity: Entity,
    body: Option<RigidBody>,
) -> anyhow::Result<()> {
    let args = match body {
        Some(body) => json!({
            "entity": entity_id(entity),
            "kind": body.kind,
            "mass": body.mass,
        }),
        None => json!({ "entity": entity_id(entity), "kind": null }),
    };
    apply(reg, engine, "physics.set_body", args)?;
    Ok(())
}

/// Gives `entity` the `collider` (GE-034), or removes its collider with
/// `None`, with one recorded `physics.set_collider`.
pub fn commit_collider(
    reg: &ToolRegistry,
    engine: &mut Headless,
    entity: Entity,
    collider: Option<Collider>,
) -> anyhow::Result<()> {
    let args = match collider {
        Some(collider) => json!({
            "entity": entity_id(entity),
            "shape": ShapeDesc::from(collider.shape),
            "friction": collider.friction,
            "restitution": collider.restitution,
        }),
        None => json!({ "entity": entity_id(entity), "shape": null }),
    };
    apply(reg, engine, "physics.set_collider", args)?;
    Ok(())
}

/// Writes `desc` into `entity`'s [`Transform`] without recording it.
/// Does nothing if the entity has no transform.
pub fn preview_transform(engine: &mut Headless, entity: Entity, desc: &TransformDesc) {
    if let Ok(transform) = engine.world.query_one_mut::<&mut Transform>(entity) {
        *transform = desc.to_transform();
    }
}

/// Writes `color` into `entity`'s [`MeshInstance`] without recording it.
/// Does nothing if the entity draws no mesh.
pub fn preview_color(engine: &mut Headless, entity: Entity, color: [f32; 4]) {
    if let Ok(instance) = engine.world.query_one_mut::<&mut MeshInstance>(entity) {
        instance.color = color;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shin::command::History;
    use shin::ecs::Name;
    use shin::math::{Camera, Projection};

    fn setup() -> anyhow::Result<(ToolRegistry, Headless, Entity)> {
        let reg = ToolRegistry::with_engine_tools();
        let mut engine = Headless::new(8, 8)?;
        let entity = spawn(
            &reg,
            &mut engine,
            "builtin://cube",
            Vec3::new(1.0, 2.0, 3.0),
            Some("box"),
            None,
        )?;
        Ok((reg, engine, entity))
    }

    fn undo_len(engine: &Headless) -> usize {
        engine
            .resources
            .get::<History>()
            .map_or(0, History::undo_len)
    }

    fn transform(engine: &Headless, entity: Entity) -> Transform {
        *engine.world.get::<&Transform>(entity).unwrap()
    }

    fn instance(engine: &Headless, entity: Entity) -> MeshInstance {
        *engine.world.get::<&MeshInstance>(entity).unwrap()
    }

    fn name(engine: &Headless, entity: Entity) -> Option<String> {
        engine.world.get::<&Name>(entity).ok().map(|n| n.0.clone())
    }

    #[test]
    fn spawn_is_one_entry_at_the_given_place() -> anyhow::Result<()> {
        let (_, engine, entity) = setup()?;
        assert_eq!(undo_len(&engine), 1, "asset.load is not recorded");
        assert_eq!(
            transform(&engine, entity).translation,
            Vec3::new(1.0, 2.0, 3.0)
        );
        assert_eq!(name(&engine, entity).as_deref(), Some("box"));
        Ok(())
    }

    #[test]
    fn spawn_rejects_an_unknown_builtin() -> anyhow::Result<()> {
        let (reg, mut engine, _) = setup()?;
        assert!(spawn(&reg, &mut engine, "builtin://torus", Vec3::ZERO, None, None).is_err());
        assert_eq!(undo_len(&engine), 1);
        Ok(())
    }

    #[test]
    fn tree_spawn_under_a_parent_and_set_parent_undo() -> anyhow::Result<()> {
        let (reg, mut engine, parent) = setup()?;
        commit_transform(
            &reg,
            &mut engine,
            parent,
            &TransformDesc {
                translation: [1.0, 2.0, 3.0],
                rotation_euler_deg: [0.0, 90.0, 0.0],
                scale: [2.0, 2.0, 2.0],
            },
        )?;
        let at = Vec3::new(4.0, 0.0, -1.0);
        let child = spawn(&reg, &mut engine, "builtin://cube", at, None, Some(parent))?;
        assert_eq!(shin::ecs::parent_of(&engine.world, child), Some(parent));
        let world = world_transform(&engine.world, child).translation;
        assert!(world.abs_diff_eq(at, 1e-5), "added where asked: {world}");

        set_parent(&reg, &mut engine, child, None)?;
        assert_eq!(shin::ecs::parent_of(&engine.world, child), None);
        let world = world_transform(&engine.world, child).translation;
        assert!(world.abs_diff_eq(at, 1e-5), "keep_world: {world}");
        command::undo(&mut engine)?;
        assert_eq!(shin::ecs::parent_of(&engine.world, child), Some(parent));
        Ok(())
    }

    #[test]
    fn despawn_undo_redo() -> anyhow::Result<()> {
        let (reg, mut engine, entity) = setup()?;
        despawn(&reg, &mut engine, entity)?;
        assert!(!engine.world.contains(entity));
        command::undo(&mut engine)?;
        assert!(
            engine.world.contains(entity),
            "undo brings back the same id"
        );
        assert_eq!(name(&engine, entity).as_deref(), Some("box"));
        command::redo(&mut engine)?;
        assert!(!engine.world.contains(entity));
        Ok(())
    }

    #[test]
    fn commit_name_undo_redo() -> anyhow::Result<()> {
        let (reg, mut engine, entity) = setup()?;
        commit_name(&reg, &mut engine, entity, "crate")?;
        assert_eq!(name(&engine, entity).as_deref(), Some("crate"));
        assert_eq!(undo_len(&engine), 2);
        command::undo(&mut engine)?;
        assert_eq!(name(&engine, entity).as_deref(), Some("box"));
        command::redo(&mut engine)?;
        assert_eq!(name(&engine, entity).as_deref(), Some("crate"));
        Ok(())
    }

    #[test]
    fn commit_transform_undo_redo() -> anyhow::Result<()> {
        let (reg, mut engine, entity) = setup()?;
        let before = transform(&engine, entity);
        let desc = TransformDesc {
            translation: [4.0, 5.0, 6.0],
            rotation_euler_deg: [0.0, 90.0, 0.0],
            scale: [2.0, 2.0, 2.0],
        };
        commit_transform(&reg, &mut engine, entity, &desc)?;
        let after = transform(&engine, entity);
        assert_eq!(after.translation, Vec3::new(4.0, 5.0, 6.0));
        assert_eq!(after.scale, Vec3::splat(2.0));
        assert_eq!(TransformDesc::from_transform(&after), desc);
        command::undo(&mut engine)?;
        assert_eq!(transform(&engine, entity), before);
        command::redo(&mut engine)?;
        assert_eq!(transform(&engine, entity), after);
        Ok(())
    }

    #[test]
    fn previewed_drag_commits_one_entry_from_the_original() -> anyhow::Result<()> {
        let (reg, mut engine, entity) = setup()?;
        let original = TransformDesc::from_transform(&transform(&engine, entity));
        // A drag: several previews, none recorded.
        for x in 1..=5 {
            let mut desc = original;
            desc.translation[0] = x as f32;
            preview_transform(&mut engine, entity, &desc);
        }
        assert_eq!(transform(&engine, entity).translation.x, 5.0);
        assert_eq!(undo_len(&engine), 1, "previews are not recorded");

        // Release: restore the original, then commit the final value.
        let mut last = original;
        last.translation[0] = 5.0;
        preview_transform(&mut engine, entity, &original);
        commit_transform(&reg, &mut engine, entity, &last)?;
        assert_eq!(undo_len(&engine), 2, "one entry for the whole drag");
        command::undo(&mut engine)?;
        assert_eq!(transform(&engine, entity).translation.x, 1.0);
        Ok(())
    }

    #[test]
    fn commit_color_undo_redo_and_preview() -> anyhow::Result<()> {
        let (reg, mut engine, entity) = setup()?;
        preview_color(&mut engine, entity, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(instance(&engine, entity).color, [0.0, 0.0, 1.0, 1.0]);
        preview_color(&mut engine, entity, [1.0; 4]);
        commit_color(&reg, &mut engine, entity, [0.0, 1.0, 0.0, 1.0])?;
        assert_eq!(instance(&engine, entity).color, [0.0, 1.0, 0.0, 1.0]);
        command::undo(&mut engine)?;
        assert_eq!(instance(&engine, entity).color, [1.0; 4]);
        command::redo(&mut engine)?;
        assert_eq!(instance(&engine, entity).color, [0.0, 1.0, 0.0, 1.0]);
        Ok(())
    }

    #[test]
    fn commit_camera_undo_redo_keeps_the_fov() -> anyhow::Result<()> {
        let (reg, mut engine, _) = setup()?;
        let before = *engine.resources.expect::<Camera>();
        let Projection::Perspective { fov_y_radians, .. } = before.projection else {
            panic!("the default camera is perspective");
        };
        commit_camera(
            &reg,
            &mut engine,
            Vec3::new(3.0, 1.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
        )?;
        let after = *engine.resources.expect::<Camera>();
        assert_eq!(after.transform.translation, Vec3::new(3.0, 1.0, 0.0));
        assert!(after.transform.forward().distance(Vec3::NEG_X) < 1e-5);
        assert!(
            matches!(after.projection, Projection::Perspective { fov_y_radians: f, .. } if f == fov_y_radians),
            "{:?}",
            after.projection
        );
        assert_eq!(undo_len(&engine), 2);
        command::undo(&mut engine)?;
        assert_eq!(
            engine.resources.expect::<Camera>().transform,
            before.transform
        );
        command::redo(&mut engine)?;
        assert_eq!(
            engine.resources.expect::<Camera>().transform,
            after.transform
        );
        Ok(())
    }

    #[test]
    fn commit_mesh_undo_redo() -> anyhow::Result<()> {
        let (reg, mut engine, entity) = setup()?;
        let cube = instance(&engine, entity).mesh;
        commit_mesh(&reg, &mut engine, entity, "builtin://sphere")?;
        let sphere = instance(&engine, entity).mesh;
        assert_ne!(cube, sphere);
        command::undo(&mut engine)?;
        assert_eq!(instance(&engine, entity).mesh, cube);
        command::redo(&mut engine)?;
        assert_eq!(instance(&engine, entity).mesh, sphere);
        assert!(commit_mesh(&reg, &mut engine, entity, "no/such.gltf").is_err());
        assert_eq!(instance(&engine, entity).mesh, sphere);
        Ok(())
    }

    /// GE-036: the Inspector's audio commits are one undoable entry each,
    /// and its preview plays without being recorded.
    #[test]
    fn commit_audio_undo_redo_and_preview() -> anyhow::Result<()> {
        use shin::asset::Assets;
        use shin::audio::{AudioEngine, AudioSource};

        let (reg, mut engine, entity) = setup()?;
        let engine_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../shin-engine");
        engine.resources.insert(Assets::new(engine_dir));
        engine.resources.insert(AudioEngine::silent());
        let audio = |engine: &Headless| {
            engine
                .world
                .get::<&AudioSource>(entity)
                .ok()
                .map(|a| AudioDesc::from_source(&a))
        };
        let blip = AudioDesc {
            source: "assets/sounds/blip.wav".into(),
            spatial: true,
            ..Default::default()
        };
        commit_audio(&reg, &mut engine, entity, Some(&blip))?;
        assert_eq!(audio(&engine), Some(blip.clone()));
        let quieter = AudioDesc {
            volume: 0.5,
            ..blip.clone()
        };
        commit_audio(&reg, &mut engine, entity, Some(&quieter))?;
        commit_audio(&reg, &mut engine, entity, None)?;
        assert_eq!(audio(&engine), None);
        assert_eq!(undo_len(&engine), 4);
        command::undo(&mut engine)?;
        assert_eq!(audio(&engine), Some(quieter));
        command::undo(&mut engine)?;
        assert_eq!(audio(&engine), Some(blip));
        command::undo(&mut engine)?;
        assert_eq!(audio(&engine), None);

        preview_sound(&reg, &mut engine, "assets/sounds/blip.wav")?;
        assert_eq!(engine.resources.expect::<AudioEngine>().plays(), 1);
        assert_eq!(undo_len(&engine), 1, "previews are not recorded");
        Ok(())
    }
}
