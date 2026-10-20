# Multiplayer

Multiplayer is being built in this milestone. This page describes the
model and the parts that exist today: a game runs as a server
([Hosting](#hosting)) and as a client ([Joining](#joining)) over the
network ([Over the network](#over-the-network)), and players control
their own entities ([Players and input](#players-and-input)). What is
still to come is listed [at the end](#coming-in-this-milestone).

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
children. Lost snapshots do not lose a removal: the next full snapshot
resyncs (see [Bandwidth](#bandwidth)).

## Messages and transports

Peers exchange a small set of messages, encoded compactly in binary:
`Hello` (a client joining, with its protocol version and name), `Welcome`
(the server's reply: the player id, current tick, and timestep),
`Snapshot`, `Input`, and `Goodbye`. A client whose protocol version
differs from the server's is turned away with a `Goodbye` that says so.

Messages travel over a **transport**. The engine ships an in-process
loopback transport used by the tests (it can simulate latency and loss;
see [Contributing](contributing.md#multiplayer-tests)); the `shin-net` crate provides the
network transport, QUIC (see [Over the network](#over-the-network)).

## Hosting

The server is the game itself: your project's binary, running its own
systems, with a network server attached. There are three ways to run it,
all the same server underneath:

```sh
cargo run -- --serve            # dedicated: no window
cargo run -- --host             # listen server: you play in the window and host
cargo run                       # solo: a server only you play on
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
needs no connection; everyone else joins over the network. **Solo** play
is the same with no network: the game is a server whose only player is
you (`NetServer::solo()`), so the scripts that run a multiplayer game run
a single-player one too (see [Players and input](#players-and-input)).
The template runs `--mcp` the same way.

A server tick has two halves. **First**, before your systems, the server:

1. answers each joining client's `Hello` with a `Welcome` (its player id,
   the server's current tick, and the timestep), or turns it away if its
   protocol version differs;
2. forgets players who said `Goodbye` or disconnected;
3. takes in the input every player sent (see
   [Players and input](#players-and-input)).

Then your systems and scripts run, seeing this tick's players and input.
**Last**, every third tick (ticks 0, 3, 6, ...: 20 snapshots a second),
it captures the replicated entities and sends the snapshot to every
player, as a delta of what changed where it can (see
[Bandwidth](#bandwidth)).

When a dedicated server stops on its own (a system sets `ExitRequest`),
it sends every player a `Goodbye` first.

In Rust, the pieces are in `shin::net`: `NetServer` (a resource holding
the transport and players), `net_server_input_system` (the first half:
add it first to the fixed schedule), `net_server_system` (the second
half: add it last), and `run_server`, which inserts the server and adds
`net_server_system` on a headless engine and runs the dedicated loop. A
schedule without `net_server_input_system` still works, with input
reaching the game a tick later. The template's `src/main.rs` shows every
mode.

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

## Players and input

A player must be able to do something. Every tick, a client sends the
server its **input actions** (the ones bound in `shin.toml`; see
[Input actions](scripting.md#input-actions)) in an `Input` message. The
message lists every action that is not at rest (held, off-centre, or just
pressed), so each one is complete on its own: it travels unreliably, and
a lost one is corrected by the next.

The server keeps each player's newest input in the `PlayerInputs`
resource, updated in the first half of every tick:

- an action stays as the newest message says until a newer one arrives;
  messages that arrive late are ignored;
- `just_pressed` lasts exactly one tick, and a press is not lost when two
  messages arrive in one tick or the message carrying it is lost;
- the host's own player (on a listen server or solo) sends nothing: its
  input is the local `Actions` resource.

A client knows which player it is from the `LocalPlayer` resource, which
appears once the server welcomes it.

### Scripts and players

Player logic lives in the **server script**, `scripts/server.rhai` in the
project. It belongs to no entity. Wherever the game is the server (solo,
`--host`, `--serve`), the engine calls its `on_player_join(id)` once when
a player joins and `on_player_leave(id)` once when they leave, before
the entity scripts of that tick. Both are optional. Joining clients never
run it.

```rhai
// scripts/server.rhai
fn on_player_join(id) {
    spawn_entity()
        .name(`player ${id}`)
        .mesh("assets/cube.gltf")
        .at(vec3(1.5 * (id - 1), 0.5, 2.5))
        .script("scripts/player.rhai")
        .replicated(true)
        .owner(id);
}

fn on_player_leave(id) {
    despawn_owned(id);
}
```

The cube's own script reads its owner's input:

```rhai
// scripts/player.rhai
fn update(entity, dt) {
    let p = entity.transform.translation;
    p.x += player_input(entity.owner, "move_x") * 3.0 * dt;
    if player_input_pressed(entity.owner, "jump") { p.y += 4.0 * dt; }
    entity.transform.translation = p;
}
```

[Scripting](scripting.md#players) lists the functions. This is what a
new project does: each player who joins gets a cube of their own in
front of the centre cube, which they move with the arrow keys and lift
with Space, and their cube goes when they leave. Played solo, you are
player 1 and the same scripts give you your cube.

Stopping play (in the editor, or `sim.stop`) puts the world back and
forgets who joined, so the next play greets the players again.

In Rust, read `PlayerInputs` from a system on the server:

```rust
use shin::net::{Owner, PlayerInputs};

fn move_players(world: &mut World, res: &mut Resources) {
    let inputs = res.expect::<PlayerInputs>();
    for (owner, transform) in world.query_mut::<(&Owner, &mut Transform)>() {
        transform.translation.x += inputs.axis(owner.0, "move_x") * 0.05;
    }
}
```

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

## Bandwidth

The server sends each client **deltas**: only the entities that changed
since the newest snapshot that client has acknowledged, plus the ones
removed since. A client acknowledges in every input it sends, and
rebuilds the full picture by applying the delta to the snapshot it
holds. An entity that moved, turned, or changed anything else is sent
whole; one that stood still costs nothing.

A client gets a **full** snapshot instead when it joins, when the
snapshot it acknowledged is older than the server keeps, and at least
once a second (every 60 ticks; `ServerOptions::full_snapshot_every`
changes it, and `1` turns deltas off). Lost deltas therefore never leave
a client wrong for long: the next full snapshot lists everything, and the
client despawns whatever it still shows that the full snapshot lacks.

A typical prop (a name, a transform, a mesh source such as
`builtin://cube`, and a colour) takes about 80 bytes in a full snapshot:

| Replicated entities | Bytes per full snapshot | Per client at 20 snapshots/s |
|--------------------:|------------------------:|-----------------------------:|
| 100                 | 7,799                   | about 156 KB/s               |
| 1000                | 79,774                  | about 1.6 MB/s               |

With deltas, what counts is how much moves. With 1000 props of which 5 %
move every tick, a delta is 3,993 bytes against 79,772 for the full
snapshot: **20 times smaller**, about 80 KB/s per client instead of
1.6 MB/s. A scene at rest costs a few bytes per snapshot, plus the full
snapshot once a second.

Positions and scales travel as 32-bit floats, exactly. Rotations travel
as "smallest three" quaternions in 7 bytes (against 12 for three Euler
angles): the largest component is dropped and the other three are stored
in 16 bits each. A rotation arrives within **0.004°** of the server's
(the worst case measured over a million random rotations is 0.0037°);
the identity and quarter turns about one axis arrive exactly. That is
far below what a pixel can show: in the engine's own tests the client's
frame matches the server's pixel for pixel.

Shorter names and mesh sources make entities smaller; a child entity
adds a few bytes for its parent. Snapshots bigger than one datagram (about
1200 bytes) travel on the reliable stream (see
[Over the network](#over-the-network)); most deltas fit in one. The
numbers come from `cargo bench -p shin-engine --bench net_snapshot`,
which also times encoding and decoding (tens of microseconds for 1000
entities). On a running server, `net.status` reports the live numbers.

## Tools

`net.status` reports the engine's multiplayer role. An engine that is not
networked reports `{ "role": "none" }`. A server reports its tick, the
number of players, and the snapshot traffic to all remote players over
the last second: what full snapshots would have cost (before), what was
actually sent (after), and how many times smaller that is:

```json
{
  "role": "server",
  "tick": 5400,
  "players": 2,
  "bandwidth": {
    "full_bytes_per_s": 3190960.0,
    "sent_bytes_per_s": 172400.0,
    "compression_ratio": 18.5
  }
}
```

`compression_ratio` is `null` until something has been sent.

## Coming in this milestone

- Host and Join in the editor, and `net.*` tools to host, join, and
  inspect a session.
- Client-side prediction for the player's own entity.
