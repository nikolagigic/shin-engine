//! The editor's UI: File, Edit, and View menus above an `egui_dock` area
//! with seven panels: Viewport (centre), Scene and Assets (left), Inspector
//! and History (right), and Log and Console (below the viewport).
//!
//! The Viewport shows the scene texture and turns pointer input into
//! orbit-camera moves. The Scene panel shows entities as a collapsible tree
//! (GE-031; click to select, drag a row onto another to reparent, drop it
//! below the tree or use the right-click menu to unparent) and adds and
//! deletes them; "+" adds a child of the selection when there is one. The
//! Inspector ([`crate::inspector`]) edits the selected one and shows the
//! scene's own camera, with buttons to move the viewport camera to it and
//! to set it from the viewport (GE-030); the Assets panel
//! ([`crate::assets`]) lists the project's files and uses them. Edits are
//! not made here: the UI pushes [`Action`]s that
//! [`crate::editor::EditorCore`] runs after the UI pass.
//!
//! Keys, while no text field is being typed in: Ctrl+Z (Cmd+Z on macOS)
//! undoes and Ctrl+Shift+Z redoes (also in an empty console), Delete or
//! Backspace deletes the selected entity, and Escape clears the selection.
//!
//! The menu bar ends in the play toolbar (GE-024): Play, Pause, Stop, and
//! the simulated time. While playing, editing is disabled: the Inspector,
//! the Scene panel's add and delete controls, undo and redo, the Delete
//! key, and File > Open and Save are greyed out or ignored (entities can
//! still be selected).

use std::collections::HashMap;
use std::path::Path;

use egui::collapsing_header::CollapsingState;
use egui::{
    Color32, Key, KeyboardShortcut, Modifiers, PointerButton, Rect, Sense, Stroke, StrokeKind,
    TextureId, Ui, WidgetText, pos2,
};
use egui_dock::{DockArea, DockState, NodeIndex, Style, TabViewer};
use serde_json::Value;
use shin::command::History;
use shin::ecs::{Entity, Name, child_map, parent_of};
use shin::headless::Headless;
use shin::math::{Transform, Vec2};
use shin::scene::TransformDesc;

use crate::assets::AssetBrowser;
use crate::edit::entity_id;
use crate::inspector::{Edit, Inspector};
use crate::log_sink::LogBuffer;
use crate::orbit::OrbitCamera;

/// One dockable panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tab {
    /// The live scene, rendered offscreen and shown as an image.
    Viewport,
    /// The entities in the world.
    Scene,
    /// The project's meshes, textures, scenes, and scripts (GE-032).
    Assets,
    /// Properties of the selected entity.
    Inspector,
    /// The most recent log lines.
    Log,
    /// The command history, oldest first (read-only).
    History,
    /// A one-line prompt that runs tools: `<tool.name> [json args]`.
    Console,
}

/// The panels the View menu can hide and show; the Viewport always stays.
pub const CLOSEABLE_TABS: [Tab; 6] = [
    Tab::Scene,
    Tab::Assets,
    Tab::Inspector,
    Tab::History,
    Tab::Log,
    Tab::Console,
];

/// The selected entity, if any (one at a time).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection(pub Option<Entity>);

/// The drag-and-drop payload of a Scene panel row: the entity being
/// dragged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DraggedEntity(pub Entity);

/// The built-in meshes the Scene panel's "+ Cube" and "+ Sphere" add.
pub const ADD_BUTTONS: [(&str, &str); 2] = [
    ("+ Cube", "builtin://cube"),
    ("+ Sphere", "builtin://sphere"),
];

