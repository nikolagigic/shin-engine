# Multiplayer

A Shin game can be played by several people at once: one engine runs the
game as a **server**, and the others join it as **clients** over the
network. Every new project is multiplayer-ready: the same game runs solo,
as a server, or as a client, chosen by a command-line flag.

This chapter goes from the model to hosting, joining, players and their
input, the scripting hooks, tools for agents, bandwidth, and the current
limits.

## Try it

Two terminals in a new project (`shin new mygame`, see
[Projects](projects.md)):

```sh
cargo run -- --serve                                # terminal 1: the server
cargo run -- --join 127.0.0.1:7777 --insecure       # terminal 2: a player
```

The second terminal opens a window on the starter scene with a cube
that is yours, spawned by the server, which you walk with the arrow keys
and jump with Space. It moves the moment you press a key: the client
[predicts](#prediction) it. Run the join command again in a third
terminal and both windows show both cubes.

The engine repository has a smaller example, `net_arena`: a floor, a
coin, and a cube per player; touch the coin to score.

```sh
cargo run -p shin-net --example net_arena -- --serve
cargo run -p shin-net --example net_arena -- --join 127.0.0.1:7777 --insecure
cargo run -p shin-net --example net_arena -- --join 127.0.0.1:7777 --insecure --latency 100
```

`--latency <ms>` delays everything that client sends and receives by
that much each way, to feel [prediction](#prediction) on one machine:
your own cube stays immediate, the others stay smooth.

Each player's cube is named `player <id>: <score>`, so the score reaches
every client inside a replicated name, and each client logs its own
score when it changes.

## The model

Shin multiplayer is **server-authoritative snapshot replication**:

- One engine is the **server**. It runs the game: scripts, physics, and
  every system. It owns the truth.
- Every other engine is a **client**. It does not simulate replicated
  entities itself; it sends the server its player's input and draws what
  the server tells it.
- Every third tick (20 times a second) the server captures a
  **snapshot** of the replicated entities and sends it to every client.
  A client applies each snapshot to its own world, spawning, moving, and
  despawning entities to match, and smooths the motion between
  snapshots.

Because the server decides everything, clients cannot cheat by editing
their world, and there is a single place where game logic runs. A game
played alone is a server too, with you as its only player, so the code
that runs a multiplayer game runs a single-player one unchanged.

## Replicated entities

Only entities you mark are sent. In a script, call `.replicated(true)`
on `spawn_entity()` (see [Scripting hooks](#scripting-hooks)); in Rust,
add the `Replicated` component from `shin::net`:

```rust
use shin::net::{Owner, PlayerId, Replicated};

world.spawn((transform, mesh_instance, Replicated));
// Optionally: the player who controls it.
world.insert_one(player_entity, Owner(PlayerId(1)))?;
```

Mark each entity you want replicated; children of a replicated entity
are not replicated unless they carry `Replicated` too. A replicated child
of a replicated parent keeps its parent on the client; a replicated child
of an entity that is not replicated arrives as a root at its world
position.

Each replicated entity gets a **network id** (`net_id`) the first time
it is captured. Server and clients use the same id for the same entity,
and an id is never reused in a session. Ids live only for the session:
scene files do not store them, and `Replicated` is not saved in scenes
yet.

| Replicated                                  | Not replicated                    |
|---------------------------------------------|-----------------------------------|
| Transform (local to the parent)             | Scripts                           |
| Parent, when the parent is replicated       | Audio sources                     |
| Name                                        | Physics bodies and colliders      |
| Mesh, colour, and texture (by asset source) | Entities without `Replicated`     |
| Owner (the player who controls it)          | Camera and light                  |
| Mover, with its state (see [Prediction](#prediction)) |                         |

Scripts, sounds, and physics run on the server; clients see their effect
through the transforms. Meshes and textures are sent by their
[asset source](scenes.md#asset-sources), such as `builtin://cube` or
`assets/cube.gltf`, so a client must have the same project files: it
loads each source from its own `assets/` directory. A mesh added from
code without a source is not sent (the server logs a warning).

When an entity is despawned on the server, or loses `Replicated`, the
next snapshot lists it as removed and clients despawn it with its
children. A snapshot never despawns entities it does not mention, so a
client's own entities (such as the level it loads from the entry scene)
stay as they are.

## Hosting

The server is the game itself: your project's binary, running its own
systems, with a network server attached. There are three ways to run it,
all the same server underneath:

```sh
cargo run -- --serve            # dedicated: no window
cargo run -- --host             # listen server: you play in the window and host
cargo run                       # solo: a server only you play on
```

Both server modes take `--port N` (default 7777). `shin serve <dir>` is
a shortcut for `cargo run -- --serve` in the project directory.

A **dedicated** server opens no window. It loads the project and its
entry scene, starts play mode, and steps the simulation at the fixed
rate (60 ticks per second by default) by the wall clock until you press
Ctrl-C. It logs where it listens, the command clients join with, every
player who joins or leaves, and every 10 seconds the tick and player
count:

```text
INFO  serving `mygame` on port 7777; press Ctrl-C to stop
INFO  net: QUIC server listening on 0.0.0.0:7777; certificate SHA-256 fingerprint 3F:A2:...
INFO  clients join with `cargo run -- --join <this machine>:7777 --cert /home/you/mygame/target/shin-server-cert.der` (or `--insecure` instead of `--cert`, for development only)
INFO  net: server running at 60 ticks per second, a snapshot every 3 tick(s)
INFO  net: `ada` joined as player 1 (peer 1)
INFO  net: tick 600, 1 player(s)
```

A **listen server** (`--host`) is the normal game window with the server
running in the same engine. The person at the window is player one and
needs no connection; everyone else joins over the network. **Solo** play,
and the game under `--mcp`, is the same with no network.

A server tick has two halves. **First**, before your systems, the
server answers each joining client (with its player id, the current
tick, and the timestep, or turns it away if its protocol version
differs), forgets players who left or disconnected, and takes in the
input every player sent. **Then** your systems and scripts run, seeing
this tick's players and input. **Last**, on every third tick, it
captures the replicated entities and sends the snapshot to every player.
When a dedicated server stops on its own, it says goodbye to every
player first.

### Ports and certificates

Servers speak **QUIC** over UDP, encrypted with TLS 1.3, from the
`shin-net` crate every new project depends on. The server listens on
UDP port 7777 on every network interface. Players on other machines
need that port reachable: open it in the firewall, and on a home network
forward it on the router. There is no NAT traversal or relay.

QUIC needs a certificate, so the server makes a fresh self-signed one
every time it starts, logs its SHA-256 fingerprint, and saves it to
`target/shin-server-cert.der` in the project. A client trusts the server
in one of two ways:

- `--cert <path>`: exactly the certificate in that file. On the same
  machine, point it at the server's `target/shin-server-cert.der`;
  elsewhere, copy the file over. It changes every time the server starts.
- `--insecure`: any certificate. The connection is still encrypted, but
  nothing checks that the server is the one you meant, so use it only
  for development on a network you trust. The client logs a warning.

## Joining

```sh
cargo run -- --join 127.0.0.1:7777 --cert target/shin-server-cert.der
cargo run -- --join 127.0.0.1:7777 --insecure
```

`--join <host:port>` opens the game's window as a **client** of the
server at that address, trusting its certificate as `--cert` or
`--insecure` says (one of the two is required). The client introduces
itself with the protocol version and a player name (your login name),
the server answers with a player id, and from then on the client applies
the server's snapshots: replicated entities appear, move, and disappear
as they do on the server.

Entities that come from the server belong to it: **physics never
simulates them and their scripts do not run** on the client, even if a
body, collider, or script is added to them locally. What you see is
exactly what the server sent.

If the server does not answer within five seconds, or its certificate
is not the one trusted, `--join` exits with an error saying so. When the
server shuts down or turns the client away (the protocol versions
differ), or the connection drops, the client is disconnected and
replicated entities stay where they were.

### Smooth motion

On a real network snapshots arrive unevenly, so the client shows the
world slightly in the past and moves entities smoothly between them. It
shows the newest snapshot that is at least the **interpolation delay**
(2 ticks by default) older than the newest received, and places each
remote entity between that snapshot and the next by how far the client
has played, including the fraction of a tick the frame lands on.
Position and scale move in a straight line; rotation turns along the
shortest arc. With snapshots at tick 10 (x = 0) and tick 12 (x = 2), an
entity is at x = 1 at tick 11.

Late and duplicate snapshots are dropped. If one is lost, the entity
**holds** at the newest pose it has rather than guessing ahead, so it
never jumps. A longer delay hides more jitter but shows the world
further in the past; `0` shows each snapshot as soon as it arrives.

## Players and input

Every tick, a client sends the server its **input actions**, the ones
bound in `shin.toml` (see [Input actions](scripting.md#input-actions)).
Each message lists every action that is not at rest, so each one is
complete on its own: it travels unreliably, and a lost one is corrected
by the next.

The server keeps each player's newest input, updated in the first half
of every tick:

- an action stays as the newest message says until a newer one arrives;
  messages that arrive late are ignored;
- `just_pressed` lasts exactly one tick, and a press is not lost when
  two messages arrive in one tick or the message carrying it is lost;
- the host's own player (on a listen server, solo, or under `--mcp`)
  sends nothing: its input is the local keyboard and gamepad, or
  `input.set` from an agent.

## Scripting hooks

Player logic lives in the **server script**, `scripts/server.rhai` in
the project. It belongs to no entity. Wherever the game is the server
(solo, `--host`, `--serve`, `--mcp`), the engine calls
`on_player_join(id)` once when a player joins and `on_player_leave(id)`
once when they leave, before the entity scripts of that tick. Both are
optional. Clients never run it.

```rhai
// scripts/server.rhai
fn on_player_join(id) {
    spawn_entity()
        .name(`player ${id}`)
        .mesh("assets/cube.gltf")
        .at(vec3(1.5 * (id - 1), 0.5, 2.5))
        .mover(3.0, 5.0)
        .script("scripts/player.rhai")
        .replicated(true)
        .owner(id);
}

fn on_player_leave(id) {
    despawn_owned(id);
}
```

`.mover(speed, jump_impulse)` gives the cube a **mover**: it walks at
`speed` metres per second with its owner's `move_x` and `move_z`
actions, jumps with `jump` (starting upward at `jump_impulse` metres per
second) when standing, falls, and lands on the ground. Its owner's
client moves it the moment a key is pressed ([Prediction](#prediction)).
The cube's own script adds rules that run on the server only, here
putting it back when it falls off the ground:

```rhai
// scripts/player.rhai
fn update(entity, dt) {
    if entity.transform.translation.y < -10.0 {
        entity.transform.translation = vec3(0.0, 0.5, 2.5);
    }
}
```

A script can also read its owner's input itself with
`player_input(entity.owner, "move_x")`, but movement a script makes is
not predicted: the player sees it a round trip late. Use a mover for
anything the player steers.

This is what a new project does: each player who joins gets a cube in
front of the centre cube, and it goes when they leave. Played solo, you
are player 1 and the same scripts give you your cube.
[Scripting](scripting.md#players) lists the functions.

Stopping play (in the editor, or `sim.stop`) puts the world back and
forgets who joined, so the next play greets the players again.

## In Rust

Everything is in `shin::net`; the template's `src/main.rs` wires up
every mode, and `crates/shin-net/examples/net_arena.rs` is a complete
game in one file.

- **Server.** `NetServer` (a resource holding the transport and
  players; `NetServer::solo()` for a game nobody joins),
  `net_server_input_system` (the first half of a tick: add it first to
  the fixed schedule), `net_server_system` (the second half: add it
  last), and `run_server`, which runs a headless engine as a dedicated
  server. A walking player is a `shin::physics::Mover` component on an
  owned entity, moved by `mover_system` (add it after
  `net_server_input_system`). Other systems read players' input from
  the `PlayerInputs` resource:

  ```rust
  use shin::net::{Owner, PlayerInputs};

  fn move_players(world: &mut World, res: &mut Resources) {
      let inputs = res.expect::<PlayerInputs>();
      for (owner, transform) in world.query_mut::<(&Owner, &mut Transform)>() {
          transform.translation.x += inputs.axis(owner.0, "move_x") * 0.05;
      }
  }
  ```

- **Client.** `NetClient` (the transport, session state, and snapshot
  buffer; `NetClient::with_delay` sets the interpolation delay),
  `net_client_system` (fixed schedule), and `net_interpolate_system`
  (frame schedule). The engine applies snapshots itself after each
  frame's fixed steps; `NetClient::set_prediction` turns
  [prediction](#prediction) on (the default) or off. `NetClient::state()` is `Connecting`,
  `Connected { player, server_tick }`, or `Disconnected(reason)`, and
  the `LocalPlayer` resource appears once the server welcomes the
  client.

  ```rust
  use shin::net::{NetClient, net_client_system, net_interpolate_system};

  schedule.add_fixed("net_client", net_client_system);
  schedule.add_frame("net_interpolate", net_interpolate_system);
  game.resources.insert(NetClient::new(transport, "ada"));
  ```

- **Transports.** `shin_net::QuicTransport::listen(addr)` makes a server
  transport and `QuicTransport::connect(addr, &trust)` a client one,
  where `trust` is `ServerTrust::Cert(der_bytes)` or
  `ServerTrust::Insecure`. The engine also has an in-process
  `LoopbackTransport` that can simulate latency and loss, used by the
  tests (see [Contributing](contributing.md#multiplayer-tests)).

Peers exchange five messages, encoded compactly in binary: `Hello`,
`Welcome`, `Snapshot`, `Input`, and `Goodbye`. Reliable ones travel on
one ordered QUIC stream per player; snapshots and inputs are unreliable
datagrams, except a snapshot too big for one datagram (about 1200
bytes), which goes on the stream.

## Prediction

Interpolation is right for other players, but not for your own cube: it
would move a full round trip after you press a key (200 ms on a 100 ms
link), plus the interpolation delay. So a client **predicts** its own
player: entities it owns that walk with a mover are moved on the client
at once, in the same tick as the key press, and the server corrects them
when it disagrees.

Each client tick:

1. The client sends the tick's input to the server, as always, and keeps
   it in an **input history** together with where it predicted its cube
   would be after it.
2. When a snapshot arrives, it says which of the client's inputs the
   server had applied by then (`last_input_ticks`) and where the server
   had the cube. If that is more than 0.1 mm from where the client
   predicted it after that same input, the client puts the cube where
   the server had it and **replays** every input the server has not
   applied yet, from the history. Inputs the server has applied are
   forgotten.
3. The client moves the cube with this tick's input.

When nothing disturbs the cube, the server computes exactly what the
client predicted, and nothing is corrected. When the server moves the
cube some other way (a script teleports it, as the starter's
`player.rhai` does when you fall off the ground), the client snaps to the
server's position as soon as the snapshot showing it arrives, still ahead
by the input the server has not seen. Lost or reordered input is
corrected the same way.

Other players' cubes are never predicted; they stay interpolated, in the
past. The client's own cube is marked `Predicted` (a component), and
`NetClient::corrections()` counts the corrections.

### The movement contract

Prediction works because a mover's step is a **pure function** of the
input, the timestep, the mover's state (its position, settings, and
vertical speed), and the static colliders around it. The client and the
server run the same Rust function on the same numbers and get the same
answer:

- A mover walks along X and Z at its speed, jumps when `jump` is held
  while it stands, falls with its own gravity (9.81 m/s² by default), and
  lands on the highest **static** collider below it (a collider with no
  body or a `fixed` one, such as the ground). It does not collide with
  walls, dynamic bodies, or other players.
- Movers are replicated with their state, so a client predicts with the
  server's numbers.
- The client lands its cube on its own static colliders: the ones in the
  scene it loaded (colliders are not replicated). A client must load the
  same level as the server.
- Anything else that moves an owned entity (a script, a Rust system on
  the server, physics) is not predicted: the player sees it when the
  snapshot arrives, as a correction. Keep such moves rare, or the
  player's own cube will jump.

### Turning prediction off

To see exactly what the server sees, a round trip late (useful when
debugging movement), turn prediction off in `shin.toml`:

```toml
[net]
predict = false
```

A new project's `--join` passes it to `NetClient::set_prediction`. With
prediction off, your own cube is interpolated like everyone else's.

## Tools

Agents drive a game over MCP (see [Agents and MCP](agents-mcp.md)) with
the same tools as single-player, which is enough to check a multiplayer
game:

- `entity.list` shows each entity's `replicated` flag and `net_id`
  (`null` until its first snapshot), so you can see what is networked:

  ```json
  { "entity": 4294967296, "name": "player 1", "replicated": true, "net_id": 2, ... }
  ```

- Under `--mcp` the game is a solo server, so `sim.play` greets player 1
  through `server.rhai`, `input.set` drives that player's actions, and
  `sim.step` and `render.frame` show the result.
- `net.status` reports the engine's multiplayer role. The engine behind
  `--mcp` plays solo, so it reports `{ "role": "none" }`.

The `shin-multiplayer` [agent skill](agents-skills.md) walks an agent
through it: checking replication under `--mcp`, hosting and joining as
two processes, reading their logs, and the common causes of a client
that shows something different from the server.

## Bandwidth

Every snapshot carries every replicated entity in full. A typical prop
(a name, a transform, a mesh source such as `builtin://cube`, and a
colour) takes about 84 bytes, so:

| Replicated entities | Bytes per snapshot | Per client at 20 snapshots/s |
|--------------------:|-------------------:|-----------------------------:|
| 100                 | 8,298              | about 166 KB/s               |
| 1000                | 84,773             | about 1.7 MB/s               |

Shorter names and mesh sources make entities smaller; a child entity
adds a few bytes for its parent. The numbers come from
`cargo bench -p shin-engine --bench net_snapshot`, which also times
encoding and decoding (tens of microseconds for 1000 entities).

## Limits

What multiplayer does not do yet:

- **Only movers are predicted.** Your own entity moves at once only if
  it walks with a mover; anything else that moves it arrives with the
  server's snapshot, a round trip late. There is no lag compensation for
  hits, and other players' entities are never predicted.
- **Full snapshots only.** Every snapshot sends every replicated entity;
  delta snapshots that send only changes are not implemented.
- **No hosting or joining from the editor or through tools.** Servers
  and clients are started from the command line; `net.status` reports no
  session for an engine driven over MCP.
- **Development certificates only.** No certificate authorities, NAT
  traversal, relays, or browser transports.
- **`Replicated` is not saved in scene files**; mark entities in code
  or in `server.rhai`.
- Clients need the same project files as the server, since meshes and
  textures travel by asset source.
