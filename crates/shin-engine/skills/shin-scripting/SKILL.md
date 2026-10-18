---
name: shin-scripting
description: Write a Rhai gameplay script for a Shin entity, attach it, run the simulation to watch it, read script errors, and iterate with hot reload. Use when asked to make an entity move, spin, bob, or otherwise behave over time without recompiling the game.
---

# Scripting entities in Shin

## Goal

Give an entity behaviour with a Rhai script file, see it working in the
simulation, and leave the scene in edit mode with the script attached.

## When to use

- An entity should change over time (orbit, spin, bob, pulse) and you do
  not want to touch the game's Rust code.
- A script already exists and misbehaves or fails to compile.

## The script contract

A script is a `.rhai` file in the project (by convention under
`scripts/`) that defines exactly `fn update(entity, dt)`:

```rhai
// scripts/spin.rhai
fn update(entity, dt) {
    let speed = 1.5; // radians per second
    let t = entity.transform;
    t.rotation = quat_euler(0.0, speed * dt, 0.0) * t.rotation;
}
```

- The entity's `.transform` has `.translation` and `.scale` (`vec3(x, y, z)`,
  with `.x` `.y` `.z`) and `.rotation` (a quaternion; build one with
  `quat_euler(x, y, z)` in **radians**). Changes are written back.
- The entity's `.id` and `.name` are read-only. `dt` is the fixed step in
  seconds; `time()` is simulated seconds since play started.
- Only `update` and functions it calls run. Functions cannot see outer
  `let`s, so put settings inside `update` or in helpers (`fn speed() { 1.5 }`).
- Scripts cannot spawn entities or reach other entities.
- Rhai's standard library (`sin`, `cos`, `sqrt`, `PI()`) is available;
  `print(x)` writes to the engine log.

## Steps

1. **Find the entity.** `entity.list` returns ids, names, and any script
   already attached. Note the id of the entity to script. In the starter
   project the cube already runs `scripts/player.rhai` and the moon
   `scripts/moon.rhai`; attaching another script replaces the old one.

2. **Try expressions first (optional).** `script.eval` with
   `{"source": "quat_euler(0.0, PI() / 2.0, 0.0) * vec3(1.0, 0.0, 0.0)"}`
   returns the result as JSON (`[x, y, z]`), a quick way to check maths.

3. **Write the file** with your own file tools, under the project
   directory (`project.info` gives it), e.g. `scripts/spin.rhai`.

4. **Attach it while editing.** `script.attach` with
   `{"entity": <id>, "path": "scripts/spin.rhai"}`. The result has
   `error`: `null` means it compiled; otherwise it is the compile error
   with a line number. Fix the file and attach again until it is `null`.
   Attaching is undoable with `history.undo`.

5. **Play.** `sim.play` snapshots the scene and starts play mode. Scripts
   run only while playing (not while paused or editing).

6. **Step and observe.** `sim.step` with `{"steps": 60}` runs one
   simulated second. Compare `entity.list` before and after, and look at
   `render.frame`. Step a few more times for motion over time.

7. **Read errors.** `script.errors` lists each failing script's latest
   compile or runtime error: path, line, message. A failing script does
   not stop the engine or other scripts.

8. **Iterate with hot reload.** Edit the file while playing; the engine
   checks script files twice a second (wall-clock) during steps, so the
   next `sim.step` after saving runs the new version. A version that no
   longer compiles is reported by `script.errors` and the last good
   version keeps running.

9. **Stop.** `sim.stop` restores every entity to where it was at
   `sim.play`. The script stays attached. Call `scene.save` (for example
   to the entry scene from `project.info`) to keep the attachment.

## Verification

Done when, between `sim.play` and `sim.stop`, `entity.list` shows the
entity's transform changing as intended, `script.errors` returns `[]`,
and the scene with the script attached has been saved. Report the
transform before and after the steps.

## Pitfalls

- **"history is frozen while playing"**: `script.attach` and every other
  edit are refused during play. `sim.stop`, attach, then `sim.play`.
- **Nothing moves**: you did not call `sim.play`, or the engine is paused
  (`sim.pause` toggles), or you forgot `sim.step`.
- **"update" missing or wrong arity**: the function must be exactly
  `fn update(entity, dt)`.
- **Degrees vs radians**: `quat_euler` takes radians; the entity tools
  take degrees.
- **Absolute motion vs increments**: setting
  `translation.y = sin(time())` is absolute; adding `speed * dt` each
  step accumulates. Pick one deliberately.
- **Endless loops** are stopped after one million operations with an
  error in `script.errors`.
- Paths are relative to the project directory, not to the scene file.