/// Something the UI asks [`crate::editor::EditorCore`] to do after the UI
/// pass, when the engine can be borrowed mutably.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Undo the most recent edit.
    Undo,
    /// Redo the most recently undone edit.
    Redo,
    /// Run a console line.
    Console(String),
    /// Add an entity drawing the mesh at this asset source at the orbit
    /// target, as a child of the selected entity if there is one, and
    /// select it.
    Spawn(String),
    /// Make `parent` the parent of `entity`, or make `entity` a root with
    /// `None`, keeping its world transform (GE-031).
    Reparent {
        /// The entity that moves in the tree.
        entity: Entity,
        /// Its new parent; `None` for a root.
        parent: Option<Entity>,
    },
    /// Delete this entity.
    Despawn(Entity),
    /// Show this transform on the entity without recording it (a drag in
    /// progress).
    PreviewTransform(Entity, TransformDesc),
    /// Show this colour on the entity without recording it.
    PreviewColor(Entity, [f32; 4]),
    /// Record an inspector change.
    Edit(Edit),
    /// File > Open: load the project's entry scene.
    Open,
    /// Assets panel Open: load this project-relative scene file and make
    /// it the one Save writes.
    OpenScene(String),
    /// File > Save: save to the current scene path.
    Save,
    /// File > Save As: save to this project-relative path and make it the
    /// current one.
    SaveAs(String),
    /// Assets panel Import: copy the file at this absolute path into the
    /// project's `assets/` and rescan.
    Import(String),
    /// Toolbar Play: snapshot the scene and start (or resume) playing.
    Play,
    /// Toolbar Pause: toggle pause while playing.
    Pause,
    /// Toolbar Stop: restore the scene from the play snapshot.
    Stop,
    /// Inspector "View from scene camera": move the viewport's orbit camera
    /// to the scene camera's placement. Not an edit.
    ViewFromSceneCamera,
    /// Inspector "Set scene camera from view": record one `camera.set` that
    /// moves the scene camera to the viewport's orbit camera.
    SetSceneCameraFromView,
}

impl Action {
    /// Whether this action edits the scene or its history, which is not
    /// allowed while playing. Console lines, the toolbar, Import (which
    /// only copies a file), and View from scene camera are allowed.
    pub fn is_edit(&self) -> bool {
        !matches!(
            self,
            Self::Console(_)
                | Self::Import(_)
                | Self::Play
                | Self::Pause
                | Self::Stop
                | Self::ViewFromSceneCamera
        )
    }
}

/// The Console panel's state: the line being typed and past lines with
/// their results.
#[derive(Debug, Default)]
pub struct Console {
    /// The text in the input field.
    pub input: String,
    /// `(line, result)` of every line run, oldest first; `Err` holds the
    /// error message.
    pub transcript: Vec<(String, Result<String, String>)>,
}

impl Console {
    /// Appends a line that was run and its result to the transcript.
    pub fn record(&mut self, line: String, result: Result<String, String>) {
        self.transcript.push((line, result));
    }
}

/// Splits a console line into a tool name and its JSON arguments; with no
/// arguments the tool gets `{}`.
///
/// `entity.spawn {"mesh": 0}` gives `("entity.spawn", {"mesh": 0})`.
pub fn parse_console_line(line: &str) -> Result<(&str, Value), String> {
    let line = line.trim();
    let (tool, args) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    if tool.is_empty() {
        return Err("type a tool name, e.g. engine.list_tools".to_string());
    }
    let args = args.trim();
    let args = if args.is_empty() {
        Value::Object(Default::default())
    } else {
        serde_json::from_str(args).map_err(|err| format!("arguments are not JSON: {err}"))?
    };
    Ok((tool, args))
}

/// The initial layout: Scene takes the left 18% with Assets under it,
/// Inspector the right 22% of what remains with History under it, and Log
/// and Console share the bottom quarter under the Viewport side by side.
///
/// An `egui_dock` split fraction is the share of the left (or top) child,
/// whichever side the new tabs go on.
pub fn default_layout() -> DockState<Tab> {
    let mut dock = DockState::new(vec![Tab::Viewport]);
    let surface = dock.main_surface_mut();
    let [centre, scene] = surface.split_left(NodeIndex::root(), 0.18, vec![Tab::Scene]);
    let [_scene, _assets] = surface.split_below(scene, 0.5, vec![Tab::Assets]);
    let [centre, inspector] = surface.split_right(centre, 0.78, vec![Tab::Inspector]);
    let [_inspector, _history] = surface.split_below(inspector, 0.55, vec![Tab::History]);
    let [_viewport, log] = surface.split_below(centre, 0.75, vec![Tab::Log]);
    let [_log, _console] = surface.split_right(log, 0.5, vec![Tab::Console]);
    dock
}

/// What the Viewport panel shows and what it asks for.
#[derive(Debug, Default)]
pub struct Viewport {
    /// The scene texture to draw, once registered with the egui renderer.
    pub texture: Option<TextureId>,
    /// The panel's size in physical pixels, measured during the last UI
    /// pass; the offscreen target is resized to match. Zero before the
    /// panel has been laid out.
    pub size_px: [u32; 2],
}

