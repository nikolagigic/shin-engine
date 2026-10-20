//! Engine-native tool registry: named, schema-described operations on a
//! running engine. The MCP server (`shin-mcp` crate) exposes this registry
//! one-to-one. Owned by GE-013; each later ticket registers its own tools.
//!
//! A [`Tool`] has a `<subsystem>.<verb>` name, a description, and a JSON
//! Schema for its single input object, and runs against a
//! [`Headless`] engine. Most tools are written with the [`TypedTool`]
//! adapter, which derives the schema from a typed args struct and
//! deserializes arguments into it:
//!
//! ```no_run
//! use shin::headless::Headless;
//! use shin::tool::{ToolOutput, ToolRegistry, TypedTool};
//!
//! #[derive(serde::Deserialize, schemars::JsonSchema)]
//! struct StepArgs {
//!     /// Number of fixed steps to run.
//!     steps: u32,
//! }
//!
//! let mut registry = ToolRegistry::with_builtins();
//! registry.register(TypedTool::new(
//!     "sim.step",
//!     "Runs fixed simulation steps.",
//!     |engine: &mut Headless, args: StepArgs| {
//!         engine.step(args.steps);
//!         Ok(ToolOutput::Text(format!("ran {} steps", args.steps)))
//!     },
//! ));
//!
//! let mut engine = Headless::new(64, 64)?;
//! registry.call(&mut engine, "sim.step", serde_json::json!({ "steps": 3 }))?;
//! # anyhow::Ok(())
//! ```

use std::marker::PhantomData;
use std::sync::{Arc, RwLock};

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::command::Affected;
use crate::headless::Headless;

/// What a tool returns.
#[derive(Debug, Clone)]
pub enum ToolOutput {
    /// Human-readable text.
    Text(String),
    /// Structured data.
    Json(Value),
    /// A PNG-encoded image, e.g. a rendered frame.
    Image {
        /// PNG file bytes.
        png: Vec<u8>,
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
    },
}

/// Why a tool call failed.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// No tool with this name is registered.
    #[error("unknown tool `{0}`")]
    Unknown(String),
    /// The arguments do not match the tool's input schema.
    #[error("invalid arguments for `{tool}`: {message}")]
    InvalidArgs {
        /// The tool that rejected its arguments.
        tool: String,
        /// What is wrong, naming the offending field when known.
        message: String,
    },
    /// The tool ran and failed.
    #[error(transparent)]
    Failed(#[from] anyhow::Error),
}

/// A named, schema-described operation on a [`Headless`] engine.
pub trait Tool: Send + Sync + 'static {
    /// `<subsystem>.<verb>`, snake_case, e.g. `entity.spawn`.
    fn name(&self) -> &'static str;
    /// One or two sentences for humans and agents choosing a tool.
    fn description(&self) -> &'static str;
    /// JSON Schema for the single input object (use `schemars::schema_for!`).
    fn input_schema(&self) -> Value;
    /// Runs the tool on `engine` with `args`, which should match
    /// [`Self::input_schema`].
    fn call(&self, engine: &mut Headless, args: Value) -> Result<ToolOutput, ToolError>;
    /// What state a call with `args` may change, so
    /// [`crate::command::History`] can snapshot it for undo. Called before
    /// the call runs. The default, [`Affected::None`], means the call is
    /// not undoable and is not recorded.
    fn affected(&self, engine: &Headless, args: &Value) -> Affected {
        let _ = (engine, args);
        Affected::None
    }
}

/// How a [`TypedTool`] declares what it changes; see [`Tool::affected`].
pub type AffectedFn = fn(&Headless, &Value) -> Affected;

/// A [`Tool`] built from a typed args struct and a function.
///
/// The input schema is `schemars::schema_for!(A)`. Arguments are
/// deserialized into `A` before `f` runs; a JSON `null` counts as `{}`, so
/// tools whose fields are all optional can be called without arguments.
/// Deserialization failures become [`ToolError::InvalidArgs`] naming the
/// offending field where possible; errors from `f` become
/// [`ToolError::Failed`].
pub struct TypedTool<A, F> {
    name: &'static str,
    description: &'static str,
    f: F,
    affected: Option<AffectedFn>,
    // `fn(A)` keeps the adapter Send + Sync regardless of `A`.
    _args: PhantomData<fn(A)>,
}

impl<A, F> TypedTool<A, F>
where
    A: DeserializeOwned + JsonSchema + 'static,
    F: Fn(&mut Headless, A) -> anyhow::Result<ToolOutput> + Send + Sync + 'static,
{
    /// A tool called `name` that deserializes its arguments into `A` and
    /// runs `f`.
    pub fn new(name: &'static str, description: &'static str, f: F) -> Self {
        Self {
            name,
            description,
            f,
            affected: None,
            _args: PhantomData,
        }
    }

    /// Declares what a call changes (see [`Tool::affected`]); without this
    /// the tool reports [`Affected::None`].
    pub fn with_affected(mut self, affected: AffectedFn) -> Self {
        self.affected = Some(affected);
        self
    }
}

