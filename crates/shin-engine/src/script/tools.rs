//! Script tools (GE-011): `script.attach`, `script.eval`, and
//! `script.errors`.

use anyhow::Context as _;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Script, ScriptEngine, ScriptError, bindings, script_base};
use crate::command::Affected;
use crate::ecs::Entity;
use crate::headless::Headless;
use crate::input::Actions;
use crate::net::PlayerInputs;
use crate::tool::{ToolOutput, ToolRegistry, TypedTool};

/// Registers `script.attach`, `script.eval`, and `script.errors`.
pub fn register_tools(reg: &mut ToolRegistry) {
    reg.register(
        TypedTool::new(
            "script.attach",
            "Attaches a Rhai script (a .rhai file relative to the project directory, which \
             must define `fn update(entity, dt)`) to an entity, replacing any script it had; \
             an empty path removes the script. Scripts run every fixed step while playing. \
             The script is compiled straight away; returns the entity, the path (null when \
             removed), and `error`, the compile error or null.",
            script_attach,
        )
        .with_affected(|_, args| {
            let entity = args
                .get("entity")
                .and_then(Value::as_u64)
                .and_then(Entity::from_bits);
            Affected::Entities(entity.into_iter().collect())
        }),
    );
    reg.register(TypedTool::new(
        "script.eval",
        "Evaluates Rhai source with the script API (vec3, quat_euler, time(), action_pressed \
         and friends, math) and returns its result as JSON (`result`). Fails with the line \
         and message on a syntax or runtime error.",
        script_eval,
    ));
    reg.register(TypedTool::new(
        "script.errors",
        "Lists the latest compile or runtime error of each script that has one: path, \
         line (or null), and message.",
        script_errors,
    ));
}

/// Arguments of `script.attach`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AttachArgs {
    /// Entity id, as returned by entity.spawn or entity.list.
    entity: u64,
    /// Path of the .rhai file, relative to the project directory; empty
    /// removes the entity's script.
    path: String,
}

/// The engine's [`ScriptEngine`], inserted if missing.
fn script_engine(engine: &mut Headless) -> &mut ScriptEngine {
    if engine.resources.get::<ScriptEngine>().is_none() {
        engine.resources.insert(ScriptEngine::new());
    }
    engine.resources.expect_mut::<ScriptEngine>()
}

fn script_attach(engine: &mut Headless, args: AttachArgs) -> anyhow::Result<ToolOutput> {
    let entity = Entity::from_bits(args.entity)
        .filter(|&e| engine.world.contains(e))
        .with_context(|| format!("no entity with id {}", args.entity))?;
    let path = args.path.trim();
    if path.is_empty() {
        // Removing a script the entity does not have is not an error.
        let _ = engine.world.remove_one::<Script>(entity);
        return Ok(ToolOutput::Json(
            json!({ "entity": args.entity, "path": null, "error": null }),
        ));
    }
    let script = Script::new(path);
    let base = script_base(&engine.resources);
    let error = script_engine(engine)
        .load(&script.path, &base)
        .err()
        .map(|err| err.to_string());
    engine.world.insert_one(entity, script)?;
    Ok(ToolOutput::Json(
        json!({ "entity": args.entity, "path": path, "error": error }),
    ))
}

/// Arguments of `script.eval`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EvalArgs {
    /// Rhai source; its last expression is the result.
    source: String,
}

fn script_eval(engine: &mut Headless, args: EvalArgs) -> anyhow::Result<ToolOutput> {
    let actions = crate::script::input::snapshot(engine.resources.get::<Actions>());
    let inputs = engine
        .resources
        .get::<PlayerInputs>()
        .cloned()
        .unwrap_or_default();
    let scripts = script_engine(engine);
    *scripts.actions.borrow_mut() = actions;
    *scripts.inputs.borrow_mut() = inputs;
    let value = scripts.eval(&args.source)?;
    Ok(ToolOutput::Json(
        json!({ "result": bindings::to_json(&value) }),
    ))
}

/// `script.errors` takes no arguments.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

fn script_errors(engine: &mut Headless, _: NoArgs) -> anyhow::Result<ToolOutput> {
    let errors: Vec<Value> = engine
        .resources
        .get::<ScriptEngine>()
        .map(|scripts| scripts.errors().map(error_json).collect())
        .unwrap_or_default();
    Ok(ToolOutput::Json(json!({ "errors": errors })))
}

fn error_json(err: &ScriptError) -> Value {
    json!({
        "path": err.path.display().to_string(),
        "line": err.line,
        "message": err.message,
    })
}