/// Text typed into fields that outlive one frame: the Scene panel's asset
/// source and File > Save As path.
#[derive(Debug, Default)]
pub struct Fields {
    /// The asset source "+ From asset" adds, e.g. `assets/cube.gltf#0`.
    pub asset_source: String,
    /// The path Save As writes, relative to the project.
    pub save_as: String,
}

/// Everything the panels read or change during one UI pass.
pub struct EditorUi<'a> {
    /// The engine whose world the Scene panel lists and whose command
    /// history the History panel shows.
    pub engine: &'a Headless,
    /// The lines the Log panel shows.
    pub log: &'a LogBuffer,
    /// The camera viewport input drives.
    pub orbit: &'a mut OrbitCamera,
    /// The viewport's texture and requested size.
    pub viewport: &'a mut Viewport,
    /// The Console panel's input and transcript.
    pub console: &'a mut Console,
    /// The selected entity.
    pub selection: &'a mut Selection,
    /// The Inspector's edit buffers.
    pub inspector: &'a mut Inspector,
    /// The Assets panel's entries and fields.
    pub assets: &'a mut AssetBrowser,
    /// The open project's directory, which the Assets panel scans.
    pub project_dir: &'a Path,
    /// Text fields of the Scene panel and File menu.
    pub fields: &'a mut Fields,
    /// The scene file Save writes, relative to the project; shown in the
    /// File menu.
    pub scene_path: &'a str,
    /// What to run after the UI pass.
    pub actions: &'a mut Vec<Action>,
    /// Set when File > Quit is clicked.
    pub quit: &'a mut bool,
}

/// Ctrl+Z (Cmd+Z on macOS).
const UNDO: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Z);

/// Ctrl+Shift+Z (Cmd+Shift+Z on macOS).
const REDO: KeyboardShortcut =
    KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::Z);

