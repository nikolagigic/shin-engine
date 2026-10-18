# The editor

`shin-editor` is an egui application around a live engine viewport. It
owns one headless engine with your project open and a tool registry, and
every edit you make in it is a tool call recorded in the engine's command
history, so everything is undoable and an agent can reproduce it with the
same call.

```text
shin edit <dir> [-- <editor args>...]
shin-editor <project-dir> [--screenshot <png>] [--demo-orbit]
```

`shin edit` runs the `shin-editor` binary on a project directory (the one
containing `shin.toml`): the binary named by `$SHIN_EDITOR`, else the one
next to `shin`, else the one on `PATH`. Arguments after `--` go to the
editor, for example `shin edit mygame -- --screenshot out.png`.

![The editor on the starter project: the viewport in the centre, Scene on the left, Inspector and History on the right, Log and Console below](images/editor.png)

## Panels and the View menu

The window opens at 1600 × 900 with seven docked panels:

| Panel         | Where      | Shows |
|---------------|------------|-------|
| **Viewport**  | centre     | The scene, rendered live from the editor's orbit camera (from the scene's own camera while playing). The selected entity is drawn with its colour mixed halfway toward white. |
| **Scene**     | left, top  | Every entity that has a transform, in id order, labelled by its name or `#<id>` when it has none; buttons to add and delete. |
| **Assets**    | left, bottom | The project's meshes, textures, scenes, and scripts, with a button per entry to use it, and an Import row. |
| **Inspector** | right, top | The selected entity's properties, editable, and the scene camera. |
| **History**   | right, bottom | The command history: undoable edits oldest first as `tool {args}`, then undone edits greyed out. |
| **Log**       | bottom, left | The engine's log lines, newest at the bottom. |
| **Console**   | bottom, right | A one-line prompt that runs tools. |

Every panel except the Viewport has a close button on its tab. The
**View** menu has a checkbox per closeable panel: untick to close it,
tick to dock it again at its original edge.

## Moving the camera

The viewport camera is the editor's own orbit camera, circling a target
point. It starts above and in front of the origin, looking down at a unit
cube resting on y = 0.

- **Left-drag** orbits around the target. Pitch stays within ±89°, so
  the view never flips over the pole.
- **Right-drag** pans the target in the view plane; the scene follows the
  pointer.
- **Scroll wheel** zooms: wheel up moves closer, down moves away, between
  0.1 and 500 units from the target.

Drags only count when they start on the viewport, so dragging a panel's
widgets never moves the camera.

The orbit camera is the editor's own. The scene's camera, the one the
scene file holds and the game starts from, is left exactly as authored
while you orbit: it is only swapped out for the viewport render, and it
keeps the field of view. **Save never writes the editor view into the
scene.** To look through the scene camera, or to make it match the
viewport, use the Inspector's [Scene camera](#scene-camera) buttons.

## Selecting

Click an entity in the Scene panel to select it. One entity is selected
at a time. Escape clears the selection. New entities are selected as they
are added, and an entity that is deleted (or that disappears when play
stops) is deselected.

## The inspector

The Inspector shows `Entity #<id>` and these rows for the selected entity:

