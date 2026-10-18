---
name: shin-scene-building
description: Build a Shin scene from a description through the engine's tools (meshes, named entities, camera, light), check it with a rendered frame, and save it as a .scene.ron file. Use when asked to create, lay out, or change what a Shin scene contains.
---

# Building a scene in Shin

## Goal

Turn a description ("a red crate on a green floor, lit from the left")
into entities in a running Shin engine, confirm it looks right in a
rendered frame, and save it to a scene file.

## When to use

- You are connected to a Shin MCP server (`shin-mcp`, or a game run with
  `cargo run -- --mcp`) and are asked to create or change a scene.
- You need named, saved entities rather than a one-off picture.

## Conventions

- Right-handed, +Y up, -Z forward. A unit cube spans -0.5..0.5, so a cube
  resting on the ground at y = 0 has `translation` y = 0.5.
- Vectors are JSON arrays: `[x, y, z]`. Rotations are Euler degrees,
  applied in X, Y, Z order.
- Colours are **linear** RGBA `[r, g, b, a]`, not sRGB. Pick darker
  values than you would in CSS: sRGB mid-grey `#808080` is linear 0.22.
- `translation`, rotation, and scale are local to the entity's parent (a
  root's parent is the world); `entity.list` also reports
  `world_translation`.
- Entity ids are large numbers (`4294967296`); always take them from
  `entity.spawn` or `entity.list`, never guess.

## Steps

1. **See what is there.** Call `engine.info` (entity count, frame size)
   and `entity.list`. If a project is open, `project.info` tells you its
   directory and entry scene. To start empty, call `scene.clear`.

2. **Get mesh handles.** Built-in shapes:

   `mesh.builtin` with `{"kind": "cube"}` returns `{"handle": 0}`;
   `{"kind": "sphere", "segments": 32, "rings": 16}` makes a sphere.

   A glTF file or a texture: `asset.load` with
   `{"source": "assets/cube.gltf"}` returns `{"handle": n}`, and
   `{"source": "assets/checker.png"}` returns `{"texture": n}`.
   Loading the same source twice returns the same handle, and
   `mesh.list` shows every handle with its source.

3. **Spawn named entities.** Give every entity a `name`; names are saved
   in scene files and make later edits readable. `entity.spawn` with:

   ```json
   {"mesh": 0, "name": "floor", "translation": [0, -0.25, 0],
    "scale": [10, 0.5, 10], "color": [0.15, 0.3, 0.12, 1]}
   ```

   then:

   ```json
   {"mesh": 0, "name": "crate", "translation": [0, 0.5, 0],
    "rotation_euler_deg": [0, 30, 0], "color": [0.8, 0.1, 0.1, 1]}
   ```

   Each returns `{"entity": <id>}`. Adjust later with
   `entity.set_transform`, `entity.set_color`, `entity.set_name`,
   `entity.set_mesh` (an asset source such as `"builtin://sphere"`), or
   `entity.set_texture` (an image source such as `"assets/checker.png"`),
   and remove with `entity.despawn`. To group objects so they move
   together, `entity.set_parent` with `{"entity": <child>, "parent": <id>}`;
   the child stays where it is in the world by default.

4. **Frame it.** `camera.set` with
   `{"position": [0, 3, 6], "look_at": [0, 0.5, 0], "fov_y_deg": 60}`.
   Place the camera a few units back from the scene's centre and above it.

5. **Light it.** `light.set` with
   `{"direction": [0.6, -1, -0.4], "color": [1, 0.95, 0.85], "ambient": [0.1, 0.1, 0.12]}`.
   `direction` is where the light travels *to*: `[0, -1, 0]` is straight
   down, and a positive x component lights the scene from the left.

6. **Verify.** `render.frame` with `{"width": 512, "height": 288}` returns
   a PNG. Look at it. Check that every named object is visible, sits where
   it should (on the ground, not inside it), and has the colour you
   meant. `entity.list` gives exact positions if the picture is ambiguous.

7. **Fix mistakes.** `history.undo` reverts the last edit (spawn,
   despawn, transform, colour, name, mesh, clear, camera, light, scene
   load); `history.redo` re-applies it. Prefer undo over hand-reverting
   values.

8. **Persist.** `scene.save` with `{"path": "scenes/main.scene.ron"}`.
   With a project open, relative paths land in the project folder and
   paths that leave it are rejected. `scene.load` with the same path
   reloads it. The project's entry scene (from `project.info`) is what
   the game loads on start, so save there to change the game itself.

## Verification

The task is done when a `render.frame` shows the described scene and
`scene.save` has returned the path and the expected entity count. Report
both.

## Pitfalls

- **Nothing visible**: the camera is probably looking away or is inside
  an object. Use the `shin-visual-debugging` skill.
- **Washed-out colours**: you passed sRGB values. Colours are linear.
- **Objects sunk into the floor**: translation is the centre of the
  mesh; lift by half the scaled height.
- **A `scale` of 0** in any axis makes an entity invisible.
- **Rotations in radians**: the tools take degrees.
- **Edits fail with "history is frozen while playing"**: something called
  `sim.play`. Call `sim.stop` (it restores the scene from before play),
  then edit and save.
