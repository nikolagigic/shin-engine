# Physics

Shin simulates rigid bodies with [rapier3d](https://rapier.rs): things
fall, stack, slide, and bounce off each other. Physics is two components
you put on an entity, a system that runs in the fixed step, and four
tools.

A new project shows it at once: its starter scene has a stack of five
crates hanging above the ground. Press **Play** in the editor and they
drop into a stack; **Stop** puts them back.

## Bodies and colliders

An entity takes part in physics when it has a **rigid body**, a
**collider**, or both.

The **rigid body** says how the entity moves:

| Kind        | Moves how |
|-------------|-----------|
| `dynamic`   | Moved by gravity and contacts. The simulation writes its position and rotation back into its transform every step. Has a `mass` in kilograms (default 1). |
| `kinematic` | Follows its transform, which your scripts or systems set, and pushes dynamic bodies out of the way. Gravity and contacts do not move it. |
| `fixed`     | Never moves. Use it for the ground, walls, and level geometry. |

The **collider** is the entity's shape for contacts and ray casts:

| Shape     | Sized by |
|-----------|----------|
| `cuboid`  | Half extents `(hx, hy, hz)`: a box from `-hx` to `hx` along X, and so on. |
| `ball`    | A radius. |
| `capsule` | `half_height` and `radius`: a cylinder along the entity's local +Y, `2 × half_height` long, capped by half-spheres of `radius`. |

A collider also has **friction** (0 or more, default 0.5; 0 is ice) and
**restitution**, its bounciness (0 to 1, default 0; 1 bounces back to the
height it fell from).

A collider without a body is fixed in place, like a `fixed` body. A body
without a collider has its mass but touches nothing.

## Sizes, units, and scale

Units are metres, kilograms, and seconds. Gravity is `(0, -9.81, 0)` by
default; change it with `physics.gravity`.

Collider sizes are in the entity's **own units** and are multiplied by
its transform's scale, just like its mesh. A `cuboid((0.5, 0.5, 0.5))`
fits the built-in unit cube at any scale: on a crate scaled to 0.4 it is
a 0.4 m box, and on a ground slab scaled `(20, 0.5, 20)` it is a 20 m by
0.5 m by 20 m slab. A ball uses the entity's largest scale axis; a capsule
uses the Y scale for its length and the larger of the X and Z scales for
its radius. The built-in sphere mesh has radius 1, so a sphere entity
wants `ball(1.0)`.

## In a scene file

Bodies and colliders are saved with the entity:

```ron
(
    name: "crate",
    transform: (translation: (0.0, 2.0, 0.0), scale: (0.4, 0.4, 0.4)),
    mesh: "builtin://cube",
    body: (kind: dynamic, mass: 1.0),
    collider: (shape: cuboid((0.5, 0.5, 0.5)), friction: 0.5, restitution: 0.0),
),
```

See [Scenes](scenes.md#body-and-collider) for every field.

## When physics runs

The physics system runs in the fixed step (60 times a simulated second by
default), after scripts. Each step it:

1. creates a body for every top-level entity with a rigid body or
   collider it has not seen yet, and rebuilds one whose body, collider,
   or scale changed;
2. moves a body to its entity's transform if something else (a script, a
   tool, the inspector) moved that entity since the last step: kinematic
   bodies glide there, pushing what is in the way; dynamic and fixed ones
   jump there;
3. advances the simulation one step;
4. writes each dynamic body's new position and rotation into its
   entity's transform (scale is left alone).

In the editor, and in any engine with a play mode (`shin-mcp`, a game
under `--mcp`), physics runs **only while playing**. Bodies are created
from the components when Play starts, and torn down at Stop, which
restores every transform. Pause stops the simulation. A game running in
its own window has no play mode, so physics runs from the first frame;
the starter game's `src/main.rs` adds `physics_system` to its schedule.

## Limits

- Bodies and colliders attach to **top-level entities only**. One on a
  child entity is ignored (a warning is logged once); move the entity to
  the top level to simulate it. Children of a simulated entity still move
  with it.
- No joints, character controllers, trigger or contact events, or
  continuous collision detection yet. Very fast, thin objects can pass
  through each other.
- One collider per entity, of the three shapes above. Mesh-shaped
  colliders are not supported.
- Velocities are not exposed: to launch something, move it with a
  kinematic body or set its transform.

## Tools

| Tool                   | What it does |
|------------------------|--------------|
| `physics.set_body`     | Gives an entity a rigid body (`kind`: `dynamic`, `kinematic`, or `fixed`; `mass` for dynamic bodies), or removes it with `kind: null`. Undoable. |
| `physics.set_collider` | Gives an entity a collider (`shape`: `{"cuboid": [hx, hy, hz]}`, `{"ball": r}`, or `{"capsule": {"half_height": h, "radius": r}}`; optional `friction` and `restitution`), or removes it with `shape: null`. Undoable. |
| `physics.gravity`      | Sets the gravity vector, or reports it when called without one. Setting it is undoable. Gravity is not saved in scene files. |
| `physics.raycast`      | Casts a ray from `origin` along `dir` up to `max` metres against every collider at its current transform, in edit or play mode, and returns the nearest hit: the `entity`, its `name`, the `point`, and the `distance`. |

`entity.list` reports each entity's `body` and `collider`. For example,
in a new project (whose ground has a fixed collider and whose mesh 0 is
the cube), an agent can drop another crate from three metres and find
where it landed:

```text
entity.spawn          {"mesh": 0, "name": "crate", "translation": [0, 3, 0], "scale": [0.4, 0.4, 0.4]}
physics.set_body      {"entity": <id>, "kind": "dynamic"}
physics.set_collider  {"entity": <id>, "shape": {"cuboid": [0.5, 0.5, 0.5]}}
sim.play              {}
sim.step              {"steps": 120}
physics.raycast       {"origin": [0, 10, 0], "dir": [0, -1, 0]}
sim.stop              {}
```

See the [Tools reference](reference/tools.md) for the full schemas.
