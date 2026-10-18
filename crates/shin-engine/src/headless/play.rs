//! Play mode (GE-024): run the simulation from the scene being edited and
//! put the scene back exactly as it was when play stops.
//!
//! [`Headless::play`] captures the scene as a [`SceneFile`] in memory (the
//! snapshot is the scene file format), together with the [`Time`] resource
//! and the ids of the captured entities, and stores it in the [`PlayState`]
//! resource. While playing, [`Headless::step`] (or `sim.step`, or the
//! editor's own fixed-timestep loop) runs the fixed systems as usual.
//! [`Headless::stop`] loads the snapshot back with [`scene::apply`],
//! respawns each entity under the id it had before play (so ids handed out
//! by `entity.spawn`, the editor's selection, and the command history stay
//! valid), and restores [`Time`].
//!
//! Only what a scene file holds survives a play session: entities with a
//! [`Transform`] (their transform, mesh, colour, texture, name, script,
//! and audio source), the
//! camera, and the light. Other components and entities without a
//! transform are gone after [`Headless::stop`].
//!
//! Audio (GE-036): [`Headless::play`] starts every [`AudioSource`] with
//! `autoplay` set and [`Headless::stop`] stops every sound.
//!
//! Tools: [`register_tools`] adds `sim.play`, `sim.pause`, and `sim.stop`.

use anyhow::Context as _;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::Headless;
use crate::audio::{self, AudioSource};
use crate::ecs::{Entity, Name, Time};
use crate::math::Transform;
use crate::render::MeshInstance;
use crate::scene::{self, SceneFile};
use crate::script::Script;
use crate::tool::{ToolOutput, ToolRegistry, TypedTool};

/// Resource: whether the scene is being edited or played. [`Headless`]
/// installs [`PlayState::Edit`]; [`Headless::play`], [`Headless::pause`],
/// and [`Headless::stop`] change it.
#[derive(Clone, Debug, Default)]
pub enum PlayState {
    /// The scene is being edited; nothing steps it on its own.
    #[default]
    Edit,
    /// The scene is being played from `snapshot`.
    Playing {
        /// What [`Headless::stop`] restores.
        snapshot: Box<PlaySnapshot>,
        /// Whether stepping is suspended (the editor does not advance the
        /// simulation while paused).
        paused: bool,
    },
}

impl PlayState {
    /// Whether a play session is running, paused or not.
    pub fn is_playing(&self) -> bool {
        matches!(self, Self::Playing { .. })
    }

    /// Whether a play session is running and paused.
    pub fn is_paused(&self) -> bool {
        matches!(self, Self::Playing { paused: true, .. })
    }

    /// `"edit"`, `"playing"`, or `"paused"`, as the `sim.*` tools report it.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Edit => "edit",
            Self::Playing { paused: false, .. } => "playing",
            Self::Playing { paused: true, .. } => "paused",
        }
    }
}

/// The state captured when play starts.
#[derive(Clone, Debug)]
pub struct PlaySnapshot {
    /// The scene as it was, in scene file form.
    scene: SceneFile,
    /// The id of each entity in `scene.entities`, in the same order.
    ids: Vec<Entity>,
    /// The [`Time`] resource as it was.
    time: Time,
}

impl PlaySnapshot {
    /// The scene as it was when play started.
    pub fn scene(&self) -> &SceneFile {
        &self.scene
    }

    /// The [`Time`] resource as it was when play started.
    pub fn time(&self) -> Time {
        self.time
    }
}

impl Headless {
    /// The current [`PlayState`] (`Edit` if the resource is missing).
    pub fn play_state(&self) -> &PlayState {
        const EDIT: &PlayState = &PlayState::Edit;
        self.resources.get::<PlayState>().unwrap_or(EDIT)
    }

    /// Simulation seconds since play started, or `None` while editing.
    pub fn play_time(&self) -> Option<f64> {
        let PlayState::Playing { snapshot, .. } = self.play_state() else {
            return None;
        };
        let now = self.resources.get::<Time>().map_or(0.0, |t| t.elapsed);
        Some(now - snapshot.time.elapsed)
    }

