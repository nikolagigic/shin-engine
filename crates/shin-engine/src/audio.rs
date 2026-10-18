//! Audio (GE-036): sound sources and one-shot playback through
//! [kira](https://docs.rs/kira/0.12).
//!
//! Minimal, honest audio: load an `.ogg` or `.wav` file, play it, optionally
//! positioned. The [`AudioEngine`] resource owns kira's audio manager. It is
//! created lazily, on the first sound, so headless tests and CI machines
//! without an output device still run: when no device can be opened the
//! engine logs once and becomes **silent**, counting plays without playing
//! anything ([`AudioEngine::is_silent`], [`AudioEngine::plays`]).
//!
//! Sounds are decoded once and cached by [`crate::asset::AssetSource`]
//! ([`crate::asset::Assets::sound`]). An entity with an [`AudioSource`]
//! component names a sound and how to play it; `autoplay` sources start
//! when play starts ([`crate::headless::Headless::play`]) and every sound
//! stops when it stops. Spatial sources attenuate by distance to the
//! [`Camera`] (the listener) with a simple inverse-distance falloff
//! ([`spatial_gain`]); there is no HRTF or panning.
//!
//! [`audio_system`] (a frame system) keeps spatial volumes up to date as
//! the camera and the sources move and forgets sounds that have finished.
//!
//! Tools ([`register_tools`]): `audio.play`, `audio.stop_all`, and
//! `entity.set_audio` (recorded for undo). Scripts call `play_sound(path)`
//! and `play_sound_at(entity, path)` (see [`crate::script`]).

mod tools;

use anyhow::Context as _;
use kira::sound::PlaybackState;
use kira::sound::static_sound::{StaticSoundData, StaticSoundHandle};
use kira::{AudioManager, AudioManagerSettings, Decibels, DefaultBackend, Tween};

pub(crate) use tools::audio_json;
pub use tools::register_tools;

use crate::asset::{AssetSource, Assets};
use crate::ecs::{Entity, Resources, World};
use crate::math::{Camera, Transform, Vec3};

/// The distance (in metres) within which a spatial sound plays at full
/// volume; beyond it the gain falls off as `1 / distance`.
pub const REFERENCE_DISTANCE: f32 = 1.0;

/// How many sounds a silent engine remembers as "playing" (it cannot tell
/// when they would have finished), oldest dropped first.
const SILENT_CAPACITY: usize = 64;

/// Component: a sound attached to an entity, saved in scene files as the
/// entity's `audio` field.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioSource {
    /// The sound, an `.ogg` or `.wav` file ([`AssetSource::Audio`]).
    pub source: AssetSource,
    /// Linear volume; `1.0` plays the file as recorded, `0.0` is silent.
    pub volume: f32,
    /// Whether the sound repeats until stopped.
    pub looping: bool,
    /// Whether the volume falls off with the distance from the camera.
    pub spatial: bool,
    /// Whether the sound starts when play starts.
    pub autoplay: bool,
}

impl AudioSource {
    /// A one-shot, full-volume, non-spatial source that does not autoplay.
    pub fn new(source: AssetSource) -> Self {
        Self {
            source,
            volume: 1.0,
            looping: false,
            spatial: false,
            autoplay: false,
        }
    }
}

/// How to play a sound; see [`AudioEngine::play`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayParams {
    /// Linear volume before any distance attenuation.
    pub volume: f32,
    /// Whether the sound repeats until stopped.
    pub looping: bool,
    /// Where the sound is, for distance attenuation; `None` plays it
    /// everywhere at `volume`.
    pub position: Option<Vec3>,
    /// The entity the sound follows while it plays, if any
    /// ([`audio_system`] refreshes `position` from its [`Transform`]).
    pub entity: Option<Entity>,
}

impl Default for PlayParams {
    /// Full volume, one-shot, not positioned.
    fn default() -> Self {
        Self {
            volume: 1.0,
            looping: false,
            position: None,
            entity: None,
        }
    }
}

