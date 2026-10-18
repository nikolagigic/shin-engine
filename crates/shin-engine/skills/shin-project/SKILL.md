---
name: shin-project
description: Create, open, run, and extend a Shin game project - scaffold it, understand its layout, run it in a window or serve it to agents with --mcp, and add a project-specific tool. Use when starting a Shin game, working out where files belong, or adding a game.* tool.
---

# Working with a Shin project

## Goal

Get a Shin game project to a known, runnable state, and extend it the
way the engine expects: scenes and scripts as data files, game logic and
agent tools in `src/main.rs`.

## When to use

- Starting a new game, or opening an existing one in a running engine.
- You need to know where scenes, scripts, and assets go.
- You want the game itself to offer an agent a new tool.

## Create a project

From a shell: `shin new mygame` (options: `--dir <parent>`,
`--engine-path <checkout>/crates/shin-engine` for a local engine).

From an agent: `project.new` with
`{"name": "mygame", "dir": "/path/to/parent"}` (optionally
`"engine_path"`). It returns the list of files written. The name must be
ASCII letters, digits, `-` and `_`, not starting with a digit, and the
target folder must be missing or empty.

## Layout

```text
shin.toml              project name, window title and size, entry scene
Cargo.toml             depends on shin-engine and shin-mcp
src/main.rs            the game: systems, game tools, window and --mcp modes
scenes/main.scene.ron  the entry scene, loaded on start
scripts/               Rhai scripts: player.rhai (the cube), moon.rhai
assets/                glTF meshes, textures, and sounds/blip.wav
.mcp.json              runs the game with --mcp for MCP clients
CLAUDE.md              project notes for agents
.claude/skills/        these skills
```

Relative asset, scene, and script paths resolve against the project
root. With a project open, `scene.save` and `scene.load` reject paths
that leave it.

## Open a project in a running engine

`project.open` with `{"dir": "/path/to/mygame"}` replaces the world,
roots paths at the project, sizes frames to the window in `shin.toml`,
and loads the entry scene. `project.info` (no arguments) re-reads
`shin.toml` and returns `name`, `dir`, `entry_scene`, and `window`.

## Run it

- `cargo run`: plays the game in a window (WASD + right mouse to fly).
- `cargo run -- --mcp`: no window; serves the game over MCP on stdio with
  its own systems and tools and the project open. `.mcp.json` does this
  for Claude Code; the first start compiles the game (about a minute).
- `shin edit .`: opens the project in the editor.
- `shin-mcp --project .`: the engine's own server on this project,
  without the game's systems or tools.

Call `engine.info` and `engine.list_tools` to see what the connected
server offers; a game under `--mcp` lists its `game.*` tools too.

## Add a project tool

Game tools live in `register_game_tools` in `src/main.rs`; the starter
registers `game.hello`, which returns the game's name. Add another
`TypedTool` there:

```rust
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

- Name it `<subsystem>.<verb>`; game tools use `game.`.
- The description is what an agent reads to decide to call it: say what
  it does and what it returns in one or two sentences.
- Doc comments on argument fields become the JSON schema.
  `deny_unknown_fields` turns a misspelt argument into an error.
- A tool that edits entities should declare what it touches with
  `with_affected`, so `history.undo` can revert it; queries need nothing.
- Systems added to `game_schedule` run under `--mcp` too, so `sim.step`
  advances the real game.

## Verification

- New project: `cargo run -- --mcp` starts, `engine.info` answers, and
  `render.frame` shows the starter cube, ground, and moon.
- New tool: restart the server (the game recompiles), confirm it in
  `engine.list_tools`, call it, and check the result.

## Pitfalls

- The MCP server is the compiled game: after editing `src/main.rs`,
  restart it (in Claude Code, reconnect the `shin` server) to see changes.
- stdout carries only JSON-RPC under `--mcp`; log with the `log` macros
  (they go to stderr), never `println!`.
- Scene and script files are data: edit them freely; the game and the
  editor read them by path.
- See the online guide for more: https://nikolagigic.github.io/shin-engine/
