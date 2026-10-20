// The server script: the game's own script, attached to no entity.
//
// The engine calls `on_player_join` once for every player who joins and
// `on_player_leave` once for every player who leaves. That happens
// wherever the game is the server: playing solo (you are player 1),
// hosting with `--host`, and on a dedicated server with `--serve`. A
// client that joins with `--join` never runs it; it sees what the server
// spawns.
//
// Each player gets a cube of their own that walks with that player's
// input (a mover) and follows the rules in scripts/player.rhai. Edit
// this file while the game runs and it reloads within a second; players
// already in keep their cubes.

fn on_player_join(id) {
    let c = colour(id);
    spawn_entity()
        .name(`player ${id}`)
        .mesh("assets/cube.gltf")
        .color(c[0], c[1], c[2])
        .at(spot(id))
        .mover(3.0, 5.0)    // walks at 3 m/s, jumps at 5 m/s with its owner's input
        .script("scripts/player.rhai")
        .replicated(true)   // sent to every client
        .owner(id);         // whose input moves it, and who predicts it
}

fn on_player_leave(id) {
    despawn_owned(id);
}

// Where player `id` starts: rows of four in front of the centre cube,
// 1.5 m apart, so no two players start inside each other.
fn spot(id) {
    let i = id - 1;
    vec3(1.5 * (i % 4) - 2.25, 0.5, 2.5 + 1.5 * (i / 4))
}

// A colour per player, linear RGB.
fn colour(id) {
    let colours = [
        [0.85, 0.25, 0.2],
        [0.2, 0.45, 0.9],
        [0.25, 0.75, 0.3],
        [0.9, 0.75, 0.15],
    ];
    colours[(id - 1) % colours.len()]
}
