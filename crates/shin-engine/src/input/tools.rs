//! Input tools (GE-033): `input.list` and `input.set`, so agents and tests
//! can read and drive actions on an engine that has no devices.

use anyhow::Context as _;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ActionBinding, ActionValue, Actions};
use crate::headless::Headless;
use crate::tool::{ToolOutput, ToolRegistry, TypedTool};

/// Registers `input.list` and `input.set`.
pub fn register_tools(reg: &mut ToolRegistry) {
    reg.register(TypedTool::new(
        "input.list",
        "Lists the input actions (from the project's shin.toml `[input.actions]`) with \
         their bindings and current values: `actions`, each { name, binding, pressed, \
         just_pressed, axis }. `binding` is the manifest form: an array of button names, \
         or { axis: [negative, positive], gamepad_axis }.",
        input_list,
    ));
    reg.register(TypedTool::new(
        "input.set",
        "Drives an action by name on this engine, which has no keyboard or gamepad: \
         `pressed` (true/false) or `axis` (-1..1), one of the two. The value stays until \
         the next input.set; `just_pressed` is true until a fixed step (sim.step) has run. \
         Returns the action's new { name, pressed, just_pressed, axis }. Fails for an \
         action the project does not define.",
        input_set,
    ));
}

/// `input.list` takes no arguments.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

/// Arguments of `input.set`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetArgs {
    /// The action's name, as in shin.toml.
    action: String,
    /// Hold (true) or release (false) the action.
    pressed: Option<bool>,
    /// The action's value in -1..1 (clamped); at or beyond ±0.5 it
    /// counts as pressed.
    axis: Option<f32>,
}

/// `{ name, binding, pressed, just_pressed, axis }`.
fn action_json(name: &str, binding: Option<&ActionBinding>, value: ActionValue) -> Value {
    let mut json = json!({
        "name": name,
        "pressed": value.pressed,
        "just_pressed": value.just_pressed,
        "axis": value.axis,
    });
    if let Some(binding) = binding {
        json["binding"] = serde_json::to_value(binding).unwrap_or(Value::Null);
    }
    json
}

fn input_list(engine: &mut Headless, _: NoArgs) -> anyhow::Result<ToolOutput> {
    let actions: Vec<Value> = engine
        .resources
        .get::<Actions>()
        .map(|actions| {
            actions
                .iter()
                .map(|(name, binding, value)| action_json(name, Some(binding), value))
                .collect()
        })
        .unwrap_or_default();
    Ok(ToolOutput::Json(json!({ "actions": actions })))
}

fn input_set(engine: &mut Headless, args: SetArgs) -> anyhow::Result<ToolOutput> {
    let axis = match (args.pressed, args.axis) {
        (Some(pressed), None) => {
            if pressed {
                1.0
            } else {
                0.0
            }
        }
        (None, Some(axis)) => axis,
        _ => anyhow::bail!("input.set takes `pressed` or `axis`, exactly one of them"),
    };
    let actions = engine
        .resources
        .get_mut::<Actions>()
        .context("this engine has no input actions")?;
    let value = actions.set_value(&args.action, axis)?;
    Ok(ToolOutput::Json(action_json(&args.action, None, value)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::InputConfig;
    use crate::tool::ToolError;

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

    #[test]
    fn input_tools_are_listed() {
        let tools = ToolRegistry::with_engine_tools().list();
        for name in ["input.list", "input.set"] {
            assert!(tools.iter().any(|t| t.name == name), "{name} listed");
        }
    }

    #[test]
    fn input_set_drives_actions_and_list_reports_them() -> anyhow::Result<()> {
        let reg = ToolRegistry::with_engine_tools();
        let mut engine = Headless::new(4, 4)?;
        let listed = call(&reg, &mut engine, "input.list", json!({}));
        assert_eq!(
            listed["actions"],
            json!([]),
            "a fresh engine has no actions"
        );
        let err = call_err(
            &reg,
            &mut engine,
            "input.set",
            json!({ "action": "jump", "pressed": true }),
        );
        assert!(err.contains("unknown action `jump`"), "{err}");

        let config: InputConfig = toml::from_str(
            "[actions]\njump = [\"Space\", \"Gamepad:South\"]\n\
             move_x = { axis = [\"KeyA\", \"KeyD\"], gamepad_axis = \"LeftStickX\" }\n",
        )?;
        engine.resources.insert(Actions::from_config(&config));

        let out = call(
            &reg,
            &mut engine,
            "input.set",
            json!({ "action": "jump", "pressed": true }),
        );
        assert_eq!(
            out,
            json!({ "name": "jump", "pressed": true, "just_pressed": true, "axis": 1.0 })
        );
        let out = call(
            &reg,
            &mut engine,
            "input.set",
            json!({ "action": "move_x", "axis": -0.75 }),
        );
        assert_eq!(out["axis"], -0.75);
        assert_eq!(out["pressed"], true);

        // A step consumes the edges; held state stays.
        engine.step(1);
        let listed = call(&reg, &mut engine, "input.list", json!({}));
        assert_eq!(
            listed["actions"],
            json!([
                {
                    "name": "jump",
                    "binding": ["Space", "Gamepad:South"],
                    "pressed": true,
                    "just_pressed": false,
                    "axis": 1.0
                },
                {
                    "name": "move_x",
                    "binding": { "axis": ["KeyA", "KeyD"], "gamepad_axis": "LeftStickX" },
                    "pressed": true,
                    "just_pressed": false,
                    "axis": -0.75
                }
            ])
        );
        // Rendering (frame systems) does not touch injected values.
        engine.render()?;
        assert!(engine.resources.expect::<Actions>().pressed("jump"));

        let out = call(
            &reg,
            &mut engine,
            "input.set",
            json!({ "action": "jump", "pressed": false }),
        );
        assert_eq!(out["pressed"], false);

        for args in [
            json!({ "action": "jump" }),
            json!({ "action": "jump", "pressed": true, "axis": 1.0 }),
        ] {
            let err = call_err(&reg, &mut engine, "input.set", args);
            assert!(err.contains("exactly one"), "{err}");
        }
        let err = call_err(
            &reg,
            &mut engine,
            "input.set",
            json!({ "action": "fly", "axis": 1.0 }),
        );
        assert!(err.contains("jump, move_x"), "{err}");
        Ok(())
    }
}
