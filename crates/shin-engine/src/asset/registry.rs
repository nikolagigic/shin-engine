//! Stable asset names (GE-019): [`AssetSource`] says where a mesh or a
//! texture comes from, and the [`Assets`] resource turns sources into GPU
//! meshes ([`Assets::mesh`]) and textures ([`Assets::texture`], GE-025),
//! uploading each source once.
//!
//! Sources have a string form used by tools and scene files:
//!
//! | String                  | Source                                        |
//! |-------------------------|-----------------------------------------------|
//! | `builtin://cube`        | [`AssetSource::Builtin`]`("cube")`            |
//! | `assets/x.gltf`         | [`AssetSource::File`] with `index: 0`         |
//! | `assets/x.gltf#2`       | [`AssetSource::File`] with `index: 2`         |
//! | `assets/x.png`          | [`AssetSource::Image`] (also `.jpg`, `.jpeg`) |
//! | `assets/x.gltf#image=1` | [`AssetSource::GltfImage`] with `index: 1`    |
//! | `assets/x.ogg`          | [`AssetSource::Audio`] (also `.wav`)          |
//!
//! The first three are meshes, the next two textures, and the last a sound
//! (GE-036, decoded by [`Assets::sound`]).
//!
//! Built-in names are `cube` ([`super::cube`]), `sphere`
//! ([`super::uv_sphere`] with 32 segments and 16 rings), and
//! `sphere-<segments>x<rings>` (e.g. `sphere-8x4`) for other resolutions.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::Context as _;
use kira::sound::static_sound::StaticSoundData;
use serde::{Deserialize, Serialize};

use super::{ImageData, MeshData};
use crate::render::{MeshHandle, MeshRegistry, TextureHandle, TextureRegistry};

/// Prefix of a built-in source's string form.
const BUILTIN_SCHEME: &str = "builtin://";

/// What follows `#` in a glTF image source: `x.gltf#image=<index>`.
const GLTF_IMAGE_FRAGMENT: &str = "image=";

/// File extensions (lowercase) that make a path an [`AssetSource::Image`].
const IMAGE_EXTENSIONS: [&str; 3] = ["png", "jpg", "jpeg"];

/// File extensions (lowercase) that make a path an [`AssetSource::Audio`].
const AUDIO_EXTENSIONS: [&str; 2] = ["ogg", "wav"];

/// Segments and rings of the plain `sphere` built-in.
const DEFAULT_SPHERE: (u32, u32) = (32, 16);

/// Largest segment or ring count a built-in sphere may have.
const MAX_SPHERE_DIVISIONS: u32 = 1024;

/// Where a mesh or texture comes from: a stable name that survives
/// restarts, unlike a runtime [`MeshHandle`] or [`TextureHandle`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AssetSource {
    /// A mesh generated in code, by name: `cube`, `sphere`, or
    /// `sphere-<segments>x<rings>`.
    Builtin(String),
    /// A triangle primitive in a glTF file.
    File {
        /// Path to a `.gltf` or `.glb` file; relative paths are resolved
        /// against [`Assets`]' root.
        path: PathBuf,
        /// Index into the file's triangle primitives in document order
        /// ([`super::SceneData::meshes`]); 0 for single-mesh files.
        index: usize,
    },
    /// A texture: a PNG or JPEG file. The path must end in `.png`, `.jpg`,
    /// or `.jpeg` (any case) for the string form to parse back to this.
    Image {
        /// Path to the image; relative paths are resolved against
        /// [`Assets`]' root.
        path: PathBuf,
    },
    /// A texture: an image of a glTF file, embedded or referenced.
    GltfImage {
        /// Path to a `.gltf` or `.glb` file; relative paths are resolved
        /// against [`Assets`]' root.
        path: PathBuf,
        /// Index into the file's images ([`super::SceneData::images`]).
        index: usize,
    },
    /// A sound: an Ogg Vorbis or WAV file (GE-036). The path must end in
    /// `.ogg` or `.wav` (any case) for the string form to parse back to
    /// this.
    Audio {
        /// Path to the sound file; relative paths are resolved against
        /// [`Assets`]' root.
        path: PathBuf,
    },
}