/// One sound that is (or, on a silent engine, would be) playing.
struct Playing {
    /// kira's handle; `None` on a silent engine.
    handle: Option<StaticSoundHandle>,
    params: PlayParams,
}

/// Resource: the audio output and every sound playing through it.
///
/// Create it with [`AudioEngine::new`] (an output device if one can be
/// opened, otherwise silent) or [`AudioEngine::silent`] (always silent, for
/// tests). Most code never does either: [`engine`] inserts one on first
/// use.
pub struct AudioEngine {
    /// `None` when silent.
    manager: Option<AudioManager<DefaultBackend>>,
    playing: Vec<Playing>,
    plays: u64,
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioEngine {
    /// Opens the default output device. If there is none, or it cannot be
    /// opened, logs the reason once and returns a silent engine.
    pub fn new() -> Self {
        match AudioManager::<DefaultBackend>::new(AudioManagerSettings::default()) {
            Ok(manager) => Self {
                manager: Some(manager),
                playing: Vec::new(),
                plays: 0,
            },
            Err(err) => {
                log::warn!("no audio output ({err}); sounds will be counted but not heard");
                Self::silent()
            }
        }
    }

    /// An engine with no output: [`Self::play`] succeeds and counts, but
    /// nothing is heard. What headless tests and machines without a sound
    /// device use.
    pub fn silent() -> Self {
        Self {
            manager: None,
            playing: Vec::new(),
            plays: 0,
        }
    }

    /// Whether this engine has no output device.
    pub fn is_silent(&self) -> bool {
        self.manager.is_none()
    }

    /// How many sounds [`Self::play`] has started, heard or not.
    pub fn plays(&self) -> u64 {
        self.plays
    }

    /// How many sounds are playing now. A silent engine counts a sound
    /// until [`Self::stop_all`], since it cannot tell when it would end.
    pub fn playing(&self) -> usize {
        self.playing.len()
    }

    /// Starts `sound` with `params`, attenuated for a listener at
    /// `listener` if it is positioned, and returns the total play count.
    ///
    /// Fails only if kira refuses the sound (its sound limit is reached);
    /// a silent engine never fails.
    pub fn play(
        &mut self,
        sound: &StaticSoundData,
        params: PlayParams,
        listener: Vec3,
    ) -> anyhow::Result<u64> {
        let handle = match &mut self.manager {
            Some(manager) => {
                let mut data = sound.volume(decibels(gain(&params, listener)));
                if params.looping {
                    data = data.loop_region(..);
                }
                Some(
                    manager
                        .play(data)
                        .map_err(|err| anyhow::anyhow!("cannot play sound: {err}"))?,
                )
            }
            None => {
                if self.playing.len() >= SILENT_CAPACITY {
                    self.playing.remove(0);
                }
                None
            }
        };
        self.playing.push(Playing { handle, params });
        self.plays += 1;
        Ok(self.plays)
    }

    /// Stops every sound and returns how many were playing.
    pub fn stop_all(&mut self) -> usize {
        let count = self.playing.len();
        for mut playing in self.playing.drain(..) {
            if let Some(handle) = &mut playing.handle {
                handle.stop(Tween::default());
            }
        }
        count
    }

    /// Forgets sounds that have finished and re-attenuates the positioned
    /// ones for a listener at `listener`. [`audio_system`] calls this
    /// every frame.
    pub fn update(&mut self, listener: Vec3) {
        self.playing.retain(|playing| {
            playing
                .handle
                .as_ref()
                .is_none_or(|handle| handle.state() != PlaybackState::Stopped)
        });
        for playing in &mut self.playing {
            if playing.params.position.is_some()
                && let Some(handle) = &mut playing.handle
            {
                handle.set_volume(decibels(gain(&playing.params, listener)), Tween::default());
            }
        }
    }

    /// The position of each playing sound that follows an entity, as
    /// `(entity, position)`, so [`audio_system`] can move them.
    fn follow(&mut self, position_of: impl Fn(Entity) -> Option<Vec3>) {
        for playing in &mut self.playing {
            if let Some(entity) = playing.params.entity
                && let Some(position) = position_of(entity)
            {
                playing.params.position = Some(position);
            }
        }
    }
}

/// The linear gain of `params` for a listener at `listener`: its volume,
/// times [`spatial_gain`] of the distance when it is positioned.
fn gain(params: &PlayParams, listener: Vec3) -> f32 {
    let distance = params
        .position
        .map_or(0.0, |position| position.distance(listener));
    params.volume * spatial_gain(distance)
}

/// The inverse-distance falloff of a spatial sound: `1.0` up to
/// [`REFERENCE_DISTANCE`], then `REFERENCE_DISTANCE / distance`.
pub fn spatial_gain(distance: f32) -> f32 {
    REFERENCE_DISTANCE / distance.max(REFERENCE_DISTANCE)
}

/// A linear amplitude as kira decibels: `1.0` is `0 dB`, `0.5` is about
/// `-6 dB`, and `0.0` (or anything at or below kira's `-60 dB` floor) is
/// [`Decibels::SILENCE`].
pub fn decibels(amplitude: f32) -> Decibels {
    if amplitude <= 0.0 {
        return Decibels::SILENCE;
    }
    let db = 20.0 * amplitude.log10();
    if db <= Decibels::SILENCE.0 {
        Decibels::SILENCE
    } else {
        Decibels(db)
    }
}

/// The engine's [`AudioEngine`], inserting [`AudioEngine::new`] if there is
/// none. This is the lazy creation: no device is opened until a sound is
/// played.
pub fn engine(res: &mut Resources) -> &mut AudioEngine {
    if res.get::<AudioEngine>().is_none() {
        res.insert(AudioEngine::new());
    }
    res.expect_mut::<AudioEngine>()
}

/// The listener position: the [`Camera`]'s, or the origin without one.
fn listener(res: &Resources) -> Vec3 {
    res.get::<Camera>()
        .map_or(Vec3::ZERO, |camera| camera.transform.translation)
}

/// Decodes `source` through the [`Assets`] cache and plays it with
/// `params` on the engine's [`AudioEngine`] (created if needed). Returns
/// the total play count.
///
/// Fails if `source` is not an audio source, the file cannot be decoded,
/// or there is no [`Assets`] resource.
pub fn play(res: &mut Resources, source: &AssetSource, params: PlayParams) -> anyhow::Result<u64> {
    let sound = res
        .get_mut::<Assets>()
        .context("no Assets resource")?
        .sound(source)?;
    let listener = listener(res);
    engine(res).play(&sound, params, listener)
}

/// Plays `source` as `entity` would: its volume and looping, positioned at
/// (and following) the entity when the source is spatial.
pub fn play_source(
    world: &World,
    res: &mut Resources,
    entity: Entity,
    source: &AudioSource,
) -> anyhow::Result<u64> {
    let position = source
        .spatial
        .then(|| world.get::<&Transform>(entity).ok().map(|t| t.translation))
        .flatten();
    play(
        res,
        &source.source,
        PlayParams {
            volume: source.volume,
            looping: source.looping,
            position,
            entity: source.spatial.then_some(entity),
        },
    )
}

/// Starts every [`AudioSource`] with `autoplay` set, as
/// [`crate::headless::Headless::play`] does. A source that fails to play
/// is logged and skipped, so one missing file does not stop play mode.
pub fn play_autoplay_sources(world: &World, res: &mut Resources) {
    let sources: Vec<(Entity, AudioSource)> = world
        .query::<(Entity, &AudioSource)>()
        .iter()
        .filter(|(_, source)| source.autoplay)
        .map(|(entity, source)| (entity, source.clone()))
        .collect();
    for (entity, source) in sources {
        if let Err(err) = play_source(world, res, entity, &source) {
            log::warn!(
                "entity {} could not autoplay {}: {err:#}",
                entity.to_bits().get(),
                source.source
            );
        }
    }
}

/// Stops every sound, if an [`AudioEngine`] exists. Returns how many were
/// playing.
pub fn stop_all(res: &mut Resources) -> usize {
    res.get_mut::<AudioEngine>()
        .map_or(0, AudioEngine::stop_all)
}

/// Frame system: moves sounds that follow an entity to the entity's
/// current position, drops sounds that have finished, and re-attenuates
/// positioned sounds for the camera. Does nothing until a sound has been
/// played (it never creates the [`AudioEngine`]).
pub fn audio_system(world: &mut World, res: &mut Resources) {
    let listener = listener(res);
    let Some(engine) = res.get_mut::<AudioEngine>() else {
        return;
    };
    engine.follow(|entity| world.get::<&Transform>(entity).ok().map(|t| t.translation));
    engine.update(listener);
}

/// The sample rate of [`blip_wav`].
const BLIP_SAMPLE_RATE: u32 = 22_050;
/// The length of [`blip_wav`] in samples: 0.2 s.
const BLIP_SAMPLES: usize = BLIP_SAMPLE_RATE as usize / 5;
/// `cos` and `sin` of one sample's phase step at 880 Hz (an A5), written
/// out so the file is generated bit for bit the same on every platform.
const BLIP_COS: f64 = 0.968_724_733_047_448_5;
const BLIP_SIN: f64 = 0.248_137_847_943_737_9;

/// A 0.2 s, 880 Hz sine blip fading out linearly, as a 16-bit mono WAV file
/// (8,864 bytes). Starter projects get it as `assets/sounds/blip.wav`; the
/// committed copy is `crates/shin-engine/assets/sounds/blip.wav`, checked
/// against this by a test.
pub fn blip_wav() -> Vec<u8> {
    let data_len = (BLIP_SAMPLES * 2) as u32;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&BLIP_SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(BLIP_SAMPLE_RATE * 2).to_le_bytes()); // bytes per second
    wav.extend_from_slice(&2u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    // Rotate a unit vector by the phase step each sample instead of calling
    // `sin`, whose last bit may differ between platforms' libm.
    let (mut x, mut y) = (1.0_f64, 0.0_f64);
    for i in 0..BLIP_SAMPLES {
        let envelope = 1.0 - i as f64 / BLIP_SAMPLES as f64;
        let sample = (y * envelope * 0.5 * f64::from(i16::MAX)).round() as i16;
        wav.extend_from_slice(&sample.to_le_bytes());
        (x, y) = (x * BLIP_COS - y * BLIP_SIN, x * BLIP_SIN + y * BLIP_COS);
    }
    wav
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use serde_json::{Value, json};

    use super::*;
    use crate::command;
    use crate::ecs::Name;
    use crate::headless::Headless;
    use crate::scene::{AudioDesc, SceneFile};
    use crate::tool::{ToolError, ToolOutput, ToolRegistry};

    /// The committed blip, relative to the engine crate.
    const BLIP: &str = "assets/sounds/blip.wav";

    fn engine_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    /// A headless engine rooted at the engine crate (so `assets/sounds/
    /// blip.wav` resolves) with a silent audio engine.
    fn silent_engine() -> anyhow::Result<Headless> {
        let mut engine = Headless::new(8, 8)?;
        engine.resources.insert(Assets::new(engine_dir()));
        engine.resources.insert(AudioEngine::silent());
        Ok(engine)
    }

    fn audio(engine: &Headless) -> &AudioEngine {
        engine.resources.expect::<AudioEngine>()
    }

    fn call(reg: &ToolRegistry, engine: &mut Headless, tool: &str, args: Value) -> Value {
        match reg.call(engine, tool, args) {
            Ok(ToolOutput::Json(value)) => value,
            other => panic!("`{tool}` did not return JSON: {other:?}"),
        }
    }

    fn call_err(reg: &ToolRegistry, engine: &mut Headless, tool: &str, args: Value) -> String {
        match reg.call(engine, tool, args) {
            Err(ToolError::Failed(err)) => format!("{err:#}"),
            other => panic!("`{tool}` should have failed: {other:?}"),
        }
    }

    /// Spawns a cube named `name` at `at` through the tools and returns its id.
    fn spawn(reg: &ToolRegistry, engine: &mut Headless, name: &str, at: [f32; 3]) -> u64 {
        call(reg, engine, "mesh.builtin", json!({ "kind": "cube" }));
        call(
            reg,
            engine,
            "entity.spawn",
            json!({ "mesh": 0, "name": name, "translation": at }),
        )["entity"]
            .as_u64()
            .expect("entity id")
    }

    #[test]
    fn spatial_gain_is_inverse_distance_beyond_the_reference() {
        assert_eq!(spatial_gain(0.0), 1.0);
        assert_eq!(spatial_gain(0.5), 1.0);
        assert_eq!(spatial_gain(1.0), 1.0);
        assert_eq!(spatial_gain(2.0), 0.5);
        assert_eq!(spatial_gain(10.0), 0.1);
        let params = PlayParams {
            volume: 0.5,
            position: Some(Vec3::new(4.0, 0.0, 0.0)),
            ..Default::default()
        };
        assert_eq!(gain(&params, Vec3::ZERO), 0.125);
        assert_eq!(gain(&params, Vec3::new(4.0, 0.0, 0.0)), 0.5);
        let everywhere = PlayParams::default();
        assert_eq!(gain(&everywhere, Vec3::new(100.0, 0.0, 0.0)), 1.0);
    }

    #[test]
    fn decibels_map_amplitude_to_kira_volume() {
        assert_eq!(decibels(1.0), Decibels::IDENTITY);
        assert_eq!(decibels(0.0), Decibels::SILENCE);
        assert_eq!(decibels(-1.0), Decibels::SILENCE);
        assert_eq!(decibels(0.0001), Decibels::SILENCE);
        let half = decibels(0.5);
        assert!((half.0 + 6.0206).abs() < 1e-3, "{half:?}");
        assert!((half.as_amplitude() - 0.5).abs() < 1e-6);
        assert!((decibels(0.1).as_amplitude() - 0.1).abs() < 1e-6);
    }

    #[test]
    fn blip_wav_is_committed_and_decodes() -> anyhow::Result<()> {
        let generated = blip_wav();
        assert_eq!(generated.len(), 8_864);
        assert!(
            generated.len() <= 20 * 1024,
            "the blip must stay under 20 KB"
        );
        let committed = std::fs::read(engine_dir().join(BLIP))?;
        assert_eq!(
            committed, generated,
            "{BLIP} is stale; run `cargo test -p shin-engine audio:: -- --ignored regenerate_blip`"
        );
        let mut assets = Assets::new(engine_dir());
        let source = AssetSource::parse(BLIP)?;
        assert!(source.is_audio());
        let sound = assets.sound(&source)?;
        assert_eq!(sound.sample_rate, BLIP_SAMPLE_RATE);
        assert_eq!(sound.num_frames(), BLIP_SAMPLES);
        assert!((sound.duration().as_secs_f64() - 0.2).abs() < 1e-6);
        let peak = (0..sound.num_frames())
            .filter_map(|i| sound.frame_at_index(i))
            .map(|frame| frame.left.abs())
            .fold(0.0_f32, f32::max);
        assert!(peak > 0.4 && peak <= 0.5, "peak {peak}");
        // The cache hands out the same decoded samples without re-reading.
        let again = assets.sound(&source)?;
        assert!(std::sync::Arc::ptr_eq(&again.frames, &sound.frames));
        Ok(())
    }

    /// Regenerates `assets/sounds/blip.wav` from [`blip_wav`].
    ///
    /// ```sh
    /// cargo test -p shin-engine audio:: -- --ignored regenerate_blip
    /// ```
    #[test]
    #[ignore = "writes the committed sound; run by hand"]
    fn regenerate_blip() -> anyhow::Result<()> {
        let path = engine_dir().join(BLIP);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, blip_wav())?;
        Ok(())
    }

