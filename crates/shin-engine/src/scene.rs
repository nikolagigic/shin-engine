//! Scene files (GE-021): the world's entities, the camera, and the light as
//! a diffable, hand-editable RON document with the extension `.scene.ron`.
//!
//! ```ron
//! (
//!     version: 1,
//!     camera: (position: (0.0, 2.0, 5.0), look_at: (0.0, 0.0, 0.0), fov_y_deg: 45.0),
//!     light: (direction: (1.0, -2.0, -1.0), color: (1.0, 1.0, 1.0), ambient: (0.1, 0.1, 0.1)),
//!     entities: [
//!         (
//!             name: "player",
//!             transform: (translation: (0.0, 1.0, 0.0)),
//!             mesh: "builtin://cube",
//!             color: (0.8, 0.1, 0.1, 1.0),
//!             texture: "assets/checker.png",
//!             script: "scripts/orbit.rhai",
//!             audio: (source: "assets/sounds/blip.wav", spatial: true),
//!         ),
//!     ],
//! )
//! ```
//!
//! [`save`] writes and [`load`] reads that format. Optional fields
//! (`camera`, `light`, `name`, `mesh`, `color`, `texture`, `script`,
//! `audio`, and every transform part) may be left out, and are written
//! without `Some(...)`. Unknown fields are an error naming the field.
//! Meshes, textures (GE-025), and sounds (GE-036) are [`AssetSource`]
//! strings, so a scene file is independent of runtime handles.
//!
//! Angles (`rotation_euler_deg`, `fov_y_deg`) and `look_at` are derived from
//! the engine's quaternions and radians, so [`save`] rounds them to four
//! decimals; saving a freshly loaded file reproduces it byte for byte.
//!
//! Tools: [`register_tools`] adds `scene.save` and `scene.load`.

mod tools;

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use glam::EulerRot;
use serde::{Deserialize, Serialize};

pub use tools::register_tools;

use crate::asset::{AssetSource, Assets};
use crate::audio::AudioSource;
use crate::ecs::{Entity, Name, Resources, World};
use crate::math::{Camera, Projection, Quat, Transform, Vec3};
use crate::render::{LightUniform, MeshHandle, MeshInstance, RenderState, TextureHandle};
use crate::script::Script;

/// The scene format version [`save`] writes and [`load`] accepts.
pub const VERSION: u32 = 1;

/// The file extension scene files use (without the leading dot).
pub const EXTENSION: &str = "scene.ron";

/// A whole scene file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneFile {
    /// Format version; must be [`VERSION`].
    pub version: u32,
    /// The camera; when absent, [`load`] keeps the current camera.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<CameraDesc>,
    /// The directional light; when absent, [`load`] keeps the current light.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light: Option<LightDesc>,
    /// Entities, in spawn order.
    #[serde(default)]
    pub entities: Vec<EntityDesc>,
}

impl Default for SceneFile {
    /// An empty version-1 scene.
    fn default() -> Self {
        Self {
            version: VERSION,
            camera: None,
            light: None,
            entities: Vec::new(),
        }
    }
}

/// A perspective camera, aimed with +Y as up.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraDesc {
    /// World-space position.
    pub position: [f32; 3],
    /// World-space point the camera looks at; must differ from `position`
    /// and not be straight above or below it.
    pub look_at: [f32; 3],
    /// Vertical field of view in degrees, between 1 and 179.
    pub fov_y_deg: f32,
}

/// The directional light, in linear RGB.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LightDesc {
    /// Direction the light travels, from the light toward the scene; must
    /// not be zero.
    pub direction: [f32; 3],
    /// Light colour.
    pub color: [f32; 3],
    /// Ambient light added to every surface.
    pub ambient: [f32; 3],
}

