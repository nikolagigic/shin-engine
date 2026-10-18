//! The Assets panel (GE-032): what the project contains, grouped by kind,
//! with a button per entry to use it, and an Import row that copies a file
//! into `assets/`.
//!
//! The entries come from [`shin::asset::scan`] of the project directory.
//! [`AssetBrowser::sync`] rescans when asked to ([`AssetBrowser::invalidate`]:
//! the panel was just opened, or a file was imported) and otherwise checks
//! every [`RESCAN_INTERVAL`], while the panel is drawn, whether anything
//! under `assets/`, `scenes/`, or `scripts/` changed (paths and
//! modification times), rescanning only then.
//!
//! Like the other panels, this one makes no edits itself: each button
//! pushes an [`Action`] that [`crate::editor::EditorCore`] runs after the UI
//! pass. Mesh entries spawn (`entity.spawn` at the orbit target) or are
//! assigned to the selected entity (`entity.set_mesh`); texture entries
//! are assigned (`entity.set_texture`); scripts are attached
//! (`script.attach`); scenes are opened (`scene.load`).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context as _;
use egui::Ui;
use shin::asset::{AssetEntry, AssetKind, SCANNED_DIRS, scan};
use shin::ecs::Entity;

use crate::inspector::Edit;
use crate::ui::Action;

/// How often the panel checks the project directories for changes while
/// it is shown.
pub const RESCAN_INTERVAL: Duration = Duration::from_secs(2);

/// The directory Import copies files into, relative to the project.
pub const IMPORT_DIR: &str = "assets";

/// Every path under the scanned directories with its modification time:
/// when it is unchanged, nothing was added, removed, or rewritten.
type Fingerprint = Vec<(PathBuf, Option<SystemTime>)>;

/// The Assets panel's state.
#[derive(Debug, Default)]
pub struct AssetBrowser {
    entries: Vec<AssetEntry>,
    /// Why the last scan failed, shown in the panel.
    error: Option<String>,
    /// The text in the filter box: entries whose source or path contains
    /// it (ignoring case) are shown.
    pub filter: String,
    /// The text in the Import field: an absolute path to copy into
    /// `assets/`.
    pub import_path: String,
    /// Whether the next [`Self::sync`] must rescan.
    dirty: bool,
    /// When the directories were last scanned or checked.
    last_check: Option<Instant>,
    fingerprint: Fingerprint,
}

impl AssetBrowser {
    /// The entries of the last scan, as [`scan`] ordered them.
    pub fn entries(&self) -> &[AssetEntry] {
        &self.entries
    }

    /// Makes the next [`Self::sync`] rescan whatever the time.
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// Brings the entries up to date with the project in `root`, as of
    /// `now`: rescans if [`Self::invalidate`]d or never scanned; otherwise,
    /// once [`RESCAN_INTERVAL`] has passed since the last check, rescans if
    /// the directories' fingerprint changed. Returns whether a scan ran.
    pub fn sync(&mut self, root: &Path, now: Instant) -> bool {
        let due = self
            .last_check
            .is_none_or(|last| now.duration_since(last) >= RESCAN_INTERVAL);
        if !self.dirty && !due {
            return false;
        }
        let fingerprint = fingerprint(root);
        if !self.dirty && self.last_check.is_some() && fingerprint == self.fingerprint {
            self.last_check = Some(now);
            return false;
        }
        match scan(root) {
            Ok(entries) => {
                self.entries = entries;
                self.error = None;
            }
            Err(err) => self.error = Some(format!("{err:#}")),
        }
        self.fingerprint = fingerprint;
        self.last_check = Some(now);
        self.dirty = false;
        true
    }

    /// Draws the panel for the project in `root`: the filter box, the
    /// Import row, then the entries grouped by kind, each with its
    /// buttons. Buttons that edit the scene are disabled unless `editing`
    /// (not playing); the ones that need a selection are disabled without
    /// one.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        root: &Path,
        selection: Option<Entity>,
        editing: bool,
        actions: &mut Vec<Action>,
    ) {
        self.sync(root, Instant::now());
        ui.horizontal(|ui| {
            ui.label("Filter");
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("name")
                    .desired_width(f32::INFINITY),
            );
        });
        self.import_row(ui, actions);
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        ui.separator();

        let filter = self.filter.trim().to_ascii_lowercase();
        let mut shown = 0;
        for kind in [
            AssetKind::Mesh,
            AssetKind::Texture,
            AssetKind::Scene,
            AssetKind::Script,
        ] {
            let rows: Vec<&AssetEntry> = self
                .entries()
                .iter()
                .filter(|entry| entry.kind == kind && matches_filter(entry, &filter))
                .collect();
            if rows.is_empty() {
                continue;
            }
            shown += rows.len();
            egui::CollapsingHeader::new(format!("{} ({})", group_title(kind), rows.len()))
                .default_open(true)
                .show(ui, |ui| {
                    for entry in rows {
                        entry_row(ui, entry, selection, editing, actions);
                    }
                });
        }
        if shown == 0 {
            ui.weak(if self.entries.is_empty() {
                "No assets"
            } else {
                "No matches"
            });
        }
    }

    /// The Import field and button: an absolute path, copied into
    /// `assets/` by [`Action::Import`]. Enter in the field does the same.
    fn import_row(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.import_path)
                    .hint_text("/absolute/path/to/file.png")
                    .desired_width(f32::INFINITY),
            );
            let submitted = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let path = self.import_path.trim().to_string();
            if (ui.button("Import…").clicked() || submitted) && !path.is_empty() {
                actions.push(Action::Import(path));
            }
        });
    }
}

