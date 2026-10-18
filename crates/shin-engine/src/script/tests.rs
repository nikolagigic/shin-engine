use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};

use super::*;
use crate::command;
use crate::ecs::{Schedule, fixed_step};
use crate::headless::Headless;
use crate::math::Vec3;
use crate::tool::{ToolError, ToolOutput, ToolRegistry};

const DT: f64 = 1.0 / 60.0;

/// Moves the entity up and down: `y = sin(t)`.
const BOB: &str = "fn update(entity, dt) {\n    entity.transform.translation.y = sin(time());\n}\n";

/// A fresh, empty directory under the system temp dir for one test.
fn temp_dir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("shin-script-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("scripts")).expect("create temp dir");
    dir
}

/// Writes `source` to `dir/rel` and moves its modification time `age`
/// into the future, so a rewrite within the file system's timestamp
/// resolution still counts as a change.
fn write_script(dir: &Path, rel: &str, source: &str, age: u64) {
    let path = dir.join(rel);
    std::fs::write(&path, source).expect("write script");
    let file = std::fs::File::options()
        .write(true)
        .open(&path)
        .expect("open script");
    file.set_modified(SystemTime::now() + Duration::from_secs(age))
        .expect("set mtime");
}

/// A world with one scripted entity, resources rooted at `dir` with no
/// `PlayState` (so scripts run), and a schedule holding the script system.
fn scripted_world(dir: &Path, rel: &str) -> (World, Resources, Schedule, Entity) {
    let mut world = World::new();
    let entity = world.spawn((
        Transform::from_translation(Vec3::new(2.0, 0.0, 0.0)),
        Script::new(rel),
        Name("bob".into()),
    ));
    let mut res = Resources::new();
    res.insert(Assets::new(dir.to_path_buf()));
    let mut schedule = Schedule::new();
    schedule.add_fixed("script", script_system);
    (world, res, schedule, entity)
}

fn translation(world: &World, entity: Entity) -> Vec3 {
    world.get::<&Transform>(entity).unwrap().translation
}