impl EditorUi<'_> {
    /// Draws the whole editor into `ui`, which covers the window.
    pub fn show(&mut self, ui: &mut Ui, dock: &mut DockState<Tab>) {
        self.shortcuts(ui);
        let editing = !self.engine.play_state().is_playing();
        let history = self.engine.resources.get::<History>();
        let can_undo = editing && history.is_some_and(|h| h.undo_len() > 0);
        let can_redo = editing && history.is_some_and(|h| h.redo_len() > 0);
        egui::Panel::top("menu_bar").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| self.file_menu(ui));
                ui.menu_button("Edit", |ui| {
                    let undo =
                        egui::Button::new("Undo").shortcut_text(ui.ctx().format_shortcut(&UNDO));
                    if ui.add_enabled(can_undo, undo).clicked() {
                        self.actions.push(Action::Undo);
                    }
                    let redo =
                        egui::Button::new("Redo").shortcut_text(ui.ctx().format_shortcut(&REDO));
                    if ui.add_enabled(can_redo, redo).clicked() {
                        self.actions.push(Action::Redo);
                    }
                });
                ui.menu_button("View", |ui| view_menu(ui, dock));
                ui.separator();
                self.toolbar(ui);
            });
        });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                DockArea::new(dock)
                    .style(Style::from_egui(ui.style()))
                    .show_close_buttons(true)
                    .show_leaf_close_all_buttons(false)
                    .show_leaf_collapse_buttons(false)
                    .show_inside(ui, self);
            });
    }

    /// Play, Pause (highlighted while paused; click again to resume), Stop,
    /// and the simulated seconds since Play.
    fn toolbar(&mut self, ui: &mut Ui) {
        let state = self.engine.play_state();
        let (playing, paused) = (state.is_playing(), state.is_paused());
        let play = egui::Button::new("Play").selected(playing && !paused);
        if ui
            .add_enabled(!playing || paused, play)
            .on_hover_text("Run the simulation; Stop puts the scene back")
            .clicked()
        {
            self.actions.push(Action::Play);
        }
        let pause = egui::Button::new("Pause").selected(paused);
        if ui
            .add_enabled(playing, pause)
            .on_hover_text("Suspend or resume stepping")
            .clicked()
        {
            self.actions.push(Action::Pause);
        }
        if ui
            .add_enabled(playing, egui::Button::new("Stop"))
            .on_hover_text("Restore the scene as it was at Play")
            .clicked()
        {
            self.actions.push(Action::Stop);
        }
        let time = self.engine.play_time().unwrap_or(0.0);
        ui.label(egui::RichText::new(format!("{time:.2} s")).monospace())
            .on_hover_text("Simulated time since Play");
    }

    /// File > Open, Save, Save As, Quit. All but Quit are disabled while
    /// playing.
    fn file_menu(&mut self, ui: &mut Ui) {
        let editing = !self.engine.play_state().is_playing();
        if ui
            .add_enabled(editing, egui::Button::new("Open"))
            .on_hover_text("Load the project's entry scene")
            .clicked()
        {
            self.actions.push(Action::Open);
            ui.close();
        }
        if ui
            .add_enabled(editing, egui::Button::new("Save"))
            .on_hover_text(format!("Save to {}", self.scene_path))
            .clicked()
        {
            self.actions.push(Action::Save);
            ui.close();
        }
        ui.add_enabled_ui(editing, |ui| self.save_as_row(ui));
        ui.separator();
        if ui.button("Quit").clicked() {
            *self.quit = true;
        }
    }

    /// File > Save As: a path field and its button.
    fn save_as_row(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.fields.save_as)
                    .hint_text("scenes/other.scene.ron")
                    .desired_width(180.0),
            );
            let submitted = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            let path = self.fields.save_as.trim().to_string();
            if (ui.button("Save As").clicked() || submitted) && !path.is_empty() {
                self.actions.push(Action::SaveAs(path));
                ui.close();
            }
        });
    }

    /// Keyboard shortcuts. Undo and redo are left to the text field while
    /// one has focus and holds text, so typing in the console keeps its own
    /// text undo; in an empty console (right after running a line) they
    /// undo and redo edits. Delete and Escape act only when no text field
    /// has focus. While playing only Escape acts.
    fn shortcuts(&mut self, ui: &mut Ui) {
        let typing = ui.ctx().egui_wants_keyboard_input();
        if self.engine.play_state().is_playing() {
            if !typing && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
                self.selection.0 = None;
            }
            return;
        }
        if !typing {
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
                self.selection.0 = None;
            }
            let delete = ui.input_mut(|i| {
                i.consume_key(Modifiers::NONE, Key::Delete)
                    || i.consume_key(Modifiers::NONE, Key::Backspace)
            });
            if delete && let Some(entity) = self.selection.0 {
                self.actions.push(Action::Despawn(entity));
            }
        }
        if typing && !self.console.input.is_empty() {
            return;
        }
        // Redo first: consuming Ctrl+Z would also match Ctrl+Shift+Z.
        if ui.input_mut(|input| input.consume_shortcut(&REDO)) {
            self.actions.push(Action::Redo);
        } else if ui.input_mut(|input| input.consume_shortcut(&UNDO)) {
            self.actions.push(Action::Undo);
        }
    }

    fn viewport_ui(&mut self, ui: &mut Ui) {
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let ppp = ui.ctx().pixels_per_point();
        self.viewport.size_px = [
            (rect.width() * ppp).round().max(0.0) as u32,
            (rect.height() * ppp).round().max(0.0) as u32,
        ];
        if let Some(texture) = self.viewport.texture {
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            ui.painter().image(texture, rect, uv, Color32::WHITE);
        }

        // Drags only count when they started on the viewport, and hover is
        // false while a menu, popup, or another panel is under the pointer,
        // so panel interaction never moves the camera.
        let drag = response.drag_delta();
        let drag = Vec2::new(drag.x, drag.y);
        if response.dragged_by(PointerButton::Primary) {
            self.orbit.orbit(drag);
        } else if response.dragged_by(PointerButton::Secondary) {
            self.orbit.pan(drag);
        }
        if response.hovered() {
            let scroll = ui.input(|input| input.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.orbit.zoom(scroll);
            }
        }
    }

    /// Add and Delete buttons above the list of entities that have a
    /// transform, in id order; clicking one selects it. The buttons are
    /// disabled while playing; selecting still works.
    fn scene_ui(&mut self, ui: &mut Ui) {
        let editing = !self.engine.play_state().is_playing();
        ui.add_enabled_ui(editing, |ui| self.scene_controls(ui));
        ui.separator();
        self.entity_list(ui);
    }

    /// The Scene panel's add and delete controls.
    fn scene_controls(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            for (label, source) in ADD_BUTTONS {
                if ui.button(label).clicked() {
                    self.actions.push(Action::Spawn(source.to_string()));
                }
            }
            let selected = self.selection.0;
            if ui
                .add_enabled(selected.is_some(), egui::Button::new("Delete"))
                .clicked()
                && let Some(entity) = selected
            {
                self.actions.push(Action::Despawn(entity));
            }
        });
        ui.horizontal(|ui| {
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.fields.asset_source)
                    .hint_text("assets/x.gltf#0")
                    .desired_width(110.0),
            );
            let submitted = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            let source = self.fields.asset_source.trim().to_string();
            if (ui.button("+ From asset").clicked() || submitted) && !source.is_empty() {
                self.actions.push(Action::Spawn(source));
            }
        });
    }

    /// The entities that have a transform as a tree: roots in id order,
    /// each with its children (in id order) in a collapsible branch.
    /// Clicking a row selects it. While editing, dragging a row onto
    /// another makes it that entity's child, dropping it on the space below
    /// the tree makes it a root, and a row's right-click menu has Unparent.
    fn entity_list(&mut self, ui: &mut Ui) {
        let world = &self.engine.world;
        let mut labels: HashMap<Entity, String> = world
            .query::<(Entity, &Transform, Option<&Name>)>()
            .iter()
            .map(|(entity, _, name)| (entity, entity_label(entity, name)))
            .collect();
        let children = child_map(world);
        // A child whose parent has no transform is listed as a root.
        let mut roots: Vec<Entity> = labels
            .keys()
            .copied()
            .filter(|&e| !parent_of(world, e).is_some_and(|p| labels.contains_key(&p)))
            .collect();
        roots.sort_by_key(|e| entity_id(*e));
        if roots.is_empty() {
            ui.weak("No entities");
        }
        let editing = !self.engine.play_state().is_playing();
        for root in roots {
            self.tree_node(ui, root, &children, &mut labels, editing);
        }
        if editing {
            // The rest of the panel is a drop target that unparents.
            let width = ui.available_width();
            let height = ui.available_height().max(24.0);
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(width, height), Sense::hover());
            if response.dnd_hover_payload::<DraggedEntity>().is_some() {
                ui.painter().rect_stroke(
                    rect,
                    2.0,
                    ui.visuals().selection.stroke,
                    StrokeKind::Inside,
                );
            }
            if let Some(dragged) = response.dnd_release_payload::<DraggedEntity>() {
                self.reparent(dragged.0, None);
            }
        }
    }

    /// One row of the tree and, if `entity` has children, its collapsible
    /// branch (open by default). `labels` holds the rows not drawn yet, so
    /// each entity is drawn once.
    fn tree_node(
        &mut self,
        ui: &mut Ui,
        entity: Entity,
        children: &HashMap<Entity, Vec<Entity>>,
        labels: &mut HashMap<Entity, String>,
        editing: bool,
    ) {
        let Some(label) = labels.remove(&entity) else {
            return;
        };
        let kids: Vec<Entity> = children
            .get(&entity)
            .map(|kids| {
                kids.iter()
                    .copied()
                    .filter(|k| labels.contains_key(k))
                    .collect()
            })
            .unwrap_or_default();
        if kids.is_empty() {
            ui.horizontal(|ui| {
                // Line up with rows that have a collapse arrow.
                ui.add_space(ui.spacing().indent);
                self.tree_row(ui, entity, label, editing);
            });
            return;
        }
        let id = ui.make_persistent_id(("scene_tree", entity_id(entity)));
        let _ = CollapsingState::load_with_default_open(ui.ctx(), id, true)
            .show_header(ui, |ui| self.tree_row(ui, entity, label, editing))
            .body(|ui| {
                for kid in kids {
                    self.tree_node(ui, kid, children, labels, editing);
                }
            });
    }

    /// A selectable row; while editing also a drag source, a drop target
    /// (reparent onto this entity), and a right-click menu with Unparent.
    fn tree_row(&mut self, ui: &mut Ui, entity: Entity, label: String, editing: bool) {
        let selected = self.selection.0 == Some(entity);
        let sense = if editing {
            Sense::click_and_drag()
        } else {
            Sense::click()
        };
        let response = ui.add(egui::Button::selectable(selected, label).sense(sense));
        if response.clicked() {
            self.selection.0 = Some(entity);
        }
        if !editing {
            return;
        }
        response.dnd_set_drag_payload(DraggedEntity(entity));
        if response
            .dnd_hover_payload::<DraggedEntity>()
            .is_some_and(|dragged| dragged.0 != entity)
        {
            ui.painter().rect_stroke(
                response.rect,
                2.0,
                Stroke::new(2.0, ui.visuals().selection.stroke.color),
                StrokeKind::Outside,
            );
        }
        if let Some(dragged) = response.dnd_release_payload::<DraggedEntity>()
            && dragged.0 != entity
        {
            self.reparent(dragged.0, Some(entity));
        }
        let has_parent = parent_of(&self.engine.world, entity).is_some();
        response.context_menu(|ui| {
            if ui
                .add_enabled(has_parent, egui::Button::new("Unparent"))
                .clicked()
            {
                self.reparent(entity, None);
                ui.close();
            }
        });
    }

    /// Asks for `entity` to move under `parent` (or to the top level with
    /// `None`), unless it is already there.
    fn reparent(&mut self, entity: Entity, parent: Option<Entity>) {
        if parent_of(&self.engine.world, entity) != parent {
            self.actions.push(Action::Reparent { entity, parent });
        }
    }

    /// The undoable edits, oldest first, then the undone ones greyed out.
    fn history_ui(&self, ui: &mut Ui) {
        let history = self.engine.resources.get::<History>();
        if history.is_none_or(|h| h.undo_len() + h.redo_len() == 0) {
            ui.weak("No edits yet");
        }
        let Some(history) = history else {
            return;
        };
        for (tool, args) in history.undo_entries() {
            ui.label(egui::RichText::new(format!("{tool} {args}")).monospace());
        }
        for (tool, args) in history.redo_entries().rev() {
            ui.weak(egui::RichText::new(format!("{tool} {args}")).monospace());
        }
    }

    /// Past lines with their results above a one-line input; Enter runs it.
    fn console_ui(&mut self, ui: &mut Ui) {
        egui::Panel::bottom("console_input")
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let input = egui::TextEdit::singleline(&mut self.console.input)
                    .hint_text("tool.name {\"json\": \"args\"}")
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY);
                let response = ui.add(input);
                if response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    let line = std::mem::take(&mut self.console.input);
                    if !line.trim().is_empty() {
                        self.actions.push(Action::Console(line));
                    }
                    response.request_focus();
                }
            });
        egui::ScrollArea::both()
            .auto_shrink(false)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for (line, result) in &self.console.transcript {
                    ui.label(egui::RichText::new(format!("> {line}")).monospace());
                    let (text, color) = match result {
                        Ok(text) => (text, ui.visuals().text_color()),
                        Err(text) => (text, ui.visuals().error_fg_color),
                    };
                    ui.add(
                        egui::Label::new(egui::RichText::new(text).monospace().color(color))
                            .wrap_mode(egui::TextWrapMode::Extend),
                    );
                }
            });
    }

    /// The log lines, scrolled to the newest unless the user scrolls up.
    fn log_ui(&self, ui: &mut Ui) {
        egui::ScrollArea::both()
            .auto_shrink(false)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in self.log.lines() {
                    ui.add(
                        egui::Label::new(egui::RichText::new(line).monospace())
                            .wrap_mode(egui::TextWrapMode::Extend),
                    );
                }
            });
    }
}

