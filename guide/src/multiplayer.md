# Multiplayer

Multiplayer is being built in this milestone. This page describes the
model and the parts that exist today; hosting and joining a game are not
available yet (see [Coming in this milestone](#coming-in-this-milestone)).

## The model

Shin multiplayer is **server-authoritative snapshot replication**:

- One engine is the **server**. It runs the game: scripts, physics, and
  every system. It owns the truth.
- Every other engine is a **client**. It does not simulate replicated
  entities itself; it sends the server its player's input and draws what
  the server tells it.
- Every few ticks the server captures a **snapshot** of the replicated
  entities and sends it to every client. A client applies each snapshot
  to its own world, spawning, moving, and despawning entities to match.

Because the server decides everything, clients cannot cheat by editing
their world, and there is a single place where game logic runs.

## Replicated entities

Only entities you mark are sent. In Rust, add the `Replicated` component
from `shin::net`:

```rust
use shin::net::{Owner, PlayerId, Replicated};

world.spawn((transform, mesh_instance, Replicated));
// Optionally: the player who controls it.
world.insert_one(player_entity, Owner(PlayerId(1)))?;
```

Mark each entity you want replicated; children of a replicated entity are
not replicated unless they carry `Replicated` too. A replicated child of
a replicated parent keeps its parent on the client; a replicated child of
an entity that is not replicated arrives as a root at its world position.

Each replicated entity gets a **network id** (`NetId`) the first time it
is captured. Server and clients use the same id for the same entity, and
an id is never reused in a session. Ids live only for the session: scene
files do not store them, and `Replicated` is not saved in scenes yet.

`entity.list` shows both, so you can check what is networked:

```json
{ "entity": 4294967296, "name": "cube", "replicated": true, "net_id": 2, ... }
```

`net_id` is `null` until the entity's first snapshot.

## What is replicated

| Replicated                                  | Not replicated                    |
|---------------------------------------------|-----------------------------------|
| Transform (local to the parent)             | Scripts                           |
| Parent, when the parent is replicated       | Audio sources                     |
| Name                                        | Physics bodies and colliders      |
| Mesh, colour, and texture (by asset source) | Entities without `Replicated`     |
| Owner (`PlayerId`)                          | Camera and light                  |

Scripts, sounds, and physics run on the server; clients see their effect
through the transforms. Meshes and textures are sent by their
[asset source](scenes.md#asset-sources), such as `builtin://cube` or
`assets/cube.gltf`, so a client must have the same project files: it loads
each source from its own `assets/` directory.

When an entity is despawned on the server, or loses `Replicated`, the next
snapshot lists it as removed and clients despawn it together with its
children. A snapshot never despawns entities it simply does not mention.

## Messages and transports

Peers exchange a small set of messages, encoded compactly in binary:
`Hello` (a client joining, with its protocol version and name), `Welcome`
(the server's reply: the player id, current tick, and timestep),
`Snapshot`, `Input`, and `Goodbye`. A client whose protocol version
differs from the server's is turned away with a `Goodbye` that says so.

Messages travel over a **transport**. The engine ships an in-process
loopback transport used by the tests; the network transport comes later in
this milestone.

## Tools

`net.status` reports the engine's multiplayer role. Today that is always
`{ "role": "none" }`.

## Coming in this milestone

- A server that accepts players and broadcasts snapshots.
- A client that joins with `--join`, applies snapshots, and smooths remote
  entities between them (interpolation).
- Player inputs sent to the server, and script hooks for players joining
  and leaving.
- A real network transport (QUIC).
- Host and Join in the editor, and `net.*` tools to host, join, and
  inspect a session.
- Client-side prediction for the player's own entity.
- Delta snapshots that send only what changed.
