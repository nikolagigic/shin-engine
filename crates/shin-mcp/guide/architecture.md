# Architecture

A tour for people reading or changing the code. The engine is one library
crate with one module per subsystem, three small binaries around it, and
a handful of decisions that everything else follows.

## Crates

```text
crates/shin-engine/   the engine library (package `shin-engine`, lib `shin`)
  src/<subsystem>.rs  one module per subsystem (see src/lib.rs)
  examples/           runnable demos (the manual smoke tests)
  tests/              integration and golden-image tests
crates/shin-mcp/      MCP server: a library (`shin_mcp::serve_stdio`) and
                      the `shin-mcp` binary
crates/shin-cli/      the `shin` command-line tool: new, edit, docs
crates/shin-editor/   the egui editor
```

The engine library never depends on tokio or rmcp; MCP lives entirely in
`shin-mcp`. Every dependency is pinned once in the workspace `Cargo.toml`
and crates use `{ workspace = true }`.

## Modules

| Module | Owns |
|--------|------|
| `app` | The window, the event loop, the fixed-timestep loop, `AppConfig`, the `Game` trait, and `EcsGame`, a ready-made game that is "a world plus systems". |
| `gpu` | `GpuContext` (instance, adapter, device, queue), the surface target, and `OffscreenTarget` with pixel readback. |
| `math` | glam re-exports, `Transform` (translation, rotation, scale), and `Camera` with its projection. Right-handed, +Y up, −Z forward, depth 0 to 1. |
| `ecs` | A `World` wrapper over hecs, a `Resources` map for singletons, a `Schedule` of plain-function systems, and the `Time` resource. |
| `asset` | glTF and image loading into CPU-side mesh and image data; `asset::registry` has `AssetSource` and the `Assets` cache that uploads each source once. |
| `render` | GPU mesh upload, the mesh and texture registries, the one lit WGSL pipeline, and instanced drawing of every `Transform` + `MeshInstance`. |
| `input` | `InputState` from winit events and the free-fly camera controller. |
| `stats` | A frame-time ring buffer and its summary. |
| `tool` | The `Tool` trait, `TypedTool`, `ToolRegistry`, `ToolOutput`, and `ToolError`. |
| `headless` | `Headless`, the window-less engine every tool runs on; `headless::play` has `PlayState` and play, pause, stop. |
| `project` | The `shin.toml` manifest, project scaffolding, `ProjectRoot`, and the `project.*` tools. |
| `scene` | `.scene.ron` files: `SceneFile`, `save`, `load`, `apply`, and the `scene.*` tools. |
| `command` | `History`: undo and redo over tool calls, and the `history.*` tools. |
| `script` | Rhai: the `Script` component, `ScriptEngine`, hot reload, `script_system`, and the `script.*` tools. |

`lib.rs` is module declarations only. Public API surface is kept small:
items are `pub(crate)` until a ticket asks for a public type, and every
public item has a doc comment (`cargo doc --no-deps` must build without
warnings).

## The world and systems

The world is a hecs ECS. Scene objects are entities with plain-struct
components: `Transform`, `MeshInstance` (mesh handle, colour, optional
texture handle), `Name`, `Script`. Systems are plain functions
`fn(&mut World, &mut Resources)`; a `Schedule` keeps two lists of them,
run in insertion order, one per fixed step and one per rendered frame.
There are no derive macros, no dependency graph, and no hidden
parallelism.

Things that exist once (time, input, the camera, the light inside the
render state, the asset cache, frame stats, the play state, the command
history, the script engine) are **resources**: values in a typed map next
to the world, looked up by type.

## The fixed-timestep loop

Simulation ticks at a fixed `dt` (1/60 s by default) and rendering runs
once per frame. `app::FixedTimestep` is an accumulator: each frame adds
the frame time, clamped to at most 0.25 s so a stall never turns into
hundreds of catch-up steps, and runs as many fixed steps as fit. The
leftover fraction of a step is published as `Time::alpha` for
interpolation. `Time` also carries `fixed_dt`, `elapsed` (fixed steps
times `dt`), `fixed_steps`, and `frame_dt`.

The same `ecs::fixed_step` advances `Time` and runs the fixed systems
whether the caller is the window loop, the editor's play clock, or the
`sim.step` tool, so a headless engine stepped sixty times and a game that
ran for a second are in the same state.

