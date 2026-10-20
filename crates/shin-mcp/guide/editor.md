# The editor

`shin-editor` is an egui application around a live engine viewport. It
owns one headless engine with your project open and a tool registry, and
every edit you make in it is a tool call recorded in the engine's command
history, so everything is undoable and an agent can reproduce it with the
same call.

```text
shin edit <dir> [-- <editor args>...]
shin-editor <project-dir> [--screenshot <png> [--select <name>]] [--demo-orbit]
```

`shin edit` runs the `shin-editor` binary on a project directory (the one
containing `shin.toml`): the binary named by `$SHIN_EDITOR`, else the one
next to `shin`, else the one on `PATH`. Arguments after `--` go to the
editor, for example `shin edit mygame -- --screenshot out.png`.

![The editor on the starter project: the viewport in the centre, Scene on the left, Inspector and History on the right, Log and Console below](images/editor.png)

## Panels and the View menu

The window opens at 1600 × 900 with eight docked panels and a status
bar along the bottom that shows the [multiplayer](#multiplayer)
connection state:

| Panel         | Where      | Shows |
|---------------|------------|-------|
| **Viewport**  | centre     | The scene, rendered live from the editor's orbit camera (from the scene's own camera while playing). Click an entity to select it. The selected entity is drawn with its colour mixed halfway toward white and carries a [gizmo](#moving-rotating-and-scaling-with-the-gizmo). |
| **Scene**     | left, top  | Every entity that has a transform, as a tree: children under their parent, each level in id order, labelled by its name or `#<id>` when it has none; buttons to add and delete. |
| **Assets**    | left, bottom | The project's meshes, textures, scenes, and scripts, with a button per entry to use it, and an Import row. |
| **Inspector** | right, top | The selected entity's properties, editable, and the scene camera. |
| **History**   | right, bottom | The command history: undoable edits oldest first as `tool {args}`, then undone edits greyed out. |
| **Multiplayer** | right, bottom (a tab beside History) | Host on Play, Join, the players with their round-trip times, and a bandwidth sparkline. See [Multiplayer](#multiplayer). |
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
widgets never moves the camera. A left-drag that starts on a
[gizmo](#moving-rotating-and-scaling-with-the-gizmo) handle edits the
selection instead of orbiting.

The orbit camera is the editor's own. The scene's camera, the one the
scene file holds and the game starts from, is left exactly as authored
while you orbit: it is only swapped out for the viewport render, and it
keeps the field of view. **Save never writes the editor view into the
scene.** To look through the scene camera, or to make it match the
viewport, use the Inspector's [Scene camera](#scene-camera) buttons.

## Selecting

Click an entity in the viewport or in the Scene panel to select it.
Clicking empty space in the viewport (the sky, or past the edge of
everything) clears the selection, as does Escape. One entity is selected
at a time; shift-click and box selection are not supported.

A viewport click picks the nearest entity under the pointer that draws a
mesh. Each one is tested as a box: its mesh's bounding box, turned,
scaled, and placed by the entity's world transform (so children are
picked where they appear). The box can be a little larger than the
shape it holds, so a click just beside a sphere's outline can still pick
it. A click is a press and release without moving; a drag orbits and
leaves the selection alone. Picking works while playing too, through the
scene camera. New entities are selected as they
are added, and an entity that is deleted (or that disappears when play
stops) is deselected.

## Moving, rotating, and scaling with the gizmo

![The cube selected in the viewport with its move gizmo: red, green, and blue arrows and plane handles at its centre](images/editor-gizmo.png)

While editing, the selected entity shows a gizmo at its origin. Drag a
handle to change the entity's transform:

| Mode       | Key | Handles | Axes |
|------------|-----|---------|------|
| **Move**   | W   | An arrow per axis (X red, Y green, Z blue), a square per plane, and a circle in the middle that moves in the view plane. | World |
| **Rotate** | E   | A ring per axis, and an outer ring that turns about the view direction. | World |
| **Scale**  | R   | A handle per axis, a square per plane, and the middle for uniform scale. | The entity's own |

Pick the mode with the **Move**, **Rotate**, and **Scale** buttons at the
end of the toolbar, or with W, E, and R while the pointer is over the
viewport and no text field has focus.

**Hold Ctrl** (Cmd on macOS) while dragging to snap: moves go in steps of
0.5 units, rotations in steps of 15°, and scaling in steps of 0.1, each
counted from where the drag started.

The gizmo works in world space and the editor converts the result into
the entity's own transform, local to its parent, so a child of a turned
or scaled parent still moves along the world axis you drag. Scaling uses
the entity's own axes, because a world-axis scale of a turned entity
cannot be stored in a transform.

A drag previews the change live and records one `entity.set_transform`
when you release the button, exactly as an Inspector drag does: the
History panel shows one entry, and Ctrl+Z puts the entity back where the
drag started. A press on a handle never orbits the camera and a release
on one never changes the selection. The gizmo is hidden while playing.

## The scene tree

The Scene panel shows the [hierarchy](scenes.md#children). An entity with
children has an arrow; click it to fold or unfold the branch.

- **Drag a row onto another row** to make the dragged entity a child of
  the one you drop it on. The target row is outlined while you hover. The
  entity keeps its place in the world: its transform is rewritten to be
  local to the new parent. You cannot drop an entity onto itself or onto
  one of its own descendants; that is logged as an error and nothing
  changes.
- **Drop a row on the empty space below the tree**, or right-click a row
  and choose **Unparent**, to make it a top-level entity again, also in
  the same world position.

Each reparent is one recorded `entity.set_parent` call, so Ctrl+Z undoes
it. Save writes children nested under their parent in the scene file.
Dragging is off while playing.

## The inspector

The Inspector shows `Entity #<id>` and these rows for the selected entity:

| Row              | Edit | Tool call recorded |
|------------------|------|--------------------|
| **Name**         | A text field; commits when it loses focus. An empty name removes the name. | `entity.set_name` |
| **Parent**       | The parent's name, or `(none)`. Read-only; reparent in the Scene panel. | |
| **Translation**, **Rotation (°)**, **Scale** | Local to the parent (world space for a top-level entity). Three drag values each. Dragging previews the change live in the viewport and records one edit when the pointer is released; a typed number commits when its field loses focus. Rotation is Euler angles in degrees, applied in X, Y, Z order. | `entity.set_transform` |
| **World position**, **World rotation (°)**, **World scale** | The entity's place in the world, after its parents' transforms. Read-only. | |
| **Mesh**         | A drop-down of every mesh source loaded so far plus `builtin://cube` and `builtin://sphere`, and a text field for any asset source (press Enter). See [asset sources](scenes.md#asset-sources). | `entity.set_mesh` |
| **Colour**       | A colour picker for the mesh's linear RGBA tint, with alpha. Picking previews live and records one edit when the pointer is released. Only shown for entities that draw a mesh. | `entity.set_color` |
| **Script**       | The path of the entity's Rhai script, relative to the project (`scripts/name.rhai`); commits when the field loses focus. Clearing it removes the script. The script's latest compile or runtime error, if any, is shown in red below the grid. | `script.attach` |
| **Audio**        | The entity's sound, an `.ogg` or `.wav` path; commits when the field loses focus, and clearing it removes the sound. **▶ Play** previews it (not recorded). Once the entity has a sound, a **Volume** drag value and **Loop**, **Spatial**, and **Autoplay** checkboxes appear. See [Audio](audio.md). | `entity.set_audio` |

Values are refreshed from the world every frame except for the field you
are typing in. Every commit is one entry in the History panel, so a drag
of any length undoes in one step.

### Physics

Below the entity rows, a **Physics** section edits the entity's
[rigid body and collider](physics.md):

| Row              | Edit | Tool call recorded |
|------------------|------|--------------------|
| **Body**         | A drop-down: `(none)`, `dynamic`, `kinematic`, or `fixed`. Choosing commits at once, keeping the mass. | `physics.set_body` |
| **Mass (kg)**    | A drag value, shown for dynamic bodies. | `physics.set_body` |
| **Collider**     | A drop-down: `(none)`, `cuboid`, `ball`, or `capsule`. A new shape starts sized to fit the unit cube (half extents 0.5, radius 0.5, or a capsule of half height and radius 0.25). | `physics.set_collider` |
| **Half extents**, **Radius**, **Half height** | The shape's size in the entity's own units (multiplied by its scale), as drag values. | `physics.set_collider` |
| **Friction**, **Restitution** | Drag values; restitution is 0 to 1. | `physics.set_collider` |

A drag value commits one edit when you release it or, when typed, when
its field loses focus.

While a selected top-level entity has a collider, the viewport draws the
collider over the scene as a **translucent green shape** at the size the
simulation uses: a box for a cuboid, a ball for a ball, and the
ellipsoid filling a capsule's bounding box for a capsule. Compare it with
the mesh to check that the collider fits. Colliders on child entities are
not drawn, since physics ignores them.

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
  `sphere`, and select it. When an entity is selected, the new one is
  added as its child, still at the orbit target in the world. Press
  Escape first to add a top-level entity.
- **+ From asset** adds an entity drawing any [asset
  source](scenes.md#asset-sources) typed in the field next to it (the
  hint is `assets/x.gltf#0`), named after the file (`assets/crate.gltf#1`
  gives `crate`). Enter in the field does the same. Like the buttons, it
  adds a child of the selection when there is one.
- **Delete** removes the selected entity and all its children, as do the
  Delete and Backspace keys while no text field has focus.

Adding loads the mesh with `asset.load` (not recorded: loading changes no
entity) and spawns with one recorded `entity.spawn`; deleting is one
recorded `entity.despawn`. Undoing a delete brings the entity and its
children back under the same ids.

## The Assets panel

The Assets panel lists what the project directory contains, so you can
use a file without typing its asset source. It scans three directories,
including subdirectories:

| Directory  | Files                        | Entries |
|------------|------------------------------|---------|
| `assets/`  | `.gltf`, `.glb`              | One **mesh** per triangle primitive (`assets/x.gltf`, `assets/x.gltf#1`, ...) and one **texture** per image (`assets/x.gltf#image=0`). |
| `assets/`  | `.png`, `.jpg`, `.jpeg`      | One **texture**. |
| `assets/`  | `.ogg`, `.wav`               | One **audio** entry, listed under **Sounds**. |
| `scenes/`  | `.scene.ron`                 | One **scene**. |
| `scripts/` | `.rhai`                      | One **script**. |

Other files (a glTF's `.bin` buffer, `src/`, `target/`) are not listed; a
glTF that does not parse is skipped with a warning in the Log. Entries
are grouped by kind under collapsible headings and labelled by their
[asset source](scenes.md#asset-sources) or path; hovering shows the file.
The **Filter** box narrows the list to entries whose source or path
contains the text, ignoring case.

Each entry has buttons for what it can do. Those that need a selected
entity are greyed out without one, and those that change the scene are
disabled while playing:

| Kind    | Button     | Does | Tool call recorded |
|---------|------------|------|--------------------|
| mesh    | **Spawn**  | Adds an entity drawing the mesh at the orbit camera's target, named after the file, and selects it (like **+ From asset**, it becomes a child of the selection when there is one). | `entity.spawn` |
| mesh    | **Assign** | Makes the selected entity draw this mesh; colour and texture are kept. | `entity.set_mesh` |
| texture | **Assign** | Gives the selected entity this base-colour texture (it must draw a mesh). | `entity.set_texture` |
| script  | **Attach** | Attaches the script to the selected entity, replacing any it had. | `script.attach` |
| audio   | **▶ Play** | Plays the sound once at full volume, as a preview. It changes nothing, so it works with no selection and while playing. See [Audio](audio.md). | `audio.play` (not recorded) |
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

Only recorded tool calls appear: edits from the Inspector, the viewport
gizmo, the Scene panel, the Assets panel, and the Console. Queries, renders, asset loads,
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
  Rhai scripts run every step, then [physics](physics.md): bodies are
  created from the entities' rigid bodies and colliders, and dynamic ones
  fall and collide. While playing, the Play button is shown
  selected and the viewport renders from the scene camera, as the game
  would; Stop returns to the orbit camera's view.
- **Pause** suspends stepping; click it again (or Play) to resume. No time
  builds up while paused.
- **Stop** restores the scene exactly as it was at Play: every entity
  comes back under the id it had, so the selection and the history stay
  valid, and the clock is restored. The physics simulation is torn down;
  the next Play starts it again from the components.

While playing, editing is disabled: the Inspector is read-only (it still
shows the live values), the gizmo is hidden, the Scene panel's add and
delete controls, the Assets panel's buttons, undo
and redo, the Delete key, and File > Open and Save are greyed out or
ignored. Selecting still works. Console lines still run, but they are not
recorded, since Stop discards whatever they changed, and `history.*`
tools are refused until you stop. The toolbar, `sim.play`, `sim.pause`,
and `sim.stop` from the Console, and an agent's calls are all the same
play mode.

Only what a scene file holds survives a play session: entities with a
transform (their transform, mesh, colour, texture, name, script, parent,
rigid body, and collider), the camera, and the light. Playing is not an edit, so it does not mark the
scene unsaved.

The editor does not run your project's compiled Rust systems; only the
engine's own systems (scripts and physics) run under Play. In a new
project, Play drops the starter scene's stack of crates onto the ground. To see
the starter scene's moon move in the editor, either attach the orbit
script from [Scripting](scripting.md) to it, or start the editor with
`--demo-orbit`, which registers a stand-in system that moves the entity
named `moon` on a circle of radius 1.6 at height 0.5, one radian per
simulated second, like the starter game does.

## Multiplayer

The editor plays as a [multiplayer](multiplayer.md) server. Without
hosting, Play is solo, as the game run alone is: the server is yours
alone, so the project's `scripts/server.rhai` greets you as player 1 (in
a new project, a cube of your own appears).

The **Multiplayer** panel (a tab beside History):

- **Host on Play** and a **port** (default 7777): with the box ticked,
  Play hosts a session (`net.host`) instead. The editor is the server:
  run the game in another terminal with `cargo run -- --join
  127.0.0.1:7777 --cert target/shin-server-cert.der` (the editor saves
  the certificate there in the project) and it sees the editor's world,
  with a cube of its own that its keys move.
- An address and **Join**: joins another server (`net.join`) and starts
  playing as its client; the server's replicated entities appear in the
  viewport. Without **Trust any server**, Join trusts the certificate a
  host in this project saved; tick it to join a server whose certificate
  you do not have (development only).
- While in a session: the players with their round-trip times (`(you)`
  marks your own), bytes in and out per second, and a sparkline of the
  bytes per tick over the last second.

The status bar says `Offline`, `Hosting on port 7777 · 1 joined · tick
…`, `Joining …`, `Connected to … as player 2, 1 ms`, or why the
connection closed. **Stop** ends hosting or leaves the server, then
restores the scene as usual; the editor is solo again. The panel's
controls are greyed out while playing, so a session starts from editing.

These are the `net.*` tools, so `net.status` in the Console shows the
same numbers.

## Screenshots

```sh
shin-editor mygame --screenshot editor.png
```

renders one full editor frame of the project, 1600 × 900 pixels, without
opening a window, writes it to the PNG, and exits. The editor's own tests
use it, and the image at the top of this page was made with it.

```sh
shin-editor mygame --screenshot editor-gizmo.png --select cube
```

selects the entity named `cube` first, so the frame shows its highlight
and gizmo, as in the [gizmo](#moving-rotating-and-scaling-with-the-gizmo)
image. An unknown name is an error.
