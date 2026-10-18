//! `shin_mcp`: serves a [`ToolRegistry`] over MCP on stdio (GE-014).
//!
//! The `shin-mcp` binary calls [`serve_stdio`] with the engine's tools. A
//! game can call it too (GE-027) with its own tools added to the registry
//! and a [`Headless`] engine built from its own world, resources, and
//! systems ([`Headless::from_parts`]), so an agent drives the game itself:
//!
//! ```no_run
//! use shin::ecs::{Resources, Schedule, World};
//! use shin::headless::Headless;
//! use shin::tool::ToolRegistry;
//!
//! fn main() -> anyhow::Result<()> {
//!     let registry = ToolRegistry::with_engine_tools();
//!     shin_mcp::serve_stdio(registry, || {
//!         Headless::from_parts(World::new(), Resources::new(), Schedule::new(), (640, 360))
//!     })
//! }
//! ```
//!
//! Besides tools, the server offers the engine's agent skills
//! ([`shin::project::skills`]) as MCP prompts, and the skills and the user
//! guide's pages as resources (`shin://skills/<name>`,
//! `shin://guide/<page>`), so a game's own server has them too.
//!
//! Every `tools/call` runs through [`shin::command::apply`], so edits made
//! over MCP are recorded in the engine's history and `history.undo` /
//! `history.redo` work for them.
//!
//! stdout carries only JSON-RPC, so the caller must send logs to stderr
//! (`env_logger`'s default target).

mod engine_thread;
mod library;
mod server;

use anyhow::Context as _;
use rmcp::ServiceExt as _;
use shin::headless::Headless;
use shin::tool::ToolRegistry;

use crate::engine_thread::EngineHandle;
use crate::server::EngineServer;

/// Serves every tool in `registry` over MCP on stdin/stdout until the
/// client disconnects.
///
/// The engine is created by `make_engine` on a dedicated engine thread,
/// where it stays (`Headless` is not `Send`); tool calls run there one at
/// a time. Fails if `make_engine` fails, before anything is read from
/// stdin, or if the MCP handshake or transport fails.
pub fn serve_stdio(
    registry: ToolRegistry,
    make_engine: impl FnOnce() -> anyhow::Result<Headless> + Send + 'static,
) -> anyhow::Result<()> {
    let tools = registry.list();
    let engine = EngineHandle::spawn(registry, make_engine)?;
    log::info!(
        "shin-mcp: headless engine ready, {} tools, serving on stdio",
        tools.len()
    );

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to start the tokio runtime")?;
    runtime.block_on(async {
        let service = EngineServer::new(engine, &tools)
            .serve(rmcp::transport::stdio())
            .await
            .context("MCP handshake failed")?;
        let reason = service.waiting().await?;
        log::info!("shin-mcp: stopped ({reason:?})");
        anyhow::Ok(())
    })
}
