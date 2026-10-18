//! Game projects: the `shin.toml` manifest and scaffolding a new, runnable
//! project (`shin new`, the `project.new` tool). Owned by GE-018.
//!
//! A project is a normal Cargo package that depends on `shin-engine`, plus a
//! `shin.toml` the engine and tools read:
//!
//! ```toml
//! [project]
//! name = "demo-game"
//!
//! [window]
//! title = "demo-game"
//! width = 1280
//! height = 720
//! ```
//!
//! [`create`] writes the starter project: `Cargo.toml`, `shin.toml` (with
//! the `jump`, `move_x`, and `move_z` input actions; GE-033),
//! `src/main.rs` (the game: it plays `scenes/main.scene.ron` in a window
//! with the fly camera and an orbit system of its own, or with `--mcp`
//! serves the same world, systems, and a `game.hello` tool over MCP; GE-027),
//! `scenes/main.scene.ron` (a lit cube on a ground slab with a small moon
//! orbiting it, the project's entry scene), `scripts/player.rhai` (moves
//! the cube with those actions), `assets/cube.gltf` + `assets/cube.bin`,
//! `assets/sounds/blip.wav` + `scripts/moon.rhai` (the moon blips once per
//! revolution; GE-036), `.mcp.json` (runs the game with `--mcp`),
//! `.gitignore`, `README.md`, and for agents (GE-039) a `CLAUDE.md` and
//! every [`skills::SKILLS`] entry as `.claude/skills/<name>/SKILL.md`.
//!
//! An open project (GE-026, [`Headless::open_project`]) is marked by the
//! [`ProjectRoot`] resource; the `project.open` and `project.info` tools
//! open and describe one.

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::headless::Headless;
use crate::input::InputConfig;
use crate::scene::{AudioDesc, CameraDesc, EntityDesc, LightDesc, SceneFile, TransformDesc};
use crate::tool::{ToolOutput, ToolRegistry, TypedTool};

pub mod skills;

/// Name of the project manifest in a project's root directory.
pub const MANIFEST: &str = "shin.toml";

/// Git URL of the engine, used by [`EngineDep::Git`] by default.
pub const ENGINE_GIT_URL: &str = "https://github.com/nikolagigic/shin-engine";

/// A game project, as described by its `shin.toml`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    /// The project (and Cargo package) name.
    pub name: String,
    /// The game window.
    pub window: WindowSettings,
    /// The scene loaded at start-up, relative to the project root, e.g.
    /// `scenes/main.scene.ron`. [`Headless::open_project`] loads it.
    pub entry_scene: Option<PathBuf>,
    /// The `[input]` table: named actions and their bindings. Empty when
    /// the manifest has none.
    pub input: InputConfig,
}

/// The game window's initial title and size, the `[window]` table of
/// `shin.toml`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowSettings {
    /// Window title.
    pub title: String,
    /// Initial width in logical pixels.
    pub width: u32,
    /// Initial height in logical pixels.
    pub height: u32,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            title: "shin".to_string(),
            width: 1280,
            height: 720,
        }
    }
}

/// The on-disk layout of `shin.toml`: `[project]`, `[window]`, and an
/// optional `[input]` table.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestFile {
    project: ProjectTable,
    window: WindowSettings,
    #[serde(default, skip_serializing_if = "InputConfig::is_empty")]
    input: InputConfig,
}

/// The `[project]` table of `shin.toml`.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectTable {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    entry_scene: Option<PathBuf>,
}

impl Project {
    /// A project called `name` with a window titled `name` and the default
    /// size, no entry scene, and no input actions.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            window: WindowSettings {
                title: name.to_string(),
                ..Default::default()
            },
            entry_scene: None,
            input: InputConfig::default(),
        }
    }

    /// Reads `<dir>/shin.toml`.
    pub fn load(dir: &Path) -> anyhow::Result<Self> {
        let path = dir.join(MANIFEST);
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let file: ManifestFile =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        Ok(Self {
            name: file.project.name,
            window: file.window,
            entry_scene: file.project.entry_scene,
            input: file.input,
        })
    }

    /// Writes `<dir>/shin.toml`, replacing any existing one.
    pub fn save(&self, dir: &Path) -> anyhow::Result<()> {
        let path = dir.join(MANIFEST);
        fs::write(&path, self.to_toml()?).with_context(|| format!("writing {}", path.display()))
    }

    /// The `shin.toml` text for this project.
    fn to_toml(&self) -> anyhow::Result<String> {
        let file = ManifestFile {
            project: ProjectTable {
                name: self.name.clone(),
                entry_scene: self.entry_scene.clone(),
            },
            window: self.window.clone(),
            input: self.input.clone(),
        };
        Ok(toml::to_string(&file)?)
    }
}

