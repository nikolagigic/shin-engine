//! Network tools: `net.status`. GE-046 adds hosting, joining, and the
//! session numbers; until then the engine is never networked.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::headless::Headless;
use crate::tool::{ToolOutput, ToolRegistry, TypedTool};

/// Registers `net.status`.
pub fn register_tools(reg: &mut ToolRegistry) {
    reg.register(TypedTool::new(
        "net.status",
        "Reports this engine's multiplayer role: { role: \"none\" } when it is \
         neither hosting nor joined to a session (the only role until hosting \
         and joining land).",
        net_status,
    ));
}

/// Arguments of `net.status`: none.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

fn net_status(_: &mut Headless, _: NoArgs) -> anyhow::Result<ToolOutput> {
    Ok(ToolOutput::Json(json!({ "role": "none" })))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::headless::Headless;
    use crate::tool::{ToolOutput, ToolRegistry};

    #[test]
    fn net_status_reports_no_role() -> anyhow::Result<()> {
        let reg = ToolRegistry::with_engine_tools();
        assert!(reg.list().iter().any(|tool| tool.name == "net.status"));
        let mut engine = Headless::new(8, 8)?;
        let ToolOutput::Json(status) = reg.call(&mut engine, "net.status", json!({}))? else {
            panic!("expected JSON");
        };
        assert_eq!(status, json!({ "role": "none" }));
        assert!(
            reg.call(&mut engine, "net.status", json!({ "x": 1 }))
                .is_err()
        );
        Ok(())
    }
}
