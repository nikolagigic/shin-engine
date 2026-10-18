//! Audio tools (GE-036): `audio.play`, `audio.stop_all`, and
//! `entity.set_audio`.

use anyhow::Context as _;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use super::{AudioEngine, AudioSource, PlayParams};
use crate::asset::AssetSource;
use crate::command::Affected;
use crate::ecs::Entity;
use crate::headless::Headless;
use crate::math::Transform;
use crate::tool::{ToolOutput, ToolRegistry, TypedTool};

/// Registers `audio.play`, `audio.stop_all`, and `entity.set_audio`.
pub fn register_tools(reg: &mut ToolRegistry) {
    reg.register(TypedTool::new(
        "audio.play",
        "Plays a sound once: an .ogg or .wav asset source, at a linear volume, optionally \
         positioned at (and following) an entity so it fades with the distance from the \
         camera. Returns `plays`, the number of sounds played so far, `playing`, how many \
         are playing now, and `silent`, true when there is no audio output device (sounds \
         are then counted but not heard).",
        audio_play,
    ));
    reg.register(TypedTool::new(
        "audio.stop_all",
        "Stops every playing sound. Returns `stopped`, how many were playing.",
        audio_stop_all,
    ));
    reg.register(
        TypedTool::new(
            "entity.set_audio",
            "Sets the sound attached to an entity: its source (an .ogg or .wav path; null \
             removes the sound), volume, and whether it loops, is spatial (fades with the \
             distance from the camera), and autoplays when play starts. Omitted fields keep \
             their values; a new sound starts at full volume, one-shot, not spatial, no \
             autoplay. Returns the entity and its `audio` (null when removed).",
            entity_set_audio,
        )
        .with_affected(|_, args| {
            let entity = args
                .get("entity")
                .and_then(Value::as_u64)
                .and_then(Entity::from_bits);
            Affected::Entities(entity.into_iter().collect())
        }),
    );
}

/// Arguments of `audio.play`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PlayArgs {
    /// Sound asset source: an .ogg or .wav path, e.g. "assets/sounds/blip.wav",
    /// relative to the project directory.
    source: String,
    /// Linear volume; default 1.0 (as recorded), 0.0 is silent.
    volume: Option<f32>,
    /// Entity id to play the sound at: the volume then falls off with the
    /// distance from the camera and the sound follows the entity. Default
    /// none: the sound plays everywhere at `volume`.
    entity: Option<u64>,
}

fn audio_play(engine: &mut Headless, args: PlayArgs) -> anyhow::Result<ToolOutput> {
    let source = AssetSource::parse(&args.source)?;
    anyhow::ensure!(
        source.is_audio(),
        "{source} is not a sound source (.ogg or .wav)"
    );
    let entity = args.entity.map(|id| find_entity(engine, id)).transpose()?;
    let position = entity
        .map(|entity| {
            engine
                .world
                .get::<&Transform>(entity)
                .map(|t| t.translation)
                .with_context(|| {
                    format!(
                        "entity {} has no transform to play a sound at",
                        entity.to_bits().get()
                    )
                })
        })
        .transpose()?;
    let params = PlayParams {
        volume: args.volume.unwrap_or(1.0),
        looping: false,
        position,
        entity,
    };
    let plays = super::play(&mut engine.resources, &source, params)?;
    let audio = engine.resources.expect::<AudioEngine>();
    Ok(ToolOutput::Json(json!({
        "plays": plays,
        "playing": audio.playing(),
        "silent": audio.is_silent(),
    })))
}

/// `audio.stop_all` takes no arguments.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

fn audio_stop_all(engine: &mut Headless, _: NoArgs) -> anyhow::Result<ToolOutput> {
    let stopped = super::stop_all(&mut engine.resources);
    Ok(ToolOutput::Json(json!({ "stopped": stopped })))
}

/// Arguments of `entity.set_audio`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetAudioArgs {
    /// Entity id, as returned by entity.spawn or entity.list.
    entity: u64,
    /// Sound asset source: an .ogg or .wav path relative to the project
    /// directory, e.g. "assets/sounds/blip.wav". `null` removes the
    /// entity's sound; omitted keeps the current one.
    #[serde(default, deserialize_with = "explicit_null")]
    source: Option<Option<String>>,
    /// Linear volume; 1.0 plays the file as recorded.
    volume: Option<f32>,
    /// Whether the sound repeats until stopped.
    looping: Option<bool>,
    /// Whether the volume falls off with the distance from the camera.
    spatial: Option<bool>,
    /// Whether the sound starts when play starts.
    autoplay: Option<bool>,
}

/// Deserializes a field so that a missing field (via `#[serde(default)]`)
/// is `None`, an explicit `null` is `Some(None)`, and a value is
/// `Some(Some(value))`.
fn explicit_null<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

fn entity_set_audio(engine: &mut Headless, args: SetAudioArgs) -> anyhow::Result<ToolOutput> {
    let entity = find_entity(engine, args.entity)?;
    let current = engine
        .world
        .get::<&AudioSource>(entity)
        .ok()
        .map(|s| (*s).clone());
    let mut audio = match (args.source, current) {
        (Some(None), _) => {
            // Removing a sound the entity does not have is not an error.
            let _ = engine.world.remove_one::<AudioSource>(entity);
            return Ok(ToolOutput::Json(
                json!({ "entity": args.entity, "audio": null }),
            ));
        }
        (Some(Some(text)), current) => {
            let source = AssetSource::parse(&text)?;
            anyhow::ensure!(
                source.is_audio(),
                "{source} is not a sound source (.ogg or .wav)"
            );
            // Decode now so a bad file is reported here, not at play time.
            engine
                .resources
                .get_mut::<crate::asset::Assets>()
                .context("no Assets resource")?
                .sound(&source)?;
            match current {
                Some(mut current) => {
                    current.source = source;
                    current
                }
                None => AudioSource::new(source),
            }
        }
        (None, Some(current)) => current,
        (None, None) => anyhow::bail!(
            "entity {} has no audio source; give `source` to add one",
            args.entity
        ),
    };
    if let Some(volume) = args.volume {
        audio.volume = volume;
    }
    if let Some(looping) = args.looping {
        audio.looping = looping;
    }
    if let Some(spatial) = args.spatial {
        audio.spatial = spatial;
    }
    if let Some(autoplay) = args.autoplay {
        audio.autoplay = autoplay;
    }
    let description = audio_json(&audio);
    engine.world.insert_one(entity, audio)?;
    Ok(ToolOutput::Json(
        json!({ "entity": args.entity, "audio": description }),
    ))
}

/// An [`AudioSource`] as the tools report it.
pub(crate) fn audio_json(audio: &AudioSource) -> Value {
    json!({
        "source": audio.source.to_string(),
        "volume": audio.volume,
        "looping": audio.looping,
        "spatial": audio.spatial,
        "autoplay": audio.autoplay,
    })
}

/// The live entity with tool id `id`.
fn find_entity(engine: &Headless, id: u64) -> anyhow::Result<Entity> {
    Entity::from_bits(id)
        .filter(|&entity| engine.world.contains(entity))
        .with_context(|| format!("no entity with id {id}"))
}