/// One entity.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityDesc {
    /// The entity's [`Name`] component, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Placement; omitted parts are the identity.
    #[serde(default)]
    pub transform: TransformDesc,
    /// Mesh to draw, as an [`AssetSource`] string such as `builtin://cube`
    /// or `assets/x.gltf#1`; none for an entity that draws nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh: Option<String>,
    /// Linear RGBA tint of the mesh; default white. Ignored without `mesh`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[f32; 4]>,
    /// Base-colour texture, as a texture [`AssetSource`] string such as
    /// `assets/x.png` or `assets/x.gltf#image=0`; none for untextured.
    /// Ignored without `mesh`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub texture: Option<String>,
    /// The entity's [`Script`] (GE-011): a `.rhai` path relative to the
    /// project directory; none for an unscripted entity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    /// The entity's [`AudioSource`] (GE-036); none for a silent entity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioDesc>,
}

/// An [`AudioSource`] in file form. Every field but `source` has a default
/// and is left out when it has it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioDesc {
    /// The sound, as an audio [`AssetSource`] string: an `.ogg` or `.wav`
    /// path such as `assets/sounds/blip.wav`.
    pub source: String,
    /// Linear volume; default `1.0`.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub volume: f32,
    /// Whether the sound repeats until stopped; default `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub looping: bool,
    /// Whether the volume falls off with the distance from the camera;
    /// default `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub spatial: bool,
    /// Whether the sound starts when play starts; default `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub autoplay: bool,
}

impl Default for AudioDesc {
    /// No source, full volume, one-shot, not spatial, no autoplay.
    fn default() -> Self {
        Self {
            source: String::new(),
            volume: 1.0,
            looping: false,
            spatial: false,
            autoplay: false,
        }
    }
}

impl AudioDesc {
    /// `audio` in file form.
    pub fn from_source(audio: &AudioSource) -> Self {
        Self {
            source: audio.source.to_string(),
            volume: audio.volume,
            looping: audio.looping,
            spatial: audio.spatial,
            autoplay: audio.autoplay,
        }
    }

    /// The component this describes. Fails if `source` does not parse as
    /// an audio asset source.
    pub fn to_source(&self) -> anyhow::Result<AudioSource> {
        let source = AssetSource::parse(&self.source)?;
        anyhow::ensure!(
            source.is_audio(),
            "{source} is not a sound source (.ogg or .wav)"
        );
        Ok(AudioSource {
            source,
            volume: self.volume,
            looping: self.looping,
            spatial: self.spatial,
            autoplay: self.autoplay,
        })
    }
}

fn one() -> f32 {
    1.0
}

fn is_one(x: &f32) -> bool {
    *x == 1.0
}

fn is_false(x: &bool) -> bool {
    !*x
}

/// A [`Transform`] in editable units.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TransformDesc {
    /// Position; default the origin.
    pub translation: [f32; 3],
    /// Euler angles in degrees, intrinsic X then Y then Z (as the
    /// `entity.*` tools use); default no rotation.
    pub rotation_euler_deg: [f32; 3],
    /// Per-axis scale; default `[1, 1, 1]`.
    pub scale: [f32; 3],
}

impl Default for TransformDesc {
    /// The identity transform.
    fn default() -> Self {
        Self {
            translation: [0.0; 3],
            rotation_euler_deg: [0.0; 3],
            scale: [1.0; 3],
        }
    }
}

impl TransformDesc {
    /// `t` in editable units, with the Euler angles rounded.
    pub fn from_transform(t: &Transform) -> Self {
        let (x, y, z) = t.rotation.to_euler(EulerRot::XYZ);
        Self {
            translation: t.translation.to_array(),
            rotation_euler_deg: [x, y, z].map(|a| round(a.to_degrees())),
            scale: t.scale.to_array(),
        }
    }

    /// The engine transform this describes.
    pub fn to_transform(&self) -> Transform {
        let [x, y, z] = self.rotation_euler_deg.map(f32::to_radians);
        Transform {
            translation: Vec3::from(self.translation),
            rotation: Quat::from_euler(EulerRot::XYZ, x, y, z),
            scale: Vec3::from(self.scale),
        }
    }
}

