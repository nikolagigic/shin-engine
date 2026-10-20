# Multiplayer

Multiplayer is being built in this milestone. This page describes the
model and the parts that exist today. A game can already run as a
server ([Hosting](#hosting)) and as a client ([Joining](#joining)), but
until the network transport lands the two can only talk inside one
process (see [Coming in this milestone](#coming-in-this-milestone)).

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
loopback transport used by the tests; the `shin-net` crate provides the
network transport, QUIC (see [Over the network](#over-the-network)).

## Hosting

The server is the game itself: your project's binary, running its own
systems, with a network server attached. There are three ways to run it,
all the same server underneath:

```sh
cargo run -- --serve            # dedicated: no window
cargo run -- --host             # listen server: you play in the window and host
cargo run                       # solo: just you
```

Both server modes take `--port N` (default 7777). `shin serve <dir>`
is a shortcut for `cargo run -- --serve` in the project directory.

A **dedicated** server opens no window. It loads the project and its
entry scene, starts play mode, and steps the simulation at the fixed rate
(60 ticks per second by default) by the wall clock until you press
Ctrl-C. It logs where it listens, every player who joins or leaves, and
every 10 seconds the tick and player count:

```text
INFO  serving `mygame` on port 7777; press Ctrl-C to stop
INFO  net: server running at 60 ticks per second, a snapshot every 3 tick(s)
INFO  net: `ada` joined as player 1 (peer 1)
INFO  net: tick 600, 1 player(s)
```

A **listen server** (`--host`) is the normal game window with the server
running in the same engine. The person at the window is player one and
needs no connection; everyone else joins over the network.

Each tick, after all your systems, the server:

1. answers each joining client's `Hello` with a `Welcome` (its player id,
   the server's current tick, and the timestep), or turns it away if its
   protocol version differs;
2. forgets players who said `Goodbye` or disconnected;
3. every third tick (ticks 0, 3, 6, ...: 20 snapshots a second) captures
   the replicated entities and sends the snapshot to every player.

When a dedicated server stops on its own (a system sets `ExitRequest`),
it sends every player a `Goodbye` first.

In Rust, the pieces are in `shin::net`: `NetServer` (a resource holding
the transport and players), `net_server_system` (add it last to the fixed
schedule), and `run_server`, which does both on a headless engine and runs
the dedicated loop. The template's `src/main.rs` shows both modes.

## Joining

```sh
cargo run -- --join 127.0.0.1:7777 --insecure
```

`--join <host:port>` opens the game's window as a **client** of the
server at that address. It also needs `--insecure` or `--cert <path>` to
say which server certificate to trust (see
[Over the network](#over-the-network)). The client says `Hello` (its protocol version
and a player name, your login name), the server answers `Welcome` with a
player id, and from then on the client applies the server's snapshots:
replicated entities appear, move, and disappear as they do on the
server. The client's own entities, such as the level it loads from the
entry scene, stay as they are.

Entities that come from the server are marked `RemoteEntity` on the
client. They belong to the server: **physics never simulates them and
their scripts do not run**, even if a body, collider, or script is added
to them locally. What you see is exactly what the server sent.

When the server says `Goodbye` (it shut down, or turned the client away
because the protocol versions differ) or the connection drops, the
client is disconnected and replicated entities stay where they were.

If the server does not answer within five seconds, or its certificate is
not the one trusted, `--join` exits with an error saying so.

In Rust, the pieces are in `shin::net`: `NetClient` (a resource holding
the transport, the session state, and the snapshot buffer),
`net_client_system` (add it to the fixed schedule), and
`net_interpolate_system` (add it to the frame schedule). The engine
applies snapshots itself, after the fixed steps of each frame
(`apply_pending`), because spawning entities can mean uploading meshes
to the GPU. The template's `src/main.rs` shows the setup.

```rust
use shin::net::{NetClient, net_client_system, net_interpolate_system};

schedule.add_fixed("net_client", net_client_system);
schedule.add_frame("net_interpolate", net_interpolate_system);
game.resources.insert(NetClient::new(transport, "ada"));
```

`NetClient::state()` says where the session is: `Connecting`,
`Connected { player, server_tick }`, or `Disconnected(reason)`.

## Interpolation

The server sends a snapshot every third tick, and on a real network they
arrive unevenly. Drawing each one the moment it arrives would make
remote entities stutter. Instead the client shows the world slightly in
the past and moves entities smoothly between snapshots:

- Snapshots go into a buffer in tick order. Late ones (older than the
  one shown) and duplicates are dropped.
- The client shows the newest snapshot that is at least the
  **interpolation delay** (2 ticks by default) older than the newest it
  has received. Waiting means the next snapshot is usually already there.
- Every frame, each remote entity is placed between the shown snapshot
  and the next one, by how far the client has played since the shown
  tick, including the fraction of a tick the frame lands on. Position and
  scale move in a straight line; rotation turns along the shortest arc.

For example, with snapshots at tick 10 (x = 0) and tick 12 (x = 2), the
entity is at x = 1 at tick 11.

If a snapshot is lost or late, the entity **holds** at the newest pose it
has rather than guessing ahead, and moves on from there when the next
snapshot arrives, so it never jumps. Despawns in snapshots the client
skipped still happen.

`NetClient::with_delay(transport, name, ticks)` changes the delay. A
longer delay hides more network jitter but shows the world further in
the past; `0` shows each snapshot as soon as it arrives, with no
smoothing.

## Over the network

Both server modes listen with **QUIC** over UDP, from the `shin-net`
crate that every new project depends on. QUIC carries reliable messages
(`Hello`, `Welcome`, `Goodbye`) on one ordered stream per player and
snapshots and inputs as unreliable datagrams, all encrypted with TLS 1.3.
A snapshot too big for one datagram (about 1200 bytes) is sent on the
stream instead, so large worlds still replicate.

**Ports.** The server listens on UDP port 7777 on every network interface
(`--port N` changes it). Players on other machines need that UDP port
reachable: open it in the firewall, and on a home network forward it on
the router. There is no NAT traversal or relay.

**Certificates.** QUIC needs a certificate, so the server makes a fresh
self-signed one for `localhost` every time it starts. It logs the
certificate's SHA-256 fingerprint and saves it to
`target/shin-server-cert.der` in the project, with the command a client
joins with:

```text
INFO  net: QUIC server listening on 0.0.0.0:7777; certificate SHA-256 fingerprint 3F:A2:...
INFO  clients join with `cargo run -- --join <this machine>:7777 --cert /home/you/mygame/target/shin-server-cert.der` (or `--insecure` instead of `--cert`, for development only)
```

A client trusts the server in one of two ways:

- `--cert <path>`: exactly the certificate in that file. On the same
  machine, point it at the server's `target/shin-server-cert.der`;
  elsewhere, copy the file over. The file changes every time the server
  starts.
- `--insecure`: any certificate. The connection is still encrypted, but
  nothing checks that the server is the one you meant, so use it only
  for development on a network you trust. The client logs a warning.

Two processes on one machine, the second showing the server's crates
falling:

```sh
cargo run -- --serve                                # terminal 1
cargo run -- --join 127.0.0.1:7777 --insecure       # terminal 2
cargo run -- --join 127.0.0.1:7777 --cert target/shin-server-cert.der   # or this
```

These certificates are for development; real certificate authorities,
relays, and browser transports are not part of this milestone.

In Rust, `shin_net::QuicTransport::listen(addr)` makes a server transport
and `QuicTransport::connect(addr, &trust)` a client one, where `trust` is
`ServerTrust::Cert(der_bytes)` or `ServerTrust::Insecure`. Both are
`Send`, so they go wherever a transport does, such as
`NetServer::new(Box::new(transport), ...)`. The template's
`server_transport` and `client_transport` functions are the examples.


## Tools

`net.status` reports the engine's multiplayer role. Today that is always
`{ "role": "none" }`.

## Coming in this milestone

- Player inputs sent to the server, and script hooks for players joining
  and leaving.
- Host and Join in the editor, and `net.*` tools to host, join, and
  inspect a session.
- Client-side prediction for the player's own entity.
- Delta snapshots that send only what changed.