    #[test]
    fn sound_sources_reject_meshes_and_meshes_reject_sounds() -> anyhow::Result<()> {
        let mut assets = Assets::new(engine_dir());
        let err = assets
            .sound(&AssetSource::parse("builtin://cube")?)
            .unwrap_err();
        assert!(err.to_string().contains("not a sound"), "{err}");
        let err = assets
            .sound(&AssetSource::parse("assets/sounds/missing.ogg")?)
            .unwrap_err();
        assert!(format!("{err:#}").contains("missing.ogg"), "{err:#}");
        let gpu = crate::gpu::GpuContext::new(None)?;
        let mut registry = crate::render::MeshRegistry::new();
        let err = assets
            .mesh(&gpu.device, &mut registry, &AssetSource::parse(BLIP)?)
            .unwrap_err();
        assert!(err.to_string().contains("not a mesh"), "{err}");
        Ok(())
    }

    #[test]
    fn silent_engine_counts_plays_and_stops() -> anyhow::Result<()> {
        let mut audio = AudioEngine::silent();
        assert!(audio.is_silent());
        let sound = Assets::new(engine_dir()).sound(&AssetSource::parse(BLIP)?)?;
        assert_eq!(audio.play(&sound, PlayParams::default(), Vec3::ZERO)?, 1);
        assert_eq!(audio.play(&sound, PlayParams::default(), Vec3::ZERO)?, 2);
        assert_eq!((audio.plays(), audio.playing()), (2, 2));
        audio.update(Vec3::ZERO);
        assert_eq!(
            audio.playing(),
            2,
            "a silent engine keeps sounds until stopped"
        );
        assert_eq!(audio.stop_all(), 2);
        assert_eq!((audio.plays(), audio.playing()), (2, 0));
        for _ in 0..(SILENT_CAPACITY + 5) {
            audio.play(&sound, PlayParams::default(), Vec3::ZERO)?;
        }
        assert_eq!(audio.playing(), SILENT_CAPACITY, "bounded memory");
        Ok(())
    }

