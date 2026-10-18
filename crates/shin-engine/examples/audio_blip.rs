//! Audio smoke test (GE-036): opens the default output device and plays
//! the starter blip three times, the last one 4 m away at a quarter of the
//! volume. Prints whether a device was found; on a machine without one it
//! runs silently and says so.
//!
//! ```sh
//! cargo run -p shin-engine --example audio_blip
//! ```

use std::path::PathBuf;
use std::time::Duration;

use shin::asset::{AssetSource, Assets};
use shin::audio::{AudioEngine, PlayParams};
use shin::math::Vec3;

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let mut assets = Assets::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    let blip = assets.sound(&AssetSource::parse("assets/sounds/blip.wav")?)?;
    let mut audio = AudioEngine::new();
    println!(
        "audio output: {}",
        if audio.is_silent() {
            "none (silent engine)"
        } else {
            "default device"
        }
    );
    for params in [
        PlayParams::default(),
        PlayParams::default(),
        PlayParams {
            position: Some(Vec3::new(4.0, 0.0, 0.0)),
            ..Default::default()
        },
    ] {
        audio.play(&blip, params, Vec3::ZERO)?;
        std::thread::sleep(Duration::from_millis(400));
        audio.update(Vec3::ZERO);
    }
    println!(
        "played {} sounds; {} still playing",
        audio.plays(),
        audio.playing()
    );
    Ok(())
}
