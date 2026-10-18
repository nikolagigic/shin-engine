//! Command history (GE-022): undo and redo over tool calls. ADR 0007: the
//! editor edits through tools, and undo is a log of tool calls.
//!
//! [`History::apply`] runs a tool through a [`ToolRegistry`] and records a
//! [`Snapshot`] of what the tool touched before and after it ran. Which
//! state that is, the tool declares through
//! [`Tool::affected`](crate::tool::Tool::affected). [`History::undo`] restores the
//! "before" snapshot and [`History::redo`] the "after" one.
//!
//! The engine keeps one [`History`] as a resource in
//! [`Headless::resources`], so every client shares it: [`apply`],
//! [`undo`], and [`redo`] use that resource, and so do the
//! `history.undo` and `history.redo` tools ([`register_tools`]).
//!
//! The history is frozen while the engine is playing (GE-011): recorded
//! calls, [`History::undo`], and [`History::redo`] fail with
//! [`FROZEN_WHILE_PLAYING`], because play mode restores the scene on stop
//! and an edit made mid-play would be lost or would undo the wrong state.
//! Unrecorded calls ([`Affected::None`], such as `sim.stop`) still run.
//!
//! Only tool calls made through [`History::apply`] (or [`apply`]) are
//! recorded; a plain [`ToolRegistry::call`] is not. Tools whose
//! [`Affected`] is [`Affected::None`] (queries, renders, asset loads,
//! `sim.step`, ...) run without being recorded and leave the redo stack
//! alone.
//!
//! A snapshot holds, per entity, the components the editing tools change:
//! [`Transform`], [`MeshInstance`], [`Name`], [`Script`], and
//! [`AudioSource`]. Restoring one keeps every
//! entity's [`Entity`] id (via `World::spawn_at`), so ids handed out by
//! `entity.spawn` stay valid across undo and redo. Other components on a
//! restored entity are kept if it is alive and lost if it is respawned.
//!
//! ```no_run
//! use serde_json::json;
//! use shin::command::History;
//! use shin::headless::Headless;
//! use shin::tool::ToolRegistry;
//!
//! let reg = ToolRegistry::with_engine_tools();
//! let mut engine = Headless::new(64, 64)?;
//! let mut history = History::default();
//! let cube = reg.call(&mut engine, "mesh.builtin", json!({ "kind": "cube" }))?;
//! # let _ = cube;
//! history.apply(&reg, &mut engine, "entity.spawn", json!({ "mesh": 0 }))?;
//! assert_eq!(engine.world.len(), 1);
//! history.undo(&mut engine)?;
//! assert_eq!(engine.world.len(), 0);
//! # anyhow::Ok(())
//! ```

mod tools;

use std::collections::HashSet;

use anyhow::Context as _;
use serde_json::Value;

pub use tools::register_tools;

use crate::audio::AudioSource;
use crate::ecs::{Entity, Name, World};
use crate::headless::Headless;
use crate::math::{Camera, Transform};
use crate::render::{LightUniform, MeshInstance, RenderState};
use crate::script::Script;
use crate::tool::{ToolError, ToolOutput, ToolRegistry};

/// The error recorded edits, undo, and redo fail with while the engine is
/// playing.
pub const FROZEN_WHILE_PLAYING: &str = "history is frozen while playing";

/// Fails with [`FROZEN_WHILE_PLAYING`] if `engine` is playing (paused or
/// not).
fn ensure_not_playing(engine: &Headless) -> anyhow::Result<()> {
    anyhow::ensure!(
        !engine.play_state().is_playing(),
        "{FROZEN_WHILE_PLAYING}; call sim.stop first"
    );
    Ok(())
}

/// The number of entries a [`History`] keeps by default.
pub const DEFAULT_LIMIT: usize = 100;

