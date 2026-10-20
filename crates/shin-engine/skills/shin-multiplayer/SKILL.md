---
name: shin-multiplayer
description: Host a Shin game as a multiplayer server, join it from a second engine, and check that entities replicate and players move, using entity.list, render.frame, input.set, net.host, net.join, net.status, net.players, net.disconnect, and the client log. Use when making a game multiplayer, checking what is networked, or debugging a client that does not see what the server does.
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
`on_player_leave(id)`. Players walk with `.mover(speed, jump_impulse)`
on that spawn: the client predicts its own walking entity (it moves at
once, the server corrects it), while scripted movement arrives a round
trip late. `[net] predict = false` in `shin.toml` turns prediction off.

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

4. **Host.** Over MCP, `net.host` with `{"port": 7777}` makes the engine
   you are connected to a listen server (you are player 1) and starts play
   mode. It saves the server certificate to `target/shin-server-cert.der`
   and returns `net.status`: `"role": "host"`, `port`, and `cert_path`.
   Under MCP nothing steps on its own, so keep calling `sim.step` while a
   client joins. (To host outside MCP instead, run `cargo run -- --serve
   --port 7777` in a background shell; `--host` plays in a window and
   serves from it.)

5. **Join a second engine.** In a background shell in the project:
   `cargo run -- --join 127.0.0.1:7777 --cert target/shin-server-cert.der`
   (or `--insecure` instead of `--cert`, for development only). Then
   `sim.step` with `{"steps": 30}` on the host. The client opens a window
   and logs `joined as player 2`. The other way round, with a server
   already running, `net.join` with `{"addr": "127.0.0.1:7777"}` (it
   trusts the project's saved certificate; add `"insecure": true`
   without it) makes your engine the client: `net.status` goes from
   `"state": "connecting"` to `"connected"` with your `player` after a
   few `sim.step`s, and `entity.list` shows the server's entities.
   With no window available, the repository's `cargo test -p
   shin-engine --test net_loopback` runs a server and client in one
   process and compares their `entity.list` and `render.frame`.

6. **Watch the session.** `net.status` on the host lists the joined
   players with their round-trip times (`players`: `[{"id": 2,
   "rtt_ms": 0.4}]`), the server `tick`, and `bandwidth`: bytes in and
   out per second, bytes per tick, and the snapshot savings from deltas
   (`compression_ratio`). `net.players` lists every player, your own
   with `"local": true`.

7. **Stop.** `net.disconnect` ends the session (play continues), and
   `sim.stop` ends it and restores the scene; either way `net.status`
   is back to `{"role": "none"}`. The host says goodbye, so the client
   logs `the server said goodbye`; kill the client process after. A
   server started from a shell stops with Ctrl-C; it logs `player 2
   left` and `on_player_leave` removes that player's entity.

## Verification

Replication works when, in step 1, every entity players should see is
`replicated` with a `net_id`; in step 3 the player's entity moves under
`input.set` and `render.frame` shows it; and in steps 5 and 6 the
`net.status` on the host lists the second player and the client shows
the same world. Report which entities are replicated and how you checked.

## Debugging

| Symptom | Likely cause | Fix |
|---|---|---|
| Entity on the server, not on the client | Not `replicated`, or its mesh has no asset source | Step 2 |
| `net_id` is `null` | No snapshot taken yet, or not playing | `sim.play`, `sim.step` with `{"steps": 3}` |
| Client sees no player entity | `server.rhai` has no `on_player_join`, or it fails | `script.errors`; read the server's log |
| Player does not move | Action names in the script differ from `shin.toml` | `input.list`; fix the names |
| Own player lags or snaps back on a client | Moved by a script, not a mover; or the client has no ground (movers land on its own static colliders) | `.mover(...)`; load the same scene on both sides |
| Every player moves the same entity | The script reads one fixed player id | Read the owner's input: `player_input(entity.owner, "move_x")` in `player.rhai` |
| Join fails with a certificate error | The server restarted with a new certificate | Use the new `target/shin-server-cert.der`, or `--insecure` |
| Join times out after five seconds | Server not running, wrong port, or UDP blocked | Check the server log's port; open UDP in the firewall |
| Client mesh fails to load | The client's project lacks the asset file | Clients load sources from their own `assets/` |
| Edits on the client are undone | Server entities belong to the server | Change them on the server |

## Pitfalls

- Run game processes (`--serve`, `--join`) in the background and read
  their logs; they run until stopped.
- Until `net.host` or `net.join`, the `--mcp` engine plays solo: it runs
  the server's game logic with you as player 1 but nobody can join it,
  so `net.status` reports `"role": "none"`.
- A host under MCP only advances when you call `sim.step`: a client
  waiting to be welcomed needs a few steps.
- Scripts, physics, and sounds never run on a client for server
  entities; check game logic on the server or under `--mcp`.
- Ids in `entity.list` differ between engines; compare `net_id` and
  `name`, never `entity`.
- The certificate changes on every server start.