impl TabViewer for EditorUi<'_> {
    type Tab = Tab;

    fn id(&mut self, tab: &mut Tab) -> egui::Id {
        egui::Id::new(*tab)
    }

    fn title(&mut self, tab: &mut Tab) -> WidgetText {
        format!("{tab:?}").into()
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut Tab) {
        match tab {
            Tab::Viewport => self.viewport_ui(ui),
            Tab::Scene => self.scene_ui(ui),
            Tab::Assets => {
                let editing = !self.engine.play_state().is_playing();
                self.assets.ui(
                    ui,
                    self.project_dir,
                    self.selection.0,
                    editing,
                    self.actions,
                );
            }
            Tab::Inspector => {
                // Read-only while playing: it still shows the live values.
                let editing = !self.engine.play_state().is_playing();
                ui.add_enabled_ui(editing, |ui| {
                    self.inspector
                        .ui(ui, self.engine, self.selection.0, self.actions);
                    ui.separator();
                    crate::inspector::scene_camera_ui(ui, self.engine, self.actions);
                });
            }
            Tab::Log => self.log_ui(ui),
            Tab::History => self.history_ui(ui),
            Tab::Console => self.console_ui(ui),
        }
    }

    /// Every panel but the Viewport can be closed; the View menu brings
    /// it back.
    fn is_closeable(&self, tab: &Tab) -> bool {
        *tab != Tab::Viewport
    }

    /// The viewport fills its panel exactly (scroll bars would let it grow
    /// and would swallow the zoom wheel); the Log and Console scroll
    /// themselves so they can stick to the newest line.
    fn scroll_bars(&self, tab: &Tab) -> [bool; 2] {
        match tab {
            Tab::Viewport | Tab::Log | Tab::Console => [false, false],
            Tab::Scene | Tab::Assets | Tab::Inspector | Tab::History => [true, true],
        }
    }
}

