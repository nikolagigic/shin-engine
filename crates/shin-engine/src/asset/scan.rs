//! Listing what a project contains (GE-032): [`scan`] walks a project's
//! `assets/`, `scenes/`, and `scripts/` directories and returns one
//! [`AssetEntry`] per usable thing in them, and the `asset.list` tool
//! returns the same entries as JSON so agents can see them too.
//!
//! | File                      | Entries                                              |
//! |---------------------------|------------------------------------------------------|
//! | `assets/**/*.gltf`, `.glb` | one [`AssetKind::Mesh`] per triangle primitive (`path#index`) and one [`AssetKind::Texture`] per image (`path#image=index`) |
//! | `assets/**/*.png`, `.jpg`, `.jpeg` | one [`AssetKind::Texture`]                   |
//! | `assets/**/*.ogg`, `.wav` | one [`AssetKind::Audio`]                             |
//! | `scenes/**/*.scene.ron`   | one [`AssetKind::Scene`]                             |
//! | `scripts/**/*.rhai`       | one [`AssetKind::Script`]                            |
//!
//! Other files are ignored. A glTF file that does not parse is skipped
//! with a warning, so one broken file does not hide the rest. Only the
//! three directories are walked, so `target/` and `src/` never appear.
//! Entries are sorted by path, then by index.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{AssetSource, Assets};
use crate::headless::Headless;
use crate::project::ProjectRoot;
use crate::tool::ToolOutput;

/// The directories [`scan`] walks, relative to the project root.
pub const SCANNED_DIRS: [&str; 3] = ["assets", "scenes", "scripts"];

/// Suffix of a scene file.
const SCENE_SUFFIX: &str = ".scene.ron";

/// What an [`AssetEntry`] is, which decides what can be done with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AssetKind {
    /// A triangle primitive: `asset.load`, `entity.spawn`,
    /// `entity.set_mesh`.
    Mesh,
    /// An image or a glTF image: `asset.load`, `entity.set_texture`.
    Texture,
    /// A `.scene.ron` file: `scene.load`.
    Scene,
    /// A `.rhai` file: `script.attach`.
    Script,
    /// An `.ogg` or `.wav` file: `audio.play`, `entity.set_audio`.
    Audio,
}

impl AssetKind {
    /// The lower-case name tools use: `mesh`, `texture`, `scene`, `script`,
    /// `audio`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mesh => "mesh",
            Self::Texture => "texture",
            Self::Scene => "scene",
            Self::Script => "script",
            Self::Audio => "audio",
        }
    }
}

impl fmt::Display for AssetKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How an entry is named to the tool that uses it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntrySource {
    /// A mesh, texture, or sound source for `asset.load`,
    /// `entity.set_mesh`, `entity.set_texture`, `audio.play`, and
    /// `entity.set_audio`.
    Asset(AssetSource),
    /// A scene file path, relative to the project, for `scene.load`.
    Scene(PathBuf),
    /// A script path, relative to the project, for `script.attach`.
    Script(PathBuf),
}

impl fmt::Display for EntrySource {
    /// The string the tools take: the [`AssetSource`] string form or the
    /// relative path.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Asset(source) => source.fmt(f),
            Self::Scene(path) | Self::Script(path) => path.display().fmt(f),
        }
    }
}

/// One thing a project contains that a tool can use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetEntry {
    /// What to pass to the tool that uses it.
    pub source: EntrySource,
    /// What it is.
    pub kind: AssetKind,
    /// The file it comes from, relative to the project root, e.g.
    /// `assets/cube.gltf`. Several entries share one file when a glTF has
    /// several primitives or images.
    pub relative_path: PathBuf,
}

impl AssetEntry {
    /// `{ "source", "kind", "relative_path" }`, as `asset.list` returns it.
    pub fn to_json(&self) -> Value {
        json!({
            "source": self.source.to_string(),
            "kind": self.kind.as_str(),
            "relative_path": self.relative_path.display().to_string(),
        })
    }
}

