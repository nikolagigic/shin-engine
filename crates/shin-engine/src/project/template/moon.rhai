// The moon's script: a blip each time it completes a revolution.
//
// `src/main.rs` moves the moon around the cube at ORBIT_SPEED (1 rad/s),
// so one revolution takes 2π seconds. This script only watches the clock
// and plays the sound at the moon, so it fades with the distance from the
// camera. Edit it while the game runs: changes take effect within a second.

fn update(entity, dt) {
    let period = 2.0 * PI(); // seconds per revolution
    let now = time();
    let before = now - dt;
    if before >= 0.0 && floor(now / period) != floor(before / period) {
        play_sound_at(entity, "assets/sounds/blip.wav");
    }
}