    #[test]
    fn tools_are_listed() {
        let reg = ToolRegistry::with_engine_tools();
        for name in ["audio.play", "audio.stop_all", "entity.set_audio"] {
            assert!(reg.list().iter().any(|t| t.name == name), "{name} listed");
        }
    }

    /// The ticket's registry test: `audio.play` returns ok and increments
    /// the play counter the silent engine exposes.
    #[test]
    fn audio_play_increments_the_counter() -> anyhow::Result<()> {
        let reg = ToolRegistry::with_engine_tools();
        let mut engine = silent_engine()?;
        let out = call(&reg, &mut engine, "audio.play", json!({ "source": BLIP }));
        assert_eq!(out, json!({ "plays": 1, "playing": 1, "silent": true }));
        let moon = spawn(&reg, &mut engine, "moon", [3.0, 0.0, 0.0]);
        let out = call(
            &reg,
            &mut engine,
            "audio.play",
            json!({ "source": BLIP, "volume": 0.5, "entity": moon }),
        );
        assert_eq!(out["plays"], 2);
        assert_eq!(audio(&engine).plays(), 2);
        assert_eq!(audio(&engine).playing(), 2);
        let followed = &audio(&engine).playing[1].params;
        assert_eq!(followed.position, Some(Vec3::new(3.0, 0.0, 0.0)));
        assert_eq!(followed.volume, 0.5);

        // The sound follows its entity through the frame system.
        call(
            &reg,
            &mut engine,
            "entity.set_transform",
            json!({ "entity": moon, "translation": [0, 0, -2] }),
        );
        engine.render()?;
        let followed = &audio(&engine).playing[1].params;
        assert_eq!(followed.position, Some(Vec3::new(0.0, 0.0, -2.0)));

        let err = call_err(
            &reg,
            &mut engine,
            "audio.play",
            json!({ "source": "builtin://cube" }),
        );
        assert!(err.contains("not a sound"), "{err}");
        let err = call_err(
            &reg,
            &mut engine,
            "audio.play",
            json!({ "source": BLIP, "entity": 12345 }),
        );
        assert!(err.contains("no entity"), "{err}");
        assert_eq!(audio(&engine).plays(), 2, "failures do not count");

        let out = call(&reg, &mut engine, "audio.stop_all", json!({}));
        assert_eq!(out, json!({ "stopped": 2 }));
        assert_eq!(audio(&engine).playing(), 0);
        Ok(())
    }