/// The root directory of the open project, as an absolute, canonical path.
///
/// [`Headless::open_project`] inserts it as a resource. While it is set,
/// the `scene.*` tools resolve their paths with [`ProjectRoot::resolve`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectRoot(pub PathBuf);

impl ProjectRoot {
    /// `path` joined onto the root and normalised (`.` and `..` removed
    /// without touching the file system, so the file need not exist).
    ///
    /// Fails, naming `path`, if the result is outside the root: a relative
    /// path that climbs out with `..`, or an absolute path elsewhere.
    pub fn resolve(&self, path: &Path) -> anyhow::Result<PathBuf> {
        let mut resolved = PathBuf::new();
        for component in self.0.join(path).components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    resolved.pop();
                }
                other => resolved.push(other),
            }
        }
        anyhow::ensure!(
            resolved.starts_with(&self.0),
            "path `{}` escapes the project root {}",
            path.display(),
            self.0.display()
        );
        Ok(resolved)
    }
}

/// Where a new project gets `shin-engine` from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineDep {
    /// A git repository, e.g. [`ENGINE_GIT_URL`].
    Git {
        /// Repository URL.
        url: String,
    },
    /// A local checkout: the `crates/shin-engine` directory. Relative paths
    /// are made absolute against the current directory.
    Path(PathBuf),
}

impl Default for EngineDep {
    fn default() -> Self {
        Self::Git {
            url: ENGINE_GIT_URL.to_string(),
        }
    }
}

/// What [`create`] should make.
#[derive(Clone, Debug)]
pub struct NewProjectOptions {
    /// Project and Cargo package name; see [`validate_name`].
    pub name: String,
    /// The project's root directory. It must not exist or be empty.
    pub dir: PathBuf,
    /// Where the project gets the engine from.
    pub engine: EngineDep,
}

/// Checks that `name` is usable as a Cargo package name: non-empty, ASCII
/// letters, digits, `-` and `_` only, not starting with a digit.
pub fn validate_name(name: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!name.is_empty(), "project name is empty");
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_'))
    {
        anyhow::bail!(
            "invalid project name `{name}`: {bad:?} is not allowed; \
             use ASCII letters, digits, `-` and `_`"
        );
    }
    anyhow::ensure!(
        !name.starts_with(|c: char| c.is_ascii_digit()),
        "invalid project name `{name}`: it must not start with a digit"
    );
    Ok(())
}

/// The starter `src/main.rs`. It reads its window settings from `shin.toml`
/// at run time, so it needs no substitution.
const MAIN_RS: &str = include_str!("project/template/main.rs");
/// The project's `CLAUDE.md`, notes for agents; `{name}` is replaced by the
/// project name.
const CLAUDE_MD: &str = include_str!("project/template/CLAUDE.md");
/// The starter's player script, attached to the cube: moves it with the
/// `move_x`, `move_z`, and `jump` actions of [`starter_input`].
const PLAYER_RHAI: &str = include_str!("project/template/player.rhai");
/// Where [`create`] writes the starter's player script, relative to the
/// project root; the starter scene's cube refers to it.
pub const STARTER_SCRIPT: &str = "scripts/player.rhai";
/// The glTF cube the starter scene draws, copied into `assets/`.
const CUBE_GLTF: &[u8] = include_bytes!("../assets/test/cube.gltf");
/// The cube's vertex buffer, referenced by [`CUBE_GLTF`] as `cube.bin`.
const CUBE_BIN: &[u8] = include_bytes!("../assets/test/cube.bin");
/// The starter sound (GE-036): a 0.2 s blip, copied to
/// `assets/sounds/blip.wav`. Generated by [`crate::audio::blip_wav`].
const BLIP_WAV: &[u8] = include_bytes!("../assets/sounds/blip.wav");
/// The moon's script (GE-036): plays the blip once per revolution through
/// the script API, copied to `scripts/moon.rhai`.
const MOON_RHAI: &str = include_str!("project/template/moon.rhai");

/// Where [`create`] writes the starter sound, relative to the project root.
pub const STARTER_SOUND: &str = "assets/sounds/blip.wav";
/// Where [`create`] writes the moon's script, relative to the project root.
pub const MOON_SCRIPT: &str = "scripts/moon.rhai";