/// Whether `entry`'s source or path contains `filter` (already lower
/// case); everything matches an empty filter.
fn matches_filter(entry: &AssetEntry, filter: &str) -> bool {
    filter.is_empty()
        || entry
            .source
            .to_string()
            .to_ascii_lowercase()
            .contains(filter)
        || entry
            .relative_path
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains(filter)
}

/// The heading of a kind's group.
fn group_title(kind: AssetKind) -> &'static str {
    match kind {
        AssetKind::Mesh => "Meshes",
        AssetKind::Texture => "Textures",
        AssetKind::Scene => "Scenes",
        AssetKind::Script => "Scripts",
    }
}

/// One entry: its source string (the path on hover) and its buttons.
fn entry_row(
    ui: &mut Ui,
    entry: &AssetEntry,
    selection: Option<Entity>,
    editing: bool,
    actions: &mut Vec<Action>,
) {
    let source = entry.source.to_string();
    ui.horizontal(|ui| {
        ui.label(&source)
            .on_hover_text(entry.relative_path.display().to_string());
        let with_selection = editing && selection.is_some();
        match entry.kind {
            AssetKind::Mesh => {
                if ui
                    .add_enabled(editing, egui::Button::new("Spawn"))
                    .on_hover_text("Add an entity drawing this mesh at the orbit target")
                    .clicked()
                {
                    actions.push(Action::Spawn(source.clone()));
                }
                if ui
                    .add_enabled(with_selection, egui::Button::new("Assign"))
                    .on_hover_text("Make the selected entity draw this mesh")
                    .clicked()
                    && let Some(entity) = selection
                {
                    actions.push(Action::Edit(Edit::Mesh(entity, source.clone())));
                }
            }
            AssetKind::Texture => {
                if ui
                    .add_enabled(with_selection, egui::Button::new("Assign"))
                    .on_hover_text("Give the selected entity this base-colour texture")
                    .clicked()
                    && let Some(entity) = selection
                {
                    actions.push(Action::Edit(Edit::Texture(entity, Some(source.clone()))));
                }
            }
            AssetKind::Script => {
                if ui
                    .add_enabled(with_selection, egui::Button::new("Attach"))
                    .on_hover_text("Attach this script to the selected entity")
                    .clicked()
                    && let Some(entity) = selection
                {
                    actions.push(Action::Edit(Edit::Script(entity, source.clone())));
                }
            }
            AssetKind::Scene => {
                if ui
                    .add_enabled(editing, egui::Button::new("Open"))
                    .on_hover_text(
                        "Replace the scene with this file; it becomes the one Save writes",
                    )
                    .clicked()
                {
                    actions.push(Action::OpenScene(source.clone()));
                }
            }
        }
    });
}

/// Every directory and file under `root`'s [`SCANNED_DIRS`], with its
/// modification time, sorted. Directories that cannot be read contribute
/// nothing.
fn fingerprint(root: &Path) -> Fingerprint {
    fn walk(dir: &Path, out: &mut Fingerprint) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let modified = entry.metadata().and_then(|m| m.modified()).ok();
            if path.is_dir() {
                walk(&path, out);
            }
            out.push((path, modified));
        }
    }
    let mut out = Vec::new();
    for dir in SCANNED_DIRS {
        let dir = root.join(dir);
        let modified = fs::metadata(&dir).and_then(|m| m.modified()).ok();
        out.push((dir.clone(), modified));
        walk(&dir, &mut out);
    }
    out.sort();
    out
}

