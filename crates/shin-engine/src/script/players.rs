//! Multiplayer for scripts (GE-044): player input, spawning, and the
//! server script's join and leave hooks.
//!
//! - `player_input(id, name)` and `player_input_pressed(id, name)`: player
//!   `id`'s axis and held state of action `name`, from the server's
//!   [`PlayerInputs`]. An action the project does not define is a runtime
//!   error, as with `action_axis`; a player with no input reads as at rest.
//! - `spawn_entity()` (`spawn` is a reserved word in Rhai): a new entity,
//!   set up by chaining `name(text)`, `mesh(source)`, `color(r, g, b)`,
//!   `at(vec3)`, `script(path)`, `replicated(bool)`, `owner(id)`, and
//!   `mover(speed, jump_impulse)` (a [`Mover`]: it walks with its owner's
//!   input, predicted on that player's client; GE-048). It appears after
//!   the step, when the engine can upload its mesh
//!   ([`apply_commands`]).
//! - `despawn_owned(id)`: removes every entity player `id` owns (with its
//!   children), also after the step.
//!
//! Like sounds, spawns and despawns are queued [`Command`]s: Rhai
//! functions cannot reach the world, and a mesh needs the GPU.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use anyhow::Context as _;
use rhai::{Engine, EvalAltResult, FLOAT, INT};

use super::input::{ActionSnapshot, check};
use crate::asset::{AssetSource, Assets};
use crate::ecs::{Entity, Name, Resources, World, despawn_recursive};
use crate::math::{Transform, Vec3};
use crate::net::{Owner, PlayerId, PlayerInputs, Replicated};
use crate::physics::Mover;
use crate::render::{MeshInstance, RenderState};

use super::{Script, ScriptEngine};

/// The [`PlayerInputs`] scripts read, copied before each batch of calls.
pub(crate) type InputSnapshot = Rc<RefCell<PlayerInputs>>;

/// What scripts asked the world to do, in the order they asked.
pub(crate) type CommandQueue = Rc<RefCell<Vec<Command>>>;

/// A change to the world a script asked for.
#[derive(Debug)]
pub(crate) enum Command {
    /// Spawn this entity. Shared with the script's `spawn_entity()` handle, so
    /// calls chained after `spawn_entity()` still change it.
    Spawn(Rc<RefCell<SpawnDesc>>),
    /// Despawn every entity this player owns.
    DespawnOwned(PlayerId),
}

/// An entity a script spawns.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SpawnDesc {
    /// Its [`Name`].
    name: Option<String>,
    /// Mesh source string, e.g. `builtin://cube`.
    mesh: Option<String>,
    /// Linear RGBA tint of the mesh.
    color: [f32; 4],
    /// Where it starts, in world space.
    at: Vec3,
    /// Its script, if any.
    script: Option<String>,
    /// Whether snapshots carry it ([`Replicated`]).
    replicated: bool,
    /// The player who owns it ([`Owner`]).
    owner: Option<PlayerId>,
    /// How it walks, if it does ([`Mover`]).
    mover: Option<Mover>,
}

impl Default for SpawnDesc {
    fn default() -> Self {
        Self {
            name: None,
            mesh: None,
            color: [1.0; 4],
            at: Vec3::ZERO,
            script: None,
            replicated: false,
            owner: None,
            mover: None,
        }
    }
}

/// What `spawn_entity()` returns: a handle to the queued [`SpawnDesc`]; every
/// setter changes it and returns the handle, so calls chain.
#[derive(Clone, Debug)]
pub(crate) struct Spawn(Rc<RefCell<SpawnDesc>>);

/// Registers `player_input`, `player_input_pressed`, `spawn` and its
/// setters, and `despawn_owned`. Action names are checked against
/// `actions`; values come from `inputs`; spawns and despawns go onto
/// `commands`.
pub(crate) fn register(
    engine: &mut Engine,
    actions: ActionSnapshot,
    inputs: InputSnapshot,
    commands: CommandQueue,
) {
    let (axis_actions, axis_inputs) = (Rc::clone(&actions), Rc::clone(&inputs));
    engine.register_fn("player_input", move |id: INT, name: &str| {
        check(&axis_actions, name)?;
        let player = player_id(id)?;
        Ok::<_, Box<EvalAltResult>>(axis_inputs.borrow().axis(player, name) as FLOAT)
    });
    engine.register_fn("player_input_pressed", move |id: INT, name: &str| {
        check(&actions, name)?;
        let player = player_id(id)?;
        Ok::<_, Box<EvalAltResult>>(inputs.borrow().pressed(player, name))
    });

    let spawns = Rc::clone(&commands);
    engine
        .register_type_with_name::<Spawn>("Spawn")
        .register_fn("spawn_entity", move || {
            let desc = Rc::new(RefCell::new(SpawnDesc::default()));
            spawns.borrow_mut().push(Command::Spawn(Rc::clone(&desc)));
            Spawn(desc)
        })
        .register_fn("name", |s: &mut Spawn, name: &str| {
            s.0.borrow_mut().name = Some(name.to_string());
            s.clone()
        })
        .register_fn("mesh", |s: &mut Spawn, source: &str| {
            s.0.borrow_mut().mesh = Some(source.to_string());
            s.clone()
        })
        .register_fn("color", |s: &mut Spawn, r: FLOAT, g: FLOAT, b: FLOAT| {
            s.0.borrow_mut().color = [r as f32, g as f32, b as f32, 1.0];
            s.clone()
        })
        .register_fn("at", |s: &mut Spawn, at: Vec3| {
            s.0.borrow_mut().at = at;
            s.clone()
        })
        .register_fn("script", |s: &mut Spawn, path: &str| {
            s.0.borrow_mut().script = Some(path.to_string());
            s.clone()
        })
        .register_fn("replicated", |s: &mut Spawn, on: bool| {
            s.0.borrow_mut().replicated = on;
            s.clone()
        })
        .register_fn("owner", |s: &mut Spawn, id: INT| {
            s.0.borrow_mut().owner = Some(player_id(id)?);
            Ok::<_, Box<EvalAltResult>>(s.clone())
        })
        .register_fn("mover", |s: &mut Spawn, speed: FLOAT, jump: FLOAT| {
            let mover = Mover::new(speed as f32, jump as f32);
            mover.validate().map_err(|err| format!("{err:#}"))?;
            s.0.borrow_mut().mover = Some(mover);
            Ok::<_, Box<EvalAltResult>>(s.clone())
        })
        .register_fn("to_string", |s: &mut Spawn| format!("{:?}", *s.0.borrow()));

    engine.register_fn("despawn_owned", move |id: INT| {
        commands
            .borrow_mut()
            .push(Command::DespawnOwned(player_id(id)?));
        Ok::<_, Box<EvalAltResult>>(())
    });
}