    #[test]
    fn set_audio_round_trips_through_a_scene_file() -> anyhow::Result<()> {
        let reg = ToolRegistry::with_engine_tools();
        let mut engine = silent_engine()?;
        let moon = spawn(&reg, &mut engine, "moon", [1.6, 0.5, 0.0]);
        let out = call(
            &reg,
            &mut engine,
            "entity.set_audio",
            json!({ "entity": moon, "source": BLIP, "volume": 0.5, "spatial": true, "autoplay": true }),
        );
        assert_eq!(
            out["audio"],
            json!({ "source": BLIP, "volume": 0.5, "looping": false, "spatial": true, "autoplay": true })
        );

        let dir = std::env::temp_dir().join(format!("shin-audio-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("audio.scene.ron");
        call(&reg, &mut engine, "scene.save", json!({ "path": path }));
        let text = std::fs::read_to_string(&path)?;
        assert!(
            text.contains(&format!(
                "audio: (\n                source: {BLIP:?},\n                volume: 0.5,\n                spatial: true,\n                autoplay: true,\n            ),"
            )),
            "{text}"
        );
        assert!(!text.contains("looping"), "defaults are omitted: {text}");

        call(&reg, &mut engine, "scene.clear", json!({}));
        call(&reg, &mut engine, "scene.load", json!({ "path": path }));
        let (entity, source) = engine
            .world
            .query::<(Entity, &AudioSource)>()
            .iter()
            .map(|(e, s)| (e, s.clone()))
            .next()
            .expect("the loaded moon has its audio");
        assert_eq!(
            source,
            AudioSource {
                source: AssetSource::parse(BLIP)?,
                volume: 0.5,
                looping: false,
                spatial: true,
                autoplay: true,
            }
        );
        assert_eq!(engine.world.get::<&Name>(entity)?.0, "moon");
        let again = dir.join("again.scene.ron");
        call(&reg, &mut engine, "scene.save", json!({ "path": again }));
        assert_eq!(std::fs::read_to_string(&again)?, text);

        // A scene naming a sound that does not exist fails before touching
        // the world, naming the entity and the source.
        let bad = SceneFile {
            entities: vec![crate::scene::EntityDesc {
                audio: Some(AudioDesc {
                    source: "assets/sounds/nope.ogg".into(),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        std::fs::write(dir.join("bad.scene.ron"), bad.to_ron()?)?;
        let err = call_err(
            &reg,
            &mut engine,
            "scene.load",
            json!({ "path": dir.join("bad.scene.ron") }),
        );
        assert!(
            err.contains("entity 0") && err.contains("nope.ogg"),
            "{err}"
        );
        assert_eq!(engine.world.len(), 1, "the world is untouched");
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn set_audio_is_undoable_and_null_removes() -> anyhow::Result<()> {
        let reg = ToolRegistry::with_engine_tools();
        let mut engine = silent_engine()?;
        let moon = spawn(&reg, &mut engine, "moon", [0.0; 3]);
        let has_audio = |engine: &Headless| -> Option<AudioSource> {
            engine
                .world
                .get::<&AudioSource>(Entity::from_bits(moon).unwrap())
                .ok()
                .map(|s| (*s).clone())
        };
        command::apply(
            &reg,
            &mut engine,
            "entity.set_audio",
            json!({ "entity": moon, "source": BLIP, "looping": true }),
        )?;
        let first = has_audio(&engine).expect("audio set");
        assert!(first.looping && !first.spatial);

        // Omitted fields keep their values; given ones change.
        command::apply(
            &reg,
            &mut engine,
            "entity.set_audio",
            json!({ "entity": moon, "volume": 0.25 }),
        )?;
        let second = has_audio(&engine).expect("audio kept");
        assert_eq!((second.volume, second.looping), (0.25, true));
        assert_eq!(second.source, first.source);

        command::undo(&mut engine)?;
        assert_eq!(has_audio(&engine), Some(first.clone()));
        command::undo(&mut engine)?;
        assert_eq!(has_audio(&engine), None);
        command::redo(&mut engine)?;
        assert_eq!(has_audio(&engine), Some(first));

        command::apply(
            &reg,
            &mut engine,
            "entity.set_audio",
            json!({ "entity": moon, "source": null }),
        )?;
        assert_eq!(has_audio(&engine), None);
        command::undo(&mut engine)?;
        assert!(has_audio(&engine).is_some(), "undo of a removal");

        // Without a source there is nothing to change on a silent entity.
        let cube = spawn(&reg, &mut engine, "cube", [0.0; 3]);
        let err = call_err(
            &reg,
            &mut engine,
            "entity.set_audio",
            json!({ "entity": cube, "volume": 0.5 }),
        );
        assert!(err.contains("has no audio"), "{err}");
        let err = call_err(
            &reg,
            &mut engine,
            "entity.set_audio",
            json!({ "entity": cube, "source": "assets/checker.png" }),
        );
        assert!(err.contains("not a sound"), "{err}");
        Ok(())
    }

    #[test]
    fn autoplay_sources_start_on_play_and_stop_stops_everything() -> anyhow::Result<()> {
        let reg = ToolRegistry::with_engine_tools();
        let mut engine = silent_engine()?;
        let moon = spawn(&reg, &mut engine, "moon", [2.0, 0.0, 0.0]);
        let cube = spawn(&reg, &mut engine, "cube", [0.0; 3]);
        call(
            &reg,
            &mut engine,
            "entity.set_audio",
            json!({ "entity": moon, "source": BLIP, "autoplay": true, "looping": true, "spatial": true }),
        );
        call(
            &reg,
            &mut engine,
            "entity.set_audio",
            json!({ "entity": cube, "source": BLIP }),
        );
        assert_eq!(audio(&engine).plays(), 0, "nothing plays while editing");

        call(&reg, &mut engine, "sim.play", json!({}));
        assert_eq!(
            audio(&engine).plays(),
            1,
            "only the autoplay source started"
        );
        let started = &audio(&engine).playing[0].params;
        assert!(started.looping);
        assert_eq!(started.position, Some(Vec3::new(2.0, 0.0, 0.0)));
        assert_eq!(started.entity, Entity::from_bits(moon));
        call(&reg, &mut engine, "audio.play", json!({ "source": BLIP }));
        assert_eq!(audio(&engine).playing(), 2);

        call(&reg, &mut engine, "sim.stop", json!({}));
        assert_eq!(audio(&engine).playing(), 0, "stop stops everything");
        // The audio components survive the play session.
        let kept = engine
            .world
            .get::<&AudioSource>(Entity::from_bits(moon).unwrap())?;
        assert!(kept.autoplay && kept.looping && kept.spatial);
        assert!(
            engine
                .world
                .get::<&AudioSource>(Entity::from_bits(cube).unwrap())
                .is_ok()
        );
        Ok(())
    }

    #[test]
    fn audio_system_without_an_engine_does_nothing() -> anyhow::Result<()> {
        let mut engine = Headless::new(4, 4)?;
        engine.render()?;
        assert!(
            engine.resources.get::<AudioEngine>().is_none(),
            "no device is opened until a sound is played"
        );
        Ok(())
    }

    #[test]
    fn audio_source_parses_wav_and_ogg() -> anyhow::Result<()> {
        for s in ["assets/sounds/blip.wav", "sfx/Jump.OGG", "odd#name.wav"] {
            let source = AssetSource::parse(s)?;
            assert!(source.is_audio(), "{s}");
            assert!(!source.is_texture(), "{s}");
            assert_eq!(source.to_string(), s);
            assert_eq!(
                source,
                AssetSource::Audio {
                    path: Path::new(s).to_path_buf()
                }
            );
        }
        assert!(
            !AssetSource::parse("x.wav#1")?.is_audio(),
            "an index makes it a mesh"
        );
        Ok(())
    }
}
