---
name: shin-multiplayer
description: Host a Shin game as a multiplayer server, join it from a second engine, and check that entities replicate and players move, using entity.list, render.frame, input.set, the server and client logs, and net.status. Use when making a game multiplayer, checking what is networked, or debugging a client that does not see what the server does.
---

# Hosting, joining, and checking replication

## Goal

Get a server and a client of the same project running, confirm that the
entities you mean to network are replicated and that each player
controls their own entity, and find the cause when a client shows
something different from the server.

## When to use

- Making a game multiplayer, or adding entities that players should see.
- A client is missing entities, shows them in the wrong place, or its
  player cannot move.
- Checking how many players joined and whether the server is running.

## How it fits together

The server runs the game (scripts, physics, every system) and sends a
snapshot of the entities marked `replicated` 20 times a second. Clients
draw what it sends and send back their player's input actions. A game
played solo, and the game under `--mcp`, is a server whose only player
is player 1, so the same scripts run everywhere. `scripts/server.rhai`
spawns each player's entity in `on_player_join(id)` and removes it in
`on_player_leave(id)`.

## Steps

1. **Check what is networked.** Under `--mcp`, `sim.play`, then
   `sim.step` with `{"steps": 3}`. `entity.list` with `{"limit": 100}`
   shows `replicated` and `net_id` for each entity. Everything players
   must see needs `"replicated": true` and, after a snapshot, a number in
   `net_id`. Expect one player entity (player 1's) from `server.rhai`.

2. **Mark what is missing.** Entities are replicated in their scene file
   or by the code that spawns them: `.replicated(true)` on
   `spawn_entity()` in Rhai, or the `Replicated` component in Rust.
   Only mesh, colour, and texture by asset source, transform, parent,
   name, and owner are sent: a mesh with no asset source does not
   replicate, and scripts, physics, and sounds stay on the server.
   `sim.stop`, edit, and repeat step 1.

3. **Check input moves the player.** While playing, `input.list` shows
   the actions; `input.set` with `{"action": "move_x", "axis": 1.0}`,
   `sim.step` with `{"steps": 30}`, then `entity.list`: player 1's
   entity has moved. `input.set` with `{"action": "move_x", "axis": 0.0}`
   releases it. `render.frame` shows the result.

4. **Host.** In a shell in the project, start the dedicated server in the
   background: `cargo run -- --serve --port 7777`. It logs
   `QUIC server listening on 0.0.0.0:7777` and the exact join command,
   with the path of the certificate it saved
   (`target/shin-server-cert.der`). `--host` instead plays in a window
   and serves from it.

5. **Join a second engine.** In another background shell:
   `cargo run -- --join 127.0.0.1:7777 --cert target/shin-server-cert.der`
   (or `--insecure` instead of `--cert`, for development only). The
   client opens a window and logs `joined as player 2`; the server logs
   `` `<name>` joined as player 2 ``. With no window available, the
   repository's `cargo test -p shin-engine --test net_loopback` runs a
   server and client in one process and compares their `entity.list`
   and `render.frame`.

6. **Watch the session.** Every 10 seconds the server logs the tick and
   the player count (`net: tick 600, 2 player(s)`), and it logs each
   join and leave. `net.status` reports the role of the engine you are
   connected to over MCP.

7. **Stop.** Ctrl-C (or kill) the client, then the server. The server
   logs `player 2 left` and `on_player_leave` removes its entity.

## Verification

Replication works when, in step 1, every entity players should see is
`replicated` with a `net_id`; in step 3 the player's entity moves under
`input.set` and `render.frame` shows it; and in steps 5 and 6 the
server logs the second player joining and the client shows the same
world. Report which entities are replicated and how you checked.

## Debugging

| Symptom | Likely cause | Fix |
|---|---|---|
| Entity on the server, not on the client | Not `replicated`, or its mesh has no asset source | Step 2 |
| `net_id` is `null` | No snapshot taken yet, or not playing | `sim.play`, `sim.step` with `{"steps": 3}` |
| Client sees no player entity | `server.rhai` has no `on_player_join`, or it fails | `script.errors`; read the server's log |
| Player does not move | Action names in the script differ from `shin.toml` | `input.list`; fix the names |
| Every player moves the same entity | The script reads one player, not `entity.owner` | Use `player_input(entity.owner, ...)` |
| Join fails with a certificate error | The server restarted with a new certificate | Use the new `target/shin-server-cert.der`, or `--insecure` |
| Join times out after five seconds | Server not running, wrong port, or UDP blocked | Check the server log's port; open UDP in the firewall |
| Client mesh fails to load | The client's project lacks the asset file | Clients load sources from their own `assets/` |
| Edits on the client are undone | Server entities belong to the server | Change them on the server |

## Pitfalls

- Run servers and clients as background processes and read their logs;
  they run until stopped.
- The `--mcp` engine plays solo: it runs the server's game logic with
  you as player 1 but nobody can join it, so `net.status` reports
  `"role": "none"` there.
- Scripts, physics, and sounds never run on a client for server
  entities; check game logic on the server or under `--mcp`.
- Ids in `entity.list` differ between engines; compare `net_id` and
  `name`, never `entity`.
- The certificate changes on every server start.