impl AssetSource {
    /// Parses the string form: `builtin://<name>`, `<path>`,
    /// `<path>#<index>`, an image path ending in `.png`, `.jpg`, or `.jpeg`,
    /// `<path>#image=<index>`, or a sound path ending in `.ogg` or `.wav`.
    ///
    /// Only the syntax is checked; whether the built-in exists or the file
    /// loads is checked by [`Assets::mesh`] and [`Assets::texture`]. Fails
    /// on an empty string, an empty or non-`[A-Za-z0-9_-]` built-in name,
    /// an empty path, an index that is not a non-negative integer, or any
    /// other `scheme://`.
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        if let Some(name) = s.strip_prefix(BUILTIN_SCHEME) {
            let valid = !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
            anyhow::ensure!(
                valid,
                "invalid asset source {s:?}: a built-in name must be non-empty and use only \
                 letters, digits, '_' and '-'"
            );
            return Ok(Self::Builtin(name.to_string()));
        }
        anyhow::ensure!(
            !s.contains("://"),
            "invalid asset source {s:?}: only builtin:// and file paths are supported"
        );
        if has_extension(Path::new(s), &IMAGE_EXTENSIONS) {
            return Ok(Self::Image {
                path: PathBuf::from(s),
            });
        }
        if has_extension(Path::new(s), &AUDIO_EXTENSIONS) {
            return Ok(Self::Audio {
                path: PathBuf::from(s),
            });
        }
        if let Some((path, index)) = s
            .rsplit_once('#')
            .and_then(|(path, fragment)| Some((path, fragment.strip_prefix(GLTF_IMAGE_FRAGMENT)?)))
        {
            let index = index.parse().with_context(|| {
                format!("invalid asset source {s:?}: {index:?} after '#image=' is not an index")
            })?;
            anyhow::ensure!(!path.is_empty(), "invalid asset source {s:?}: empty path");
            return Ok(Self::GltfImage {
                path: PathBuf::from(path),
                index,
            });
        }
        let (path, index) = match s.rsplit_once('#') {
            Some((path, index)) => {
                let index = index.parse().with_context(|| {
                    format!("invalid asset source {s:?}: {index:?} after '#' is not an index")
                })?;
                (path, index)
            }
            None => (s, 0),
        };
        anyhow::ensure!(!path.is_empty(), "invalid asset source {s:?}: empty path");
        Ok(Self::File {
            path: PathBuf::from(path),
            index,
        })
    }

    /// Whether this names a texture ([`Self::Image`] or
    /// [`Self::GltfImage`]) rather than a mesh.
    pub fn is_texture(&self) -> bool {
        matches!(self, Self::Image { .. } | Self::GltfImage { .. })
    }

    /// Whether this names a sound ([`Self::Audio`]) rather than a mesh or
    /// a texture.
    pub fn is_audio(&self) -> bool {
        matches!(self, Self::Audio { .. })
    }
}

/// Whether `path` ends in one of `extensions` (lowercase), ignoring case.
fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| extensions.contains(&e.to_ascii_lowercase().as_str()))
}

impl fmt::Display for AssetSource {
    /// The string form [`AssetSource::parse`] reads. `#0` is omitted unless
    /// the path itself contains a `#`, so parsing the output gives back the
    /// same source.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Builtin(name) => write!(f, "{BUILTIN_SCHEME}{name}"),
            Self::File { path, index } => {
                let path = path.display().to_string();
                if *index == 0 && !path.contains('#') {
                    f.write_str(&path)
                } else {
                    write!(f, "{path}#{index}")
                }
            }
            Self::Image { path } | Self::Audio { path } => write!(f, "{}", path.display()),
            Self::GltfImage { path, index } => {
                write!(f, "{}#{GLTF_IMAGE_FRAGMENT}{index}", path.display())
            }
        }
    }
}