impl SceneFile {
    /// Captures `world`'s entities that have a [`Transform`] (in entity id
    /// order, which is spawn order after a [`load`]), the [`Camera`]
    /// resource, and the [`RenderState`] light.
    ///
    /// Fails if an entity draws a mesh with no [`AssetSource`] (one added
    /// from code rather than through [`Assets`]) or the camera is
    /// orthographic.
    pub fn capture(world: &World, res: &Resources) -> anyhow::Result<Self> {
        let state = res.get::<RenderState>();
        let mut rows: Vec<(u64, EntityDesc)> = Vec::new();
        for (entity, transform, instance, name, script, audio) in world
            .query::<(
                Entity,
                &Transform,
                Option<&MeshInstance>,
                Option<&Name>,
                Option<&Script>,
                Option<&AudioSource>,
            )>()
            .iter()
        {
            let id = entity.to_bits().get();
            let (mesh, color, texture) = match instance {
                Some(instance) => {
                    let source = state
                        .and_then(|s| s.registry.source(instance.mesh))
                        .with_context(|| {
                            format!(
                                "entity {id} draws mesh handle {} which has no asset source, \
                                 so it cannot be saved; load meshes with asset.load or \
                                 mesh.builtin",
                                instance.mesh.0
                            )
                        })?;
                    let texture = instance
                        .texture
                        .map(|handle| {
                            state
                                .and_then(|s| s.textures.source(handle))
                                .map(|s| s.to_string())
                                .with_context(|| {
                                    format!(
                                        "entity {id} uses texture handle {} which has no asset \
                                         source, so it cannot be saved; load textures with \
                                         asset.load",
                                        handle.0
                                    )
                                })
                        })
                        .transpose()?;
                    (Some(source.to_string()), Some(instance.color), texture)
                }
                None => (None, None, None),
            };
            let desc = EntityDesc {
                name: name.map(|Name(n)| n.clone()),
                transform: TransformDesc::from_transform(transform),
                mesh,
                color,
                texture,
                script: script.map(|s| s.path.to_string_lossy().into_owned()),
                audio: audio.map(AudioDesc::from_source),
            };
            rows.push((id, desc));
        }
        rows.sort_by_key(|(id, _)| *id);

        let camera = res.get::<Camera>().map(camera_desc).transpose()?;
        let light = state.map(|s| light_desc(&s.light));
        Ok(Self {
            version: VERSION,
            camera,
            light,
            entities: rows.into_iter().map(|(_, desc)| desc).collect(),
        })
    }

    /// The scene as RON text, the exact bytes [`save`] writes.
    pub fn to_ron(&self) -> anyhow::Result<String> {
        let config = ron::ser::PrettyConfig::new().indentor("    ");
        let mut text = ron_options()
            .to_string_pretty(self, config)
            .context("failed to serialise the scene")?;
        text.push('\n');
        Ok(text)
    }

    /// Parses RON text and checks the version. Errors name unknown or
    /// missing fields and give the line and column.
    pub fn from_ron(text: &str) -> anyhow::Result<Self> {
        let scene: Self = ron_options()
            .from_str(text)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        anyhow::ensure!(
            scene.version == VERSION,
            "unsupported scene version {}; this engine reads version {VERSION}",
            scene.version
        );
        Ok(scene)
    }
}

/// Writes `world`'s entities, the camera, and the light to `path` (see
/// [`SceneFile::capture`]), creating missing parent directories.
///
/// `path` must end in `.scene.ron`.
pub fn save(world: &World, res: &Resources, path: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        path.to_string_lossy().ends_with(&format!(".{EXTENSION}")),
        "scene files must end in .{EXTENSION}, got {}",
        path.display()
    );
    let text = SceneFile::capture(world, res)?.to_ron()?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::write(path, text).with_context(|| format!("failed to write {}", path.display()))
}

