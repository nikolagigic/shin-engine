# Agents and MCP

Every capability of the engine is a named, schema-described tool. The
[Model Context Protocol](https://modelcontextprotocol.io) (MCP) is how an
AI agent such as Claude Code gets at those tools: an MCP server runs one
headless engine and exposes every tool in the registry over stdio. New
engine tools appear automatically, and a project can add its own.

There are two servers, and they serve the same tools:

- **`shin-mcp`**, the engine's server, for an engine with no game around
  it (or a project opened with `--project`).
- **Your game with `--mcp`**: the starter `src/main.rs` serves itself,
  with the game's own systems running and the game's own tools added.
  Whatever the agent sees is the game itself.

## Serving your project

`shin new` writes a `.mcp.json` in the project:

```json
{
  "mcpServers": {
    "shin": {
      "command": "cargo",
      "args": ["run", "--quiet", "--", "--mcp"]
    }
  }
}
```

Open the project folder in Claude Code and the game is started with
`--mcp` for you. The first start compiles the game, which can take a
minute. To run it by hand:

```sh
cargo run -- --mcp
```

With `--mcp` the game opens no window. It serves itself over MCP on
stdin and stdout (logs go to stderr), with the same world, systems, and
resources as the windowed game, the project open, and its entry scene
loaded. Frames render at the window size from `shin.toml`. An agent can
step the simulation and watch the moon move, inspect and edit the scene,
render frames, save scenes, and undo its edits, all running your game's
own code.

## Serving the engine alone

`shin-mcp` runs an engine without a game:

```text
shin-mcp [--project DIR] [--width W] [--height H] [--log-level L]
```

Without `--project` the engine starts empty, with a camera at (0, 2, 5)
looking at the origin, a white directional light, and a 512 × 288 render
target. With `--project DIR` it opens the project in `DIR`: assets and
scene paths are rooted there, the entry scene is loaded, and frames
default to the project's window size. `--width` and `--height` override
the frame size. `--log-level` takes an `env_logger` filter (`debug`,
`shin=trace`) and overrides `RUST_LOG`; logs go to stderr, and stdout
carries only JSON-RPC.

Register it with Claude Code from the engine checkout, either with the
command line:

```sh
claude mcp add shin -- cargo run -q -p shin-mcp
```

or by copying the repository's `.mcp.json.example` to `.mcp.json`:

```json
{{#include ../../.mcp.json.example}}
```

Then ask the agent to call `engine.info`.

## A sample session

The calls below were made against `shin-mcp` by piping JSON-RPC into it,
and the results are what it returned, with long floating-point numbers
shortened. An agent sees the same results, with the rendered frame
delivered as an image.

Upload the built-in cube and spawn a red one named `crate`, half a unit
up:

```text
mesh.builtin  {"kind": "cube"}
-> {"handle": 0}

entity.spawn  {"mesh": 0, "name": "crate", "translation": [0, 0.5, 0], "color": [0.8, 0.1, 0.1, 1]}
-> {"entity": 4294967296}
```

Run one second of simulation (60 fixed steps) and look at the result:

```text
sim.step  {"steps": 60}
-> {"alpha": 0.0, "elapsed": 1.0, "fixed_dt": 0.0167, "fixed_steps": 60, "frame_dt": 0.0}

render.frame  {"width": 256, "height": 144}
-> a 256 x 144 PNG
```

Undo the spawn, and confirm the world is empty again:

```text
history.undo  {}
-> {"ok": true, "remaining": 0}

entity.list  {}
-> []
```

Every `tools/call` runs through the engine's command history, so
`history.undo` and `history.redo` work for an agent's edits exactly as
Ctrl+Z does in the editor. Vectors are JSON arrays, colours are linear
RGBA, rotations are Euler angles in degrees in X, Y, Z order, and entity
ids are the numbers `entity.spawn` and `entity.list` return. The
[Tools reference](reference/tools.md) has every tool and argument, and
`engine.list_tools` returns the same information, with the JSON schemas,
at run time.

To try the protocol by hand without an agent:

```sh
cargo run -q -p shin-mcp <<'EOF'
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"me","version":"0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"engine.info","arguments":{}}}
EOF
```

## Prompts and resources

Besides tools, both servers offer the engine's [agent skills](agents-skills.md)
and this guide, so an agent can learn how to use the tools from the
server itself.

- **Prompts.** `prompts/list` returns one prompt per skill
  (`shin-scene-building`, `shin-visual-debugging`, `shin-scripting`,
  `shin-project`) with its description and one optional argument, `goal`.
  `prompts/get` returns the skill's Markdown as a single user message,
  followed by a `## Your goal` section when `goal` is given. Claude Code
  shows MCP prompts as slash commands, e.g.
  `/mcp__shin__shin-scene-building a red crate on a green floor`.
- **Resources.** `resources/list` returns every skill as
  `shin://skills/<name>` and every page of this guide as
  `shin://guide/<page>`, where `<page>` is the page's path without `.md`:
  `shin://guide/scripting`, `shin://guide/reference/tools`.
  `resources/read` returns the Markdown (`text/markdown`). Guide pages are
  the book's sources, so `\{{#include}}` lines appear unexpanded.

```text
prompts/get     {"name": "shin-scripting", "arguments": {"goal": "make the moon spin"}}
-> one user message: the shin-scripting skill, then "## Your goal\n\nmake the moon spin"

resources/read  {"uri": "shin://guide/agents-mcp"}
-> {"contents": [{"uri": "shin://guide/agents-mcp", "mimeType": "text/markdown", "text": "# Agents and MCP ..."}]}
```

An unknown prompt is an invalid-params error and an unknown resource a
resource-not-found error, both naming what was asked for.

## Adding a project tool

Agents get the engine's tools plus the game's own. The starter
`src/main.rs` registers one example, `game.hello`, which returns the
game's name:

```rust,noplayground
{{#include ../../crates/shin-engine/src/project/template/main.rs:97:112}}
```

To add yours, register another `TypedTool` in `register_game_tools`: a
name of the form `<subsystem>.<verb>`, a one- or two-sentence description
for agents, an arguments struct whose doc comments become the JSON schema
the agent reads, and a function of the engine. This example, from the
README that `shin new` writes, lets an agent change the simulation speed:

```rust,noplayground
/// Arguments of `game.set_speed`.
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SpeedArgs {
    /// Simulation speed: 1.0 is normal, 2.0 is twice as fast.
    factor: f64,
}

registry.register(TypedTool::new(
    "game.set_speed",
    "Sets how much simulated time each step covers, relative to normal.",
    |engine: &mut Headless, args: SpeedArgs| {
        engine.fixed_dt = args.factor / 60.0;
        Ok(ToolOutput::Text(format!("speed {}", args.factor)))
    },
));
```

`deny_unknown_fields` makes a misspelt argument an error instead of
silently ignored. Systems you add to `game_schedule` run under `--mcp`
too, so `sim.step` advances your game, not just the engine.

If a tool edits entities, the engine's undo needs to know what it
touches: give the `TypedTool` a `with_affected` closure, as the engine's
own editing tools do. Tools without one run without being recorded, which
is right for queries and renders.