/// AC 1: a script that sets `transform.translation.y = sin(t)` moves the
/// entity, step after step.
#[test]
fn sin_script_moves_the_entity() {
    let dir = temp_dir("sin");
    write_script(&dir, "scripts/bob.rhai", BOB, 0);
    let (mut world, mut res, mut schedule, entity) = scripted_world(&dir, "scripts/bob.rhai");

    for step in 1..=30 {
        fixed_step(&mut world, &mut res, &mut schedule, DT);
        let t = step as f64 * DT;
        let at = translation(&world, entity);
        assert!(
            (at.y - t.sin() as f32).abs() < 1e-5,
            "step {step}: y = {}, sin(t) = {}",
            at.y,
            t.sin()
        );
        assert_eq!(at.x, 2.0, "other components are kept");
    }
    assert_eq!(res.expect::<ScriptEngine>().errors().count(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scripts_see_id_name_dt_and_full_transform() {
    let dir = temp_dir("api");
    let source = r#"
        fn update(entity, dt) {
            let t = entity.transform;
            t.scale = vec3(1.0, 2.0, 3.0) * 2.0;
            t.rotation = quat_euler(0.0, PI() / 2.0, 0.0);
            let p = t.translation;
            p.x += dt * 60;
            if entity.name == "bob" && entity.id > 0 { p.z = 7; }
            entity.transform.translation = p;
        }
    "#;
    write_script(&dir, "scripts/api.rhai", source, 0);
    let (mut world, mut res, mut schedule, entity) = scripted_world(&dir, "scripts/api.rhai");
    fixed_step(&mut world, &mut res, &mut schedule, DT);

    let errors: Vec<String> = res
        .expect::<ScriptEngine>()
        .errors()
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    let t = *world.get::<&Transform>(entity).unwrap();
    assert!(
        (t.translation - Vec3::new(3.0, 0.0, 7.0)).length() < 1e-5,
        "{t:?}"
    );
    assert_eq!(t.scale, Vec3::new(2.0, 4.0, 6.0));
    assert!((t.forward() - Vec3::NEG_X).length() < 1e-5, "{t:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// AC 3: a syntax error is logged and recorded with file and line, and the
/// engine keeps going.
/// GE-031: `entity.transform` is local to the parent; `world_transform`
/// is the composed value and writing to it changes nothing.
#[test]
fn hierarchy_scripts_see_local_and_world_transforms() {
    let dir = temp_dir("hierarchy");
    write_script(
        &dir,
        "scripts/bob.rhai",
        "fn update(entity, dt) {\n\
         \x20   let w = entity.world_transform;\n\
         \x20   entity.transform.translation.y = w.translation.x;\n\
         \x20   w.translation.z = 99.0;\n\
         \x20   entity.world_transform.translation.z = 99.0;\n\
         }\n",
        0,
    );
    let (mut world, mut res, mut schedule, entity) = scripted_world(&dir, "scripts/bob.rhai");
    let parent = world.spawn((Transform::from_translation(Vec3::new(10.0, 0.0, 0.0)),));
    crate::ecs::set_parent(&mut world, entity, Some(parent), false).unwrap();

    fixed_step(&mut world, &mut res, &mut schedule, DT);
    let local = *world.get::<&Transform>(entity).unwrap();
    // local x stays 2; world x is 12, copied into local y; z untouched.
    assert_eq!(local.translation, Vec3::new(2.0, 12.0, 0.0));
    assert_eq!(
        crate::ecs::world_transform(&world, entity).translation,
        Vec3::new(12.0, 12.0, 0.0)
    );
    assert_eq!(res.expect::<ScriptEngine>().errors().count(), 0);
}

#[test]
fn syntax_error_is_recorded_with_file_and_line_and_does_not_crash() {
    let dir = temp_dir("syntax");
    let source = "fn update(entity, dt) {\n    let x = ;\n}\n";
    write_script(&dir, "scripts/broken.rhai", source, 0);
    let (mut world, mut res, mut schedule, entity) = scripted_world(&dir, "scripts/broken.rhai");

    for _ in 0..3 {
        fixed_step(&mut world, &mut res, &mut schedule, DT);
    }
    assert_eq!(translation(&world, entity), Vec3::new(2.0, 0.0, 0.0));
    let errors: Vec<&ScriptError> = res.expect::<ScriptEngine>().errors().collect();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].path, Path::new("scripts/broken.rhai"));
    assert_eq!(errors[0].line, Some(2));
    let shown = errors[0].to_string();
    assert!(shown.starts_with("scripts/broken.rhai:2: "), "{shown}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_update_runtime_errors_and_loops_are_recorded() {
    let dir = temp_dir("runtime");
    write_script(&dir, "scripts/none.rhai", "let x = 1;", 0);
    write_script(
        &dir,
        "scripts/oops.rhai",
        "fn update(entity, dt) {\n    let a = 1;\n    entity.nope = 2;\n}\n",
        0,
    );
    write_script(
        &dir,
        "scripts/spin.rhai",
        "fn update(entity, dt) { loop { } }",
        0,
    );
    let mut world = World::new();
    for rel in [
        "scripts/none.rhai",
        "scripts/oops.rhai",
        "scripts/spin.rhai",
        "scripts/missing.rhai",
    ] {
        world.spawn((Transform::IDENTITY, Script::new(rel)));
    }
    let mut res = Resources::new();
    res.insert(Assets::new(dir.clone()));
    let mut schedule = Schedule::new();
    schedule.add_fixed("script", script_system);
    fixed_step(&mut world, &mut res, &mut schedule, DT);
    fixed_step(&mut world, &mut res, &mut schedule, DT);

    let scripts = res.expect::<ScriptEngine>();
    let error = |rel: &str| scripts.error(Path::new(rel)).cloned().expect(rel);
    assert!(
        error("scripts/none.rhai")
            .message
            .contains("fn update(entity, dt)")
    );
    let oops = error("scripts/oops.rhai");
    assert_eq!(oops.line, Some(3), "{oops}");
    assert!(oops.message.contains("nope"), "{oops}");
    let spin = error("scripts/spin.rhai");
    assert!(spin.message.contains("operations"), "{spin}");
    assert!(
        error("scripts/missing.rhai")
            .message
            .contains("cannot read")
    );
    assert_eq!(scripts.errors().count(), 4);
    let _ = std::fs::remove_dir_all(&dir);
}

/// AC 2 (the part a test can show): rewriting the file changes behaviour
/// on the next poll without restarting; a broken edit keeps the last good
/// version running, and fixing it clears the error.
#[test]
fn hot_reload_picks_up_edits() {
    let dir = temp_dir("reload");
    let rel = "scripts/move.rhai";
    let set_x =
        |x: &str| format!("fn update(entity, dt) {{ entity.transform.translation.x = {x}; }}");
    write_script(&dir, rel, &set_x("1.0"), 0);
    let (mut world, mut res, mut schedule, entity) = scripted_world(&dir, rel);
    fixed_step(&mut world, &mut res, &mut schedule, DT);
    assert_eq!(translation(&world, entity).x, 1.0);

    // Within the poll interval the change is not seen yet.
    write_script(&dir, rel, &set_x("5.0"), 10);
    fixed_step(&mut world, &mut res, &mut schedule, DT);
    assert_eq!(translation(&world, entity).x, 1.0);

    res.expect_mut::<ScriptEngine>()
        .set_poll_interval(Duration::ZERO);
    fixed_step(&mut world, &mut res, &mut schedule, DT);
    assert_eq!(translation(&world, entity).x, 5.0, "reloaded");

    write_script(
        &dir,
        rel,
        "fn update(entity, dt) { entity.transform = ; }",
        20,
    );
    fixed_step(&mut world, &mut res, &mut schedule, DT);
    assert_eq!(translation(&world, entity).x, 5.0, "last good version runs");
    let scripts = res.expect::<ScriptEngine>();
    assert_eq!(scripts.error(Path::new(rel)).unwrap().line, Some(1));

    write_script(&dir, rel, &set_x("-2.0"), 30);
    fixed_step(&mut world, &mut res, &mut schedule, DT);
    assert_eq!(translation(&world, entity).x, -2.0);
    assert_eq!(res.expect::<ScriptEngine>().errors().count(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

fn call(reg: &ToolRegistry, engine: &mut Headless, tool: &str, args: Value) -> Value {
    match reg.call(engine, tool, args) {
        Ok(ToolOutput::Json(value)) => value,
        other => panic!("`{tool}` did not return JSON: {other:?}"),
    }
}

fn call_err(reg: &ToolRegistry, engine: &mut Headless, tool: &str, args: Value) -> String {
    match reg.call(engine, tool, args) {
        Err(ToolError::Failed(err)) => format!("{err:#}"),
        other => panic!("`{tool}` should have failed: {other:?}"),
    }
}

/// A headless engine rooted at `dir` with one spawned entity; returns the
/// engine and the entity's tool id.
fn engine_in(dir: &Path, reg: &ToolRegistry) -> anyhow::Result<(Headless, u64)> {
    let mut engine = Headless::new(8, 8)?;
    engine.resources.insert(Assets::new(dir.to_path_buf()));
    call(reg, &mut engine, "mesh.builtin", json!({ "kind": "cube" }));
    let spawned = call(reg, &mut engine, "entity.spawn", json!({ "mesh": 0 }));
    Ok((engine, spawned["entity"].as_u64().unwrap()))
}

fn y_of(engine: &Headless, id: u64) -> f32 {
    let entity = Entity::from_bits(id).unwrap();
    engine
        .world
        .get::<&Transform>(entity)
        .unwrap()
        .translation
        .y
}

/// The example's script compiles and orbits: radius kept, angle advanced
/// by `speed * t`.
#[test]
fn example_orbit_script_orbits() {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let (mut world, mut res, mut schedule, entity) =
        scripted_world(&examples, "scripts/orbit.rhai");
    for _ in 0..60 {
        fixed_step(&mut world, &mut res, &mut schedule, DT);
    }
    assert_eq!(res.expect::<ScriptEngine>().errors().count(), 0);
    let p = translation(&world, entity);
    let radius = (p.x * p.x + p.z * p.z).sqrt();
    assert!((radius - 2.0).abs() < 1e-3, "radius kept: {p}");
    let angle = (-p.z).atan2(p.x);
    assert!(
        (angle - 0.8).abs() < 1e-3,
        "turned 0.8 rad in one second: {angle}"
    );
}

#[test]
fn tools_are_listed() {
    let tools = ToolRegistry::with_engine_tools().list();
    for name in ["script.attach", "script.eval", "script.errors"] {
        assert!(tools.iter().any(|t| t.name == name), "{name} listed");
    }
}

/// Through the registry: attach, then scripts run only while playing and
/// not paused, with `time()` counting from play; stop keeps the script.
#[test]
fn attached_scripts_run_only_while_playing() -> anyhow::Result<()> {
    let dir = temp_dir("play");
    write_script(&dir, "scripts/bob.rhai", BOB, 0);
    let reg = ToolRegistry::with_engine_tools();
    let (mut engine, id) = engine_in(&dir, &reg)?;
    let out = call(
        &reg,
        &mut engine,
        "script.attach",
        json!({ "entity": id, "path": "scripts/bob.rhai" }),
    );
    assert_eq!(out["error"], Value::Null);
    assert_eq!(out["path"], "scripts/bob.rhai");

    engine.step(10);
    assert_eq!(y_of(&engine, id), 0.0, "edit mode does not run scripts");

    call(&reg, &mut engine, "sim.play", json!({}));
    call(&reg, &mut engine, "sim.step", json!({ "steps": 30 }));
    let y = y_of(&engine, id);
    assert!(
        (y - 0.5f32.sin()).abs() < 1e-5,
        "time() counts from play: {y}"
    );

    call(&reg, &mut engine, "sim.pause", json!({}));
    call(&reg, &mut engine, "sim.step", json!({ "steps": 30 }));
    assert_eq!(y_of(&engine, id), y, "paused scripts do not run");

    call(&reg, &mut engine, "sim.stop", json!({}));
    assert_eq!(y_of(&engine, id), 0.0);
    let entity = Entity::from_bits(id).unwrap();
    assert_eq!(
        *engine.world.get::<&Script>(entity)?,
        Script::new("scripts/bob.rhai"),
        "stop keeps the script"
    );

    let out = call(
        &reg,
        &mut engine,
        "script.attach",
        json!({ "entity": id, "path": "" }),
    );
    assert_eq!(out["path"], Value::Null);
    assert!(engine.world.get::<&Script>(entity).is_err());
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn attach_reports_compile_errors_and_script_errors_lists_them() -> anyhow::Result<()> {
    let dir = temp_dir("errors");
    write_script(
        &dir,
        "scripts/bad.rhai",
        "fn update(entity, dt) {\n  }}\n",
        0,
    );
    let reg = ToolRegistry::with_engine_tools();
    let (mut engine, id) = engine_in(&dir, &reg)?;
    assert_eq!(
        call(&reg, &mut engine, "script.errors", json!({})),
        json!({ "errors": [] })
    );
    let out = call(
        &reg,
        &mut engine,
        "script.attach",
        json!({ "entity": id, "path": "scripts/bad.rhai" }),
    );
    let error = out["error"].as_str().expect("compile error");
    assert!(error.starts_with("scripts/bad.rhai:2: "), "{error}");

    let errors = call(&reg, &mut engine, "script.errors", json!({}));
    assert_eq!(errors["errors"][0]["path"], "scripts/bad.rhai");
    assert_eq!(errors["errors"][0]["line"], 2);
    assert!(errors["errors"][0]["message"].as_str().is_some());

    // Playing a broken script does not crash the engine.
    call(&reg, &mut engine, "sim.play", json!({}));
    call(&reg, &mut engine, "sim.step", json!({ "steps": 5 }));

    let err = call_err(
        &reg,
        &mut engine,
        "script.attach",
        json!({ "entity": 999_999, "path": "scripts/bad.rhai" }),
    );
    assert!(err.contains("no entity with id 999999"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn script_eval_returns_json() -> anyhow::Result<()> {
    let reg = ToolRegistry::with_engine_tools();
    let mut engine = Headless::new(4, 4)?;
    let eval = |engine: &mut Headless, source: &str| {
        call(&reg, engine, "script.eval", json!({ "source": source }))["result"].clone()
    };
    assert_eq!(eval(&mut engine, "40 + 2"), json!(42));
    assert_eq!(
        eval(&mut engine, "let v = vec3(1.0, 2.0, 3.0); v.y = 5; v"),
        json!([1.0, 5.0, 3.0])
    );
    assert_eq!(
        eval(&mut engine, r#"#{ name: "a", list: [1, true, 0.5, ()] }"#),
        json!({ "name": "a", "list": [1, true, 0.5, null] })
    );
    assert_eq!(eval(&mut engine, "let x = 1;"), Value::Null);

    let err = call_err(
        &reg,
        &mut engine,
        "script.eval",
        json!({ "source": "1 +\n\n+" }),
    );
    assert!(err.contains(":3: "), "{err}");
    let err = call_err(
        &reg,
        &mut engine,
        "script.eval",
        json!({ "source": "\nnope()" }),
    );
    assert!(err.contains(":2: ") && err.contains("nope"), "{err}");
    Ok(())
}

/// GE-024 follow-up: the command history refuses to change while playing,
/// through the registry as an MCP agent would call it.
#[test]
fn history_is_frozen_while_playing() -> anyhow::Result<()> {
    let dir = temp_dir("frozen");
    write_script(&dir, "scripts/bob.rhai", BOB, 0);
    let reg = ToolRegistry::with_engine_tools();
    let (mut engine, id) = engine_in(&dir, &reg)?;
    command::apply(
        &reg,
        &mut engine,
        "script.attach",
        json!({ "entity": id, "path": "scripts/bob.rhai" }),
    )?;
    command::apply(
        &reg,
        &mut engine,
        "entity.set_name",
        json!({ "entity": id, "name": "a" }),
    )?;

    command::apply(&reg, &mut engine, "sim.play", json!({}))?;
    for (tool, args) in [
        ("history.undo", json!({})),
        ("history.redo", json!({})),
        ("entity.set_name", json!({ "entity": id, "name": "b" })),
    ] {
        let err = command::apply(&reg, &mut engine, tool, args).expect_err(tool);
        let err = match err {
            ToolError::Failed(err) => format!("{err:#}"),
            other => panic!("{tool}: {other}"),
        };
        assert!(
            err.contains("history is frozen while playing"),
            "{tool}: {err}"
        );
    }
    assert!(command::undo(&mut engine).is_err());
    assert!(command::redo(&mut engine).is_err());
    // Unrecorded calls still run, so play can be stepped and stopped.
    command::apply(&reg, &mut engine, "sim.step", json!({ "steps": 2 }))?;
    command::apply(&reg, &mut engine, "sim.pause", json!({}))?;
    assert!(
        command::undo(&mut engine).is_err(),
        "paused is still playing"
    );
    command::apply(&reg, &mut engine, "sim.stop", json!({}))?;

    // After stop the history works again: undo the rename, then the attach.
    let entity = Entity::from_bits(id).unwrap();
    assert!(command::undo(&mut engine)?);
    assert!(engine.world.get::<&Name>(entity).is_err());
    assert!(engine.world.get::<&Script>(entity).is_ok());
    assert!(command::undo(&mut engine)?);
    assert!(
        engine.world.get::<&Script>(entity).is_err(),
        "attach undone"
    );
    assert!(command::redo(&mut engine)?);
    assert!(engine.world.get::<&Script>(entity).is_ok(), "attach redone");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn scene_files_carry_the_script() -> anyhow::Result<()> {
    let dir = temp_dir("scene");
    let reg = ToolRegistry::with_engine_tools();
    let (mut engine, id) = engine_in(&dir, &reg)?;
    write_script(&dir, "scripts/bob.rhai", BOB, 0);
    call(
        &reg,
        &mut engine,
        "script.attach",
        json!({ "entity": id, "path": "scripts/bob.rhai" }),
    );
    let path = dir.join("s.scene.ron");
    call(&reg, &mut engine, "scene.save", json!({ "path": path }));
    let text = std::fs::read_to_string(&path)?;
    assert!(text.contains("script: \"scripts/bob.rhai\""), "{text}");

    call(&reg, &mut engine, "scene.clear", json!({}));
    call(&reg, &mut engine, "scene.load", json!({ "path": path }));
    let scripts: Vec<Script> = engine.world.query::<&Script>().iter().cloned().collect();
    assert_eq!(scripts, [Script::new("scripts/bob.rhai")]);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// A temp project dir with the starter blip at `assets/sounds/blip.wav`.
fn dir_with_blip(test: &str) -> anyhow::Result<PathBuf> {
    let dir = temp_dir(test);
    std::fs::create_dir_all(dir.join("assets/sounds"))?;
    std::fs::write(dir.join("assets/sounds/blip.wav"), crate::audio::blip_wav())?;
    Ok(dir)
}

/// GE-036: `play_sound` and `play_sound_at` play through the engine's
/// audio (silent here); a sound that cannot load is logged and does not
/// stop the script.
#[test]
fn scripts_play_sounds() -> anyhow::Result<()> {
    use crate::audio::AudioEngine;

    let dir = dir_with_blip("sounds")?;
    let reg = ToolRegistry::with_engine_tools();
    let (mut engine, id) = engine_in(&dir, &reg)?;
    engine.resources.insert(AudioEngine::silent());
    write_script(
        &dir,
        "scripts/noisy.rhai",
        "fn update(entity, dt) {\n    play_sound(\"assets/sounds/blip.wav\");\n    \
         play_sound_at(entity, \"assets/sounds/blip.wav\");\n    \
         play_sound(\"assets/sounds/nope.ogg\");\n}\n",
        0,
    );
    let attached = call(
        &reg,
        &mut engine,
        "script.attach",
        json!({ "entity": id, "path": "scripts/noisy.rhai" }),
    );
    assert_eq!(attached["error"], Value::Null);

    engine.step(1);
    let plays = |engine: &Headless| engine.resources.expect::<AudioEngine>().plays();
    assert_eq!(plays(&engine), 0, "scripts do not run while editing");
    call(&reg, &mut engine, "sim.play", json!({}));
    engine.step(2);
    assert_eq!(
        plays(&engine),
        4,
        "two good sounds per step; the bad one is skipped"
    );
    assert_eq!(
        engine.resources.expect::<ScriptEngine>().errors().count(),
        0
    );
    call(&reg, &mut engine, "sim.stop", json!({}));
    assert_eq!(engine.resources.expect::<AudioEngine>().playing(), 0);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// GE-036: the starter project's moon script blips once per revolution
/// (every 2π seconds of play).
#[test]
fn starter_moon_script_blips_once_per_revolution() -> anyhow::Result<()> {
    use crate::audio::AudioEngine;

    let dir = dir_with_blip("starter-moon")?;
    write_script(
        &dir,
        "scripts/moon.rhai",
        include_str!("../project/template/moon.rhai"),
        0,
    );
    let reg = ToolRegistry::with_engine_tools();
    let (mut engine, id) = engine_in(&dir, &reg)?;
    engine.resources.insert(AudioEngine::silent());
    call(
        &reg,
        &mut engine,
        "script.attach",
        json!({ "entity": id, "path": "scripts/moon.rhai" }),
    );
    call(&reg, &mut engine, "sim.play", json!({}));
    let plays = |engine: &Headless| engine.resources.expect::<AudioEngine>().plays();
    // One revolution is 2π s, about 377 steps at 60 Hz.
    engine.step(370);
    assert_eq!(plays(&engine), 0, "no blip before the first revolution");
    engine.step(10);
    assert_eq!(plays(&engine), 1, "one blip after the first revolution");
    engine.step(370);
    assert_eq!(plays(&engine), 1);
    engine.step(10);
    assert_eq!(plays(&engine), 2, "and one per revolution after that");
    assert_eq!(
        engine.resources.expect::<ScriptEngine>().errors().count(),
        0
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
