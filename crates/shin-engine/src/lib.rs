//! Shin: a small, fast, cross-platform 3D engine that humans, scripts, and AI
//! agents drive through the same API.
//!
//! Module map (one subsystem per module; tickets map to modules so parallel
//! work rarely touches the same file):
//!
//! - [`app`]    — window, event loop, fixed-timestep game loop    (GE-001)
//! - [`gpu`]    — wgpu device/queue/surface context, offscreen targets (GE-001, GE-002)
//! - [`math`]   — glam re-exports, `Transform`, `Camera`          (GE-003)
//! - [`ecs`]    — hecs `World` wrapper, systems schedule, `Time`  (GE-004)
//! - [`asset`]  — glTF + texture loading into CPU-side structs    (GE-005)
//! - [`render`] — mesh upload, materials, shaders, instancing     (GE-006, GE-008)
//! - [`input`]  — keyboard/mouse state, camera controller, actions (GE-007, GE-033)
//! - [`stats`]  — frame-time statistics                           (GE-009)
//! - [`tool`]   — engine-native tool registry (what MCP exposes)  (GE-013)
//! - [`headless`] — window-less engine handle for tools and tests (GE-013)
//! - [`project`] — `shin.toml` manifest and new-project scaffolding (GE-018)
//! - [`scene`]  — `.scene.ron` scene files, `scene.save`/`scene.load` (GE-021)
//! - [`command`] — undo/redo history over tool calls, `history.*` (GE-022)
//! - [`script`] — Rhai gameplay scripts, `script.*`             (GE-011)
//! - [`physics`] — rapier3d rigid bodies and colliders, `physics.*` (GE-034)
//!
//! The MCP server lives in the separate `shin-mcp` crate (GE-014) and is
//! a thin transport over `tool::ToolRegistry` (GE-013). The engine lib never
//! depends on tokio or rmcp.
//!
//! Keep this file to module declarations and re-exports only.

pub mod app;
pub mod asset;
pub mod command;
pub mod ecs;
pub mod gpu;
pub mod headless;
pub mod input;
pub mod math;
pub mod physics;
pub mod project;
pub mod render;
pub mod scene;
pub mod script;
pub mod stats;
pub mod tool;