/// Replaces `world`'s entities with the scene in `path` and applies its
/// camera and light. Returns the number of entities spawned.
///
/// Meshes and textures are resolved through the [`Assets`] resource
/// (relative file sources against its root) and uploaded into the
/// [`RenderState`] registries (`queue` uploads texture pixels); sounds are
/// decoded into the same cache. Everything is parsed, validated, and
/// resolved before the world is cleared, so a failed load leaves the world
/// untouched. Fails on a parse error, an unknown field (named), an
/// unsupported version, an invalid camera or light, or a mesh, texture, or
/// sound source that does not resolve (the error names the source).
pub fn load(
    world: &mut World,
    res: &mut Resources,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    path: &Path,
) -> anyhow::Result<usize> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let scene =
        SceneFile::from_ron(&text).with_context(|| format!("invalid scene {}", path.display()))?;
    apply(world, res, device, queue, &scene)
}

/// [`load`] for an already parsed scene, for example one captured in
/// memory with [`SceneFile::capture`] (GE-024 restores play-mode snapshots
/// this way). Same validation and errors as [`load`].
pub fn apply(
    world: &mut World,
    res: &mut Resources,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &SceneFile,
) -> anyhow::Result<usize> {
    if let Some(camera) = &scene.camera {
        validate_camera(camera)?;
    }
    if let Some(light) = &scene.light {
        anyhow::ensure!(
            Vec3::from(light.direction).length_squared() > 0.0,
            "light direction must not be zero"
        );
    }
    let resolved = resolve_assets(res, device, queue, &scene.entities)?;
    let sounds = resolve_sounds(res, &scene.entities)?;

    world.clear();
    for ((desc, assets), audio) in scene.entities.iter().zip(resolved).zip(sounds) {
        let entity = world.spawn((desc.transform.to_transform(),));
        if let Some(audio) = audio {
            world.insert_one(entity, audio)?;
        }
        if let Some((mesh, texture)) = assets {
            let color = desc.color.unwrap_or([1.0; 4]);
            let instance = MeshInstance {
                mesh,
                color,
                texture,
            };
            world.insert_one(entity, instance)?;
        }
        if let Some(name) = &desc.name {
            world.insert_one(entity, Name(name.clone()))?;
        }
        if let Some(path) = &desc.script {
            world.insert_one(entity, Script::new(path))?;
        }
    }

    if let Some(desc) = &scene.camera {
        apply_camera(res, desc);
    }
    if let Some(desc) = &scene.light {
        let state = res
            .get_mut::<RenderState>()
            .context("no RenderState resource")?;
        state.light = LightUniform {
            direction: Vec3::from(desc.direction).extend(0.0).to_array(),
            color: Vec3::from(desc.color).extend(1.0).to_array(),
            ambient: Vec3::from(desc.ambient).extend(1.0).to_array(),
        };
    }
    Ok(scene.entities.len())
}

/// What an entity draws: its mesh and optional texture.
type DrawAssets = (MeshHandle, Option<TextureHandle>);

/// The mesh and texture handles of every entity that draws a mesh, in
/// order, uploading sources as needed. A texture without a mesh is ignored.
fn resolve_assets(
    res: &mut Resources,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    entities: &[EntityDesc],
) -> anyhow::Result<Vec<Option<DrawAssets>>> {
    // Resources lend one value mutably at a time, so `Assets` is moved out
    // for the duration and always put back.
    let slot = res.get_mut::<Assets>().context("no Assets resource")?;
    let mut assets = std::mem::replace(slot, Assets::new(PathBuf::new()));
    let result = (|| {
        let state = res
            .get_mut::<RenderState>()
            .context("no RenderState resource")?;
        entities
            .iter()
            .enumerate()
            .map(|(i, desc)| {
                let Some(text) = &desc.mesh else {
                    return Ok(None);
                };
                let mesh = AssetSource::parse(text)
                    .and_then(|source| assets.mesh(device, &mut state.registry, &source))
                    .with_context(|| format!("entity {i}: mesh {text:?} could not be loaded"))?;
                let texture = desc
                    .texture
                    .as_ref()
                    .map(|text| {
                        AssetSource::parse(text)
                            .and_then(|source| {
                                assets.texture(device, queue, &mut state.textures, &source)
                            })
                            .with_context(|| {
                                format!("entity {i}: texture {text:?} could not be loaded")
                            })
                    })
                    .transpose()?;
                Ok(Some((mesh, texture)))
            })
            .collect()
    })();
    res.insert(assets);
    result
}