    /// Starts playing: snapshots the scene ([`SceneFile::capture`]), the
    /// entity ids, and [`Time`], sets [`PlayState::Playing`], and starts
    /// every [`AudioSource`] with `autoplay` set
    /// ([`audio::play_autoplay_sources`]). Already playing, it resumes if
    /// paused and otherwise does nothing.
    ///
    /// Fails, leaving the engine in edit mode, if the scene cannot be
    /// captured (a mesh or texture with no asset source, or an
    /// orthographic camera).
    pub fn play(&mut self) -> anyhow::Result<()> {
        if let Some(PlayState::Playing { paused, .. }) = self.resources.get_mut::<PlayState>() {
            *paused = false;
            return Ok(());
        }
        let scene = SceneFile::capture(&self.world, &self.resources)
            .context("cannot play: the scene cannot be snapshotted")?;
        // `capture` lists entities with a transform in id order.
        let mut ids: Vec<Entity> = self
            .world
            .query::<(Entity, &Transform)>()
            .iter()
            .map(|(entity, _)| entity)
            .collect();
        ids.sort_by_key(|entity| entity.to_bits());
        let time = self.resources.get::<Time>().copied().unwrap_or_default();
        self.resources.insert(PlayState::Playing {
            snapshot: Box::new(PlaySnapshot { scene, ids, time }),
            paused: false,
        });
        audio::play_autoplay_sources(&self.world, &mut self.resources);
        log::info!("play");
        Ok(())
    }

    /// Toggles pause while playing and returns whether it is now paused.
    ///
    /// Fails if nothing is playing.
    pub fn pause(&mut self) -> anyhow::Result<bool> {
        match self.resources.get_mut::<PlayState>() {
            Some(PlayState::Playing { paused, .. }) => {
                *paused = !*paused;
                log::info!("{}", if *paused { "pause" } else { "resume" });
                Ok(*paused)
            }
            _ => anyhow::bail!("nothing is playing; call sim.play first"),
        }
    }

    /// Stops playing: stops every sound, restores the scene, entity ids,
    /// and [`Time`] captured by [`Self::play`], and sets
    /// [`PlayState::Edit`]. Returns `false` (and changes nothing) if
    /// nothing was playing.
    ///
    /// Fails, still playing, if the snapshot cannot be loaded back.
    pub fn stop(&mut self) -> anyhow::Result<bool> {
        let Some(PlayState::Playing { snapshot, .. }) = self.resources.get::<PlayState>() else {
            return Ok(false);
        };
        let snapshot = snapshot.clone();
        audio::stop_all(&mut self.resources);
        scene::apply(
            &mut self.world,
            &mut self.resources,
            &self.gpu.device,
            &self.gpu.queue,
            &snapshot.scene,
        )
        .context("cannot stop: the play snapshot did not load")?;
        restore_ids(&mut self.world, &snapshot.ids)?;
        self.resources.insert(snapshot.time);
        self.resources.insert(PlayState::Edit);
        log::info!("stop");
        Ok(true)
    }
}

/// Respawns the entities [`scene::apply`] just spawned (in spawn order)
/// under `ids`, one for one, keeping their scene components.
fn restore_ids(world: &mut crate::ecs::World, ids: &[Entity]) -> anyhow::Result<()> {
    type Row = (
        Transform,
        Option<MeshInstance>,
        Option<Name>,
        Option<Script>,
        Option<AudioSource>,
    );
    let mut rows: Vec<(u64, Row)> = world
        .query::<(
            Entity,
            &Transform,
            Option<&MeshInstance>,
            Option<&Name>,
            Option<&Script>,
            Option<&AudioSource>,
        )>()
        .iter()
        .map(|(e, t, i, n, s, a)| {
            let row = (*t, i.copied(), n.cloned(), s.cloned(), a.cloned());
            (e.to_bits().get(), row)
        })
        .collect();
    anyhow::ensure!(
        rows.len() == ids.len(),
        "the play snapshot has {} entities but {} ids",
        rows.len(),
        ids.len()
    );
    rows.sort_by_key(|(bits, _)| *bits);
    world.clear();
    for (&id, (_, (transform, instance, name, script, audio))) in ids.iter().zip(rows) {
        let mut builder = hecs::EntityBuilder::new();
        builder.add(transform);
        if let Some(instance) = instance {
            builder.add(instance);
        }
        if let Some(name) = name {
            builder.add(name);
        }
        if let Some(script) = script {
            builder.add(script);
        }
        if let Some(audio) = audio {
            builder.add(audio);
        }
        world.spawn_at(id, builder.build());
    }
    Ok(())
}