## Tools are the API

`shin::tool` holds the engine-native **tool registry**. A `Tool` has a
`<subsystem>.<verb>` name, a description, a JSON Schema for its single
input object, and a `call` on a `Headless` engine. Almost every tool is a
`TypedTool`: a typed arguments struct (its doc comments become the
schema; `deny_unknown_fields` makes typos errors) and a function.
`ToolRegistry::with_engine_tools()` registers every subsystem's tools;
that registry is what `shin-mcp`, a project under `--mcp`, the editor's
Console, and the tests call.

A tool also declares what it **affects** (`Tool::affected`): spawned
entities, named entities, or the whole scene. `command::History` uses
that to snapshot the touched state before and after a call, which is how
undo and redo work for the editor and for agents alike: undo is a log of
tool calls, and a restored entity keeps its id. Tools that affect nothing
(queries, renders, asset loads, `sim.step`) run without being recorded.
The history is frozen while playing, because stop restores the scene
anyway.

Every ticket that adds a user-facing capability registers a tool for it
in the same change, with a test through the registry. That rule is why
the [Tools reference](reference/tools.md) is complete, and why it is
generated rather than written.

## `Headless`

`Headless` is a window-less engine: a `GpuContext` without a surface, a
`World`, `Resources`, a `Schedule`, an `OffscreenTarget`, the fixed `dt`,
a clear colour, and a render callback. `step(n)` runs `n` fixed steps
with no wall clock; `render()` runs the frame systems, clears, draws, and
reads the pixels back, so results are deterministic. `open_project` and
`load_project` root the asset cache at a project, insert `ProjectRoot`,
and load the entry scene. `from_parts` wraps a world, resources, and
schedule a game built itself, which is how a project serves its own
systems over MCP. The editor runs a `Headless` on a clone of its window's
GPU context so the viewport can show the same textures.

Play mode (`headless::play`) snapshots the scene as a `SceneFile` in
memory together with the entity ids and the clock; stop applies the
snapshot back, respawning each entity under its old id.

## Rendering

All GPU work goes through wgpu, so Metal, Vulkan, DX12, and WebGPU share
one code path. Shaders are WGSL files next to the Rust that uses them
and included with `include_str!`. There is one lit pipeline; instances
of the same mesh are drawn in one call. Colours everywhere in the API
(clear colours, material tints, light colour, shader output) are linear
RGB; surfaces and offscreen targets use sRGB formats, so the hardware
encodes on write.

## Golden-image tests

Headless golden-image tests are the end-to-end tests. A test builds a
scene on a `Headless` engine, renders it to the offscreen target, reads
it back, and compares it with a committed PNG in
`crates/shin-engine/tests/golden/`. A pixel counts as different when any
channel differs by more than 2, and a test fails when more than 0.1 % of
pixels differ, which absorbs driver rounding without hiding real
changes. On failure the new, expected, and diff images are written to
`tests/golden/_diff/` (ignored by git) for inspection; running with
`UPDATE_GOLDEN=1` overwrites the reference, which is only right when the
change in output is intended.

CI runs the suite on Linux with Mesa's `lavapipe` software Vulkan driver
and on macOS with Metal.

## Design decisions in one place

These are the architecture decisions the code follows (the full records
are the project's ADRs):

1. **Rust end-to-end.** Rhai scripts are the layer for changing gameplay
   without recompiling, and they run only while playing.
2. **wgpu for all GPU work**, with WGSL shaders beside the Rust.
3. **hecs for the ECS**, with plain-function systems and a simple
   schedule; no app framework.
4. **glam for math**, used directly. `Vec3`, `Mat4`, and `Quat` are not
   wrapped.
5. **Headless golden-image tests are the end-to-end tests.** Correctness
   is checked, not eyeballed.
6. **One engine crate, one module per subsystem**, every dependency
   pinned in the workspace.
7. **Fixed timestep for simulation, variable for rendering**, with an
   interpolation alpha.
8. **An engine-native tool registry; MCP is one transport.** Every
   capability is a tool, registered in the same change that adds it.

The editor is a client of that registry (ADR 0007): it owns one
`Headless`, one `ToolRegistry`, and one `History`, and undo is the log of
the tool calls it made.