impl FromStr for AssetSource {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        Self::parse(s)
    }
}

/// Resource: resolves [`AssetSource`]s to GPU meshes and textures, and to
/// decoded sounds, caching by source so each source is loaded once.
pub struct Assets {
    /// Directory relative file sources are resolved against.
    root: PathBuf,
    /// Every mesh source uploaded so far.
    meshes: HashMap<AssetSource, MeshHandle>,
    /// Every texture source uploaded so far.
    textures: HashMap<AssetSource, TextureHandle>,
    /// Every sound decoded so far (GE-036). A [`StaticSoundData`] shares
    /// its samples between clones, so handing out copies is cheap.
    sounds: HashMap<AssetSource, StaticSoundData>,
}

impl Assets {
    /// An empty cache resolving relative file paths against `root` (the
    /// project directory). Built-in sources need no root.
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            meshes: HashMap::new(),
            textures: HashMap::new(),
            sounds: HashMap::new(),
        }
    }

    /// The decoded samples of `source`, reading and decoding the file on
    /// first use. Later calls with an equal source return a clone sharing
    /// the same samples without touching the disk.
    ///
    /// Fails if `source` is not an [`AssetSource::Audio`] or the file
    /// cannot be read or decoded (only mono and stereo Ogg Vorbis and WAV
    /// are supported).
    pub fn sound(&mut self, source: &AssetSource) -> anyhow::Result<StaticSoundData> {
        if let Some(sound) = self.sounds.get(source) {
            return Ok(sound.clone());
        }
        let AssetSource::Audio { path } = source else {
            anyhow::bail!("{source} is not a sound source (.ogg or .wav)");
        };
        let file = self.resolve(path);
        let sound = StaticSoundData::from_file(&file)
            .map_err(|err| anyhow::anyhow!("{err}"))
            .with_context(|| format!("failed to load sound {}", file.display()))?;
        self.sounds.insert(source.clone(), sound.clone());
        Ok(sound)
    }

    /// The directory relative file sources are resolved against.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The handle of `source`'s mesh, loading and uploading it into
    /// `registry` on first use. Later calls with an equal source return the
    /// same handle without touching the disk or the GPU.
    ///
    /// Fails if `source` is a texture source, a built-in name is unknown,
    /// the file does not load, or the file has no triangle primitive at
    /// `index`.
    pub fn mesh(
        &mut self,
        device: &wgpu::Device,
        registry: &mut MeshRegistry,
        source: &AssetSource,
    ) -> anyhow::Result<MeshHandle> {
        if let Some(handle) = self.cached(registry, source) {
            return Ok(handle);
        }
        let data = match source {
            AssetSource::Builtin(name) => builtin_mesh(name)?,
            AssetSource::File { path, index } => {
                let mut scene = super::load_gltf(self.resolve(path))?;
                let count = scene.meshes.len();
                anyhow::ensure!(
                    *index < count,
                    "{} has {count} triangle primitive(s); index {index} is out of range",
                    path.display()
                );
                scene.meshes.swap_remove(*index)
            }
            AssetSource::Image { .. } | AssetSource::GltfImage { .. } => {
                anyhow::bail!("{source} is a texture source, not a mesh")
            }
            AssetSource::Audio { .. } => {
                anyhow::bail!("{source} is a sound source, not a mesh")
            }
        };
        Ok(self.insert(device, registry, source, &data))
    }

    /// The handle of `source`'s texture, loading and uploading it into
    /// `textures` on first use. Later calls with an equal source return the
    /// same handle without touching the disk or the GPU.
    ///
    /// Fails if `source` is a mesh source, the file does not load, or a
    /// glTF file has no image at `index`.
    pub fn texture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        textures: &mut TextureRegistry,
        source: &AssetSource,
    ) -> anyhow::Result<TextureHandle> {
        if let Some(handle) = self.cached_texture(textures, source) {
            return Ok(handle);
        }
        let image = match source {
            AssetSource::Image { path } => super::load_image(self.resolve(path))?,
            AssetSource::GltfImage { path, index } => {
                let mut scene = super::load_gltf(self.resolve(path))?;
                let count = scene.images.len();
                anyhow::ensure!(
                    *index < count,
                    "{} has {count} image(s); image index {index} is out of range",
                    path.display()
                );
                scene.images.swap_remove(*index)
            }
            AssetSource::Builtin(_) | AssetSource::File { .. } => {
                anyhow::bail!("{source} is a mesh source, not a texture")
            }
            AssetSource::Audio { .. } => {
                anyhow::bail!("{source} is a sound source, not a texture")
            }
        };
        self.insert_texture(device, queue, textures, source, &image)
    }

    /// Like [`Self::texture`] for a source whose [`ImageData`] the caller
    /// has already loaded: uploads `image` unless `source` is cached.
    pub(crate) fn texture_from_data(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        textures: &mut TextureRegistry,
        source: &AssetSource,
        image: &ImageData,
    ) -> anyhow::Result<TextureHandle> {
        match self.cached_texture(textures, source) {
            Some(handle) => Ok(handle),
            None => self.insert_texture(device, queue, textures, source, image),
        }
    }

    /// The cached texture handle for `source`, if it is still in `textures`.
    fn cached_texture(
        &self,
        textures: &TextureRegistry,
        source: &AssetSource,
    ) -> Option<TextureHandle> {
        self.textures
            .get(source)
            .copied()
            .filter(|&handle| textures.contains(handle))
    }

    /// Uploads `image` tagged with `source` and caches the handle. Fails,
    /// instead of tripping wgpu validation, if the image is empty, larger
    /// than the device allows, or its pixel buffer has the wrong length.
    fn insert_texture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        textures: &mut TextureRegistry,
        source: &AssetSource,
        image: &ImageData,
    ) -> anyhow::Result<TextureHandle> {
        let max = device.limits().max_texture_dimension_2d;
        let (w, h) = (image.width, image.height);
        anyhow::ensure!(
            (1..=max).contains(&w) && (1..=max).contains(&h),
            "{source}: image is {w}x{h}; textures must be 1..={max} pixels on each side"
        );
        anyhow::ensure!(
            image.rgba.len() as u64 == u64::from(w) * u64::from(h) * 4,
            "{source}: image has {} bytes of RGBA, expected {w}x{h}x4",
            image.rgba.len()
        );
        let handle = textures.add_with_source(device, queue, image, source.clone());
        self.textures.insert(source.clone(), handle);
        Ok(handle)
    }

    /// The source `handle` was loaded from, if it came through
    /// [`Self::mesh`]. Linear in the number of cached sources.
    pub fn source_of(&self, handle: MeshHandle) -> Option<&AssetSource> {
        self.meshes
            .iter()
            .find_map(|(source, &h)| (h == handle).then_some(source))
    }

    /// `path` joined onto [`Self::root`] (absolute paths are unchanged).
    pub(crate) fn resolve(&self, path: &Path) -> PathBuf {
        self.root.join(path)
    }

    /// Like [`Self::mesh`] for a source whose [`MeshData`] the caller has
    /// already loaded (`scene.load_gltf` reads a file once for all its
    /// primitives): uploads `data` unless `source` is cached.
    pub(crate) fn mesh_from_data(
        &mut self,
        device: &wgpu::Device,
        registry: &mut MeshRegistry,
        source: &AssetSource,
        data: &MeshData,
    ) -> MeshHandle {
        match self.cached(registry, source) {
            Some(handle) => handle,
            None => self.insert(device, registry, source, data),
        }
    }

    /// The cached handle for `source`, if it is still in `registry`.
    fn cached(&self, registry: &MeshRegistry, source: &AssetSource) -> Option<MeshHandle> {
        self.meshes
            .get(source)
            .copied()
            .filter(|&handle| registry.contains(handle))
    }

    /// Uploads `data` tagged with `source` and caches the handle.
    fn insert(
        &mut self,
        device: &wgpu::Device,
        registry: &mut MeshRegistry,
        source: &AssetSource,
        data: &MeshData,
    ) -> MeshHandle {
        let handle = registry.add_with_source(device, data, source.clone());
        self.meshes.insert(source.clone(), handle);
        handle
    }
}

