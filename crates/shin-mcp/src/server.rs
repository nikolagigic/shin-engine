//! The rmcp [`ServerHandler`]: lists the registry's tools and forwards calls
//! to the engine thread, mapping engine types to MCP types. Prompts and
//! resources (skills and guide pages) come from [`crate::library`].

use std::sync::Arc;

use base64::Engine as _;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, GetPromptRequestParams,
    GetPromptResponse, Implementation, ListPromptsResult, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse, ServerCapabilities,
    ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::Value;
use shin::tool::{ToolError, ToolInfo, ToolOutput};

use crate::engine_thread::EngineHandle;
use crate::library;

/// MCP server exposing every tool in an `shin::tool::ToolRegistry`.
#[derive(Clone)]
pub struct EngineServer {
    engine: EngineHandle,
    /// The registry's tools as MCP tools. The registry is fixed at startup,
    /// so this is computed once.
    tools: Arc<Vec<Tool>>,
}

impl EngineServer {
    /// Creates a server that lists `tools` and runs calls on `engine`.
    pub fn new(engine: EngineHandle, tools: &[ToolInfo]) -> Self {
        Self {
            engine,
            tools: Arc::new(tools.iter().map(to_mcp_tool).collect()),
        }
    }
}

impl ServerHandler for EngineServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new(
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION"),
        ))
        .with_instructions(
            "Drives a headless instance of the engine. Call `engine.info` for \
                 its state and `engine.list_tools` for the available tools. Prompts are \
                 skills: step-by-step guides for building scenes, debugging frames, \
                 scripting, and projects. Resources are the skills (shin://skills/<name>) \
                 and the user guide (shin://guide/<page>).",
        )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tools.as_ref().clone()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let args = Value::Object(request.arguments.unwrap_or_default());
        let result = self.engine.call(request.name.into_owned(), args).await;
        Ok(to_call_result(result)?.into())
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Ok(ListPromptsResult::with_all_items(library::prompts()))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        Ok(library::get_prompt(&request.name, request.arguments.as_ref())?.into())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(library::resources()))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        Ok(library::read_resource(&request.uri)?.into())
    }
}

/// Maps a registry entry to an MCP tool definition.
fn to_mcp_tool(info: &ToolInfo) -> Tool {
    let schema = match &info.input_schema {
        Value::Object(map) => map.clone(),
        // Registry schemas are always objects; never expose a broken one.
        other => {
            log::warn!("tool `{}` has a non-object schema: {other}", info.name);
            serde_json::Map::from_iter([("type".to_string(), Value::from("object"))])
        }
    };
    Tool::new(info.name.clone(), info.description.clone(), schema)
}

/// Maps a tool's output to MCP content.
fn to_content(output: ToolOutput) -> Result<ContentBlock, ErrorData> {
    Ok(match output {
        ToolOutput::Text(text) => ContentBlock::text(text),
        ToolOutput::Json(value) => {
            let text = serde_json::to_string_pretty(&value)
                .map_err(|err| ErrorData::internal_error(err.to_string(), None))?;
            ContentBlock::text(text)
        }
        ToolOutput::Image { png, .. } => {
            ContentBlock::image(base64::prelude::BASE64_STANDARD.encode(png), "image/png")
        }
    })
}

/// Maps a registry call result to an MCP tool result, following the MCP
/// split between protocol errors and tool errors (messages preserved):
///
/// - an unknown tool or invalid arguments is a request the client should
///   not have sent: a JSON-RPC invalid-params error (-32602);
/// - a tool that ran and failed is a tool result with `isError: true`, so
///   the agent sees the failure and can react to it.
fn to_call_result(result: Result<ToolOutput, ToolError>) -> Result<CallToolResult, ErrorData> {
    match result {
        Ok(output) => Ok(CallToolResult::success(vec![to_content(output)?])),
        Err(err @ (ToolError::Unknown(_) | ToolError::InvalidArgs { .. })) => {
            Err(ErrorData::invalid_params(err.to_string(), None))
        }
        Err(ToolError::Failed(err)) => Ok(CallToolResult::error(vec![ContentBlock::text(
            format!("{err:#}"),
        )])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::ErrorCode;
    use serde_json::json;

    #[test]
    fn json_output_is_pretty_text() {
        let content = to_content(ToolOutput::Json(json!({ "entities": 0 }))).unwrap();
        let ContentBlock::Text(text) = content else {
            panic!("expected text, got {content:?}");
        };
        assert_eq!(text.text, "{\n  \"entities\": 0\n}");
    }

    #[test]
    fn image_output_is_base64_png() {
        let content = to_content(ToolOutput::Image {
            png: vec![1, 2, 3],
            width: 1,
            height: 1,
        })
        .unwrap();
        let ContentBlock::Image(image) = content else {
            panic!("expected image, got {content:?}");
        };
        assert_eq!(image.data, "AQID");
        assert_eq!(image.mime_type, "image/png");
    }

    #[test]
    fn bad_requests_are_invalid_params_errors() {
        let invalid = to_call_result(Err(ToolError::InvalidArgs {
            tool: "a.b".into(),
            message: "field `x`".into(),
        }))
        .unwrap_err();
        assert_eq!(invalid.code, ErrorCode::INVALID_PARAMS);
        assert!(invalid.message.contains("field `x`"));

        let unknown = to_call_result(Err(ToolError::Unknown("nope".into()))).unwrap_err();
        assert_eq!(unknown.code, ErrorCode::INVALID_PARAMS);
        assert!(unknown.message.contains("nope"));
    }

    #[test]
    fn tool_failure_is_an_error_result() {
        let failed = to_call_result(Err(ToolError::Failed(
            anyhow::anyhow!("inner cause").context("render failed"),
        )))
        .unwrap();
        assert_eq!(failed.is_error, Some(true));
        let ContentBlock::Text(text) = &failed.content[0] else {
            panic!("expected text, got {failed:?}");
        };
        assert_eq!(text.text, "render failed: inner cause");
    }

    #[test]
    fn success_is_not_an_error_result() {
        let ok = to_call_result(Ok(ToolOutput::Text("hi".into()))).unwrap();
        assert_eq!(ok.is_error, Some(false));
    }
}
