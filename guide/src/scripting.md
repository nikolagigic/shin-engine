# Scripting

Rhai scripts are Shin's "gameplay without recompiling" layer. An entity
with a script and a transform runs the script's `update` function once per
fixed simulation step, and the script can move, turn, and scale the
entity. Scripts are reloaded while the engine runs, so you can edit one
and watch the result within a second.

## A script

```rhai
// scripts/bob.rhai
fn update(entity, dt) {
    entity.transform.translation.y = sin(time());
}
```

Every script must define `fn update(entity, dt)` with exactly those two
parameters, or it fails to compile. `entity` is the entity the script is
attached to, `dt` is the fixed step in seconds (1/60 by default), and
`time()` is the simulated seconds since play started. Changes to
`entity.transform` are written back to the entity after the call.

Only `update`, and the functions it calls, runs; statements outside
functions do not. Rhai functions cannot see variables declared outside
them, so keep settings as `let`s inside `update` or as helper functions
(`fn speed() { 0.8 }`).

## Attaching a script

A script is attached by path, relative to the project directory (the
engine's working directory when no project is open), in any of three
ways:

- **In the editor**, select the entity and type the path into the
  Inspector's **Script** field (`scripts/orbit.rhai`); it commits when
  the field loses focus. Clearing the field removes the script.
- **With the `script.attach` tool**, from the Console or an agent:
  `script.attach {"entity": 4294967296, "path": "scripts/orbit.rhai"}`.
  An empty path removes the script. The script is compiled straight away
  and the result reports the compile error, if any; the script is
  attached either way and runs once the file is fixed.
- **In a scene file**, with the entity's `script` field:
  `script: "scripts/orbit.rhai"`.

All three are the same thing: a `Script` component holding the path.

## When scripts run

Scripts run only while **playing**: in the editor between Play and Stop
(not while paused), under `sim.play` for an agent, and always in a game
running outside the editor, which has no play state and counts as
playing. Entities are updated in id order. A script that fails does not
stop the engine or the other scripts.

## The script API

Scripts have Rhai's standard library (`sin`, `cos`, `atan`, `sqrt`,
`PI()`, strings, arrays, maps, and so on) plus these engine types and
functions. Numbers are floats in scripts and `f32` in the engine; the
setters below also accept integers, so `v.y = 1` works.

### `Vec3`

| Member | Meaning |
|--------|---------|
| `vec3(x, y, z)` | A new vector. |
| `.x`, `.y`, `.z` | Components, readable and writable. |
| `a + b`, `a - b`, `-a` | Vector addition, subtraction, negation. |
| `v * s`, `s * v` | Scaling by a number. |
| `v.length()` | The vector's length. |
| `to_string` | `print(v)` shows the vector. |

### `Quat`

| Member | Meaning |
|--------|---------|
| `quat_euler(x, y, z)` | A rotation from Euler angles in **radians**, applied intrinsically about X, then Y, then Z, the same order as the `entity.*` tools' `rotation_euler_deg` (which is in degrees). |
| `.x`, `.y`, `.z`, `.w` | Components, read-only. |
| `q * r` | Composition of two rotations. |
| `q * v` | A `Vec3` rotated by `q`. |
| `to_string` | `print(q)` shows the quaternion. |

### `Transform`

| Member | Meaning |
|--------|---------|
| `.translation` | A `Vec3`, readable and writable. |
| `.rotation` | A `Quat`, readable and writable; assigned values are normalised. |
| `.scale` | A `Vec3`, readable and writable. |
| `to_string` | `print(t)` shows all three. |

A transform is a shared handle: `let t = entity.transform; t.scale = ...`
changes the entity, as does `entity.transform.translation.y = 1.0`.

### `Entity`

| Member | Meaning |
|--------|---------|
| `.id` | The entity's id, the same number the tools use; read-only. |
| `.name` | Its name, or an empty string; read-only. |
| `.transform` | Its `Transform`, **local to its parent** (world space for an entity without one); readable and writable. |
| `.world_transform` | Its `Transform` in world space, after its parents', as it was at the start of the step. Read-only: it is a copy, and changing it changes nothing. |