/// How the Scene panel names an entity: its [`Name`], or `#<id>` (the id
/// tools use) when it has none or the name is empty.
pub fn entity_label(entity: Entity, name: Option<&Name>) -> String {
    match name {
        Some(Name(name)) if !name.is_empty() => name.clone(),
        _ => format!("#{}", entity_id(entity)),
    }
}

/// View: one checkbox per closeable panel; unchecking closes it, checking
/// docks it again ([`reopen`]).
fn view_menu(ui: &mut Ui, dock: &mut DockState<Tab>) {
    for tab in CLOSEABLE_TABS {
        let found = dock.find_tab(&tab);
        let mut open = found.is_some();
        if ui.checkbox(&mut open, format!("{tab:?}")).changed() {
            match found {
                Some(path) => {
                    dock.remove_tab(path);
                }
                None => reopen(dock, tab),
            }
        }
    }
}

/// Docks `tab` again against the window edge [`default_layout`] gives it.
fn reopen(dock: &mut DockState<Tab>, tab: Tab) {
    let surface = dock.main_surface_mut();
    let root = NodeIndex::root();
    match tab {
        Tab::Scene | Tab::Assets => {
            surface.split_left(root, 0.18, vec![tab]);
        }
        Tab::Inspector | Tab::History => {
            surface.split_right(root, 0.78, vec![tab]);
        }
        Tab::Log | Tab::Console | Tab::Viewport => {
            surface.split_below(root, 0.75, vec![tab]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_layout_has_all_panels() {
        let dock = default_layout();
        for tab in [
            Tab::Viewport,
            Tab::Scene,
            Tab::Assets,
            Tab::Inspector,
            Tab::Log,
            Tab::History,
            Tab::Console,
        ] {
            assert!(dock.find_tab(&tab).is_some(), "{tab:?} is docked");
        }
    }

    #[test]
    fn view_menu_closes_and_reopens_panels() {
        let mut dock = default_layout();
        for tab in CLOSEABLE_TABS {
            let path = dock.find_tab(&tab).expect("docked");
            dock.remove_tab(path);
            assert!(dock.find_tab(&tab).is_none());
            reopen(&mut dock, tab);
            assert!(dock.find_tab(&tab).is_some(), "{tab:?} is back");
        }
        assert!(dock.find_tab(&Tab::Viewport).is_some());
    }

    #[test]
    fn entities_are_labelled_by_name_or_id() {
        let mut world = shin::ecs::World::new();
        let entity = world.spawn(());
        let id = entity_id(entity);
        assert_eq!(entity_label(entity, None), format!("#{id}"));
        assert_eq!(
            entity_label(entity, Some(&Name(String::new()))),
            format!("#{id}")
        );
        assert_eq!(entity_label(entity, Some(&Name("cube".into()))), "cube");
    }

    #[test]
    fn console_lines_parse_into_tool_and_args() {
        assert_eq!(
            parse_console_line("  entity.spawn {\"mesh\": 0} "),
            Ok(("entity.spawn", serde_json::json!({ "mesh": 0 })))
        );
        assert_eq!(
            parse_console_line("history.undo"),
            Ok(("history.undo", serde_json::json!({})))
        );
        assert!(parse_console_line("   ").is_err());
        let err = parse_console_line("entity.spawn {mesh: 0}").unwrap_err();
        assert!(err.contains("not JSON"), "{err}");
    }
}
