---
name: shin-visual-debugging
description: Diagnose why a Shin frame looks wrong (empty, black, missing or misplaced objects, wrong colours) by rendering frames, listing entities, and moving the camera and light. Use when a render.frame result does not match what the scene should show.
---

# Debugging what a Shin frame shows

## Goal

Find out why a rendered frame does not show what you expect, fix the
cause with the engine's tools, and confirm the fix in a new frame.

## When to use

- `render.frame` returns an empty, black, or otherwise wrong picture.
- An entity you spawned or loaded is missing, too small, or misplaced.
- Colours or lighting look wrong.

## Steps

1. **Render and look.** `render.frame` with `{"width": 512, "height": 288}`.
   Describe what you actually see before changing anything.

2. **Check the engine.** `engine.info` returns the entity count, render
   target size, GPU backend, and fixed timestep. Zero entities means the
   scene was never loaded or was cleared. If you expected a project,
   `project.info` shows its directory and entry scene (it fails when no
   project is open; open one with `project.open` and `{"dir": "<path>"}`).

3. **List entities.** `entity.list` with `{"limit": 100}` returns each
   entity's id, translation, rotation, scale, name, mesh, colour,
   texture, and script. Look for:
   - entities with no `mesh`: they draw nothing;
   - a `scale` with a 0 component: invisible;
   - translations far from the others or from the camera's target;
   - an alpha of 0, or a colour near `[0, 0, 0]`.

4. **Check meshes.** `mesh.list` shows every uploaded mesh with its
   vertex and index counts and its source. A source that points at the
   wrong file or primitive (`assets/x.gltf#2`) explains a wrong shape.
   Replace it with `entity.set_mesh` and the right asset source.

5. **Aim the camera at the problem.** `camera.set` with
   `{"position": [x + 3, y + 2, z + 3], "look_at": [x, y, z]}`, where
   `[x, y, z]` is the entity's translation from `entity.list` (compute
   the numbers; the tool takes plain arrays). Render again. If the object
   appears, the original camera was the cause. Pull back further for
   large scales; a larger `fov_y_deg` widens the view.

6. **Check the light.** `light.set` with
   `{"direction": [0, -1, 0], "ambient": [0.3, 0.3, 0.3]}` lights
   everything from above with a strong ambient term. If objects appear
   now, the light pointed away from the visible faces (its direction is
   where the light travels *to*) or the ambient term was zero.

7. **Isolate.** To test one entity alone, despawn the others with
   `entity.despawn`, render, then bring them back with `history.undo`
   (once per despawn). Or spawn a known-good probe: `mesh.builtin` with
   `{"kind": "cube"}`, then `entity.spawn` with
   `{"mesh": <handle>, "name": "probe", "translation": [0, 0, 0]}`.

8. **Fix and confirm.** Apply the fix (`entity.set_transform`,
   `entity.set_color`, `entity.set_mesh`, `camera.set`, `light.set`),
   render again, and compare against the description. Undo diagnostic
   changes you do not want to keep with `history.undo`.

## Common causes

| Symptom | Likely cause | Fix |
|---|---|---|
| Only the clear colour | Camera faces away, or no entities | `camera.set` with a `look_at`; check `entity.list` |
| Object missing | Behind the camera, inside another object, scale 0, or no mesh | Steps 3 and 5 |
| Everything black | Light points away and ambient is 0 | Step 6 |
| Colours too bright or pale | sRGB values passed as linear | Use linear values (sRGB 0.5 is linear 0.21) |
| Object half under the floor | Translation is the mesh centre | Raise y by half the scaled height |
| Picture changes between frames | A system or script moves it | Check `script` in `entity.list`; `sim.stop` if playing |
| Texture missing | No texture handle passed, or the wrong image | `asset.load` the image and spawn with `texture` |

## Verification

The fix is done when a fresh `render.frame` shows the expected scene and
you can name the cause. Report the cause and the change that fixed it.

## Pitfalls

- Do not move the camera at random: read positions from `entity.list`
  and aim at them.
- `render.frame` with `width` or `height` keeps that size for later
  frames.
- While playing (after `sim.play`), edits are refused with "history is
  frozen while playing"; call `sim.stop` first. Stop also restores the
  scene to how it was before play, which may itself be the change you
  noticed.
