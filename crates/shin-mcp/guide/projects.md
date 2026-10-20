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
  Cargo.toml              the package; depends on shin-engine, shin-mcp, and shin-net
  shin.toml               project name, window settings, and entry scene
  src/main.rs             the game: systems, tools, window, --mcp, --serve, --host, --join
  scenes/main.scene.ron   the starter scene (the entry scene)
  scripts/player.rhai     puts a player's cube back when it falls off the ground
  scripts/server.rhai     gives every player who joins a cube of their own
  assets/cube.gltf        the cube mesh the starter scene draws
  assets/cube.bin         its vertex buffer
  assets/sounds/blip.wav  a 0.2 s blip the moon plays once per revolution
  scripts/moon.rhai       the moon's script, which plays the blip
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

The manifest has two required tables and an optional `[input]` table.
This is the one `shin new mygame` writes:

```toml
[project]
name = "mygame"
entry_scene = "scenes/main.scene.ron"

[window]
title = "mygame"
width = 1280
height = 720

[input.actions]
jump = ["Space", "Gamepad:South"]
move_x = { axis = ["ArrowLeft", "ArrowRight"], gamepad_axis = "LeftStickX" }
move_z = { axis = ["ArrowDown", "ArrowUp"], gamepad_axis = "LeftStickY" }
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

## Input actions

Games read named actions, not keys. `[input.actions]` names each action
and says what drives it. Scripts read them with `action_pressed`,
`action_just_pressed`, and `action_axis` (see
[Scripting](scripting.md#input-actions)). Rust systems read the `Actions`
resource the same way.

A **button action** is an array of bindings. The action is held while
any of them is:

```toml
jump = ["Space", "Gamepad:South"]
fire = ["Mouse:Left", "KeyF"]
```

An **axis action** is a table with a value from -1 to 1. `axis` names a
negative and a positive binding. `gamepad_axis` names a stick or trigger.
Either may be left out. When both are present, whichever is further from
zero wins:

```toml
move_x = { axis = ["KeyA", "KeyD"], gamepad_axis = "LeftStickX" }
```

| Binding | Written as |
|---------|------------|
| A key, by physical position | Its winit `KeyCode` name: `Space`, `KeyA`, `ArrowUp`, `Digit1`, `ShiftLeft`, `Enter`, `F1` |
| A mouse button | `Mouse:Left`, `Mouse:Right`, `Mouse:Middle` |
| A gamepad button, on any pad | `Gamepad:` and one of `South`, `East`, `North`, `West`, `C`, `Z`, `LeftTrigger`, `LeftTrigger2`, `RightTrigger`, `RightTrigger2`, `Select`, `Start`, `Mode`, `LeftThumb`, `RightThumb`, `DPadUp`, `DPadDown`, `DPadLeft`, `DPadRight` |
| A gamepad axis (`gamepad_axis`) | `LeftStickX`, `LeftStickY`, `LeftZ`, `RightStickX`, `RightStickY`, `RightZ`, `DPadX`, `DPadY` |

`South` is A on an Xbox pad and Cross on a PlayStation pad. Stick Y is
positive when pushed up. An unknown name is an error that names it, such
as ``unknown key `Spce` ``.

An axis action counts as pressed from 0.5 either way. A button action's
axis is 1 while held and 0 otherwise. `just_pressed` is true from the
moment an action goes down until one simulation step has run. Each press
is therefore seen by exactly one step, even a tap shorter than a frame.

The windowed game polls keyboard, mouse, and gamepads every frame.
Gamepads are found through gilrs and may be plugged in while the game
runs. Under `--mcp` and in `shin-mcp --project` there are no devices. The
`input.set` tool drives an action instead, and `input.list` shows every
action with its binding and value:

```text
input.set {"action": "move_x", "axis": 1.0}
input.set {"action": "jump", "pressed": true}
```

An injected value stays until the next `input.set`. Rebinding from the
editor is not supported yet; edit `shin.toml` and reopen the project.

## The engine dependency

The generated `Cargo.toml` depends on `shin-engine` (the engine),
`shin-mcp` (so `cargo run -- --mcp` can serve the game to agents), and
`shin-net` (the QUIC transport that `--serve` and `--host` listen on), all
from the same place:

- By default, crates.io, at the version of the `shin` that created the
  project (Cargo reads `"0.1.0"` as "0.1.0 or any later 0.1.x"):

  ```toml
  shin-engine = "0.1.0"
  shin-mcp = "0.1.0"
  shin-net = "0.1.0"
  ```

- With `--engine-path <checkout>/crates/shin-engine`, a local checkout.
  The path is made absolute, and `shin-mcp` and `shin-net` are taken
  from the sibling `crates/shin-mcp` and `crates/shin-net` directories:

  ```toml
  shin-engine = { path = "/home/you/shin-engine/crates/shin-engine" }
  shin-mcp = { path = "/home/you/shin-engine/crates/shin-mcp" }
  shin-net = { path = "/home/you/shin-engine/crates/shin-net" }
  ```

The manifest also pins `wgpu`, `anyhow`, `env_logger`, `log`, `serde`, and
`schemars` at the versions the engine uses, sets `opt-level = 1` for the
project and `3` for dependencies so debug builds run smoothly, and
declares an empty `[workspace]` so the project stays standalone even when
created inside another Cargo workspace.

## The starter game

`src/main.rs` is a complete game that reads its window settings from
`shin.toml` at run time, so it needs no substitution. It has five modes:

```text
cargo run                         play in a window
cargo run -- --mcp                serve this game to an agent over MCP (stdio)
cargo run -- --serve [--port N]   a dedicated multiplayer server, no window
cargo run -- --host [--port N]    play in a window and host a multiplayer game
cargo run -- --join <host:port> (--insecure | --cert <path>)
                                  play in a window as a client of a server
