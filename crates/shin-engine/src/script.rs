//! Rhai scripting for gameplay (GE-011): change behaviour without
//! recompiling.
//!
//! An entity with a [`Script`] component and a [`Transform`] runs the
//! script's `update` function once per fixed step:
//!
//! ```rhai
//! // scripts/bob.rhai
//! fn update(entity, dt) {
//!     entity.transform.translation.y = sin(time());
//! }
//! ```
//!
//! `entity` has `id`, `name`, `transform` (with `translation`, `rotation`,
//! and `scale`, local to the entity's parent), and `world_transform` (the
//! same fields in world space, read-only); `dt` is the fixed step in
//! seconds; `time()` is the simulated seconds since play started. Changes
//! to `entity.transform` are written back to the entity after the call.
//! Scripts cannot spawn entities or call each other.
//!
//! Only `update` (and the functions it calls) runs; statements outside
//! functions do not. Rhai functions cannot see outer variables, so keep
//! settings as `let`s inside `update` or as helper functions
//! (`fn speed() { 0.8 }`).
//!
//! # Script API
//!
//! On top of Rhai's standard library (`sin`, `cos`, `atan`, `PI()`, ...):
//!
//! - `Vec3`: `vec3(x, y, z)`, fields `x`, `y`, `z`, `+`, `-`, `* number`,
//!   `length()`.
//! - `Quat`: `quat_euler(x, y, z)` (radians, intrinsic X then Y then Z, as
//!   the `entity.*` tools use), fields `x`, `y`, `z`, `w` (read-only),
//!   `quat * quat`, `quat * vec3`.
//! - `Transform`: fields `translation`, `rotation`, `scale`.
//! - `Entity`: fields `id` and `name` (read-only), `transform` (local to
//!   the parent, written back), and `world_transform` (read-only).
//! - `time()`: simulated seconds since play started.
//! - `action_pressed(name)`, `action_just_pressed(name)`,
//!   `action_axis(name)`: the input actions bound in `shin.toml` (see
//!   [`crate::input::Actions`]); an action the project does not define is
//!   a runtime error.
//! - `play_sound(path)`: plays an `.ogg` or `.wav` file (an audio asset
//!   source relative to the project) once, everywhere; `play_sound_at(entity,
//!   path)` plays it at the entity, so it fades with the distance from the
//!   camera and follows the entity (GE-036). Sounds queued during a step
//!   start after every script has run.
//!
//! Numbers are `f64` in scripts and `f32` in the engine; vector setters
//! also accept integers, so `v.y = 1` works.
//!
//! [`script_system`] runs the scripts. It runs only while the
//! [`PlayState`] resource is playing and not paused; a world with no
//! `PlayState` at all (a game running outside the editor, such as
//! [`crate::app::EcsGame`]) always counts as playing. The compiled scripts,
//! their errors, and the Rhai engine live in the [`ScriptEngine`]
//! resource, which the system inserts on first use.
//!
//! Script paths are relative to the [`Assets`] root (the project directory
//! once a project is open, otherwise the working directory). Hot reload:
//! twice a second (wall clock) the system checks each loaded script's file
//! modification time and recompiles the ones that changed. A script that
//! fails to compile, or fails while running, does not stop the engine: the
//! error is logged as `path:line: message` and kept for `script.errors`
//! and the editor's inspector. A script that no longer compiles keeps
//! running its last good version.
//!
//! On a multiplayer client, scripts on entities replicated from the
//! server ([`RemoteEntity`]) do not run: the server runs them.
//!
//! Tools: [`register_tools`] adds `script.attach`, `script.eval`, and
//! `script.errors`.

mod bindings;
mod input;
mod tools;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use rhai::{AST, CallFnOptions, Dynamic, Engine, Scope};

pub use tools::register_tools;

use crate::asset::{AssetSource, Assets};
use crate::audio::{self, PlayParams};
use crate::ecs::{Entity, Name, Resources, Time, World};
use crate::headless::PlayState;
use crate::input::Actions;
use crate::math::Transform;
use crate::net::RemoteEntity;
use bindings::{ScriptEntity, ScriptTransform, SoundQueue, SoundRequest};
use input::ActionSnapshot;

/// The function every script defines; [`script_system`] calls it as
/// `update(entity, dt)`.
pub const UPDATE_FN: &str = "update";

