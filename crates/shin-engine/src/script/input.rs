//! Input actions for scripts (GE-033): `action_pressed(name)`,
//! `action_just_pressed(name)`, and `action_axis(name)`.
//!
//! Scripts cannot reach the [`Actions`] resource directly (Rhai functions
//! are `'static`), so [`script_system`](super::script_system) copies every
//! action's value into a shared [`ActionSnapshot`] before each batch of
//! calls and the three functions read that. Naming an action the project
//! does not define is a runtime error (recorded like any other script
//! error), so a typo shows up instead of reading as "not pressed".

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use rhai::{Engine, EvalAltResult, FLOAT};

use crate::input::{ActionValue, Actions};

/// Every action's value as scripts see it, shared between the engine and
/// the registered functions.
pub(crate) type ActionSnapshot = Rc<RefCell<BTreeMap<String, ActionValue>>>;

/// Copies the values of `actions` (none if there is no resource).
pub(crate) fn snapshot(actions: Option<&Actions>) -> BTreeMap<String, ActionValue> {
    actions
        .map(|actions| {
            actions
                .iter()
                .map(|(name, _, value)| (name.to_string(), value))
                .collect()
        })
        .unwrap_or_default()
}

/// Registers the three `action_*` functions, reading `snapshot`.
pub(crate) fn register(engine: &mut Engine, snapshot: ActionSnapshot) {
    let pressed = Rc::clone(&snapshot);
    engine.register_fn("action_pressed", move |name: &str| {
        lookup(&pressed, name).map(|value| value.pressed)
    });
    let just_pressed = Rc::clone(&snapshot);
    engine.register_fn("action_just_pressed", move |name: &str| {
        lookup(&just_pressed, name).map(|value| value.just_pressed)
    });
    engine.register_fn("action_axis", move |name: &str| {
        lookup(&snapshot, name).map(|value| value.axis as FLOAT)
    });
}

/// The value of `name`, or a runtime error naming it and the actions that
/// exist.
fn lookup(snapshot: &ActionSnapshot, name: &str) -> Result<ActionValue, Box<EvalAltResult>> {
    check(snapshot, name)?;
    Ok(snapshot.borrow()[name])
}