/// Every usable file under `root`'s [`SCANNED_DIRS`], sorted by path then
/// index: glTF primitives and images, image files, sounds, scene files,
/// and scripts (see [`AssetKind`]). Directories that do not exist are
/// skipped, so a project without `scripts/` scans fine.
///
/// Fails if `root` itself or a directory under it cannot be read.
pub fn scan(root: &Path) -> anyhow::Result<Vec<AssetEntry>> {
    anyhow::ensure!(
        root.is_dir(),
        "cannot scan {}: not a directory",
        root.display()
    );
    let mut files = Vec::new();
    for dir in SCANNED_DIRS {
        let dir = root.join(dir);
        if dir.is_dir() {
            collect_files(&dir, &mut files)?;
        }
    }
    files.sort();
    let mut entries = Vec::new();
    for file in files {
        let relative = file
            .strip_prefix(root)
            .with_context(|| format!("{} is not under {}", file.display(), root.display()))?;
        entries.extend(entries_for(root, relative));
    }
    Ok(entries)
}

/// Appends every file under `dir`, recursively.
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    let listing = fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))?;
    for entry in listing {
        let path = entry
            .with_context(|| format!("reading {}", dir.display()))?
            .path();
        if path.is_dir() {
            collect_files(&path, out)?;
        } else if path.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

/// The entries for the file at `relative` (relative to `root`), by its
/// extension and top-level directory; empty for files that are not assets.
fn entries_for(root: &Path, relative: &Path) -> Vec<AssetEntry> {
    let first_dir = relative
        .components()
        .next()
        .and_then(|c| c.as_os_str().to_str());
    let name = relative
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let extension = relative
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let entry = |source, kind| AssetEntry {
        source,
        kind,
        relative_path: relative.to_path_buf(),
    };
    match (first_dir, extension.as_str()) {
        (Some("assets"), "gltf" | "glb") => gltf_entries(root, relative),
        (Some("assets"), "png" | "jpg" | "jpeg") => vec![entry(
            EntrySource::Asset(AssetSource::Image {
                path: relative.to_path_buf(),
            }),
            AssetKind::Texture,
        )],
        (Some("assets"), "ogg" | "wav") => vec![entry(
            EntrySource::Asset(AssetSource::Audio {
                path: relative.to_path_buf(),
            }),
            AssetKind::Audio,
        )],
        (Some("scenes"), _) if name.to_ascii_lowercase().ends_with(SCENE_SUFFIX) => vec![entry(
            EntrySource::Scene(relative.to_path_buf()),
            AssetKind::Scene,
        )],
        (Some("scripts"), "rhai") => vec![entry(
            EntrySource::Script(relative.to_path_buf()),
            AssetKind::Script,
        )],
        _ => Vec::new(),
    }
}

/// One mesh entry per triangle primitive and one texture entry per image
/// of the glTF file at `relative`, indexed as [`super::load_gltf`]
/// flattens them. A file that does not parse gives no entries and a
/// warning.
fn gltf_entries(root: &Path, relative: &Path) -> Vec<AssetEntry> {
    let path = root.join(relative);
    // Reads the document only: buffers and images are not loaded.
    let document = match gltf::Gltf::open(&path) {
        Ok(gltf) => gltf.document,
        Err(err) => {
            log::warn!("skipping {}: {err}", path.display());
            return Vec::new();
        }
    };
    let primitives = document
        .meshes()
        .flat_map(|mesh| mesh.primitives())
        .filter(|primitive| primitive.mode() == gltf::mesh::Mode::Triangles)
        .count();
    let images = document.images().len();
    let mut entries = Vec::with_capacity(primitives + images);
    for index in 0..primitives {
        entries.push(AssetEntry {
            source: EntrySource::Asset(AssetSource::File {
                path: relative.to_path_buf(),
                index,
            }),
            kind: AssetKind::Mesh,
            relative_path: relative.to_path_buf(),
        });
    }
    for index in 0..images {
        entries.push(AssetEntry {
            source: EntrySource::Asset(AssetSource::GltfImage {
                path: relative.to_path_buf(),
                index,
            }),
            kind: AssetKind::Texture,
            relative_path: relative.to_path_buf(),
        });
    }
    entries
}

/// `asset.list` takes no arguments.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ListArgs {}

/// `asset.list`: [`scan`] of the open project's root ([`ProjectRoot`]), or
/// of the [`Assets`] root when no project is open, as a JSON array of
/// [`AssetEntry::to_json`].
pub(super) fn asset_list(engine: &mut Headless, _: ListArgs) -> anyhow::Result<ToolOutput> {
    let root = match engine.resources.get::<ProjectRoot>() {
        Some(root) => root.0.clone(),
        None => engine
            .resources
            .get::<Assets>()
            .context("no Assets resource")?
            .root()
            .to_path_buf(),
    };
    let entries = scan(&root)?;
    Ok(ToolOutput::Json(Value::Array(
        entries.iter().map(AssetEntry::to_json).collect(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{self, NewProjectOptions};
    use crate::tool::ToolRegistry;

    /// The engine's test assets directory.
    fn test_assets() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/test")
    }

    /// A fresh scaffolded project in a temp dir named after `label`;
    /// returns the temp root (to delete) and the project directory.
    fn demo_project(label: &str) -> anyhow::Result<(PathBuf, PathBuf)> {
        let root =
            std::env::temp_dir().join(format!("shin-asset-scan-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = root.join("demo");
        project::create(&NewProjectOptions {
            name: "demo".to_string(),
            dir: dir.clone(),
            engine: Default::default(),
        })?;
        // Something that looks like a build directory, which must not show.
        fs::create_dir_all(dir.join("target/debug"))?;
        fs::write(dir.join("target/debug/stray.gltf"), "{}")?;
        Ok((root, dir))
    }

    /// `(source, kind, relative_path)` strings of `entries`.
    fn rows(entries: &[AssetEntry]) -> Vec<(String, &'static str, String)> {
        entries
            .iter()
            .map(|e| {
                (
                    e.source.to_string(),
                    e.kind.as_str(),
                    e.relative_path.display().to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn scan_lists_a_scaffolded_project() -> anyhow::Result<()> {
        let (root, dir) = demo_project("scaffold")?;
        let entries = scan(&dir)?;
        assert_eq!(
            rows(&entries),
            [
                (
                    "assets/cube.gltf".to_string(),
                    "mesh",
                    "assets/cube.gltf".to_string()
                ),
                (
                    "assets/sounds/blip.wav".to_string(),
                    "audio",
                    "assets/sounds/blip.wav".to_string()
                ),
                (
                    "scenes/main.scene.ron".to_string(),
                    "scene",
                    "scenes/main.scene.ron".to_string()
                ),
                (
                    "scripts/moon.rhai".to_string(),
                    "script",
                    "scripts/moon.rhai".to_string()
                ),
                (
                    "scripts/player.rhai".to_string(),
                    "script",
                    "scripts/player.rhai".to_string()
                ),
                (
                    "scripts/server.rhai".to_string(),
                    "script",
                    "scripts/server.rhai".to_string()
                ),
            ],
            "the starter cube (one primitive, no images), the moon's blip, the \
             entry scene, and the starter moon, player, and server scripts; cube.bin, src/, \
             and target/ do not show"
        );
        assert_eq!(
            entries[0].source,
            EntrySource::Asset(AssetSource::File {
                path: PathBuf::from("assets/cube.gltf"),
                index: 0,
            })
        );
        assert_eq!(
            entries[1].source,
            EntrySource::Asset(AssetSource::Audio {
                path: PathBuf::from("assets/sounds/blip.wav"),
            })
        );
        assert_eq!(
            entries[2].source,
            EntrySource::Scene(PathBuf::from("scenes/main.scene.ron"))
        );
        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn scan_expands_gltf_images_and_finds_textures_scripts_and_subdirs() -> anyhow::Result<()> {
        let (root, dir) = demo_project("kinds")?;
        let assets = test_assets();
        // The textured cube: one primitive and one embedded image, and its
        // sidecar buffer (not an asset) under a subdirectory.
        fs::create_dir_all(dir.join("assets/props"))?;
        fs::copy(
            assets.join("textured_cube.gltf"),
            dir.join("assets/props/crate.gltf"),
        )?;
        fs::copy(assets.join("cube.bin"), dir.join("assets/props/cube.bin"))?;
        fs::copy(assets.join("checker.png"), dir.join("assets/Checker.PNG"))?;
        // Sounds in any case; an audio file outside assets/ does not count.
        fs::write(dir.join("assets/sounds/Theme.OGG"), "")?;
        fs::write(dir.join("scripts/stray.wav"), "")?;
        fs::create_dir_all(dir.join("scripts"))?;
        fs::write(dir.join("scripts/spin.rhai"), "fn update(entity, dt) {}")?;
        fs::write(dir.join("scripts/notes.txt"), "not a script")?;
        fs::write(dir.join("scenes/other.scene.ron"), "(version: 1)")?;
        fs::write(dir.join("scenes/README.md"), "not a scene")?;
        // A .rhai outside scripts/ and a .png outside assets/ do not count.
        fs::write(dir.join("assets/loose.rhai"), "")?;
        fs::write(dir.join("scenes/picture.png"), "")?;

        let entries = scan(&dir)?;
        assert_eq!(
            rows(&entries),
            [
                (
                    "assets/Checker.PNG".to_string(),
                    "texture",
                    "assets/Checker.PNG".to_string()
                ),
                (
                    "assets/cube.gltf".to_string(),
                    "mesh",
                    "assets/cube.gltf".to_string()
                ),
                (
                    "assets/props/crate.gltf".to_string(),
                    "mesh",
                    "assets/props/crate.gltf".to_string()
                ),
                (
                    "assets/props/crate.gltf#image=0".to_string(),
                    "texture",
                    "assets/props/crate.gltf".to_string()
                ),
                (
                    "assets/sounds/Theme.OGG".to_string(),
                    "audio",
                    "assets/sounds/Theme.OGG".to_string()
                ),
                (
                    "assets/sounds/blip.wav".to_string(),
                    "audio",
                    "assets/sounds/blip.wav".to_string()
                ),
                (
                    "scenes/main.scene.ron".to_string(),
                    "scene",
                    "scenes/main.scene.ron".to_string()
                ),
                (
                    "scenes/other.scene.ron".to_string(),
                    "scene",
                    "scenes/other.scene.ron".to_string()
                ),
                (
                    "scripts/moon.rhai".to_string(),
                    "script",
                    "scripts/moon.rhai".to_string()
                ),
                (
                    "scripts/player.rhai".to_string(),
                    "script",
                    "scripts/player.rhai".to_string()
                ),
                (
                    "scripts/server.rhai".to_string(),
                    "script",
                    "scripts/server.rhai".to_string()
                ),
                (
                    "scripts/spin.rhai".to_string(),
                    "script",
                    "scripts/spin.rhai".to_string()
                ),
            ]
        );
        // Every mesh, texture, and sound source parses back to itself, so
        // the strings can be handed to asset.load as they are.
        for entry in &entries {
            if let EntrySource::Asset(source) = &entry.source {
                assert_eq!(&AssetSource::parse(&source.to_string())?, source);
            }
        }
        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn scan_skips_a_broken_gltf_and_missing_dirs() -> anyhow::Result<()> {
        let (root, dir) = demo_project("broken")?;
        fs::write(dir.join("assets/broken.gltf"), "not json")?;
        let entries = scan(&dir)?;
        assert_eq!(
            rows(&entries).len(),
            6,
            "the broken file is skipped (cube, blip, scene, and the three scripts \
             remain): {entries:?}"
        );

        // A root with none of the directories scans to nothing.
        let empty = root.join("empty");
        fs::create_dir_all(&empty)?;
        assert!(scan(&empty)?.is_empty());
        assert!(scan(&root.join("missing")).is_err());
        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn asset_list_tool_returns_the_scan_of_the_open_project() -> anyhow::Result<()> {
        let (root, dir) = demo_project("tool")?;
        let reg = ToolRegistry::with_engine_tools();
        assert!(
            reg.list().iter().any(|t| t.name == "asset.list"),
            "asset.list is registered"
        );
        let mut engine = Headless::open_project(&dir)?;
        let ToolOutput::Json(listed) = reg.call(&mut engine, "asset.list", json!({}))? else {
            anyhow::bail!("asset.list returned no JSON");
        };
        let expected: Vec<Value> = scan(&dir)?.iter().map(AssetEntry::to_json).collect();
        assert_eq!(listed, Value::Array(expected));
        assert_eq!(
            listed[0],
            json!({
                "source": "assets/cube.gltf",
                "kind": "mesh",
                "relative_path": "assets/cube.gltf",
            })
        );

        // Without a project, the Assets root is scanned.
        let mut bare = Headless::new(8, 8)?;
        bare.resources.insert(Assets::new(dir.clone()));
        let ToolOutput::Json(again) = reg.call(&mut bare, "asset.list", json!({}))? else {
            anyhow::bail!("asset.list returned no JSON");
        };
        assert_eq!(again, listed);
        let _ = fs::remove_dir_all(&root);
        Ok(())
    }
}