/// How often [`script_system`] checks script files for changes.
/// Half a second, so an edit takes effect within one second.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// The most Rhai operations one script call may run before it is stopped
/// with an error, so a script stuck in a loop cannot freeze the engine.
const MAX_OPERATIONS: u64 = 1_000_000;

/// Component: the script that drives this entity, a `.rhai` file relative
/// to the [`Assets`] root (or absolute). Saved in scene files as
/// `script: "scripts/orbit.rhai"`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Script {
    /// Path of the `.rhai` file.
    pub path: PathBuf,
}

impl Script {
    /// A script component for the file at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

/// A script that failed to compile or to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptError {
    /// The script's path as written in its [`Script`] component.
    pub path: PathBuf,
    /// The 1-based line of the error, when Rhai knows it.
    pub line: Option<usize>,
    /// What went wrong.
    pub message: String,
}

impl fmt::Display for ScriptError {
    /// `path:line: message`, or `path: message` without a line.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "{}:{line}: {}", self.path.display(), self.message),
            None => write!(f, "{}: {}", self.path.display(), self.message),
        }
    }
}

impl std::error::Error for ScriptError {}

/// One script file as last loaded.
struct Loaded {
    /// The resolved file path.
    file: PathBuf,
    /// The last version that compiled, if any.
    ast: Option<AST>,
    /// The file's modification time when it was last read.
    modified: Option<SystemTime>,
}

/// Resource: the Rhai engine, compiled scripts, and their errors.
pub struct ScriptEngine {
    engine: Engine,
    /// What `time()` returns; set before each batch of calls.
    clock: Rc<Cell<f64>>,
    /// Sounds scripts asked for since [`Self::take_sound_requests`].
    sounds: SoundQueue,
    /// What the `action_*` functions read; set before each batch of calls.
    actions: ActionSnapshot,
    /// Loaded scripts by path as written in [`Script`].
    scripts: HashMap<PathBuf, Loaded>,
    /// The latest error of each script, by path as written.
    errors: BTreeMap<PathBuf, ScriptError>,
    /// How often [`Self::reload_changed`] actually looks at the files.
    poll_interval: Duration,
    last_poll: Option<Instant>,
}

impl Default for ScriptEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl ScriptEngine {
    /// An engine with the [script API](self#script-api) registered, no scripts
    /// loaded, and [`DEFAULT_POLL_INTERVAL`]. `print` and `debug` in
    /// scripts go to the log.
    pub fn new() -> Self {
        let clock = Rc::new(Cell::new(0.0));
        let sounds: SoundQueue = Rc::new(RefCell::new(Vec::new()));
        let mut engine = Engine::new();
        engine.set_max_operations(MAX_OPERATIONS);
        engine.on_print(|text| log::info!("script: {text}"));
        engine.on_debug(|text, source, pos| {
            log::debug!("script {}{pos:?}: {text}", source.unwrap_or(""));
        });
        bindings::register(&mut engine, Rc::clone(&clock), Rc::clone(&sounds));
        let actions = ActionSnapshot::default();
        input::register(&mut engine, Rc::clone(&actions));
        Self {
            engine,
            clock,
            sounds,
            actions,
            scripts: HashMap::new(),
            errors: BTreeMap::new(),
            poll_interval: DEFAULT_POLL_INTERVAL,
            last_poll: None,
        }
    }

    /// Sets how often [`Self::reload_changed`] checks files; zero checks on
    /// every call.
    pub fn set_poll_interval(&mut self, interval: Duration) {
        self.poll_interval = interval;
    }

    /// Makes the `action_*` functions read the current values of
    /// `actions` (none if `None`) until the next call. [`script_system`]
    /// does this before every batch of `update` calls; `script.eval` does
    /// it before evaluating.
    pub fn set_actions(&mut self, actions: Option<&Actions>) {
        *self.actions.borrow_mut() = input::snapshot(actions);
    }