/// Ok if the project defines `name`, otherwise a runtime error naming it
/// and the actions that exist. `player_input` checks names with it too.
pub(crate) fn check(snapshot: &ActionSnapshot, name: &str) -> Result<(), Box<EvalAltResult>> {
    let snapshot = snapshot.borrow();
    if snapshot.contains_key(name) {
        return Ok(());
    }
    let names: Vec<&str> = snapshot.keys().map(String::as_str).collect();
    let defined = if names.is_empty() {
        "none defined (add [input.actions] to shin.toml)".to_string()
    } else {
        names.join(", ")
    };
    Err(format!("unknown action `{name}`; actions: {defined}").into())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use serde_json::{Value, json};

    use crate::ecs::Entity;
    use crate::headless::Headless;
    use crate::input::{Actions, InputConfig};
    use crate::math::Transform;
    use crate::tool::{ToolOutput, ToolRegistry};

    /// Moves along x by `move_x`, counts `jump` presses in y, and reports
    /// `jump` held in z.
    const PLAYER: &str = r#"
        fn update(entity, dt) {
            let p = entity.transform.translation;
            p.x += action_axis("move_x") * dt;
            if action_just_pressed("jump") { p.y += 1.0; }
            p.z = if action_pressed("jump") { 1.0 } else { 0.0 };
            entity.transform.translation = p;
        }
    "#;

    fn temp_dir(test: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("shin-script-input-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("scripts")).expect("create temp dir");
        dir
    }

    fn call(reg: &ToolRegistry, engine: &mut Headless, tool: &str, args: Value) -> Value {
        match reg.call(engine, tool, args) {
            Ok(ToolOutput::Json(value)) => value,
            other => panic!("`{tool}` did not return JSON: {other:?}"),
        }
    }

    /// A headless engine rooted at `dir` with `jump` and `move_x` defined
    /// and one entity running `source` as `scripts/player.rhai`; returns
    /// the engine, the registry, and the entity's id.
    fn player_engine(dir: &Path, source: &str) -> anyhow::Result<(Headless, ToolRegistry, u64)> {
        std::fs::write(dir.join("scripts/player.rhai"), source)?;
        let reg = ToolRegistry::with_engine_tools();
        let mut engine = Headless::new(8, 8)?;
        engine
            .resources
            .insert(crate::asset::Assets::new(dir.to_path_buf()));
        let config: InputConfig = toml::from_str(
            "[actions]\njump = [\"Space\"]\n\
             move_x = { axis = [\"KeyA\", \"KeyD\"], gamepad_axis = \"LeftStickX\" }\n",
        )?;
        engine.resources.insert(Actions::from_config(&config));
        call(&reg, &mut engine, "mesh.builtin", json!({ "kind": "cube" }));
        let spawned = call(&reg, &mut engine, "entity.spawn", json!({ "mesh": 0 }));
        let id = spawned["entity"].as_u64().unwrap();
        let attached = call(
            &reg,
            &mut engine,
            "script.attach",
            json!({ "entity": id, "path": "scripts/player.rhai" }),
        );
        assert_eq!(attached["error"], Value::Null, "{attached}");
        Ok((engine, reg, id))
    }

    fn translation(engine: &Headless, id: u64) -> [f32; 3] {
        let entity = Entity::from_bits(id).unwrap();
        engine
            .world
            .get::<&Transform>(entity)
            .unwrap()
            .translation
            .to_array()
    }

    /// AC 1: a script moving an entity with `action_axis("move_x")`
    /// responds to `input.set` under `sim.play`.
    #[test]
    fn script_moves_entity_with_injected_axis_under_play() -> anyhow::Result<()> {
        let dir = temp_dir("axis");
        let (mut engine, reg, id) = player_engine(&dir, PLAYER)?;
        call(&reg, &mut engine, "sim.play", json!({}));
        call(&reg, &mut engine, "sim.step", json!({ "steps": 60 }));
        assert_eq!(translation(&engine, id), [0.0; 3], "nothing pressed");

        call(
            &reg,
            &mut engine,
            "input.set",
            json!({ "action": "move_x", "axis": 1.0 }),
        );
        call(&reg, &mut engine, "sim.step", json!({ "steps": 60 }));
        let [x, _, _] = translation(&engine, id);
        assert!((x - 1.0).abs() < 1e-4, "one second at full axis: x = {x}");

        call(
            &reg,
            &mut engine,
            "input.set",
            json!({ "action": "move_x", "axis": -0.5 }),
        );
        call(&reg, &mut engine, "sim.step", json!({ "steps": 60 }));
        let [x, _, _] = translation(&engine, id);
        assert!((x - 0.5).abs() < 1e-4, "half speed back: x = {x}");

        // Keyboard-style: the axis also rides on `pressed`.
        call(
            &reg,
            &mut engine,
            "input.set",
            json!({ "action": "move_x", "pressed": false }),
        );
        call(&reg, &mut engine, "sim.step", json!({ "steps": 60 }));
        let [x, _, _] = translation(&engine, id);
        assert!((x - 0.5).abs() < 1e-4, "released: x = {x}");

        let errors = call(&reg, &mut engine, "script.errors", json!({}));
        assert_eq!(errors["errors"], json!([]));
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    /// `just_pressed` reaches exactly one step per press; `pressed` holds.
    #[test]
    fn just_pressed_is_seen_by_exactly_one_step() -> anyhow::Result<()> {
        let dir = temp_dir("edge");
        let (mut engine, reg, id) = player_engine(&dir, PLAYER)?;
        call(&reg, &mut engine, "sim.play", json!({}));
        call(
            &reg,
            &mut engine,
            "input.set",
            json!({ "action": "jump", "pressed": true }),
        );
        call(&reg, &mut engine, "sim.step", json!({ "steps": 5 }));
        assert_eq!(translation(&engine, id), [0.0, 1.0, 1.0], "one press, held");

        // Holding is not a new press; releasing and pressing again is.
        call(
            &reg,
            &mut engine,
            "input.set",
            json!({ "action": "jump", "pressed": true }),
        );
        call(&reg, &mut engine, "sim.step", json!({ "steps": 5 }));
        assert_eq!(translation(&engine, id), [0.0, 1.0, 1.0]);
        call(
            &reg,
            &mut engine,
            "input.set",
            json!({ "action": "jump", "pressed": false }),
        );
        call(&reg, &mut engine, "sim.step", json!({ "steps": 1 }));
        assert_eq!(translation(&engine, id), [0.0, 1.0, 0.0], "released");
        call(
            &reg,
            &mut engine,
            "input.set",
            json!({ "action": "jump", "pressed": true }),
        );
        call(&reg, &mut engine, "sim.step", json!({ "steps": 3 }));
        assert_eq!(translation(&engine, id), [0.0, 2.0, 1.0], "second press");

        // Paused: steps still advance (and consume press edges) but scripts
        // do not run, so a press made while paused is not seen on resume.
        call(&reg, &mut engine, "sim.pause", json!({}));
        call(
            &reg,
            &mut engine,
            "input.set",
            json!({ "action": "jump", "pressed": false }),
        );
        call(
            &reg,
            &mut engine,
            "input.set",
            json!({ "action": "jump", "pressed": true }),
        );
        call(&reg, &mut engine, "sim.step", json!({ "steps": 3 }));
        assert_eq!(
            translation(&engine, id),
            [0.0, 2.0, 1.0],
            "paused: unchanged"
        );
        call(&reg, &mut engine, "sim.pause", json!({}));
        call(&reg, &mut engine, "sim.step", json!({ "steps": 3 }));
        assert_eq!(
            translation(&engine, id),
            [0.0, 2.0, 1.0],
            "the edge was consumed while paused"
        );
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn unknown_action_is_a_script_error_naming_it() -> anyhow::Result<()> {
        let dir = temp_dir("unknown");
        let source = "fn update(entity, dt) { if action_pressed(\"fly\") { } }";
        let (mut engine, reg, _) = player_engine(&dir, source)?;
        call(&reg, &mut engine, "sim.play", json!({}));
        call(&reg, &mut engine, "sim.step", json!({ "steps": 1 }));
        let errors = call(&reg, &mut engine, "script.errors", json!({}));
        let message = errors["errors"][0]["message"].as_str().unwrap_or_default();
        assert!(message.contains("unknown action `fly`"), "{errors}");
        assert!(message.contains("jump, move_x"), "{errors}");
        assert_eq!(errors["errors"][0]["path"], "scripts/player.rhai");
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    /// `script.eval` sees the current actions too, so an agent can check
    /// what a script would read.
    #[test]
    fn script_eval_reads_actions() -> anyhow::Result<()> {
        let dir = temp_dir("eval");
        let (mut engine, reg, _) = player_engine(&dir, PLAYER)?;
        call(
            &reg,
            &mut engine,
            "input.set",
            json!({ "action": "move_x", "axis": 0.25 }),
        );
        let out = call(
            &reg,
            &mut engine,
            "script.eval",
            json!({ "source": "[action_axis(\"move_x\"), action_pressed(\"jump\")]" }),
        );
        assert_eq!(out["result"], json!([0.25, false]));
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}