An entity's [parent](scenes.md#children) moves it along. A script on a
child that sets `entity.transform.translation` places the child relative
to its parent, so it keeps following the parent. To find out where the
entity really is, read `entity.world_transform`.

### Functions

| Function | Meaning |
|----------|---------|
| `time()` | Simulated seconds since play started. |
| `action_pressed(name)` | Whether the input action `name` is held. |
| `action_just_pressed(name)` | Whether `name` went down since the previous step. True in exactly one step per press. |
| `action_axis(name)` | The value of `name` from -1 to 1. A button action reads 1 while held. |
| `print(x)` | Writes `script: x` to the engine log at info level (the editor's Log panel). |
| `debug(x)` | Writes to the log at debug level. |

### Input actions

Actions are named and bound in the project's `shin.toml`, as
[Projects](projects.md#input-actions) describes. A script reads them by
name and never by key, so players can rebind without touching the
script. Naming an action the project does not define is a runtime error
that lists the actions that exist.

This is the starter project's `scripts/player.rhai`:

```rhai
{{#include ../../crates/shin-engine/src/project/template/player.rhai}}
```

To try a script without a keyboard, play the scene and drive the actions
with the `input.set` tool. `script.eval` reads the same values, so
`script.eval {"source": "action_axis(\"move_x\")"}` shows what a script
would see.

Scripts cannot spawn entities, reach other entities, or call other
scripts. The `script.eval` tool evaluates a Rhai expression with the same
API and returns the result as JSON (a `Vec3` as `[x, y, z]`, a `Quat` as
`[x, y, z, w]`, a transform as an object), which is a quick way to try
things.

## Hot reload

Twice a second, the engine checks the modification time of every loaded
script file and recompiles the ones that changed, logging
`reloaded script <path>`. A script that no longer compiles keeps running
its last good version until the file is fixed. There is nothing to
restart.

## Errors and the operation limit

A script that fails to compile, or fails while running, is logged once as
`path:line: message` (or `path: message` when Rhai knows no line) and
kept as the script's latest error. The editor's Inspector shows it in red
under the entity's Script field; the `script.errors` tool lists every
script's latest error with its path, line, and message. The error is
cleared the next time the script is recompiled successfully, that is,
when you save a fixed version of the file.

One `update` call may run at most one million Rhai operations before it
is stopped with an error, so a script stuck in a loop cannot freeze the
engine.

## A worked example: orbit

This script ships with the engine's `scripted_orbit` example. It turns
whatever it is attached to around the Y axis, bobs it up and down, and
spins it, keeping each entity's own radius and phase, so one script can
drive many entities:

```rhai
{{#include ../../crates/shin-engine/examples/scripts/orbit.rhai}}
```

To see it with hot reload, run the example from the engine checkout and
edit the file while it runs:

```sh
cargo run --example scripted_orbit
```

To use it in your project, copy the file to `mygame/scripts/orbit.rhai`,
open the project in the editor, select the `moon` in the Scene panel, type
`scripts/orbit.rhai` into the Inspector's Script field, and press Play.
The moon circles the cube, bobbing up and down about ground level (the
script sets the height itself, around zero). Change `let speed = 0.8;` to
`-2.0` and save the file: within a second it orbits the other way. Press
Stop and the moon is back where it started; the script stays attached,
and File > Save writes it into the scene.

## Tools

| Tool            | What it does |
|-----------------|--------------|
| `script.attach` | Attaches a `.rhai` file to an entity (or removes it with an empty path), compiles it, and reports the compile error if any. Undoable. |
| `script.eval`   | Evaluates Rhai source with the script API and returns its result as JSON. |
| `script.errors` | Lists the latest compile or runtime error of each script that has one. |
| `input.set`     | Holds, releases, or sets the axis of an input action on an engine with no devices. |
| `input.list`    | Lists the input actions with their bindings and current values. |

See the [Tools reference](reference/tools.md).