    /// Reads and compiles the script at `path` (resolved against `base`
    /// unless absolute), replacing any earlier version.
    ///
    /// On failure the error is logged, recorded for [`Self::errors`], and
    /// returned; the last version that compiled (if any) stays loaded. On
    /// success the script's recorded error is cleared.
    pub fn load(&mut self, path: &Path, base: &Path) -> Result<(), ScriptError> {
        let file = if path.is_absolute() {
            path.to_path_buf()
        } else {
            base.join(path)
        };
        let modified = modified(&file);
        let compiled = std::fs::read_to_string(&file)
            .map_err(|err| ScriptError {
                path: path.to_path_buf(),
                line: None,
                message: format!("cannot read {}: {err}", file.display()),
            })
            .and_then(|source| self.compile(path, &source));
        let previous = self.scripts.remove(path).and_then(|loaded| loaded.ast);
        let (ast, result) = match compiled {
            Ok(ast) => {
                self.errors.remove(path);
                (Some(ast), Ok(()))
            }
            Err(err) => {
                self.record(err.clone());
                (previous, Err(err))
            }
        };
        self.scripts.insert(
            path.to_path_buf(),
            Loaded {
                file,
                ast,
                modified,
            },
        );
        result
    }

    /// Compiles `source` and checks it defines `fn update(entity, dt)`.
    fn compile(&self, path: &Path, source: &str) -> Result<AST, ScriptError> {
        let mut ast = self.engine.compile(source).map_err(|err| ScriptError {
            path: path.to_path_buf(),
            line: err.position().line(),
            message: err.err_type().to_string(),
        })?;
        let has_update = ast
            .iter_functions()
            .any(|f| f.name == UPDATE_FN && f.params.len() == 2);
        if !has_update {
            return Err(ScriptError {
                path: path.to_path_buf(),
                line: None,
                message: format!("the script does not define `fn {UPDATE_FN}(entity, dt)`"),
            });
        }
        ast.set_source(path.to_string_lossy().into_owned());
        Ok(ast)
    }

    /// Reloads every loaded script whose file changed since it was read,
    /// at most once per poll interval (see [`Self::set_poll_interval`]).
    /// Returns the paths reloaded.
    pub fn reload_changed(&mut self, base: &Path) -> Vec<PathBuf> {
        let now = Instant::now();
        if self
            .last_poll
            .is_some_and(|last| now.duration_since(last) < self.poll_interval)
        {
            return Vec::new();
        }
        self.last_poll = Some(now);
        let changed: Vec<PathBuf> = self
            .scripts
            .iter()
            .filter(|(_, loaded)| modified(&loaded.file) != loaded.modified)
            .map(|(path, _)| path.clone())
            .collect();
        for path in &changed {
            if self.load(path, base).is_ok() {
                log::info!("reloaded script {}", path.display());
            }
        }
        changed
    }

    /// Evaluates `source` as a standalone script (the same API as entity
    /// scripts, with `time()` at its last value) and returns its result.
    pub fn eval(&self, source: &str) -> Result<Dynamic, ScriptError> {
        self.engine
            .eval::<Dynamic>(source)
            .map_err(|err| runtime_error(Path::new("<eval>"), &err))
    }

    /// The latest error of every script that has one, by path.
    pub fn errors(&self) -> impl Iterator<Item = &ScriptError> {
        self.errors.values()
    }

    /// The sounds scripts asked for (`play_sound`, `play_sound_at`) since
    /// the last call, oldest first, leaving the queue empty.
    fn take_sound_requests(&mut self) -> Vec<SoundRequest> {
        std::mem::take(&mut *self.sounds.borrow_mut())
    }

    /// The latest error of the script at `path` (as written in its
    /// [`Script`]), if it has one.
    pub fn error(&self, path: &Path) -> Option<&ScriptError> {
        self.errors.get(path)
    }

    /// Calls `update(entity, dt)` in the script at `path`, loading it first
    /// if needed, with `transform` as `entity.transform`. Returns the
    /// transform the script left, or `None` if the script could not run
    /// (the error is recorded).
    fn run(
        &mut self,
        path: &Path,
        base: &Path,
        entity: ScriptEntity,
        dt: f64,
    ) -> Option<Transform> {
        if !self.scripts.contains_key(path) {
            // The error, if any, is logged and recorded by `load`.
            let _ = self.load(path, base);
        }
        let ast = self.scripts.get(path)?.ast.as_ref()?;
        let transform = Rc::clone(&entity.transform.0);
        let options = CallFnOptions::new().eval_ast(false).rewind_scope(true);
        let result = self.engine.call_fn_with_options::<Dynamic>(
            options,
            &mut Scope::new(),
            ast,
            UPDATE_FN,
            (entity, dt as rhai::FLOAT),
        );
        match result {
            Ok(_) => Some(*transform.borrow()),
            Err(err) => {
                self.record(runtime_error(path, &err));
                None
            }
        }
    }