/// Creates a new project in `opts.dir` and returns the files written, in
/// order, as paths under `opts.dir`.
///
/// Fails without writing anything if the name is invalid or `opts.dir`
/// exists and is not an empty directory.
pub fn create(opts: &NewProjectOptions) -> anyhow::Result<Vec<PathBuf>> {
    validate_name(&opts.name)?;
    let dir = &opts.dir;
    if dir.exists() {
        let mut entries = fs::read_dir(dir)
            .with_context(|| format!("{} exists and is not a directory", dir.display()))?;
        anyhow::ensure!(
            entries.next().is_none(),
            "{} already exists and is not empty",
            dir.display()
        );
    }

    let mut project = Project::new(&opts.name);
    project.entry_scene = Some(PathBuf::from(STARTER_SCENE));
    project.input = starter_input();
    let mut files: Vec<(String, Vec<u8>)> = [
        ("Cargo.toml", cargo_toml(&opts.name, &opts.engine)?.into()),
        (MANIFEST, project.to_toml()?.into()),
        ("src/main.rs", MAIN_RS.into()),
        (STARTER_SCENE, starter_scene().to_ron()?.into()),
        (STARTER_SCRIPT, PLAYER_RHAI.into()),
        ("assets/cube.gltf", CUBE_GLTF.into()),
        ("assets/cube.bin", CUBE_BIN.into()),
        (STARTER_SOUND, BLIP_WAV.into()),
        (MOON_SCRIPT, MOON_RHAI.into()),
        (".mcp.json", MCP_JSON.into()),
        (".gitignore", "/target\n".into()),
        ("README.md", readme(&opts.name).into()),
        ("CLAUDE.md", CLAUDE_MD.replace("{name}", &opts.name).into()),
    ]
    .into_iter()
    .map(|(path, contents)| (path.to_string(), contents))
    .collect();
    files.extend(
        skills::SKILLS
            .iter()
            .map(|skill| (skill_path(skill.name), skill.text.as_bytes().to_vec())),
    );

    let mut written = Vec::with_capacity(files.len());
    for (relative, contents) in files {
        let path = dir.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        fs::write(&path, contents).with_context(|| format!("writing {}", path.display()))?;
        written.push(path);
    }
    log::info!("created project `{}` in {}", opts.name, dir.display());
    Ok(written)
}

/// Where [`create`] writes the skill called `name`, relative to the project
/// root: `.claude/skills/<name>/SKILL.md`, where Claude Code finds it.
pub fn skill_path(name: &str) -> String {
    format!(".claude/skills/{name}/SKILL.md")
}

/// Where [`create`] writes the starter scene, relative to the project root;
/// also the generated project's `entry_scene`.
pub const STARTER_SCENE: &str = "scenes/main.scene.ron";

/// The generated `.mcp.json`: an MCP client such as Claude Code, opened in
/// the project folder, runs the game with `--mcp` and gets the engine's
/// tools and the game's own as tools, on the game's own systems.
const MCP_JSON: &str = r#"{
  "mcpServers": {
    "shin": {
      "command": "cargo",
      "args": ["run", "--quiet", "--", "--mcp"]
    }
  }
}
"#;

/// The starter project's input actions: `jump` on Space or the gamepad's
/// south button, `move_x` on the left/right arrows or the left stick, and
/// `move_z` on the down/up arrows or the left stick (up and `ArrowUp` are
/// `+1`). The arrows rather than WASD, which the fly camera uses.
fn starter_input() -> InputConfig {
    toml::from_str(
        r#"
        [actions]
        jump = ["Space", "Gamepad:South"]
        move_x = { axis = ["ArrowLeft", "ArrowRight"], gamepad_axis = "LeftStickX" }
        move_z = { axis = ["ArrowDown", "ArrowUp"], gamepad_axis = "LeftStickY" }
        "#,
    )
    .expect("the starter input config is valid")
}