| Row              | Edit | Tool call recorded |
|------------------|------|--------------------|
| **Name**         | A text field; commits when it loses focus. An empty name removes the name. | `entity.set_name` |
| **Translation**, **Rotation (°)**, **Scale** | Three drag values each. Dragging previews the change live in the viewport and records one edit when the pointer is released; a typed number commits when its field loses focus. Rotation is Euler angles in degrees, applied in X, Y, Z order. | `entity.set_transform` |
| **Mesh**         | A drop-down of every mesh source loaded so far plus `builtin://cube` and `builtin://sphere`, and a text field for any asset source (press Enter). See [asset sources](scenes.md#asset-sources). | `entity.set_mesh` |
| **Colour**       | A colour picker for the mesh's linear RGBA tint, with alpha. Picking previews live and records one edit when the pointer is released. Only shown for entities that draw a mesh. | `entity.set_color` |
| **Script**       | The path of the entity's Rhai script, relative to the project (`scripts/name.rhai`); commits when the field loses focus. Clearing it removes the script. The script's latest compile or runtime error, if any, is shown in red below the grid. | `script.attach` |

Values are refreshed from the world every frame except for the field you
are typing in. Every commit is one entry in the History panel, so a drag
of any length undoes in one step.

### Scene camera

Below the entity rows, whether or not something is selected, the
Inspector shows **Scene camera**: the scene's own camera, read-only, as
its position, the direction it looks, and its vertical field of view.
This is the camera Save writes and Play renders through, not the
viewport's orbit camera. Two buttons move one to the other:

- **View from scene camera** moves the orbit camera to where the scene
  camera is, facing the same way, so the viewport shows what the game
  would. The orbit target becomes the point on that view nearest the
  previous target, at least one unit ahead. This is not an edit.
- **Set scene camera from view** moves the scene camera to the viewport
  camera, aimed at the orbit target and keeping its field of view, with
  one recorded `camera.set`. It appears in the History panel, Ctrl+Z
  reverts it, and the scene is marked unsaved until you save.

## Adding and deleting entities

The Scene panel's controls:

- **+ Cube** and **+ Sphere** add an entity drawing `builtin://cube` or
  `builtin://sphere` at the orbit camera's target, named `cube` or
  `sphere`, and select it.
- **+ From asset** adds an entity drawing any [asset
  source](scenes.md#asset-sources) typed in the field next to it (the
  hint is `assets/x.gltf#0`), named after the file (`assets/crate.gltf#1`
  gives `crate`). Enter in the field does the same.
- **Delete** removes the selected entity, as do the Delete and Backspace
  keys while no text field has focus.

Adding loads the mesh with `asset.load` (not recorded: loading changes no
entity) and spawns with one recorded `entity.spawn`; deleting is one
recorded `entity.despawn`. Undoing a delete brings the entity back under
the same id.

## The Assets panel

The Assets panel lists what the project directory contains, so you can
use a file without typing its asset source. It scans three directories,
including subdirectories:

| Directory  | Files                        | Entries |
|------------|------------------------------|---------|
| `assets/`  | `.gltf`, `.glb`              | One **mesh** per triangle primitive (`assets/x.gltf`, `assets/x.gltf#1`, ...) and one **texture** per image (`assets/x.gltf#image=0`). |
| `assets/`  | `.png`, `.jpg`, `.jpeg`      | One **texture**. |
| `scenes/`  | `.scene.ron`                 | One **scene**. |
| `scripts/` | `.rhai`                      | One **script**. |

Other files (a glTF's `.bin` buffer, `src/`, `target/`) are not listed; a
glTF that does not parse is skipped with a warning in the Log. Entries
are grouped by kind under collapsible headings and labelled by their
[asset source](scenes.md#asset-sources) or path; hovering shows the file.
The **Filter** box narrows the list to entries whose source or path
contains the text, ignoring case.

Each entry has buttons for what it can do. Those that need a selected
entity are greyed out without one, and all of them are disabled while
playing:

| Kind    | Button     | Does | Tool call recorded |
|---------|------------|------|--------------------|
| mesh    | **Spawn**  | Adds an entity drawing the mesh at the orbit camera's target, named after the file, and selects it (like **+ From asset**). | `entity.spawn` |
| mesh    | **Assign** | Makes the selected entity draw this mesh; colour and texture are kept. | `entity.set_mesh` |
| texture | **Assign** | Gives the selected entity this base-colour texture (it must draw a mesh). | `entity.set_texture` |
| script  | **Attach** | Attaches the script to the selected entity, replacing any it had. | `script.attach` |
| scene   | **Open**   | Replaces every entity with the scene file's, clears the history and selection, and makes the file the one **Save** writes, like File > Open. | `scene.load` (not recorded) |

Every recorded call is one entry in the History panel, so Ctrl+Z undoes
it. To remove a texture, run `entity.set_texture {"entity": <id>,
"source": null}` in the Console.

**Import…** copies a file into the project's `assets/` directory: type an
absolute path into the field next to the button and press it (or Enter).
The file keeps its name, nothing is overwritten (importing a name that
already exists fails), and the panel lists the new file at once. There
is no file dialog; paste the path from your file manager.

The panel rescans the project when it is opened from the View menu and,
while it is shown, checks every 2 seconds whether anything under the
three directories was added, removed, or modified, rescanning only then.
So a file you copy into `assets/` by hand appears within 2 seconds.
Agents see the same list through the `asset.list` tool.

## Undo, redo, and the History panel

- **Ctrl+Z** (Cmd+Z on macOS) undoes, **Ctrl+Shift+Z** (Cmd+Shift+Z)
  redoes. The **Edit** menu has the same two items, greyed out when there
  is nothing to undo or redo.
- While a text field with text in it has focus, the shortcuts belong to
  the field (its own text undo). In an empty Console, right after running
  a line, they undo and redo edits.
- The History panel lists what will be undone, oldest first, as the tool
  name and JSON arguments of each recorded call, then what redo would
  bring back, greyed out. It is read-only.

Only recorded tool calls appear: edits from the Inspector, the Scene
panel, the Assets panel, and the Console. Queries, renders, asset loads,
and `sim.step` run without being recorded.

## The Console

The Console runs any engine tool. Type a tool name, optionally followed
by its arguments as a JSON object, and press Enter:

```text
engine.list_tools
entity.spawn {"mesh": 0, "translation": [0, 1, 0], "name": "box"}
entity.list {"limit": 5}
history.undo
```

With no arguments the tool gets `{}`. The line and its result are added
to the transcript above the prompt: text results as they are, JSON
results on one line, and rendered images as `<width x height PNG>`.
Errors are shown in red, including arguments that are not valid JSON.
Every line also goes to the log.

Console lines run through the command history, so a spawn from the
Console appears in the History panel and Ctrl+Z undoes it. The
[Tools reference](reference/tools.md) lists every tool and its
arguments.

## Files: Open, Save, Save As

The **File** menu works with scene files relative to the project:

- **Open** loads the project's entry scene (or `scenes/main.scene.ron`
  when `shin.toml` names none) with `scene.load`, replacing every entity
  and clearing the history and selection. Any other scene file in
  `scenes/` opens the same way from the [Assets panel](#the-assets-panel).
- **Save** writes the scene to the current scene path with `scene.save`.
  Hovering the item shows the path. At first it is the entry scene.
- **Save As** takes a project-relative path ending in `.scene.ron`
  (`scenes/other.scene.ron`); press Enter or the button. That path becomes
  the one Save writes to.
- **Quit** closes the editor.

The window title is the project's window title followed by ` — Shin
Editor`, with a `*` after the title while the scene has unsaved edits:
the undo stack differs from what it was at the last Open or Save. Undoing
back to the saved state clears the marker.

What a scene file does and does not hold is described in
[Scenes](scenes.md). Saving fails if an entity draws a mesh that was not
loaded by asset source; everything the editor itself adds has one.

## Play, Pause, Stop

The toolbar at the end of the menu bar has **Play**, **Pause**,
**Stop**, and the simulated seconds since Play.

- **Play** snapshots the scene (in scene file form, with the entity ids
  and the simulation clock) and starts stepping the simulation at the
  engine's fixed step of 1/60 s, from wall-clock frame times. Entities'
  Rhai scripts run every step. While playing, the Play button is shown
  selected and the viewport renders from the scene camera, as the game
  would; Stop returns to the orbit camera's view.
- **Pause** suspends stepping; click it again (or Play) to resume. No time
  builds up while paused.
- **Stop** restores the scene exactly as it was at Play: every entity
  comes back under the id it had, so the selection and the history stay
  valid, and the clock is restored.

While playing, editing is disabled: the Inspector is read-only (it still
shows the live values), the Scene panel's add and delete controls, the
Assets panel's buttons, undo
and redo, the Delete key, and File > Open and Save are greyed out or
ignored. Selecting still works. Console lines still run, but they are not
recorded, since Stop discards whatever they changed, and `history.*`
tools are refused until you stop. The toolbar, `sim.play`, `sim.pause`,
and `sim.stop` from the Console, and an agent's calls are all the same
play mode.

Only what a scene file holds survives a play session: entities with a
transform (their transform, mesh, colour, texture, name, and script), the
camera, and the light. Playing is not an edit, so it does not mark the
scene unsaved.

The editor does not run your project's compiled Rust systems; only the
engine's own systems and the entities' scripts run under Play. To see
the starter scene's moon move in the editor, either attach the orbit
script from [Scripting](scripting.md) to it, or start the editor with
`--demo-orbit`, which registers a stand-in system that moves the entity
named `moon` on a circle of radius 1.6 at height 0.5, one radian per
simulated second, like the starter game does.

## Screenshots

```sh
shin-editor mygame --screenshot editor.png
```

renders one full editor frame of the project, 1600 × 900 pixels, without
opening a window, writes it to the PNG, and exits. The editor's own tests
use it, and the image at the top of this page was made with it.
