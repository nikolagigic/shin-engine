# Projects

A Shin project is a normal Cargo package that depends on `shin-engine`,
plus a `shin.toml` manifest that the engine, the editor, and the tools
read. `shin new <name>` writes one; the `project.new` tool is the same
operation for agents.

```text
shin new <name> [--dir <path>] [--engine-path <path>]
```

The project is created in `<dir>/<name>` (the current directory by
default), which must not exist or must be empty. The name must be usable
as a Cargo package name: ASCII letters, digits, `-` and `_`, not starting
with a digit.

## Layout

`shin new mygame` writes these files, in this order:

```text
mygame/
  Cargo.toml              the package; depends on shin-engine and shin-mcp
  shin.toml               project name, window settings, and entry scene
  src/main.rs             the game: systems, tools, window and --mcp modes
  scenes/main.scene.ron   the starter scene (the entry scene)
  assets/cube.gltf        the cube mesh the starter scene draws
  assets/cube.bin         its vertex buffer
  .mcp.json               runs the game with --mcp for agents
  .gitignore              ignores /target
  README.md               how to run, drive, and extend the game
  CLAUDE.md               project notes for agents
  .claude/skills/*/SKILL.md  the agent skills, one folder each
```

Relative asset and scene paths are resolved against the project
directory: `assets/cube.gltf` in a scene file means
`mygame/assets/cube.gltf`. With a project open, the `scene.save` and
`scene.load` tools reject paths that leave the project directory.

## `shin.toml`

The manifest has two tables. This is the one `shin new mygame` writes:

```toml
[project]
name = "mygame"
entry_scene = "scenes/main.scene.ron"

[window]
title = "mygame"
width = 1280
height = 720
```

| Table       | Field         | Meaning |
|-------------|---------------|---------|
| `[project]` | `name`        | The project (and Cargo package) name. |
| `[project]` | `entry_scene` | Optional. The scene loaded at start-up, relative to the project root. The game loads it on its first frame, `shin-editor` and `shin-mcp --project` load it when they open the project, and the editor's File > Open and Save use it as their default path. Without it the game and the editor start with an empty world. |
| `[window]`  | `title`       | The window title. The editor shows it too, with a `*` while the scene has unsaved edits. |
| `[window]`  | `width`       | Initial window width in logical pixels. Also the frame size `shin-mcp --project` and `--mcp` render at. |
| `[window]`  | `height`      | Initial window height in logical pixels. |

Unknown fields are an error naming the file. The `project.info` tool
returns these fields for the open project, re-reading the file each time.

## The engine dependency

The generated `Cargo.toml` depends on `shin-engine` (the engine) and
`shin-mcp` (so `cargo run -- --mcp` can serve the game to agents), from
the same place:

- By default, the engine's git repository:

  ```toml
  shin-engine = { git = "https://github.com/nikolagigic/shin-engine" }
  shin-mcp = { git = "https://github.com/nikolagigic/shin-engine" }
  ```

- With `--engine-path <checkout>/crates/shin-engine`, a local checkout.
  The path is made absolute, and `shin-mcp` is taken from the sibling
  `crates/shin-mcp` directory:

  ```toml
  shin-engine = { path = "/home/you/shin-engine/crates/shin-engine" }
  shin-mcp = { path = "/home/you/shin-engine/crates/shin-mcp" }
  ```

The manifest also pins `wgpu`, `anyhow`, `env_logger`, `log`, `serde`, and
`schemars` at the versions the engine uses, sets `opt-level = 1` for the
project and `3` for dependencies so debug builds run smoothly, and
declares an empty `[workspace]` so the project stays standalone even when
created inside another Cargo workspace.

## The starter game

`src/main.rs` is a complete game that reads its window settings from
`shin.toml` at run time, so it needs no substitution. It has two modes:

```text
cargo run            play in a window
cargo run -- --mcp   serve this game to an agent over MCP (stdio)
```

Both modes build the same world, systems, and resources: an `orbit`
system that moves the entity named `moon` on a circle around the cube, a
camera with a 60° field of view (the scene file says where it is), and one
project-defined tool, `game.hello`. Whatever an agent sees under `--mcp`
is the game itself. [Agents and MCP](agents-mcp.md) covers that mode and
how to add tools; the full file is below.

<details>
<summary>The generated <code>src/main.rs</code></summary>

```rust,noplayground
{{#include ../../crates/shin-engine/src/project/template/main.rs}}
```

</details>

The starter scene, `scenes/main.scene.ron`, is a ground slab (the cube
scaled to 20 × 0.5 × 20 so its top face is at y = 0), an orange cube
resting on it and turned 30° about Y, and a small `moon` beside the cube
at (1.6, 0.5, 0), where the orbit system puts it at time zero. The camera
sits at (0, 2, 5) looking at (0, 0.5, 0), and a warm directional light
comes from above and in front. All three entities draw
`assets/cube.gltf`. [Scenes](scenes.md) explains the format.

## Files for agents

`shin new` writes two things for AI agents, in addition to `.mcp.json`:

- **`CLAUDE.md`**, about 40 lines that Claude Code reads at the start of
  every session in the folder: what the project is, how to run it, the
  `shin` MCP server from `.mcp.json`, where scenes, scripts, and assets
  live, and how to add a game tool, with a link to this guide.
- **`.claude/skills/<name>/SKILL.md`** for each of the
  [agent skills](agents-skills.md): `shin-scene-building`,
  `shin-visual-debugging`, `shin-scripting`, and `shin-project`. Claude
  Code discovers project skills in that folder and uses one when a task
  matches its description. Other MCP clients get the same skills as
  prompts from the server.

They are plain files: edit them to describe your game, or add skills of
your own next to them. A newer engine does not rewrite them; compare with
the engine's `skills/` folder when you upgrade.

## Project tools

| Tool           | What it does |
|----------------|--------------|
| `project.new`  | Creates a project, exactly as `shin new` does, and returns the files written. |
| `project.open` | Opens a project on a running engine: replaces every entity, roots asset and scene paths at the project, resizes the render target to the window size, and loads the entry scene. |
| `project.info` | Describes the open project: name, directory, entry scene, and window settings. |

See the [Tools reference](reference/tools.md) for their arguments.