impl<A, F> Tool for TypedTool<A, F>
where
    A: DeserializeOwned + JsonSchema + 'static,
    F: Fn(&mut Headless, A) -> anyhow::Result<ToolOutput> + Send + Sync + 'static,
{
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn input_schema(&self) -> Value {
        schemars::schema_for!(A).to_value()
    }

    fn call(&self, engine: &mut Headless, args: Value) -> Result<ToolOutput, ToolError> {
        let args = if args.is_null() { json!({}) } else { args };
        let typed: A = serde_json::from_value(args.clone()).map_err(|err| {
            let message = match mistyped_field(&self.input_schema(), &args) {
                Some(field) => format!("{field} ({err})"),
                None => err.to_string(),
            };
            ToolError::InvalidArgs {
                tool: self.name.to_string(),
                message,
            }
        })?;
        Ok((self.f)(engine, typed)?)
    }

    fn affected(&self, engine: &Headless, args: &Value) -> Affected {
        self.affected
            .map_or(Affected::None, |affected| affected(engine, args))
    }
}

/// Finds the first top-level field of `args` whose JSON type is not allowed
/// by the `type` of its property in `schema`, and describes it, e.g.
/// ``field `steps`: expected integer, got string``.
///
/// serde_json's own type errors do not name the field, so this check turns
/// "invalid type: string, expected u32" into something an agent can act
/// on. Only top-level properties with an explicit `type` are checked;
/// anything else falls back to serde's message.
fn mistyped_field(schema: &Value, args: &Value) -> Option<String> {
    let args = args.as_object()?;
    let properties = schema.get("properties")?.as_object()?;
    for (field, value) in args {
        let Some(allowed) = properties.get(field).and_then(|p| p.get("type")) else {
            continue;
        };
        let allowed: Vec<&str> = match allowed {
            Value::String(t) => vec![t.as_str()],
            Value::Array(ts) => ts.iter().filter_map(Value::as_str).collect(),
            _ => continue,
        };
        if !allowed.iter().any(|t| json_type_matches(t, value)) {
            return Some(format!(
                "field `{field}`: expected {}, got {}",
                allowed.join(" or "),
                json_type_name(value)
            ));
        }
    }
    None
}

/// Whether `value` is an instance of the JSON Schema primitive type `ty`.
fn json_type_matches(ty: &str, value: &Value) -> bool {
    match (ty, value) {
        ("null", Value::Null)
        | ("boolean", Value::Bool(_))
        | ("number", Value::Number(_))
        | ("string", Value::String(_))
        | ("array", Value::Array(_))
        | ("object", Value::Object(_)) => true,
        ("integer", Value::Number(n)) => n.is_i64() || n.is_u64(),
        _ => false,
    }
}

/// The JSON Schema type name of `value`.
fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// A registered tool's public description, as listed to clients.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ToolInfo {
    /// `<subsystem>.<verb>`.
    pub name: String,
    /// What the tool does.
    pub description: String,
    /// JSON Schema for the input object.
    pub input_schema: Value,
}

/// The set of tools a client can call, by name.
#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
    /// [`ToolInfo`] for every tool, in registration order. Shared with the
    /// `engine.list_tools` built-in, which cannot otherwise see the
    /// registry it lives in.
    catalog: Arc<RwLock<Vec<ToolInfo>>>,
}