/// What state a tool call may change, as declared by
/// `Tool::affected` before the call runs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Affected {
    /// Nothing undoable: the call is not recorded.
    #[default]
    None,
    /// These existing entities (changed or despawned).
    Entities(Vec<Entity>),
    /// Entities the call creates. They are found by comparing the world
    /// before and after the call, and captured after it.
    Spawned,
    /// Every entity (for example `scene.clear`).
    AllEntities,
    /// The [`Camera`] resource.
    Camera,
    /// The [`RenderState`] light.
    Light,
    /// Every entity, the camera, and the light (for example `scene.load`).
    Scene,
}

/// The state of one entity: the components the editing tools change.
#[derive(Clone, Debug, Default)]
struct EntityState {
    transform: Option<Transform>,
    instance: Option<MeshInstance>,
    name: Option<Name>,
    script: Option<Script>,
    audio: Option<AudioSource>,
}

impl EntityState {
    /// The components of `entity`, which must be alive.
    fn capture(world: &World, entity: Entity) -> Self {
        Self {
            transform: world.get::<&Transform>(entity).ok().map(|t| *t),
            instance: world.get::<&MeshInstance>(entity).ok().map(|i| *i),
            name: world.get::<&Name>(entity).ok().map(|n| (*n).clone()),
            script: world.get::<&Script>(entity).ok().map(|s| (*s).clone()),
            audio: world.get::<&AudioSource>(entity).ok().map(|a| (*a).clone()),
        }
    }
}

/// Which entities a snapshot speaks for.
#[derive(Clone, Debug)]
enum Scope {
    /// Only these; other entities are left alone on restore.
    Only(Vec<Entity>),
    /// All of them; entities not in the snapshot are despawned on restore.
    All,
}

/// The entities part of a [`Snapshot`].
#[derive(Clone, Debug)]
struct EntitiesSnapshot {
    scope: Scope,
    /// The entities of `scope` that were alive, with their state.
    alive: Vec<(Entity, EntityState)>,
}

/// The state a tool call touched, captured before or after it: the
/// affected entities (keyed by [`Entity`] id, each with its
/// [`Transform`], [`MeshInstance`], [`Name`], and [`Script`]), plus the camera or the
/// light when the tool changes them.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    entities: Option<EntitiesSnapshot>,
    camera: Option<Camera>,
    light: Option<LightUniform>,
}

impl Snapshot {
    /// The current state of `entities` (`None`: not captured), and of the
    /// camera and light when asked for.
    fn capture(engine: &Headless, entities: Option<Scope>, camera: bool, light: bool) -> Self {
        let world = &engine.world;
        let entities = entities.map(|scope| {
            let alive = match &scope {
                Scope::Only(ids) => ids
                    .iter()
                    .filter(|&&e| world.contains(e))
                    .map(|&e| (e, EntityState::capture(world, e)))
                    .collect(),
                Scope::All => world
                    .iter()
                    .map(|e| e.entity())
                    .map(|e| (e, EntityState::capture(world, e)))
                    .collect(),
            };
            EntitiesSnapshot { scope, alive }
        });
        Self {
            entities,
            camera: camera
                .then(|| engine.resources.get::<Camera>().copied())
                .flatten(),
            light: light
                .then(|| engine.resources.get::<RenderState>().map(|s| s.light))
                .flatten(),
        }
    }

    /// Makes `engine` match this snapshot: despawns in-scope entities that
    /// are not in it, overwrites or respawns (with the same id) those that
    /// are, and sets the camera and light if they were captured.
    fn restore(&self, engine: &mut Headless) -> anyhow::Result<()> {
        if let Some(snapshot) = &self.entities {
            let world = &mut engine.world;
            let keep: HashSet<Entity> = snapshot.alive.iter().map(|(e, _)| *e).collect();
            let in_scope: Vec<Entity> = match &snapshot.scope {
                Scope::Only(ids) => ids.clone(),
                Scope::All => world.iter().map(|e| e.entity()).collect(),
            };
            for entity in in_scope {
                if !keep.contains(&entity) && world.contains(entity) {
                    world.despawn(entity)?;
                }
            }
            for (entity, state) in &snapshot.alive {
                restore_entity(world, *entity, state)?;
            }
        }
        if let Some(camera) = self.camera {
            engine.resources.insert(camera);
        }
        if let Some(light) = self.light {
            engine
                .resources
                .get_mut::<RenderState>()
                .context("no RenderState resource")?
                .light = light;
        }
        Ok(())
    }
}