/// `id` as a [`PlayerId`], or a runtime error if it is out of range.
fn player_id(id: INT) -> Result<PlayerId, Box<EvalAltResult>> {
    u32::try_from(id)
        .map(PlayerId)
        .map_err(|_| format!("{id} is not a player id").into())
}

/// Carries out the spawns and despawns scripts asked for, in order. Call
/// it after the fixed steps of each frame (or
/// [`crate::headless::Headless::step`]), with the device the world renders
/// on; the engine drivers do, next to [`crate::net::apply_pending`].
///
/// A spawned entity gets a [`Transform`] at its `at`, and its [`Name`],
/// [`MeshInstance`] (the mesh resolved through the [`Assets`] resource),
/// [`Script`], [`Replicated`], [`Owner`], and [`Mover`] as the script set
/// them.
///
/// Does nothing without a [`ScriptEngine`], or before a [`RenderState`]
/// exists (a window creates it on its first frame): the commands wait
/// until then. A spawn whose mesh does not load is skipped and its error
/// returned after the rest are carried out.
pub fn apply_commands(
    world: &mut World,
    res: &mut Resources,
    device: &wgpu::Device,
) -> anyhow::Result<()> {
    if res.get::<RenderState>().is_none() {
        return Ok(());
    }
    let Some(scripts) = res.get_mut::<ScriptEngine>() else {
        return Ok(());
    };
    let commands = std::mem::take(&mut *scripts.commands.borrow_mut());
    let mut first_error = None;
    for command in commands {
        let result = match command {
            Command::Spawn(desc) => spawn(world, res, device, &desc.borrow()).map(|_| ()),
            Command::DespawnOwned(player) => despawn_owned(world, player),
        };
        if let Err(err) = result {
            first_error.get_or_insert(err);
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Spawns `desc`; fails, spawning nothing, if its mesh does not load.
fn spawn(
    world: &mut World,
    res: &mut Resources,
    device: &wgpu::Device,
    desc: &SpawnDesc,
) -> anyhow::Result<Entity> {
    let mesh = desc
        .mesh
        .as_deref()
        .map(|text| {
            mesh_handle(res, device, text)
                .with_context(|| format!("script spawn: mesh {text:?} could not be loaded"))
        })
        .transpose()?;
    let entity = world.spawn((Transform::from_translation(desc.at),));
    if let Some(name) = &desc.name {
        world.insert_one(entity, Name(name.clone()))?;
    }
    if let Some(mesh) = mesh {
        let instance = MeshInstance {
            mesh,
            color: desc.color,
            texture: None,
        };
        world.insert_one(entity, instance)?;
    }
    if let Some(path) = &desc.script {
        world.insert_one(entity, Script::new(PathBuf::from(path)))?;
    }
    if desc.replicated {
        world.insert_one(entity, Replicated)?;
    }
    if let Some(player) = desc.owner {
        world.insert_one(entity, Owner(player))?;
    }
    if let Some(mover) = desc.mover {
        world.insert_one(entity, mover)?;
    }
    Ok(entity)
}

/// The handle of the mesh at source `text`, uploading it on first use.
fn mesh_handle(
    res: &mut Resources,
    device: &wgpu::Device,
    text: &str,
) -> anyhow::Result<crate::render::MeshHandle> {
    let source = AssetSource::parse(text)?;
    // Resources lend one value mutably at a time, so `Assets` is moved out
    // for the duration and always put back.
    let slot = res.get_mut::<Assets>().context("no Assets resource")?;
    let mut assets = std::mem::replace(slot, Assets::new(PathBuf::new()));
    let handle = assets.mesh(
        device,
        &mut res.expect_mut::<RenderState>().registry,
        &source,
    );
    res.insert(assets);
    handle
}

/// Despawns every entity `player` owns, with its descendants.
fn despawn_owned(world: &mut World, player: PlayerId) -> anyhow::Result<()> {
    let owned: Vec<Entity> = world
        .query::<(Entity, &Owner)>()
        .iter()
        .filter(|(_, owner)| owner.0 == player)
        .map(|(entity, _)| entity)
        .collect();
    for entity in owned {
        // A child of another owned entity may already be gone.
        if world.contains(entity) {
            despawn_recursive(world, entity)?;
        }
    }
    Ok(())
}
