// The player script. scripts/server.rhai spawns one cube per player with
// this script, owned by that player.
//
// It reads its owner's actions, bound in shin.toml: `move_x` and `move_z`
// (the arrow keys or the left stick, -1..1) slide the cube over the
// ground, and `jump` (Space or the gamepad's south button) lifts it while
// held; let go and it settles back onto the ground. The server runs it
// with each player's own input, whether that player is at this keyboard
// or on another machine. Edit this file while the game runs and it
// reloads within a second.
fn update(entity, dt) {
    let speed = 3.0;      // metres per second
    let lift = 4.0;       // rise while jump is held
    let gravity = 6.0;    // fall when it is not
    let ground = 0.5;     // the cube's centre when resting on the slab

    let player = entity.owner;
    let p = entity.transform.translation;
    p.x += player_input(player, "move_x") * speed * dt;
    // +1 on move_z is "forward" (ArrowUp, stick up), which is -Z here.
    p.z -= player_input(player, "move_z") * speed * dt;
    if player_input_pressed(player, "jump") {
        p.y += lift * dt;
    } else {
        p.y = max(ground, p.y - gravity * dt);
    }
    entity.transform.translation = p;
}