/// Gives `entity` exactly the components in `state`, spawning it with that
/// id if it is not alive.
fn restore_entity(world: &mut World, entity: Entity, state: &EntityState) -> anyhow::Result<()> {
    if world.contains(entity) {
        // Removing a component the entity lacks is not an error here.
        let _ = world.remove_one::<Transform>(entity);
        let _ = world.remove_one::<MeshInstance>(entity);
        let _ = world.remove_one::<Name>(entity);
        let _ = world.remove_one::<Script>(entity);
        let _ = world.remove_one::<AudioSource>(entity);
    } else {
        world.spawn_at(entity, ());
    }
    if let Some(transform) = state.transform {
        world.insert_one(entity, transform)?;
    }
    if let Some(instance) = state.instance {
        world.insert_one(entity, instance)?;
    }
    if let Some(name) = &state.name {
        world.insert_one(entity, name.clone())?;
    }
    if let Some(script) = &state.script {
        world.insert_one(entity, script.clone())?;
    }
    if let Some(audio) = &state.audio {
        world.insert_one(entity, audio.clone())?;
    }
    Ok(())
}

/// One recorded tool call.
#[derive(Clone, Debug)]
struct Entry {
    tool: String,
    args: Value,
    before: Snapshot,
    after: Snapshot,
}

/// Undo and redo stacks of tool calls, each with the state it changed.
#[derive(Debug)]
pub struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    limit: usize,
}

impl Default for History {
    /// An empty history keeping [`DEFAULT_LIMIT`] entries.
    fn default() -> Self {
        Self::with_limit(DEFAULT_LIMIT)
    }
}