```

`shin serve <dir> [--port N]` runs `cargo run -- --serve` in the project
for you. The two server modes are covered in
[Multiplayer](multiplayer.md#hosting), and joining in
[Multiplayer](multiplayer.md#joining).

Every mode builds the same world, systems, and resources: an `orbit`
system that moves the entity named `moon` on a circle around the cube,
the engine's script, physics, and audio systems (so the moon's script
runs, the crates fall by [physics](physics.md), and the blip fades with
distance), a camera with a 60° field of view (the scene file says where
it is), and one
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
at (1.6, 0.5, 0), where the orbit system puts it at time zero. To the
left, five 0.4 m crates (`crate 1` to `crate 5`) hang a little apart above
the ground; each has a dynamic rigid body and a box collider, and the
ground has a fixed one, so they drop into a stack when the simulation
runs. The camera sits at (0, 2, 5) looking at (0, 0.5, 0), and a warm
directional light comes from above and in front. Every entity draws
`assets/cube.gltf`. The moon also carries `assets/sounds/blip.wav` as a
spatial audio source and runs `scripts/moon.rhai`, which plays the blip
each time the moon completes a revolution; see [Audio](audio.md).
[Scenes](scenes.md) explains the format.

When play starts you join the game as player 1, even alone, and
`scripts/server.rhai` gives you a cube of your own in front of the
centre cube (every other player who joins gets one too; see
[Multiplayer](multiplayer.md#players-and-input)). It walks with a
mover: the arrow keys or the left stick slide it over the ground
(`move_x`, `move_z`), and Space or the gamepad's south button makes it
jump (`jump`). Walk off the edge and `scripts/player.rhai` puts it back.
WASD stays on the fly camera.

## Files for agents

`shin new` writes two things for AI agents, in addition to `.mcp.json`:

- **`CLAUDE.md`**, about 40 lines that Claude Code reads at the start of
  every session in the folder: what the project is, how to run it, the
  `shin` MCP server from `.mcp.json`, where scenes, scripts, and assets
  live, and how to add a game tool, with a link to this guide.
- **`.claude/skills/<name>/SKILL.md`** for each of the
  [agent skills](agents-skills.md): `shin-scene-building`,
  `shin-visual-debugging`, `shin-scripting`, `shin-project`, and
  `shin-multiplayer`. Claude Code discovers project skills in that folder and uses one when a task
  matches its description. Other MCP clients get the same skills as
  prompts from the server.

They are plain files: edit them to describe your game, or add skills of
your own next to them. A newer engine does not rewrite them; compare with
the engine's `crates/shin-engine/skills/` folder when you upgrade.

## Project tools

| Tool           | What it does |
|----------------|--------------|
| `project.new`  | Creates a project, exactly as `shin new` does, and returns the files written. |
| `project.open` | Opens a project on a running engine: replaces every entity, roots asset and scene paths at the project, resizes the render target to the window size, and loads the entry scene. |
| `project.info` | Describes the open project: name, directory, entry scene, and window settings. |

See the [Tools reference](reference/tools.md) for their arguments.