    /// Stores `err` as its script's latest error, logging it unless it is
    /// the same error as last time (a failing script fails every step).
    fn record(&mut self, err: ScriptError) {
        if self.errors.get(&err.path) != Some(&err) {
            log::error!("{err}");
            self.errors.insert(err.path.clone(), err);
        }
    }
}

/// A Rhai runtime error as a [`ScriptError`], with the line where it
/// happened inside the script (not where Rust called into it).
fn runtime_error(path: &Path, err: &rhai::EvalAltResult) -> ScriptError {
    let inner = err.unwrap_inner();
    ScriptError {
        path: path.to_path_buf(),
        line: inner.position().line().or_else(|| err.position().line()),
        message: inner.to_string(),
    }
}

/// The modification time of `file`, if it can be read.
fn modified(file: &Path) -> Option<SystemTime> {
    std::fs::metadata(file).and_then(|m| m.modified()).ok()
}

/// Fixed system: runs `update(entity, dt)` for every entity with a
/// [`Script`] and a [`Transform`], in entity id order, writes each
/// script's transform back, then plays the sounds the scripts asked for
/// (`play_sound`, `play_sound_at`); a sound that cannot be played is
/// logged.
///
/// Does nothing while editing or paused (see the [module docs](self)).
/// Inserts a [`ScriptEngine`] if there is none, and first reloads scripts
/// whose files changed ([`ScriptEngine::reload_changed`]).
pub fn script_system(world: &mut World, res: &mut Resources) {
    let time = res.get::<Time>().copied().unwrap_or_default();
    let t = match res.get::<PlayState>() {
        None => time.elapsed,
        Some(PlayState::Playing {
            snapshot,
            paused: false,
        }) => time.elapsed - snapshot.time().elapsed,
        Some(_) => return,
    };
    let mut scripted: Vec<(Entity, PathBuf, Transform, String)> = world
        .query::<(Entity, &Script, &Transform, Option<&Name>)>()
        .without::<&RemoteEntity>()
        .iter()
        .map(|(entity, script, transform, name)| {
            let name = name.map(|n| n.0.clone()).unwrap_or_default();
            (entity, script.path.clone(), *transform, name)
        })
        .collect();
    if scripted.is_empty() {
        return;
    }
    scripted.sort_by_key(|(entity, ..)| entity.to_bits());
    // World transforms as of the start of the step, before any script runs.
    let worlds: Vec<Transform> = scripted
        .iter()
        .map(|(entity, ..)| crate::ecs::world_transform(world, *entity))
        .collect();

    let base = script_base(res);
    let actions = input::snapshot(res.get::<Actions>());
    if res.get::<ScriptEngine>().is_none() {
        res.insert(ScriptEngine::new());
    }
    let scripts = res.expect_mut::<ScriptEngine>();
    scripts.reload_changed(&base);
    scripts.clock.set(t);
    *scripts.actions.borrow_mut() = actions;
    for ((entity, path, transform, name), world_space) in scripted.into_iter().zip(worlds) {
        let handle = ScriptEntity {
            id: entity.to_bits().get() as rhai::INT,
            name,
            transform: ScriptTransform(Rc::new(RefCell::new(transform))),
            world: world_space,
        };
        if let Some(updated) = scripts.run(&path, &base, handle, time.fixed_dt)
            && let Ok(mut transform) = world.get::<&mut Transform>(entity)
        {
            *transform = updated;
        }
    }
    let requests = scripts.take_sound_requests();
    for request in requests {
        if let Err(err) = play_requested(res, &request) {
            log::warn!("script: play_sound({:?}) failed: {err:#}", request.path);
        }
    }
}

/// Plays one [`SoundRequest`]: everywhere, or at (and following) the
/// entity it names.
fn play_requested(res: &mut Resources, request: &SoundRequest) -> anyhow::Result<()> {
    let source = AssetSource::parse(&request.path)?;
    let (entity, position) = match request.at {
        Some((id, position)) => (Entity::from_bits(id as u64), Some(position)),
        None => (None, None),
    };
    audio::play(
        res,
        &source,
        PlayParams {
            volume: 1.0,
            looping: false,
            position,
            entity,
        },
    )?;
    Ok(())
}

/// The directory script paths are relative to: the [`Assets`] root, or
/// the working directory without one.
pub(crate) fn script_base(res: &Resources) -> PathBuf {
    match res.get::<Assets>() {
        Some(assets) => assets.root().to_path_buf(),
        None => std::env::current_dir().unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests;
