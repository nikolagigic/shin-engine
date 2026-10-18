# Scenes

A scene file holds the world's entities, the camera, and the light as a
[RON](https://github.com/ron-rs/ron) document you can read, diff, and
edit by hand. The extension is `.scene.ron`. The editor's File menu and
the `scene.save` and `scene.load` tools read and write the format; a
project's `entry_scene` in `shin.toml` names the one loaded at start-up.

## A complete example

This scene ships with the engine and is rendered by its golden-image
tests: three cubes in a row under the default camera and light.

```ron
{{#include ../../crates/shin-engine/assets/scenes/tool_scene.scene.ron}}
```

Reading it top to bottom: the `version` is the format version; `camera`
places a perspective camera; `light` is the one directional light;
`entities` lists each entity with its `name`, placement, `mesh`, and
`color`. The green cube has no `transform`, so it sits at the origin with
no rotation and unit scale; the blue cube gives only the parts of its
transform that differ from that. Optional fields that are absent are
simply left out, without `Some(...)`.

Two optional entity fields do not appear above. `texture` gives a
base-colour texture and `script` attaches a Rhai script, like this
fragment of an entity:

```ron
(
    name: "player",
    transform: (translation: (0.0, 1.0, 0.0)),
    mesh: "builtin://cube",
    color: (0.8, 0.1, 0.1, 1.0),
    texture: "assets/checker.png",
    script: "scripts/orbit.rhai",
),
```

## Every field

### The file

| Field      | Type            | Required | Meaning |
|------------|-----------------|----------|---------|
| `version`  | integer         | yes      | Format version. Must be `1`; any other value is an error. |
| `camera`   | camera          | no       | The camera. When absent, loading keeps the current camera. |
| `light`    | light           | no       | The directional light. When absent, loading keeps the current light. |
| `entities` | list of entity  | no       | Entities, in spawn order. Default empty. |

### `camera`

A perspective camera aimed with +Y as up.

| Field       | Type         | Meaning |
|-------------|--------------|---------|
| `position`  | `(x, y, z)`  | World-space position. |
| `look_at`   | `(x, y, z)`  | World-space point the camera looks at. Must differ from `position` and must not be straight above or below it. |
| `fov_y_deg` | number       | Vertical field of view in degrees, between 1 and 179. |

Loading keeps the current camera's near and far clip planes. Saving
writes the point on the view ray nearest the world origin (at least one
unit ahead) as `look_at`, so a camera aimed at the origin saves
`look_at: (0.0, 0.0, 0.0)`. An orthographic camera cannot be saved.

### `light`

The directional light, in linear RGB.

| Field       | Type         | Meaning |
|-------------|--------------|---------|
| `direction` | `(x, y, z)`  | Direction the light travels, from the light toward the scene; `(0, -1, 0)` shines straight down. Must not be zero. |
| `color`     | `(r, g, b)`  | Light colour. |
| `ambient`   | `(r, g, b)`  | Ambient light added to every surface. |

### Entities

Each entry of `entities`:

| Field       | Type       | Required | Meaning |
|-------------|------------|----------|---------|
| `name`      | string     | no       | The entity's name, shown in the editor and used by systems and scripts to find it. |
| `transform` | transform  | no       | Placement; omitted parts are the identity. |
| `mesh`      | string     | no       | The mesh to draw, as an [asset source](#asset-sources). An entity without one draws nothing. |
| `color`     | `(r, g, b, a)` | no   | Linear RGBA tint of the mesh; default white. Ignored without `mesh`. |
| `texture`   | string     | no       | Base-colour texture, as a texture asset source; none for untextured. Ignored without `mesh`. |
| `script`    | string     | no       | A `.rhai` file relative to the project directory, run every fixed step while playing; see [Scripting](scripting.md). |
| `children`  | list of entity | no   | Entities that belong to this one; see [Children](#children). Default empty, and left out when empty. |

### Children

An entity can carry other entities in its `children` list, nested as deep
as you like: a turret on a tank, a light on a lamp post. A child's
`transform` is **local to its parent**. It is applied after the parent's,
so the child moves, turns, and scales with the parent. This scene ships with
the engine and is rendered by its `golden_hierarchy` test:

```ron
{{#include ../../crates/shin-engine/assets/scenes/hierarchy.scene.ron}}
```

The spheres sit at `±1.5` on the parent's own X axis. The parent is turned
a quarter turn about Y and scaled to 0.8, so in the world they end up in
front of and behind the cube, 1.2 units away.

Saving writes each entity under its parent: top-level entities in entity id
order, each `children` list in id order. Loading spawns them depth first
in file order and links each child to its parent, so saving a freshly
loaded file reproduces it byte for byte. Files without `children` load
unchanged; the format version stays 1.

Translation, rotation, and scale compose separately. If a parent has a
non-uniform scale and its child is rotated, the slant (shear) that a full
matrix product would give is dropped.

### `transform`

| Field                | Type         | Default       | Meaning |
|----------------------|--------------|---------------|---------|
| `translation`        | `(x, y, z)`  | `(0, 0, 0)`   | Position. |
| `rotation_euler_deg` | `(x, y, z)`  | `(0, 0, 0)`   | Euler angles in degrees, applied intrinsically about X, then Y, then Z, the same convention the `entity.*` tools use. |
| `scale`              | `(x, y, z)`  | `(1, 1, 1)`   | Per-axis scale. |

## Asset sources

Meshes and textures are named by **asset source** strings, so a scene
file never depends on runtime handles. The same strings are accepted by
`asset.load`, `entity.set_mesh`, and the editor's mesh field.

| String                  | What it is |
|-------------------------|------------|
| `builtin://cube`        | The built-in unit cube (extent −0.5 to 0.5). |
| `builtin://sphere`      | The built-in unit-radius UV sphere, 32 segments by 16 rings. |
| `builtin://sphere-8x4`  | A UV sphere with that many segments and rings (`sphere-<segments>x<rings>`). |
| `assets/x.gltf`         | The first triangle primitive of a glTF file (`.gltf` or `.glb`). |
| `assets/x.gltf#2`       | Triangle primitive 2 of the file, in document order. |
| `assets/x.png`          | A texture: a PNG or JPEG file (`.png`, `.jpg`, `.jpeg`, any case). |
| `assets/x.gltf#image=1` | A texture: image 1 of a glTF file, embedded or referenced. |

The first three rows are meshes, the last two textures. Relative paths
are resolved against the project directory (the engine's working
directory when no project is open). Each source is uploaded to the GPU
once and shared by every entity that names it.

## What is saved and what is not

Saving captures every entity that has a transform, in entity id order,
with its transform, mesh source, colour, texture source, name, script,
and children nested inside it; plus the camera and the light. A child
whose parent has no transform is written at the top level. Nothing else survives a save and load:
other components, and entities without a transform, are not in the file.
That is also what [Play and Stop](editor.md#play-pause-stop) in the editor
restore, because the play snapshot is a scene file in memory.

Saving fails, naming the entity, if it draws a mesh or texture that has
no asset source (one added from Rust code rather than through an asset
source), or if the camera is orthographic.

Angles and `look_at` are derived from the engine's quaternions and
radians, so saving rounds them to four decimals; saving a freshly loaded
file reproduces it byte for byte. The writer indents with four spaces.

## Loading and errors

Loading parses and validates the whole file and resolves every mesh and
texture **before** touching the world, so a failed load leaves the scene
as it was. These are errors, each naming what is wrong:

- a parse error (with line and column),
- an unknown field (named), or a missing required one,
- a version other than 1,
- an invalid camera (`look_at` equal to `position` or directly above or
  below it, a field of view outside 1 to 179) or a zero light direction,
- a mesh or texture source that does not parse or cannot be loaded (the
  error names the source and the entity index, counting nested children
  depth first).

A successful load replaces every entity, then applies the camera and the
light if the file has them.

## Tools

| Tool         | What it does |
|--------------|--------------|
| `scene.save` | Saves the scene to a `.scene.ron` path, creating missing directories. Returns the path written and the number of entities. |
| `scene.load` | Replaces every entity with those in a file and applies its camera and light. Undoable. Returns the number of entities spawned, children included. |
| `entity.set_parent` | Makes an entity the child of another, or a top-level entity again with `parent: null`. By default it keeps the entity's world position; `keep_world: false` keeps its local transform instead. Undoable. |
| `entity.despawn` | Removes an entity and, by default, all its descendants. With `recursive: false` its children stay, as top-level entities in the same world position. Undoable. |

`entity.list` reports each entity's `parent` id and its
`world_translation` alongside the local transform. `entity.spawn` takes an
optional `parent`.

With a project open, relative paths are resolved against the project
directory and paths that leave it are rejected. See the
[Tools reference](reference/tools.md).