/// Registers `sim.play`, `sim.pause`, and `sim.stop`.
pub fn register_tools(reg: &mut ToolRegistry) {
    reg.register(TypedTool::new(
        "sim.play",
        "Enters play mode: snapshots the scene so sim.stop can restore it, starts every \
         audio source with autoplay set, then lets the simulation run (advance it with \
         sim.step). Resumes if paused. Returns the play state (`state`: edit, playing, or \
         paused) and `sim_time`, the simulated seconds since play started.",
        sim_play,
    ));
    reg.register(TypedTool::new(
        "sim.pause",
        "Toggles pause while playing (the editor stops stepping the simulation while \
         paused). Fails if nothing is playing. Returns `state` and `sim_time`.",
        sim_pause,
    ));
    reg.register(TypedTool::new(
        "sim.stop",
        "Leaves play mode: stops every sound and restores the scene (every entity with its \
         id, transform, mesh, colour, texture, name, script, and audio; the camera; the \
         light) and the simulation time to what they were at sim.play. Returns `stopped` \
         (false if nothing was playing), `state`, and `entities`, the number of entities \
         restored.",
        sim_stop,
    ));
}

/// `sim.play`, `sim.pause`, and `sim.stop` take no arguments.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

/// `{ state, sim_time }` for the engine's current play state.
fn status(engine: &Headless) -> Value {
    json!({
        "state": engine.play_state().label(),
        "sim_time": engine.play_time().unwrap_or(0.0),
    })
}

fn sim_play(engine: &mut Headless, _: NoArgs) -> anyhow::Result<ToolOutput> {
    engine.play()?;
    Ok(ToolOutput::Json(status(engine)))
}

fn sim_pause(engine: &mut Headless, _: NoArgs) -> anyhow::Result<ToolOutput> {
    engine.pause()?;
    Ok(ToolOutput::Json(status(engine)))
}

