# Getting started

By the end of this page you will have the `shin` tool installed, a game
project of your own running in a window, and that project open in the
editor.

Shin is **pre-alpha**: a 0.x release may change any API in the next
minor version.

## Prerequisites

- **Rust** 1.99 or newer, installed with [rustup](https://rustup.rs).
- A GPU with a Metal, Vulkan, or DX12 driver. On Linux without a GPU,
  Mesa's `lavapipe` software Vulkan driver works for headless use.
- On Linux, the ALSA headers for the audio backend
  (`sudo apt-get install libasound2-dev pkg-config` on Debian and Ubuntu).

## Install

```sh
cargo install shin-cli shin-editor
```

That installs two binaries into `~/.cargo/bin`: `shin`, the command-line
tool, and `shin-editor`, the editor. Each
[GitHub release](https://github.com/nikolagigic/shin-engine/releases) also
has both prebuilt for macOS (Apple silicon) and Linux (x86_64).

## Create a project

```sh
shin new mygame
```

This writes `./mygame` and prints every file it wrote. The project is an
ordinary Cargo package that depends on the published `shin-engine` crate
(and `shin-mcp`, for agents) at the version of `shin` you installed. The
project layout and its `shin.toml` are described in
[Projects](projects.md).

## Run it

```sh
cd mygame
cargo run
```

The first build compiles the engine for the project (a few minutes), then
a window opens with the starter scene: a dark ground slab, an orange cube
turned a little so the light catches three faces, a small pale moon
circling the cube once every 6.3 seconds with a short blip each time it
comes round, and a stack of five crates that drops onto the ground. The
orbit is a Rust system in the project's `src/main.rs`, the blip is the
moon's script `scripts/moon.rhai`, the crates fall by
[physics](physics.md), and the scene itself is `scenes/main.scene.ron`.

Controls, from the starter game:

- W/S: forward and back, A/D: strafe, E/Q: up and down
- Hold the right mouse button to look around
- Escape: quit

## Open it in the editor

`shin edit` runs the editor on a project directory. From inside
`mygame`:

```sh
shin edit .
```

`shin edit` runs the `shin-editor` next to the `shin` binary (both are in
`~/.cargo/bin` after `cargo install`), or the one named by the
`SHIN_EDITOR` environment variable, or the one on your `PATH`. Running the
editor binary directly does the same:

```sh
shin-editor .
```

The editor opens the project's entry scene in a live viewport with the
Scene, Inspector, History, Log, and Console panels around it. Drag with
the left mouse button to orbit, the right button to pan, and scroll to
zoom. Click an entity in the Scene panel to select it, and edit it in the
Inspector. [The editor](editor.md) covers every panel.

## From source

To work on the engine itself, or to try changes that are not released
yet, build it from a checkout instead. You also need **Git**.

```sh
git clone https://github.com/nikolagigic/shin-engine
cd shin-engine
rustup toolchain install
cargo build
```

`rustup toolchain install` with no arguments reads `rust-toolchain.toml`
and installs the pinned toolchain and its components. The first build
compiles wgpu, winit, egui, and the rest of the dependencies, which takes a
few minutes; later builds are incremental.

To see the engine run before making anything, try the demo example: a
field of cubes and spheres around a pedestal with orbiting satellites.

```sh
cargo run --release --example demo
```

Fly with W/A/S/D and E/Q, hold the right mouse button to look around, and
press Escape to quit.

From the checkout, `cargo run -p shin-cli --` stands in for `shin`.
`--engine-path` makes the new project depend on the engine you just
built instead of the published crate:

```sh
cargo run -p shin-cli -- new mygame --dir .. --engine-path crates/shin-engine
cargo build -p shin-editor
cargo run -p shin-cli -- edit ../mygame
```

`shin edit` finds the editor in `target/debug/`, next to `shin`.

## Where next

- [Projects](projects.md): what `shin new` wrote and what `shin.toml`
  means.
- [Scenes](scenes.md): the scene file format, field by field.
- [Scripting](scripting.md): attach a Rhai script to the moon and edit it
  while the editor plays.
- [Physics](physics.md): rigid bodies, colliders, and ray casts.
- [Agents and MCP](agents-mcp.md): let an agent drive your game.