impl History {
    /// An empty history that keeps at most `limit` undo entries, dropping
    /// the oldest first.
    pub fn with_limit(limit: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            limit,
        }
    }

    /// Runs `tool` through `reg`, recording a snapshot of the affected
    /// state before and after.
    ///
    /// Successful calls whose [`Affected`] is not [`Affected::None`] are
    /// pushed onto the undo stack and clear the redo stack. Failed calls
    /// and [`Affected::None`] calls are not recorded. A call that would be
    /// recorded fails without running while the engine is playing.
    pub fn apply(
        &mut self,
        reg: &ToolRegistry,
        engine: &mut Headless,
        tool: &str,
        args: Value,
    ) -> Result<ToolOutput, ToolError> {
        let affected = reg.affected(engine, tool, &args)?;
        if affected != Affected::None {
            ensure_not_playing(engine)?;
        }
        let (entities, camera, light) = match &affected {
            Affected::None => return reg.call(engine, tool, args),
            Affected::Entities(ids) => (Some(Scope::Only(ids.clone())), false, false),
            Affected::Spawned => (Some(Scope::Only(Vec::new())), false, false),
            Affected::AllEntities => (Some(Scope::All), false, false),
            Affected::Camera => (None, true, false),
            Affected::Light => (None, false, true),
            Affected::Scene => (Some(Scope::All), true, true),
        };
        let existing: HashSet<Entity> = match affected {
            Affected::Spawned => engine.world.iter().map(|e| e.entity()).collect(),
            _ => HashSet::new(),
        };
        let mut before = Snapshot::capture(engine, entities.clone(), camera, light);

        let output = reg.call(engine, tool, args.clone())?;

        let entities = match affected {
            Affected::Spawned => {
                let mut new: Vec<Entity> = engine
                    .world
                    .iter()
                    .map(|e| e.entity())
                    .filter(|e| !existing.contains(e))
                    .collect();
                new.sort();
                // Before the call none of them existed.
                before.entities = Some(EntitiesSnapshot {
                    scope: Scope::Only(new.clone()),
                    alive: Vec::new(),
                });
                Some(Scope::Only(new))
            }
            _ => entities,
        };
        let after = Snapshot::capture(engine, entities, camera, light);
        self.push(Entry {
            tool: tool.to_string(),
            args,
            before,
            after,
        });
        Ok(output)
    }

    /// Restores the state before the most recent entry and moves it to the
    /// redo stack. Returns `false` if there was nothing to undo. Fails
    /// while the engine is playing.
    pub fn undo(&mut self, engine: &mut Headless) -> anyhow::Result<bool> {
        ensure_not_playing(engine)?;
        let Some(entry) = self.undo.pop() else {
            return Ok(false);
        };
        if let Err(err) = entry.before.restore(engine) {
            self.undo.push(entry);
            return Err(err.context("undo failed"));
        }
        self.redo.push(entry);
        Ok(true)
    }

    /// Restores the state after the most recently undone entry and moves it
    /// back to the undo stack. Returns `false` if there was nothing to redo.
    /// Fails while the engine is playing.
    pub fn redo(&mut self, engine: &mut Headless) -> anyhow::Result<bool> {
        ensure_not_playing(engine)?;
        let Some(entry) = self.redo.pop() else {
            return Ok(false);
        };
        if let Err(err) = entry.after.restore(engine) {
            self.redo.push(entry);
            return Err(err.context("redo failed"));
        }
        self.undo.push(entry);
        Ok(true)
    }

    /// The undoable calls, oldest first, as `(tool, args)`.
    pub fn undo_entries(&self) -> impl DoubleEndedIterator<Item = (&str, &Value)> {
        self.undo.iter().map(|e| (e.tool.as_str(), &e.args))
    }

    /// The redoable calls, the next one to redo last, as `(tool, args)`.
    pub fn redo_entries(&self) -> impl DoubleEndedIterator<Item = (&str, &Value)> {
        self.redo.iter().map(|e| (e.tool.as_str(), &e.args))
    }

    /// The number of entries [`Self::undo`] can undo.
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    /// The number of entries [`Self::redo`] can redo.
    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// Records a new entry: clears the redo stack and drops the oldest
    /// entries beyond the limit.
    fn push(&mut self, entry: Entry) {
        self.redo.clear();
        self.undo.push(entry);
        if self.undo.len() > self.limit {
            let excess = self.undo.len() - self.limit;
            self.undo.drain(..excess);
        }
    }
}

/// [`History::apply`] on the engine's [`History`] resource (inserted if
/// missing).
///
/// Calls with [`Affected::None`] run directly, so `history.undo` and
/// `history.redo` can themselves be called through here.
pub fn apply(
    reg: &ToolRegistry,
    engine: &mut Headless,
    tool: &str,
    args: Value,
) -> Result<ToolOutput, ToolError> {
    if reg.affected(engine, tool, &args)? == Affected::None {
        return reg.call(engine, tool, args);
    }
    with_history(engine, |history, engine| {
        history.apply(reg, engine, tool, args)
    })
}

/// [`History::undo`] on the engine's [`History`] resource.
pub fn undo(engine: &mut Headless) -> anyhow::Result<bool> {
    with_history(engine, History::undo)
}

/// [`History::redo`] on the engine's [`History`] resource.
pub fn redo(engine: &mut Headless) -> anyhow::Result<bool> {
    with_history(engine, History::redo)
}

/// Runs `f` with the engine's [`History`] resource (a default one if
/// missing) moved out of the resources, and puts it back afterwards.
fn with_history<T>(engine: &mut Headless, f: impl FnOnce(&mut History, &mut Headless) -> T) -> T {
    let mut history = engine
        .resources
        .get_mut::<History>()
        .map(std::mem::take)
        .unwrap_or_default();
    let result = f(&mut history, engine);
    engine.resources.insert(history);
    result
}

#[cfg(test)]
mod tests;