impl ToolRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry with the built-in tools: `engine.info` and
    /// `engine.list_tools`.
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        registry.register(TypedTool::new(
            "engine.info",
            "Reports the engine version, GPU adapter and backend, render target size, \
             fixed timestep in seconds, and entity count.",
            |engine: &mut Headless, _: NoArgs| Ok(ToolOutput::Json(engine_info(engine))),
        ));
        let catalog = Arc::clone(&registry.catalog);
        registry.register(TypedTool::new(
            "engine.list_tools",
            "Lists every registered tool with its description and JSON input schema.",
            move |_: &mut Headless, _: NoArgs| {
                let catalog = catalog
                    .read()
                    .map_err(|_| anyhow::anyhow!("tool catalog lock poisoned"))?;
                Ok(ToolOutput::Json(serde_json::to_value(&*catalog)?))
            },
        ));
        registry
    }

    /// [`Self::with_builtins`] plus every subsystem's tools: assets
    /// (`asset.load`, `asset.list`, `scene.load_gltf`, `mesh.builtin`), ECS (`entity.*`, `scene.clear`,
    /// `sim.step`), render (`mesh.list`, `camera.set`, `light.set`,
    /// `render.frame`), stats (`stats.summary`), projects
    /// (`project.new`), and scene files (`scene.save`, `scene.load`). This
    /// is the registry
    /// `shin-mcp` serves. History (`history.undo`, `history.redo`) is
    /// included too, and play mode (`sim.play`, `sim.pause`, `sim.stop`),
    /// scripts (`script.attach`, `script.eval`, `script.errors`), input
    /// actions (`input.list`, `input.set`), audio (`audio.play`,
    /// `audio.stop_all`, `entity.set_audio`), and physics (`physics.*`).
    pub fn with_engine_tools() -> Self {
        let mut registry = Self::with_builtins();
        crate::asset::register_tools(&mut registry);
        crate::ecs::register_tools(&mut registry);
        crate::render::register_tools(&mut registry);
        crate::stats::register_tools(&mut registry);
        crate::project::register_tools(&mut registry);
        crate::scene::register_tools(&mut registry);
        crate::command::register_tools(&mut registry);
        crate::headless::register_tools(&mut registry);
        crate::script::register_tools(&mut registry);
        crate::input::register_tools(&mut registry);
        crate::audio::register_tools(&mut registry);
        crate::physics::register_tools(&mut registry);
        crate::net::register_tools(&mut registry);
        registry
    }

    /// Adds `tool`.
    ///
    /// # Panics
    ///
    /// If a tool with the same name is already registered; the message
    /// names the tool.
    pub fn register(&mut self, tool: impl Tool) -> &mut Self {
        let name = tool.name();
        assert!(
            self.find(name).is_none(),
            "tool `{name}` is already registered"
        );
        let info = ToolInfo {
            name: name.to_string(),
            description: tool.description().to_string(),
            input_schema: tool.input_schema(),
        };
        self.catalog
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(info);
        self.tools.push(Box::new(tool));
        self
    }

    /// Every registered tool, in registration order.
    pub fn list(&self) -> Vec<ToolInfo> {
        self.catalog
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Calls the tool called `name` on `engine` with `args`.
    pub fn call(
        &self,
        engine: &mut Headless,
        name: &str,
        args: Value,
    ) -> Result<ToolOutput, ToolError> {
        let tool = self
            .find(name)
            .ok_or_else(|| ToolError::Unknown(name.to_string()))?;
        log::debug!("tool call `{name}` {args}");
        tool.call(engine, args)
    }

    /// What calling the tool `name` with `args` may change (see
    /// [`Tool::affected`]).
    pub fn affected(
        &self,
        engine: &Headless,
        name: &str,
        args: &Value,
    ) -> Result<Affected, ToolError> {
        let tool = self
            .find(name)
            .ok_or_else(|| ToolError::Unknown(name.to_string()))?;
        Ok(tool.affected(engine, args))
    }

    fn find(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|tool| tool.name() == name)
            .map(|tool| tool.as_ref())
    }
}

/// Arguments of a tool that takes none. Unknown fields are rejected, so a
/// misspelt argument is reported instead of silently ignored.
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

/// The `engine.info` payload.
fn engine_info(engine: &Headless) -> Value {
    let adapter = engine.gpu.adapter.get_info();
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "adapter_name": adapter.name,
        "backend": adapter.backend.to_str(),
        "target": { "width": engine.target.width, "height": engine.target.height },
        "fixed_dt": engine.fixed_dt,
        "entities": engine.world.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tool that is never called; only its name matters.
    struct Named(&'static str);

    impl Tool for Named {
        fn name(&self) -> &'static str {
            self.0
        }
        fn description(&self) -> &'static str {
            "test tool"
        }
        fn input_schema(&self) -> Value {
            json!({ "type": "object" })
        }
        fn call(&self, _: &mut Headless, _: Value) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::Text(String::new()))
        }
    }

    #[test]
    #[should_panic(expected = "tool `sim.step` is already registered")]
    fn duplicate_name_panics_naming_the_tool() {
        let mut registry = ToolRegistry::new();
        registry.register(Named("sim.step"));
        registry.register(Named("sim.step"));
    }

    #[test]
    fn list_is_in_registration_order() {
        let mut registry = ToolRegistry::new();
        registry.register(Named("b.x")).register(Named("a.y"));
        let names: Vec<_> = registry.list().into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["b.x", "a.y"]);
    }

    #[test]
    fn builtins_are_listed_without_a_gpu() {
        let names: Vec<_> = ToolRegistry::with_builtins()
            .list()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names, ["engine.info", "engine.list_tools"]);
    }

    #[test]
    fn no_args_schema_is_a_closed_object() {
        let schema = schemars::schema_for!(NoArgs).to_value();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn mistyped_field_names_the_field() {
        let schema = json!({
            "type": "object",
            "properties": {
                "steps": { "type": "integer" },
                "label": { "type": ["string", "null"] },
            },
        });
        assert_eq!(
            mistyped_field(&schema, &json!({ "label": null, "steps": "three" })).as_deref(),
            Some("field `steps`: expected integer, got string")
        );
        assert_eq!(
            mistyped_field(&schema, &json!({ "steps": 1.5 })).as_deref(),
            Some("field `steps`: expected integer, got number")
        );
        assert_eq!(
            mistyped_field(&schema, &json!({ "label": 3 })).as_deref(),
            Some("field `label`: expected string or null, got integer")
        );
        assert_eq!(mistyped_field(&schema, &json!({ "steps": 3 })), None);
        assert_eq!(mistyped_field(&schema, &json!({ "other": 3 })), None);
        assert_eq!(mistyped_field(&schema, &json!("not an object")), None);
    }
}