/// The [`AudioSource`] of every entity that has an `audio` field, in
/// order, decoding each sound into the [`Assets`] cache so a missing or
/// unreadable file is reported here, before the world is touched.
fn resolve_sounds(
    res: &mut Resources,
    entities: &[EntityDesc],
) -> anyhow::Result<Vec<Option<AudioSource>>> {
    let assets = res.get_mut::<Assets>().context("no Assets resource")?;
    entities
        .iter()
        .enumerate()
        .map(|(i, desc)| {
            desc.audio
                .as_ref()
                .map(|audio| -> anyhow::Result<AudioSource> {
                    let source = audio.to_source()?;
                    assets.sound(&source.source)?;
                    Ok(source)
                })
                .transpose()
                .with_context(|| {
                    let text = desc.audio.as_ref().map_or("", |a| a.source.as_str());
                    format!("entity {i}: sound {text:?} could not be loaded")
                })
        })
        .collect()
}

fn validate_camera(desc: &CameraDesc) -> anyhow::Result<()> {
    anyhow::ensure!(
        (1.0..=179.0).contains(&desc.fov_y_deg),
        "camera fov_y_deg must be between 1 and 179, got {}",
        desc.fov_y_deg
    );
    let view = Vec3::from(desc.look_at) - Vec3::from(desc.position);
    anyhow::ensure!(
        view.try_normalize()
            .is_some_and(|v| v.cross(Vec3::Y).length_squared() > 1e-12),
        "camera look_at must differ from position and not be straight above or below it"
    );
    Ok(())
}

/// Sets the [`Camera`] resource from `desc`, keeping its clip planes
/// (inserting a 0.1..100 camera if there is none).
fn apply_camera(res: &mut Resources, desc: &CameraDesc) {
    if res.get::<Camera>().is_none() {
        res.insert(crate::headless::default_camera());
    }
    let camera = res.expect_mut::<Camera>();
    let (near, far) = match camera.projection {
        Projection::Perspective { near, far, .. } | Projection::Orthographic { near, far, .. } => {
            (near, far)
        }
    };
    camera.transform.translation = Vec3::from(desc.position);
    camera
        .transform
        .looking_at(Vec3::from(desc.look_at), Vec3::Y);
    camera.projection = Projection::Perspective {
        fov_y_radians: desc.fov_y_deg.to_radians(),
        near,
        far,
    };
}

/// `camera` as a [`CameraDesc`].
///
/// The look-at point is the point on the view ray nearest the world origin,
/// but at least one unit ahead, so a camera aimed at the origin saves
/// `look_at: (0.0, 0.0, 0.0)`.
fn camera_desc(camera: &Camera) -> anyhow::Result<CameraDesc> {
    let Projection::Perspective { fov_y_radians, .. } = camera.projection else {
        anyhow::bail!("orthographic cameras cannot be saved in a scene file yet");
    };
    let position = camera.transform.translation;
    let forward = camera.transform.forward();
    let distance = (-position).dot(forward).max(1.0);
    let look_at = position + forward * distance;
    Ok(CameraDesc {
        position: position.to_array(),
        look_at: look_at.to_array().map(round),
        fov_y_deg: round(fov_y_radians.to_degrees()),
    })
}

/// `light` as a [`LightDesc`].
fn light_desc(light: &LightUniform) -> LightDesc {
    let xyz = |v: [f32; 4]| [v[0], v[1], v[2]];
    LightDesc {
        direction: xyz(light.direction),
        color: xyz(light.color),
        ambient: xyz(light.ambient),
    }
}

