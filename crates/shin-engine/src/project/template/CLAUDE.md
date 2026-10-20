# {name}

A game made with the Shin engine: a Cargo package whose `src/main.rs` is
the game, plus data files (scenes, scripts, assets) that the engine reads
by path. Shin is driven the same way by people, scripts, and agents: every
capability is a named tool.

## Run

- `cargo run`: play in a window (WASD to fly, hold the right mouse button
  to look, Escape quits).
- `cargo run -- --mcp`: no window; serve this game over MCP on stdio, with
  its own systems and tools and this project open. Logs go to stderr.
- `cargo run -- --serve [--port 7777]`: a dedicated multiplayer server
  (no window) running the game's systems; Ctrl-C stops it. `--host`
  instead plays in a window and serves from it. `shin serve .` runs
  `--serve`. `--join <host:port>` plays in a window as a client of a
  server (it exits with a message until the network transport exists).
- `shin edit .`: open the project in the Shin editor.

## Agents

`.mcp.json` registers an MCP server named `shin` that runs
`cargo run --quiet -- --mcp`; the first start compiles the game. It
offers the engine's tools (`engine.list_tools` lists them) plus this
game's `game.*` tools. Edits made through tools are undoable with
`history.undo`. Check visual work with `render.frame`.

Skills for common tasks are in `.claude/skills/`: `shin-scene-building`,
`shin-visual-debugging`, `shin-scripting`, and `shin-project`.

## Where things live

- `shin.toml`: project name, window title and size, `entry_scene`.
- `scenes/`: `.scene.ron` files; `scenes/main.scene.ron` loads on start.
- `scripts/`: Rhai scripts (`fn update(entity, dt)`), attached to
  entities with `script.attach`; they hot-reload while playing. The cube
  runs `player.rhai` and the moon `moon.rhai`.
- `assets/`: glTF meshes, PNG/JPEG textures, and sounds (`sounds/`).
  Paths in scenes and tools are relative to the project root.
- `src/main.rs`: systems (`game_schedule`) and game tools
  (`register_game_tools`).

## Add a tool

Register a `TypedTool` in `register_game_tools`: a `game.<verb>` name, a
one- or two-sentence description for agents, and an arguments struct with
`#[derive(Deserialize, schemars::JsonSchema)]` and
`#[serde(deny_unknown_fields)]` whose doc comments become the schema.
Restart the MCP server after changing Rust code. `README.md` has an
example.

Guide: https://nikolagigic.github.io/shin-engine/