/// The starter scene, drawn with the project's `assets/cube.gltf`: a
/// ground slab, a cube on it driven by [`STARTER_SCRIPT`], and a small
/// `moon` beside the cube. The template's orbit system moves the moon; it
/// starts where that system puts it at time zero, `(1.6, 0.5, 0)`. The
/// moon carries the starter sound as a spatial [`AudioDesc`] and the
/// script [`MOON_SCRIPT`], which plays it once per revolution (GE-036).
fn starter_scene() -> SceneFile {
    let cube = |name: &str, transform: TransformDesc, color: [f32; 4]| EntityDesc {
        name: Some(name.to_string()),
        transform,
        mesh: Some("assets/cube.gltf".to_string()),
        color: Some(color),
        script: (name == "cube").then(|| STARTER_SCRIPT.to_string()),
        ..Default::default()
    };
    SceneFile {
        camera: Some(CameraDesc {
            position: [0.0, 2.0, 5.0],
            look_at: [0.0, 0.5, 0.0],
            fov_y_deg: 60.0,
        }),
        light: Some(LightDesc {
            direction: [-0.6, -1.0, -0.4],
            color: [1.0, 0.95, 0.85],
            ambient: [0.12, 0.13, 0.16],
        }),
        entities: vec![
            // Ground: the cube flattened into a slab whose top face is y = 0.
            cube(
                "ground",
                TransformDesc {
                    translation: [0.0, -0.25, 0.0],
                    scale: [20.0, 0.5, 20.0],
                    ..Default::default()
                },
                [0.18, 0.2, 0.17, 1.0],
            ),
            // The cube, turned a little so the light catches three faces;
            // the player script moves it.
            cube(
                "cube",
                TransformDesc {
                    translation: [0.0, 0.5, 0.0],
                    rotation_euler_deg: [0.0, 30.0, 0.0],
                    ..Default::default()
                },
                [0.8, 0.25, 0.1, 1.0],
            ),
            // Orbits the cube, driven by `orbit_system` in `src/main.rs`;
            // its script blips once per revolution.
            EntityDesc {
                script: Some(MOON_SCRIPT.to_string()),
                audio: Some(AudioDesc {
                    source: STARTER_SOUND.to_string(),
                    spatial: true,
                    ..Default::default()
                }),
                ..cube(
                    "moon",
                    TransformDesc {
                        translation: [1.6, 0.5, 0.0],
                        scale: [0.3, 0.3, 0.3],
                        ..Default::default()
                    },
                    [0.75, 0.78, 0.85, 1.0],
                )
            },
        ],
        ..Default::default()
    }
}

/// The project's `Cargo.toml`. It depends on the engine and on `shin-mcp`
/// (for `--mcp`), both from the same place: the git repository, or for
/// [`EngineDep::Path`] the engine directory and its sibling `shin-mcp`
/// (`crates/shin-engine` and `crates/shin-mcp` of a checkout). It also
/// depends on `wgpu` (for `wgpu::Color`), `anyhow`, `env_logger`, `log`,
/// and `serde` + `schemars` (for tool arguments), at the versions the
/// engine itself pins.
fn cargo_toml(name: &str, engine: &EngineDep) -> anyhow::Result<String> {
    let (engine, mcp) = match engine {
        EngineDep::Git { url } => {
            let dep = format!("{{ git = {} }}", toml_string(url));
            (dep.clone(), dep)
        }
        EngineDep::Path(path) => {
            let engine = std::path::absolute(path)
                .with_context(|| format!("resolving engine path {}", path.display()))?;
            let mcp = engine
                .parent()
                .with_context(|| format!("engine path {} has no parent", engine.display()))?
                .join("shin-mcp");
            (path_dep(&engine)?, path_dep(&mcp)?)
        }
    };
    Ok(format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"
publish = false

[dependencies]
shin-engine = {engine}
shin-mcp = {mcp}
wgpu = "30.0"
anyhow = "1.0"
env_logger = "0.11"
log = "0.4"
serde = {{ version = "1.0", features = ["derive"] }}
schemars = "1.2"

# Optimise dependencies even in debug builds so the game runs smoothly.
[profile.dev]
opt-level = 1

[profile.dev.package."*"]
opt-level = 3

# A standalone project, even when created inside another Cargo workspace.
[workspace]
"#
    ))
}

/// `{ path = "<path>" }` for an absolute `path`.
fn path_dep(path: &Path) -> anyhow::Result<String> {
    let text = path
        .to_str()
        .with_context(|| format!("engine path {} is not UTF-8", path.display()))?;
    Ok(format!("{{ path = {} }}", toml_string(text)))
}