fn sim_stop(engine: &mut Headless, _: NoArgs) -> anyhow::Result<ToolOutput> {
    let stopped = engine.stop()?;
    let entities = engine.world.query::<&Transform>().iter().count();
    Ok(ToolOutput::Json(json!({
        "stopped": stopped,
        "state": engine.play_state().label(),
        "entities": entities,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ecs::{Resources, World};
    use crate::math::Vec3;

    /// An `Orbit`-style fixed system: puts every entity named `moon` on a
    /// unit circle at `Time::elapsed` radians.
    fn orbit_moon(world: &mut World, res: &mut Resources) {
        let t = res.expect::<Time>().elapsed as f32;
        for (name, transform) in world.query_mut::<(&Name, &mut Transform)>() {
            if name.0 == "moon" {
                transform.translation = Vec3::new(t.cos(), 0.5, -t.sin()) * 2.0;
            }
        }
    }

    fn call(reg: &ToolRegistry, engine: &mut Headless, tool: &str) -> anyhow::Result<Value> {
        match reg.call(engine, tool, json!({}))? {
            ToolOutput::Json(value) => Ok(value),
            other => anyhow::bail!("{tool} returned {other:?}"),
        }
    }

    /// An engine with the orbit system, a cube mesh, and a named `moon`
    /// and `cube` spawned through the tools.
    fn orbit_engine(reg: &ToolRegistry) -> anyhow::Result<(Headless, Entity)> {
        let mut engine = Headless::new(16, 16)?;
        engine.schedule.add_fixed("orbit_moon", orbit_moon);
        reg.call(&mut engine, "mesh.builtin", json!({ "kind": "cube" }))?;
        reg.call(
            &mut engine,
            "entity.spawn",
            json!({ "mesh": 0, "name": "cube", "rotation_euler_deg": [0, 30, 0] }),
        )?;
        let moon = reg.call(
            &mut engine,
            "entity.spawn",
            json!({ "mesh": 0, "name": "moon", "translation": [1.6, 0.5, 0.0], "scale": [0.3, 0.3, 0.3] }),
        )?;
        let ToolOutput::Json(moon) = moon else {
            anyhow::bail!("entity.spawn returned no JSON");
        };
        let bits = moon["entity"].as_u64().context("no entity id")?;
        let moon = Entity::from_bits(bits).context("bad entity id")?;
        Ok((engine, moon))
    }

    #[test]
    fn sim_tools_are_listed() {
        let reg = ToolRegistry::with_engine_tools();
        for name in ["sim.play", "sim.pause", "sim.stop"] {
            assert!(reg.list().iter().any(|t| t.name == name), "{name} listed");
        }
    }

    /// The ticket's registry test: play, step (elapsed and the moon move),
    /// stop: transforms equal the pre-play scene file, ids are kept.
    #[test]
    fn sim_play_step_stop_restores_the_scene() -> anyhow::Result<()> {
        let reg = ToolRegistry::with_engine_tools();
        let (mut engine, moon) = orbit_engine(&reg)?;
        engine.step(5);
        let before = SceneFile::capture(&engine.world, &engine.resources)?;
        let moon_before = *engine.world.get::<&Transform>(moon)?;
        let time_before = *engine.resources.expect::<Time>();

        let played = call(&reg, &mut engine, "sim.play")?;
        assert_eq!(played["state"], "playing");
        assert_eq!(played["sim_time"], 0.0);
        let stepped = call(&reg, &mut engine, "sim.step")?;
        reg.call(&mut engine, "sim.step", json!({ "steps": 59 }))?;
        assert!(stepped["elapsed"].as_f64().unwrap() > time_before.elapsed);
        let sim_time = engine.play_time().unwrap();
        assert!(
            (sim_time - 1.0).abs() < 1e-9,
            "one second played: {sim_time}"
        );
        let moved = engine.world.get::<&Transform>(moon)?.translation;
        assert!(
            moved.distance(moon_before.translation) > 0.1,
            "the moon moved"
        );

        let stopped = call(&reg, &mut engine, "sim.stop")?;
        assert_eq!(stopped["stopped"], true);
        assert_eq!(stopped["state"], "edit");
        assert_eq!(stopped["entities"], 2);
        let after = SceneFile::capture(&engine.world, &engine.resources)?;
        assert_eq!(after, before, "the scene equals the pre-play file");
        assert_eq!(
            *engine.world.get::<&Transform>(moon)?,
            moon_before,
            "the moon is back under its old id, exactly"
        );
        let time = engine.resources.expect::<Time>();
        assert_eq!(time.fixed_steps, time_before.fixed_steps);
        assert_eq!(time.elapsed, time_before.elapsed);
        assert!(!engine.play_state().is_playing());
        Ok(())
    }

    #[test]
    fn sim_pause_toggles_and_needs_play() -> anyhow::Result<()> {
        let reg = ToolRegistry::with_engine_tools();
        let mut engine = Headless::new(4, 4)?;
        let err = reg
            .call(&mut engine, "sim.pause", json!({}))
            .expect_err("pause without play fails");
        assert!(format!("{err:#}").contains("sim.play"), "{err:#}");

        call(&reg, &mut engine, "sim.play")?;
        assert_eq!(call(&reg, &mut engine, "sim.pause")?["state"], "paused");
        assert!(engine.play_state().is_paused());
        assert_eq!(call(&reg, &mut engine, "sim.pause")?["state"], "playing");
        call(&reg, &mut engine, "sim.pause")?;
        assert_eq!(
            call(&reg, &mut engine, "sim.play")?["state"],
            "playing",
            "play resumes"
        );
        Ok(())
    }

    #[test]
    fn sim_stop_without_play_changes_nothing() -> anyhow::Result<()> {
        let reg = ToolRegistry::with_engine_tools();
        let (mut engine, moon) = orbit_engine(&reg)?;
        let stopped = call(&reg, &mut engine, "sim.stop")?;
        assert_eq!(stopped["stopped"], false);
        assert_eq!(stopped["state"], "edit");
        assert!(engine.world.contains(moon));
        Ok(())
    }

    #[test]
    fn sim_play_twice_keeps_the_first_snapshot() -> anyhow::Result<()> {
        let reg = ToolRegistry::with_engine_tools();
        let (mut engine, moon) = orbit_engine(&reg)?;
        let start = *engine.world.get::<&Transform>(moon)?;
        call(&reg, &mut engine, "sim.play")?;
        engine.step(30);
        call(&reg, &mut engine, "sim.play")?;
        engine.step(30);
        call(&reg, &mut engine, "sim.stop")?;
        assert_eq!(*engine.world.get::<&Transform>(moon)?, start);
        Ok(())
    }
}
