// The player script. scripts/server.rhai spawns one cube per player with
// this script, owned by that player.
//
// The cube walks with a mover, not with this script: `mover(3.0, 5.0)` in
// server.rhai makes the arrow keys or the left stick (`move_x`, `move_z`)
// slide it over the ground at 3 m/s, and Space or the gamepad's south
// button (`jump`) jump at 5 m/s. Movers run on the server and, for each
// player's own cube, ahead of it on that player's machine (prediction),
// so your cube moves the moment you press a key even across a network.
//
// This script adds the rest of the game's rules for a player. It runs on
// the server only, with each player's own input
// (`player_input(entity.owner, ...)`); whatever it does to the cube
// reaches the player as a correction. Here: walk off the ground and you
// fall, and once you are well below it you are put back on top. Edit this
// file while the game runs and it reloads within a second.
fn update(entity, dt) {
    let p = entity.transform.translation;
    if p.y < -10.0 {
        entity.transform.translation = vec3(0.0, 0.5, 2.5);
    }
}
