# Changelog

All notable changes to Shin are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions
follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Shin is
pre-alpha: while the major version is 0, any minor release may contain
breaking changes. All crates (`shin-engine`, `shin-mcp`, `shin-cli`,
`shin-editor`) share one version.

The release workflow publishes the section of a version, from its heading
to the next, as that version's GitHub Release notes.

## [Unreleased]

## [0.1.0] - 2026-10-18

The first published release: everything from milestones 1 to 3.

### Added

#### Engine core (milestone 1)

- A wgpu renderer (Metal, Vulkan, DX12) with a lit, instanced mesh
  pipeline drawing straight from the ECS; a winit window with a
  fixed-timestep simulation loop and variable-rate rendering.
- glam math (`Transform`, `Camera`), a hecs-based ECS with plain-function
  systems, a `Schedule`, and typed `Resources`.
- glTF mesh and image loading; keyboard and mouse input with a free-fly
  camera; frame-time statistics.
- Headless offscreen rendering with read-back, and golden-image tests that
  check rendered pixels against reference PNGs on every CI run.
- An engine-native tool registry: every capability is a named,
  JSON-schema'd tool (`<subsystem>.<verb>`) on a headless engine, covering
  entities, meshes, the camera, the light, the simulation, and rendering.
- `shin-mcp`, an MCP server over stdio that exposes the tool registry to
  AI agents, including rendered frames as images.
- The `demo` example: a field of cubes and spheres around a pedestal with
  orbiting satellites.

#### Editor and projects (milestone 2)

- `shin new` and the `project.new` tool: a runnable game project with a
  `shin.toml` manifest, a starter scene, and a `--mcp` mode in which the
  game serves itself to agents with its own systems and tools.
- An asset registry addressed by path, and base-colour textures.
- Scene files in RON (`.scene.ron`) with `scene.save` and `scene.load`.
- The egui editor (`shin-editor`, `shin edit`): docked scene tree,
  inspector, console, history, and log panels around a live viewport;
  open and save; play and stop.
- Undo and redo as a history of tool calls, shared by the editor and MCP
  clients (`history.undo`, `history.redo`).
- Rhai scripting with hot reload: a `Script` component and `script.*`
  tools; scripts run only while playing.
- A project-aware MCP server (`shin-mcp --project`, `project.open`,
  `project.info`) and a generated `.mcp.json`.
- The user guide, an mdBook with a tools reference generated from the
  registry and checked by a test.

#### Gameplay and polish (milestone 3)

- Scene hierarchy: parents, world transforms, nested scene files, and a
  tree view in the editor.
- An asset browser in the editor, `asset.list`, and `entity.set_texture`.
- Input actions bound to keyboard, mouse, and gamepad in `shin.toml`,
  readable from scripts and settable with `input.set`.
- Physics with rapier3d: rigid bodies, colliders, ray casts, an editor
  collider overlay, and a stack of crates in the starter scene.
- Viewport picking and transform gizmos in the editor.
- Audio with kira: sound sources, one-shot and spatial playback, from
  scripts and tools, with a silent fallback when there is no audio device;
  audio previews in the asset browser.
- Agent skills: four how-to guides served as MCP prompts and resources
  alongside the user guide, and written into every new project with a
  project `CLAUDE.md`.

#### Release (GE-037)

- Crates published to crates.io; new projects depend on the published
  `shin-engine` by default (`--engine-path` for a local checkout).
- A release workflow that publishes the crates and attaches prebuilt
  `shin` and `shin-editor` binaries for macOS (arm64) and Linux (x86_64).

[Unreleased]: https://github.com/nikolagigic/shin-engine/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/nikolagigic/shin-engine/releases/tag/v0.1.0