/// Copies the file at `file` (an absolute path) into `root`'s
/// [`IMPORT_DIR`] under its own name, creating the directory if needed,
/// and returns the new path relative to `root`.
///
/// Fails if `file` is not absolute, is not a file, or a file with that
/// name already exists in [`IMPORT_DIR`] (nothing is overwritten).
pub fn import(root: &Path, file: &Path) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        file.is_absolute(),
        "import needs an absolute path, got `{}`",
        file.display()
    );
    anyhow::ensure!(file.is_file(), "`{}` is not a file", file.display());
    let name = file
        .file_name()
        .with_context(|| format!("`{}` has no file name", file.display()))?;
    let relative = Path::new(IMPORT_DIR).join(name);
    let dest = root.join(&relative);
    anyhow::ensure!(
        !dest.exists(),
        "`{}` already exists in the project; rename the file first",
        relative.display()
    );
    let dir = root.join(IMPORT_DIR);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    fs::copy(file, &dest)
        .with_context(|| format!("copying {} to {}", file.display(), dest.display()))?;
    Ok(relative)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shin::project::{self, NewProjectOptions};

    /// The engine's test assets directory.
    fn test_assets() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../shin-engine/assets/test")
    }

    /// A fresh scaffolded project in a temp dir named after `label`;
    /// returns the temp root (to delete) and the project directory.
    fn demo_project(label: &str) -> anyhow::Result<(PathBuf, PathBuf)> {
        let root = std::env::temp_dir().join(format!(
            "shin-editor-assets-{label}-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let dir = root.join("demo");
        project::create(&NewProjectOptions {
            name: "demo".to_string(),
            dir: dir.clone(),
            engine: Default::default(),
        })?;
        Ok((root, dir))
    }

    /// The source strings of `browser`'s entries.
    fn sources(browser: &AssetBrowser) -> Vec<String> {
        browser
            .entries()
            .iter()
            .map(|e| e.source.to_string())
            .collect()
    }

    /// The acceptance criterion: a file added to `assets/` is listed
    /// within [`RESCAN_INTERVAL`] of the next sync, without rescanning
    /// before that or when nothing changed.
    #[test]
    fn assets_added_to_the_project_show_within_the_interval() -> anyhow::Result<()> {
        let (root, dir) = demo_project("interval")?;
        let mut browser = AssetBrowser::default();
        let t0 = Instant::now();
        assert!(browser.sync(&dir, t0), "the first sync scans");
        assert_eq!(
            sources(&browser),
            ["assets/cube.gltf", "scenes/main.scene.ron"]
        );

        fs::copy(
            test_assets().join("checker.png"),
            dir.join("assets/checker.png"),
        )?;
        assert!(
            !browser.sync(&dir, t0 + Duration::from_millis(500)),
            "not checked before the interval"
        );
        assert_eq!(sources(&browser).len(), 2);
        assert!(
            browser.sync(&dir, t0 + RESCAN_INTERVAL),
            "checked at the interval, and the directory changed"
        );
        assert_eq!(
            sources(&browser),
            [
                "assets/checker.png",
                "assets/cube.gltf",
                "scenes/main.scene.ron"
            ]
        );
        assert!(
            !browser.sync(&dir, t0 + 2 * RESCAN_INTERVAL),
            "nothing changed: no rescan"
        );

        // Deleting is noticed too, and an invalidated browser rescans at
        // once.
        fs::remove_file(dir.join("assets/checker.png"))?;
        browser.invalidate();
        assert!(browser.sync(&dir, t0 + 2 * RESCAN_INTERVAL + Duration::from_millis(1)));
        assert_eq!(sources(&browser).len(), 2);
        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn assets_scan_errors_are_kept_for_the_panel() -> anyhow::Result<()> {
        let mut browser = AssetBrowser::default();
        let missing = std::env::temp_dir().join("shin-editor-assets-missing-never-created");
        browser.sync(&missing, Instant::now());
        let error = browser.error.as_deref().expect("the scan error is kept");
        assert!(error.contains("not a directory"), "{error}");
        assert!(browser.entries().is_empty());
        Ok(())
    }

    #[test]
    fn assets_import_copies_into_the_assets_dir_once() -> anyhow::Result<()> {
        let (root, dir) = demo_project("import")?;
        let png = test_assets().join("checker.png").canonicalize()?;
        assert_eq!(import(&dir, &png)?, Path::new("assets/checker.png"));
        assert_eq!(
            fs::read(dir.join("assets/checker.png"))?,
            fs::read(&png)?,
            "the bytes are copied"
        );
        let err = import(&dir, &png).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");

        let err = import(&dir, Path::new("relative/checker.png")).unwrap_err();
        assert!(err.to_string().contains("absolute"), "{err}");
        let err = import(&dir, &test_assets().canonicalize()?).unwrap_err();
        assert!(err.to_string().contains("not a file"), "{err}");
        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn assets_filter_matches_source_and_path_ignoring_case() -> anyhow::Result<()> {
        let (root, dir) = demo_project("filter")?;
        let mut browser = AssetBrowser::default();
        browser.sync(&dir, Instant::now());
        let entry = &browser.entries()[0];
        assert!(matches_filter(entry, ""));
        assert!(matches_filter(entry, "cube"));
        assert!(matches_filter(entry, "assets/"));
        assert!(!matches_filter(entry, "sphere"));
        assert_eq!(group_title(entry.kind), "Meshes");
        let _ = fs::remove_dir_all(&root);
        Ok(())
    }
}