/// `x` rounded to four decimals, with `-0.0` written as `0.0`.
fn round(x: f32) -> f32 {
    let rounded = ((f64::from(x) * 1e4).round() / 1e4) as f32;
    rounded + 0.0
}

/// RON options shared by reading and writing: `Some(...)` is implicit.
fn ron_options() -> ron::Options {
    ron::Options::default().with_default_extension(ron::extensions::Extensions::IMPLICIT_SOME)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::headless::Headless;
    use crate::tool::{ToolError, ToolOutput, ToolRegistry};

    /// A fresh, empty directory under the system temp dir for one test.
    fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shin-scene-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn call_json(reg: &ToolRegistry, engine: &mut Headless, name: &str, args: Value) -> Value {
        match reg.call(engine, name, args) {
            Ok(ToolOutput::Json(value)) => value,
            other => panic!("`{name}` did not return JSON: {other:?}"),
        }
    }

    fn call_err(reg: &ToolRegistry, engine: &mut Headless, name: &str, args: Value) -> String {
        match reg.call(engine, name, args) {
            Err(ToolError::Failed(err)) => format!("{err:#}"),
            other => panic!("`{name}` should have failed: {other:?}"),
        }
    }

    #[test]
    fn save_load_save_is_byte_identical() -> anyhow::Result<()> {
        let dir = temp_dir("roundtrip");
        let reg = ToolRegistry::with_engine_tools();
        let mut engine = Headless::new(8, 8)?;
        let cube =
            call_json(&reg, &mut engine, "mesh.builtin", json!({ "kind": "cube" }))["handle"]
                .clone();
        let sphere = call_json(
            &reg,
            &mut engine,
            "asset.load",
            json!({ "source": "builtin://sphere-8x4" }),
        )["handle"]
            .clone();
        // Textures (GE-025): one image file and one glTF image.
        let test_assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/test");
        let png_source = test_assets.join("checker.png").display().to_string();
        let gltf_source = format!(
            "{}#image=0",
            test_assets.join("textured_cube.gltf").display()
        );
        let load_texture = |engine: &mut Headless, source: &str| {
            call_json(&reg, engine, "asset.load", json!({ "source": source }))["texture"].clone()
        };
        let png = load_texture(&mut engine, &png_source);
        let embedded = load_texture(&mut engine, &gltf_source);
        for args in [
            json!({ "mesh": cube, "name": "tilted", "translation": [-1.5, 0.25, 0],
                    "rotation_euler_deg": [10, 30, -45], "color": [0.8, 0.1, 0.1, 1] }),
            json!({ "mesh": sphere, "translation": [0.1, 0.2, 0.3] }),
            json!({ "mesh": cube, "name": "small", "scale": [0.6, 0.6, 0.6] }),
            json!({ "mesh": cube, "name": "checkered", "texture": png }),
            json!({ "mesh": sphere, "texture": embedded, "color": [0.5, 0.5, 1, 1] }),
        ] {
            call_json(&reg, &mut engine, "entity.spawn", args);
        }
        call_json(
            &reg,
            &mut engine,
            "camera.set",
            json!({ "position": [3, 4, 6], "look_at": [0, 0.5, 0], "fov_y_deg": 50 }),
        );
        call_json(
            &reg,
            &mut engine,
            "light.set",
            json!({ "direction": [0, -1, -0.5], "ambient": [0.2, 0.2, 0.25] }),
        );

        let first = dir.join("first.scene.ron");
        let out = call_json(&reg, &mut engine, "scene.save", json!({ "path": first }));
        assert_eq!(out["entities"], 5);
        call_json(&reg, &mut engine, "scene.clear", json!({}));
        assert_eq!(engine.world.len(), 0);
        let out = call_json(&reg, &mut engine, "scene.load", json!({ "path": first }));
        assert_eq!(out, json!({ "entities": 5 }));
        let second = dir.join("second.scene.ron");
        call_json(&reg, &mut engine, "scene.save", json!({ "path": second }));

        let a = std::fs::read_to_string(&first)?;
        let b = std::fs::read_to_string(&second)?;
        assert_eq!(a, b, "save -> load -> save changed the file");
        // Spot-check that the format is the readable one.
        assert!(a.contains("name: \"tilted\""), "{a}");
        assert!(a.contains("mesh: \"builtin://sphere-8x4\""), "{a}");
        assert!(a.contains("rotation_euler_deg: (10.0, 30.0, -45.0)"), "{a}");
        assert!(a.contains("fov_y_deg: 50.0"), "{a}");
        assert!(!a.contains("Some("), "{a}");
        assert!(a.contains(&format!("texture: {png_source:?}")), "{a}");
        assert!(a.contains(&format!("texture: {gltf_source:?}")), "{a}");
        assert_eq!(
            a.matches("texture:").count(),
            2,
            "untextured entities omit it"
        );

        // Loaded entities carry their names and textures; the textures come
        // from the Assets cache, so nothing is uploaded twice.
        let list = call_json(&reg, &mut engine, "entity.list", json!({}));
        assert_eq!(list[0]["name"], "tilted");
        assert!(list[1].get("name").is_none(), "{}", list[1]);
        assert_eq!(list[2]["name"], "small");
        let textures: Vec<&Value> = (0..5).map(|i| &list[i]["texture"]).collect();
        assert_eq!(
            textures,
            [&Value::Null, &Value::Null, &Value::Null, &png, &embedded]
        );
        assert_eq!(engine.resources.expect::<RenderState>().textures.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn camera_aimed_at_the_origin_saves_look_at_origin() -> anyhow::Result<()> {
        let desc = camera_desc(&crate::headless::default_camera())?;
        assert_eq!(desc.position, [0.0, 2.0, 5.0]);
        assert_eq!(desc.look_at, [0.0, 0.0, 0.0]);
        assert_eq!(desc.fov_y_deg, 45.0);
        Ok(())
    }

    #[test]
    fn loading_the_same_file_twice_replaces_the_scene() -> anyhow::Result<()> {
        let reg = ToolRegistry::with_engine_tools();
        let mut engine = Headless::new(8, 8)?;
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/scenes/tool_scene.scene.ron");
        for _ in 0..2 {
            call_json(&reg, &mut engine, "scene.load", json!({ "path": path }));
            assert_eq!(engine.world.len(), 3, "load clears first");
        }
        // Built-in meshes are shared through the Assets cache.
        assert_eq!(engine.resources.expect::<RenderState>().registry.len(), 1);
        Ok(())
    }

    #[test]
    fn unknown_field_is_an_error_naming_it() {
        let text =
            "(version: 1, entities: [(mesh: \"builtin://cube\", colour: (1.0, 0.0, 0.0, 1.0))])";
        let err = SceneFile::from_ron(text).unwrap_err().to_string();
        assert!(err.contains("colour"), "{err}");

        let err = SceneFile::from_ron("(version: 1, sky: true)")
            .unwrap_err()
            .to_string();
        assert!(err.contains("sky"), "{err}");

        let err = SceneFile::from_ron("(version: 2)").unwrap_err().to_string();
        assert!(err.contains("version 2"), "{err}");
    }

    #[test]
    fn minimal_file_uses_defaults() -> anyhow::Result<()> {
        let scene = SceneFile::from_ron("(version: 1, entities: [()])")?;
        assert_eq!(scene.camera, None);
        assert_eq!(scene.entities.len(), 1);
        let entity = &scene.entities[0];
        assert_eq!(entity.transform, TransformDesc::default());
        assert_eq!(entity.transform.to_transform(), Transform::IDENTITY);
        assert_eq!((entity.mesh.as_deref(), entity.color), (None, None));
        Ok(())
    }

    #[test]
    fn missing_mesh_is_an_error_naming_the_source_and_keeps_the_world() -> anyhow::Result<()> {
        let dir = temp_dir("missing-mesh");
        let reg = ToolRegistry::with_engine_tools();
        let mut engine = Headless::new(8, 8)?;
        let cube =
            call_json(&reg, &mut engine, "mesh.builtin", json!({ "kind": "cube" }))["handle"]
                .clone();
        call_json(&reg, &mut engine, "entity.spawn", json!({ "mesh": cube }));

        for source in ["assets/nope/missing.gltf#1", "builtin://torus"] {
            let path = dir.join("bad.scene.ron");
            std::fs::write(
                &path,
                format!("(version: 1, entities: [(mesh: \"builtin://cube\"), (mesh: {source:?})])"),
            )?;
            let err = call_err(&reg, &mut engine, "scene.load", json!({ "path": path }));
            assert!(err.contains(source), "{err}");
            assert_eq!(engine.world.len(), 1, "a failed load leaves the world");
        }
        // The same for textures, including a mesh source given as a texture.
        for source in ["assets/nope/missing.png", "builtin://cube"] {
            let path = dir.join("bad.scene.ron");
            std::fs::write(
                &path,
                format!(
                    "(version: 1, entities: [(mesh: \"builtin://cube\", texture: {source:?})])"
                ),
            )?;
            let err = call_err(&reg, &mut engine, "scene.load", json!({ "path": path }));
            assert!(err.contains(&format!("texture {source:?}")), "{err}");
            assert_eq!(engine.world.len(), 1, "a failed load leaves the world");
        }
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn save_rejects_wrong_extension_and_unsourced_meshes() -> anyhow::Result<()> {
        let dir = temp_dir("save-errors");
        let reg = ToolRegistry::with_engine_tools();
        let mut engine = Headless::new(8, 8)?;
        let err = call_err(
            &reg,
            &mut engine,
            "scene.save",
            json!({ "path": dir.join("x.ron") }),
        );
        assert!(err.contains(".scene.ron"), "{err}");

        // A mesh added from code has no source, so it cannot be named in a file.
        let state = engine.resources.expect_mut::<RenderState>();
        let handle = state
            .registry
            .add(&engine.gpu.device, &crate::asset::cube());
        call_json(
            &reg,
            &mut engine,
            "entity.spawn",
            json!({ "mesh": handle.0 }),
        );
        let path = dir.join("x.scene.ron");
        let err = call_err(&reg, &mut engine, "scene.save", json!({ "path": path }));
        assert!(err.contains("no asset source"), "{err}");
        assert!(!path.exists());

        // Likewise a texture added from code.
        engine.world.clear();
        let cube =
            call_json(&reg, &mut engine, "mesh.builtin", json!({ "kind": "cube" }))["handle"]
                .clone();
        let state = engine.resources.expect_mut::<RenderState>();
        let image = crate::asset::ImageData {
            name: "white".into(),
            width: 1,
            height: 1,
            rgba: vec![255; 4],
        };
        let texture = state
            .textures
            .add(&engine.gpu.device, &engine.gpu.queue, &image);
        call_json(
            &reg,
            &mut engine,
            "entity.spawn",
            json!({ "mesh": cube, "texture": texture.0 }),
        );
        let err = call_err(&reg, &mut engine, "scene.save", json!({ "path": path }));
        assert!(
            err.contains("texture handle 0 which has no asset source"),
            "{err}"
        );
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn tools_are_listed_with_descriptions() {
        let tools = ToolRegistry::with_engine_tools().list();
        for name in ["scene.save", "scene.load"] {
            let tool = tools
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("{name} is not registered"));
            assert!(tool.description.contains(".scene.ron"), "{name}");
            let path = &tool.input_schema["properties"]["path"]["description"];
            assert!(path.as_str().is_some_and(|d| !d.is_empty()), "{name}");
        }
    }
}