/// The built-in source name for a UV sphere: `sphere` at the default
/// resolution, `sphere-<segments>x<rings>` otherwise.
pub(crate) fn sphere_name(segments: u32, rings: u32) -> String {
    if (segments, rings) == DEFAULT_SPHERE {
        "sphere".to_string()
    } else {
        format!("sphere-{segments}x{rings}")
    }
}

/// Generates the built-in mesh called `name`.
fn builtin_mesh(name: &str) -> anyhow::Result<MeshData> {
    if name == "cube" {
        return Ok(super::cube());
    }
    let (segments, rings) = if name == "sphere" {
        DEFAULT_SPHERE
    } else {
        name.strip_prefix("sphere-")
            .and_then(|size| size.split_once('x'))
            .and_then(|(s, r)| Some((s.parse().ok()?, r.parse().ok()?)))
            .with_context(|| {
                format!(
                    "unknown built-in mesh {name:?}; expected cube, sphere, or \
                     sphere-<segments>x<rings>"
                )
            })?
    };
    anyhow::ensure!(
        (3..=MAX_SPHERE_DIVISIONS).contains(&segments)
            && (2..=MAX_SPHERE_DIVISIONS).contains(&rings),
        "built-in sphere needs 3..={MAX_SPHERE_DIVISIONS} segments and \
         2..={MAX_SPHERE_DIVISIONS} rings, got {segments} and {rings}"
    );
    Ok(super::uv_sphere(segments, rings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu::GpuContext;

    fn file(path: &str, index: usize) -> AssetSource {
        AssetSource::File {
            path: PathBuf::from(path),
            index,
        }
    }

    #[test]
    fn parse_reads_builtins_and_files() -> anyhow::Result<()> {
        assert_eq!(
            AssetSource::parse("builtin://cube")?,
            AssetSource::Builtin("cube".into())
        );
        assert_eq!(
            AssetSource::parse("assets/test/cube.gltf")?,
            file("assets/test/cube.gltf", 0)
        );
        assert_eq!(
            AssetSource::parse("assets/test/cube.gltf#0")?,
            file("assets/test/cube.gltf", 0)
        );
        assert_eq!(
            AssetSource::parse("assets/x.gltf#2")?,
            file("assets/x.gltf", 2)
        );
        Ok(())
    }

    #[test]
    fn parse_and_format_round_trip() -> anyhow::Result<()> {
        for s in [
            "builtin://cube",
            "builtin://sphere-8x4",
            "assets/test/cube.gltf",
            "assets/test/cube.gltf#0",
            "assets/x.gltf#2",
            "/abs/x.glb#1",
            "odd#name.gltf#0",
        ] {
            let source = AssetSource::parse(s)?;
            let formatted = source.to_string();
            assert_eq!(
                AssetSource::parse(&formatted)?,
                source,
                "{s} -> {formatted}"
            );
            assert_eq!(formatted.parse::<AssetSource>()?, source);
        }
        // The canonical form drops a redundant `#0` and keeps the rest.
        assert_eq!(
            AssetSource::parse("assets/test/cube.gltf#0")?.to_string(),
            "assets/test/cube.gltf"
        );
        assert_eq!(file("assets/x.gltf", 2).to_string(), "assets/x.gltf#2");
        assert_eq!(file("odd#name.gltf", 0).to_string(), "odd#name.gltf#0");
        Ok(())
    }

    #[test]
    fn parse_reads_texture_sources() -> anyhow::Result<()> {
        let image = |path: &str| AssetSource::Image {
            path: PathBuf::from(path),
        };
        let gltf_image = |path: &str, index| AssetSource::GltfImage {
            path: PathBuf::from(path),
            index,
        };
        assert_eq!(AssetSource::parse("assets/c.png")?, image("assets/c.png"));
        assert_eq!(AssetSource::parse("c.JPG")?, image("c.JPG"));
        assert_eq!(AssetSource::parse("odd#name.jpeg")?, image("odd#name.jpeg"));
        assert_eq!(
            AssetSource::parse("assets/x.gltf#image=3")?,
            gltf_image("assets/x.gltf", 3)
        );
        assert_eq!(
            AssetSource::parse("a#b.glb#image=0")?,
            gltf_image("a#b.glb", 0)
        );
        // Not an image extension: still a mesh.
        assert_eq!(AssetSource::parse("x.png#1")?, file("x.png", 1));
        // Sounds (GE-036) are neither.
        let blip = AssetSource::parse("assets/sounds/blip.wav")?;
        assert!(blip.is_audio() && !blip.is_texture(), "{blip}");
        assert_eq!(blip.to_string(), "assets/sounds/blip.wav");
        for s in [
            "assets/c.png",
            "odd#name.jpeg",
            "assets/x.gltf#image=3",
            "a#b.glb#image=0",
        ] {
            let source = AssetSource::parse(s)?;
            assert!(source.is_texture(), "{s}");
            assert_eq!(source.to_string(), s);
        }
        assert!(!AssetSource::parse("builtin://cube")?.is_texture());
        assert!(!AssetSource::parse("x.gltf#1")?.is_texture());
        for bad in ["x.gltf#image=", "x.gltf#image=-1", "#image=0"] {
            assert!(AssetSource::parse(bad).is_err(), "{bad:?} should not parse");
        }
        Ok(())
    }

    #[test]
    fn textures_are_cached_by_source_and_kinds_do_not_mix() -> anyhow::Result<()> {
        let gpu = GpuContext::new(None)?;
        let mut state = crate::render::RenderState::new(
            &gpu.device,
            &gpu.queue,
            crate::gpu::OFFSCREEN_COLOR_FORMAT,
        );
        let mut assets = Assets::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")));
        let png = AssetSource::parse("assets/test/checker.png")?;
        let embedded = AssetSource::parse("assets/test/textured_cube.gltf#image=0")?;
        let (device, queue) = (&gpu.device, &gpu.queue);

        let first = assets.texture(device, queue, &mut state.textures, &png)?;
        assert_eq!(
            assets.texture(device, queue, &mut state.textures, &png)?,
            first
        );
        let second = assets.texture(device, queue, &mut state.textures, &embedded)?;
        assert_ne!(first, second);
        assert_eq!(state.textures.len(), 2);
        assert_eq!(state.textures.get(second).size(), (64, 64));
        assert_eq!(state.textures.source(second), Some(&embedded));

        let missing = AssetSource::parse("assets/test/textured_cube.gltf#image=4")?;
        let err = assets
            .texture(device, queue, &mut state.textures, &missing)
            .unwrap_err();
        assert!(format!("{err:#}").contains("out of range"), "{err:#}");

        // A mesh source is not a texture, and a texture source not a mesh.
        let cube = AssetSource::parse("builtin://cube")?;
        let err = assets
            .texture(device, queue, &mut state.textures, &cube)
            .unwrap_err();
        assert!(err.to_string().contains("not a texture"), "{err}");
        let err = assets.mesh(device, &mut state.registry, &png).unwrap_err();
        assert!(err.to_string().contains("not a mesh"), "{err}");
        assert!(state.registry.is_empty());

        // Images wgpu would reject fail with an error instead of a panic.
        let max = gpu.device.limits().max_texture_dimension_2d;
        for (width, height, bytes) in [(0, 4, 0), (max + 1, 1, 4 * (max as usize + 1)), (2, 2, 4)] {
            let image = ImageData {
                name: "bad".into(),
                width,
                height,
                rgba: vec![0; bytes],
            };
            let source = AssetSource::parse("bad.png")?;
            let result =
                assets.texture_from_data(device, queue, &mut state.textures, &source, &image);
            assert!(result.is_err(), "{width}x{height} with {bytes} bytes");
        }
        assert_eq!(state.textures.len(), 2);
        Ok(())
    }

    #[test]
    fn parse_rejects_bad_strings() {
        for s in [
            "",
            "builtin://",
            "builtin://cu be",
            "builtin://a/b",
            "assets/x.gltf#",
            "assets/x.gltf#-1",
            "assets/x.gltf#two",
            "#1",
            "http://example.com/x.gltf",
        ] {
            assert!(AssetSource::parse(s).is_err(), "{s:?} should not parse");
        }
    }

    #[test]
    fn builtin_names_resolve() -> anyhow::Result<()> {
        assert_eq!(builtin_mesh("cube")?.vertices.len(), 24);
        assert_eq!(builtin_mesh("sphere")?.vertices.len(), 33 * 17);
        assert_eq!(builtin_mesh(&sphere_name(8, 4))?.vertices.len(), 9 * 5);
        assert_eq!(sphere_name(32, 16), "sphere");
        for bad in [
            "torus",
            "sphere-8",
            "sphere-2x4",
            "sphere-8x1",
            "sphere-2000x4",
        ] {
            assert!(builtin_mesh(bad).is_err(), "{bad:?} should not resolve");
        }
        Ok(())
    }

    #[test]
    fn same_source_is_uploaded_once() -> anyhow::Result<()> {
        let gpu = GpuContext::new(None)?;
        let mut registry = MeshRegistry::new();
        let mut assets = Assets::new(PathBuf::new());
        let cube = AssetSource::parse("builtin://cube")?;
        let first = assets.mesh(&gpu.device, &mut registry, &cube)?;
        let second = assets.mesh(&gpu.device, &mut registry, &cube)?;
        assert_eq!(first, second);
        assert_eq!(registry.len(), 1);
        assert_eq!(assets.source_of(first), Some(&cube));
        assert_eq!(registry.source(first), Some(&cube));

        let sphere = assets.mesh(
            &gpu.device,
            &mut registry,
            &AssetSource::parse("builtin://sphere")?,
        )?;
        assert_ne!(sphere, first);
        assert_eq!(registry.len(), 2);
        assert_eq!(assets.source_of(MeshHandle(99)), None);
        Ok(())
    }

    #[test]
    fn file_sources_resolve_against_root() -> anyhow::Result<()> {
        let gpu = GpuContext::new(None)?;
        let mut registry = MeshRegistry::new();
        let mut assets = Assets::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")));
        let source = AssetSource::parse("assets/test/cube.gltf")?;
        let handle = assets.mesh(&gpu.device, &mut registry, &source)?;
        assert_eq!(registry.get(handle).index_count, 36);
        // `#0` is the same source, so it is not uploaded again.
        let again = AssetSource::parse("assets/test/cube.gltf#0")?;
        assert_eq!(assets.mesh(&gpu.device, &mut registry, &again)?, handle);
        assert_eq!(registry.len(), 1);

        let missing = AssetSource::parse("assets/test/cube.gltf#5")?;
        let err = assets
            .mesh(&gpu.device, &mut registry, &missing)
            .unwrap_err();
        assert!(format!("{err:#}").contains("out of range"), "{err:#}");

        // The same relative path from a different root is not found.
        let mut elsewhere = Assets::new(PathBuf::from("/definitely/not/here"));
        let err = elsewhere
            .mesh(&gpu.device, &mut registry, &source)
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("/definitely/not/here/assets/test/cube.gltf"),
            "{err:#}"
        );
        Ok(())
    }
}