/// `s` as a quoted, escaped TOML string.
fn toml_string(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// The project's `README.md`.
fn readme(name: &str) -> String {
    format!(
        r#"# {name}

A game made with [Shin]({ENGINE_GIT_URL}).

## Run

```sh
cargo run
```

A lit cube on a ground slab with a small moon orbiting it. The window
title and size come from `shin.toml`, the scene from
`scenes/main.scene.ron`, and the orbit from `orbit_system` in
`src/main.rs`. The cube is yours: `scripts/player.rhai` slides it with
the arrow keys or the left stick and lifts it while Space or the
gamepad's south button (A / Cross) is held. Those are the `move_x`,
`move_z`, and `jump` actions bound in `shin.toml`; rebind them there.

## Drive it with an agent

```sh
cargo run -- --mcp
```

With `--mcp` the game opens no window. It serves itself over MCP on
stdin/stdout instead (logs go to stderr), with the same world and systems
as the windowed game and this project open. An agent can step the
simulation (`sim.step`) and watch the moon move, inspect and edit the
scene, render frames, save scenes, and undo its edits, all running your
game's own code.

Open this folder in Claude Code and that happens for you: `.mcp.json` runs
`cargo run --quiet -- --mcp`. The first start compiles the game, which can
take a minute.

### Add a tool

Agents get the engine's tools plus the game's own. `register_game_tools`
in `src/main.rs` registers one example, `game.hello`, which returns the
game's name. To add yours, register another `TypedTool` there:

```rust
/// Arguments of `game.set_speed`.
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SpeedArgs {{
    /// Simulation speed: 1.0 is normal, 2.0 is twice as fast.
    factor: f64,
}}

registry.register(TypedTool::new(
    "game.set_speed",
    "Sets how much simulated time each step covers, relative to normal.",
    |engine: &mut Headless, args: SpeedArgs| {{
        engine.fixed_dt = args.factor / 60.0;
        Ok(ToolOutput::Text(format!("speed {{}}", args.factor)))
    }},
));
```

The doc comments on the arguments become the tool's JSON schema, which is
what the agent reads. Name tools `<subsystem>.<verb>`. Systems you add to
`game_schedule` in `src/main.rs` run under `--mcp` too.

## Controls

- Arrow keys or left stick: move the cube; Space or gamepad south: lift it
- W/S: camera forward and back, A/D: strafe, E/Q: up and down
- Hold the right mouse button to look around
- Escape: quit

## Layout

```
shin.toml       project name, window settings, entry scene, input actions
src/main.rs     the game: systems, tools, window and --mcp modes
scenes/         scene files (.scene.ron)
scripts/        Rhai scripts (player.rhai drives the cube; moon.rhai blips)
assets/         glTF meshes, textures, and sounds (.ogg, .wav)
.mcp.json       runs the game with --mcp for agents
CLAUDE.md       project notes for agents
.claude/skills/ agent skills: building scenes, debugging frames, scripting
```
"#
    )
}

/// Arguments of `project.new`.
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct NewArgs {
    /// Project and Cargo package name: ASCII letters, digits, `-` and `_`,
    /// not starting with a digit.
    name: String,
    /// Parent directory; the project is created in `<dir>/<name>`.
    dir: PathBuf,
    /// Path to a local `crates/shin-engine` to depend on instead of the
    /// engine's git repository.
    engine_path: Option<PathBuf>,
}

/// Arguments of `project.open`.
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct OpenArgs {
    /// The project's root directory, the one containing `shin.toml`.
    dir: PathBuf,
}

/// `project.info` takes no arguments.
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct InfoArgs {}

/// The `project.info` description of the open project: `{ name, dir,
/// entry_scene, window: { title, width, height }, input: { actions } }`.
/// Re-reads `shin.toml`, so edits made since opening show up.
fn info(engine: &Headless) -> anyhow::Result<serde_json::Value> {
    let root = engine
        .resources
        .get::<ProjectRoot>()
        .context("no project is open; call project.open first")?;
    let project = Project::load(&root.0)?;
    Ok(json!({
        "name": project.name,
        "dir": root.0,
        "entry_scene": project.entry_scene,
        "window": project.window,
        "input": project.input,
    }))
}

/// Registers `project.new`, `project.open`, and `project.info`.
pub fn register_tools(reg: &mut ToolRegistry) {
    reg.register(TypedTool::new(
        "project.open",
        "Opens the project in `dir` (the folder with shin.toml): replaces every entity, roots \
         asset and scene paths at the project, resizes the render target to the project's \
         window size, and loads its entry scene if it has one. Returns the same as \
         project.info.",
        |engine: &mut Headless, args: OpenArgs| {
            engine.load_project(&args.dir)?;
            Ok(ToolOutput::Json(info(engine)?))
        },
    ));
    reg.register(TypedTool::new(
        "project.info",
        "Describes the open project: { name, dir, entry_scene, window: { title, width, \
         height }, input: { actions } }, read from its shin.toml. Fails if no project is \
         open.",
        |engine: &mut Headless, _: InfoArgs| Ok(ToolOutput::Json(info(engine)?)),
    ));
    reg.register(TypedTool::new(
        "project.new",
        "Creates a runnable game project in `<dir>/<name>` (Cargo.toml, shin.toml, \
         src/main.rs with a lit cube scene, assets) and returns the list of files written. \
         Fails if that directory exists and is not empty.",
        |_: &mut Headless, args: NewArgs| {
            let opts = NewProjectOptions {
                dir: args.dir.join(&args.name),
                name: args.name,
                engine: args.engine_path.map(EngineDep::Path).unwrap_or_default(),
            };
            let files = create(&opts)?;
            Ok(ToolOutput::Json(serde_json::to_value(files)?))
        },
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset::Assets;

    /// A fresh, empty-or-missing directory under the system temp dir.
    fn temp_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("shin-project-test-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn valid_names_pass() {
        for name in ["demo", "demo-game", "my_game2", "A"] {
            validate_name(name).unwrap_or_else(|e| panic!("`{name}`: {e}"));
        }
    }

    #[test]
    fn invalid_names_are_rejected() {
        for name in ["", "my game", "2fast", "caf\u{e9}", "a/b"] {
            assert!(validate_name(name).is_err(), "`{name}` accepted");
        }
        let err = validate_name("9lives").unwrap_err().to_string();
        assert!(err.contains("start with a digit"), "{err}");
        let err = validate_name("my game").unwrap_err().to_string();
        assert!(err.contains("my game"), "{err}");
    }

    #[test]
    fn create_rejects_invalid_name_without_writing() {
        let dir = temp_dir("bad-name");
        let opts = NewProjectOptions {
            name: "my game".to_string(),
            dir: dir.clone(),
            engine: EngineDep::default(),
        };
        let err = create(&opts).unwrap_err().to_string();
        assert!(err.contains("invalid project name"), "{err}");
        assert!(!dir.exists());
    }

    #[test]
    fn create_rejects_non_empty_dir_naming_it() -> anyhow::Result<()> {
        let dir = temp_dir("non-empty");
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("keep.txt"), "mine")?;
        let opts = NewProjectOptions {
            name: "demo".to_string(),
            dir: dir.clone(),
            engine: EngineDep::default(),
        };
        let err = create(&opts).unwrap_err().to_string();
        assert!(err.contains(&dir.display().to_string()), "{err}");
        assert!(err.contains("not empty"), "{err}");
        assert_eq!(fs::read_dir(&dir)?.count(), 1, "nothing else was written");
        fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn create_writes_the_layout_into_an_empty_dir() -> anyhow::Result<()> {
        let dir = temp_dir("layout");
        fs::create_dir_all(&dir)?;
        let opts = NewProjectOptions {
            name: "demo-game".to_string(),
            dir: dir.clone(),
            engine: EngineDep::default(),
        };
        let files = create(&opts)?;
        let relative: Vec<_> = files
            .iter()
            .map(|f| f.strip_prefix(&dir).unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(
            relative,
            [
                "Cargo.toml",
                "shin.toml",
                "src/main.rs",
                "scenes/main.scene.ron",
                "scripts/player.rhai",
                "assets/cube.gltf",
                "assets/cube.bin",
                "assets/sounds/blip.wav",
                "scripts/moon.rhai",
                ".mcp.json",
                ".gitignore",
                "README.md",
                "CLAUDE.md",
                ".claude/skills/shin-scene-building/SKILL.md",
                ".claude/skills/shin-visual-debugging/SKILL.md",
                ".claude/skills/shin-scripting/SKILL.md",
                ".claude/skills/shin-project/SKILL.md",
            ]
        );
        let claude = fs::read_to_string(dir.join("CLAUDE.md"))?;
        assert!(claude.starts_with("# demo-game\n"), "{claude}");
        assert!(!claude.contains("{name}"), "{claude}");
        for skill in skills::SKILLS {
            assert_eq!(
                fs::read_to_string(dir.join(skill_path(skill.name)))?,
                skill.text
            );
            assert!(
                claude.contains(skill.name),
                "CLAUDE.md lists {}",
                skill.name
            );
        }
        assert_eq!(fs::read(dir.join(STARTER_SOUND))?, crate::audio::blip_wav());
        assert!(
            fs::read_to_string(dir.join(MOON_SCRIPT))?.contains("play_sound_at(entity"),
            "the moon script uses the script API"
        );
        let cargo = fs::read_to_string(dir.join("Cargo.toml"))?;
        assert!(cargo.contains(r#"name = "demo-game""#), "{cargo}");
        assert!(
            cargo.contains(&format!(r#"git = "{ENGINE_GIT_URL}""#)),
            "{cargo}"
        );
        let project = Project::load(&dir)?;
        assert_eq!(project.entry_scene, Some(PathBuf::from(STARTER_SCENE)));
        assert_eq!(project.input, starter_input());
        let manifest = fs::read_to_string(dir.join(MANIFEST))?;
        assert!(manifest.contains("[input.actions]"), "{manifest}");
        assert!(
            manifest.contains(r#"jump = ["Space", "Gamepad:South"]"#),
            "{manifest}"
        );
        let scene = SceneFile::from_ron(&fs::read_to_string(dir.join(STARTER_SCENE))?)?;
        assert_eq!(scene, starter_scene());
        let cube = scene
            .entities
            .iter()
            .find(|e| e.name.as_deref() == Some("cube"))
            .expect("the starter scene has a cube");
        assert_eq!(cube.script.as_deref(), Some(STARTER_SCRIPT));
        assert!(dir.join(STARTER_SCRIPT).is_file());
        fs::remove_dir_all(&dir)?;
        Ok(())
    }

    /// The starter's player script compiles against the script API and
    /// defines `update`.
    #[test]
    fn starter_script_compiles() {
        let mut scripts = crate::script::ScriptEngine::new();
        let dir = temp_dir("player-script");
        fs::create_dir_all(dir.join("scripts")).unwrap();
        fs::write(dir.join(STARTER_SCRIPT), PLAYER_RHAI).unwrap();
        scripts
            .load(Path::new(STARTER_SCRIPT), &dir)
            .expect("player.rhai compiles");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_rejects_a_bad_key_name_naming_it() -> anyhow::Result<()> {
        let dir = temp_dir("bad-key");
        fs::create_dir_all(&dir)?;
        fs::write(
            dir.join(MANIFEST),
            "[project]\nname = \"x\"\n[window]\ntitle = \"x\"\nwidth = 8\nheight = 8\n\
             [input.actions]\njump = [\"Spce\"]\n",
        )?;
        let err = format!("{:#}", Project::load(&dir).unwrap_err());
        assert!(err.contains("shin.toml"), "names the file: {err}");
        assert!(err.contains("unknown key `Spce`"), "names the key: {err}");
        fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn generated_mcp_json_runs_the_game_with_mcp() -> anyhow::Result<()> {
        let dir = temp_dir("mcp-json");
        create(&NewProjectOptions {
            name: "demo".to_string(),
            dir: dir.clone(),
            engine: EngineDep::default(),
        })?;
        let config: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join(".mcp.json"))?)?;
        assert_eq!(
            config,
            json!({ "mcpServers": { "shin": {
                "command": "cargo",
                "args": ["run", "--quiet", "--", "--mcp"],
            } } })
        );
        let readme = fs::read_to_string(dir.join("README.md"))?;
        assert!(readme.contains("Claude Code"), "{readme}");
        assert!(readme.contains("cargo run -- --mcp"), "{readme}");
        assert!(readme.contains("register_game_tools"), "{readme}");
        fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn project_root_resolves_inside_and_rejects_escapes() -> anyhow::Result<()> {
        let root = ProjectRoot(PathBuf::from("/games/demo"));
        assert_eq!(
            root.resolve(Path::new("scenes/main.scene.ron"))?,
            Path::new("/games/demo/scenes/main.scene.ron")
        );
        assert_eq!(
            root.resolve(Path::new("./a/../b.scene.ron"))?,
            Path::new("/games/demo/b.scene.ron")
        );
        assert_eq!(
            root.resolve(Path::new("/games/demo/x.scene.ron"))?,
            Path::new("/games/demo/x.scene.ron")
        );
        for bad in [
            "../other/x.scene.ron",
            "scenes/../../x.scene.ron",
            "/etc/x.scene.ron",
            "..",
        ] {
            let err = root.resolve(Path::new(bad)).unwrap_err().to_string();
            assert!(err.contains(&format!("`{bad}`")), "{err}");
            assert!(err.contains("escapes the project root"), "{err}");
        }
        // A sibling directory sharing the root's name as a prefix is outside.
        assert!(root.resolve(Path::new("../demo2/x.scene.ron")).is_err());
        Ok(())
    }

    #[test]
    fn project_tools_open_describe_and_confine_scene_paths() -> anyhow::Result<()> {
        let dir = temp_dir("tools");
        create(&NewProjectOptions {
            name: "tool-game".to_string(),
            dir: dir.clone(),
            engine: EngineDep::default(),
        })?;
        let reg = ToolRegistry::with_engine_tools();
        for name in ["project.open", "project.info"] {
            let info = reg
                .list()
                .into_iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("{name} is not registered"));
            assert!(!info.description.is_empty(), "{name} has no description");
        }

        let mut engine = Headless::new(8, 8)?;
        let err = reg
            .call(&mut engine, "project.info", json!({}))
            .unwrap_err();
        assert!(err.to_string().contains("no project is open"), "{err}");

        let ToolOutput::Json(opened) =
            reg.call(&mut engine, "project.open", json!({ "dir": dir }))?
        else {
            panic!("expected JSON");
        };
        let root = dir.canonicalize()?;
        assert_eq!(opened["name"], "tool-game");
        assert_eq!(opened["dir"], json!(root));
        assert_eq!(opened["entry_scene"], STARTER_SCENE);
        assert_eq!(
            opened["window"],
            json!({ "title": "tool-game", "width": 1280, "height": 720 })
        );
        assert_eq!((engine.target.width, engine.target.height), (1280, 720));
        assert_eq!(engine.world.len(), 3, "the entry scene's entities");
        let ToolOutput::Json(info) = reg.call(&mut engine, "project.info", json!({}))? else {
            panic!("expected JSON");
        };
        assert_eq!(info, opened);

        // Relative scene paths land under the project root.
        reg.call(
            &mut engine,
            "scene.save",
            json!({ "path": "scenes/copy.scene.ron" }),
        )?;
        assert!(root.join("scenes/copy.scene.ron").is_file());
        reg.call(
            &mut engine,
            "scene.load",
            json!({ "path": "scenes/copy.scene.ron" }),
        )?;
        assert_eq!(engine.world.len(), 3);

        // Escaping the root is an error naming the path, for both tools.
        for tool in ["scene.save", "scene.load"] {
            let err = reg
                .call(&mut engine, tool, json!({ "path": "../escape.scene.ron" }))
                .unwrap_err()
                .to_string();
            assert!(err.contains("../escape.scene.ron"), "{tool}: {err}");
        }
        assert!(!dir.join("../escape.scene.ron").exists());

        // A directory without shin.toml fails and leaves the project open.
        let err = reg
            .call(
                &mut engine,
                "project.open",
                json!({ "dir": dir.join("scenes") }),
            )
            .unwrap_err();
        assert!(format!("{err:#}").contains(MANIFEST), "{err:#}");
        assert_eq!(
            engine.resources.get::<ProjectRoot>(),
            Some(&ProjectRoot(root))
        );
        assert_eq!(engine.world.len(), 3);

        fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn broken_entry_scene_leaves_the_engine_unchanged() -> anyhow::Result<()> {
        let dir = temp_dir("broken-entry");
        create(&NewProjectOptions {
            name: "broken".to_string(),
            dir: dir.clone(),
            engine: EngineDep::default(),
        })?;
        fs::write(dir.join(STARTER_SCENE), "(version: 1, bogus: 3)")?;
        let mut engine = Headless::new(8, 8)?;
        let assets_root = engine.resources.expect::<Assets>().root().to_path_buf();
        let err = engine.load_project(&dir).unwrap_err();
        assert!(format!("{err:#}").contains("bogus"), "{err:#}");
        assert!(engine.resources.get::<ProjectRoot>().is_none());
        assert_eq!(engine.resources.expect::<Assets>().root(), assets_root);
        assert_eq!((engine.target.width, engine.target.height), (8, 8));
        fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn open_project_sizes_the_target_from_shin_toml() -> anyhow::Result<()> {
        let dir = temp_dir("open");
        create(&NewProjectOptions {
            name: "sized".to_string(),
            dir: dir.clone(),
            engine: EngineDep::default(),
        })?;
        let mut project = Project::load(&dir)?;
        project.window.width = 64;
        project.window.height = 32;
        project.save(&dir)?;
        let mut engine = Headless::open_project(&dir)?;
        assert_eq!((engine.target.width, engine.target.height), (64, 32));
        assert_eq!(engine.world.len(), 3);
        assert_eq!(engine.render()?.dimensions(), (64, 32));
        fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn manifest_round_trips() -> anyhow::Result<()> {
        let dir = temp_dir("manifest");
        fs::create_dir_all(&dir)?;
        let mut project = Project::new("demo");
        project.window.width = 800;
        project.entry_scene = Some(PathBuf::from("scenes/main.scene.ron"));
        project.save(&dir)?;
        assert_eq!(Project::load(&dir)?, project);

        let text = fs::read_to_string(dir.join(MANIFEST))?;
        assert!(
            text.contains("[project]") && text.contains("[window]"),
            "{text}"
        );
        fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn engine_path_is_absolute_and_escaped() -> anyhow::Result<()> {
        let toml = cargo_toml("x", &EngineDep::Path(PathBuf::from("rel/eng\"ine")))?;
        let parsed: toml::Table = toml.parse()?;
        let path = parsed["dependencies"]["shin-engine"]["path"]
            .as_str()
            .expect("path is a string");
        assert!(Path::new(path).is_absolute(), "{path}");
        assert!(path.ends_with("rel/eng\"ine"), "{path}");
        // `shin-mcp` is the engine directory's sibling.
        let mcp = parsed["dependencies"]["shin-mcp"]["path"]
            .as_str()
            .expect("shin-mcp path is a string");
        assert_eq!(Path::new(mcp), Path::new(path).with_file_name("shin-mcp"));
        Ok(())
    }

    #[test]
    fn git_engine_dep_also_supplies_shin_mcp() -> anyhow::Result<()> {
        let parsed: toml::Table = cargo_toml("x", &EngineDep::default())?.parse()?;
        for dep in ["shin-engine", "shin-mcp"] {
            assert_eq!(
                parsed["dependencies"][dep]["git"].as_str(),
                Some(ENGINE_GIT_URL),
                "{dep}"
            );
        }
        Ok(())
    }
}
