//! The Inspector panel (GE-023): the selected entity's name, transform,
//! mesh, and colour, editable; (GE-011) its script path, with the
//! script's last compile or runtime error; and (GE-030) the scene's own
//! camera, read-only, with buttons to swap placements with the viewport's
//! orbit camera ([`scene_camera_ui`]).
//!
//! The panel keeps its own copy of the values being edited, refreshed from
//! the world every frame except for the field the user is working on.
//! Drags and colour picks push live previews and one commit when the
//! pointer is released; text fields commit on Enter or when they lose
//! focus. Commits become [`Edit`]s that [`crate::editor::EditorCore`] turns
//! into tool calls through the command history ([`crate::edit`]).

use std::path::Path;

use egui::{DragValue, Ui};
use shin::ecs::{Entity, Name};
use shin::headless::Headless;
use shin::math::{Camera, Projection, Transform, Vec3};
use shin::render::{MeshInstance, RenderState};
use shin::scene::TransformDesc;
use shin::script::{Script, ScriptEngine};

use crate::edit::entity_id;
use crate::ui::Action;

/// One committed inspector change, carried out as one recorded tool call.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    /// `entity.set_name`.
    Name(Entity, String),
    /// `entity.set_transform`, after putting `original` back if the value
    /// was previewed.
    Transform {
        /// The entity edited.
        entity: Entity,
        /// The new value.
        desc: TransformDesc,
        /// The value before the preview started.
        original: TransformDesc,
    },
    /// `entity.set_color`, after putting `original` back.
    Color {
        /// The entity edited.
        entity: Entity,
        /// The new linear RGBA colour.
        color: [f32; 4],
        /// The colour before the preview started.
        original: [f32; 4],
    },
    /// `entity.set_mesh` with an asset source string.
    Mesh(Entity, String),
    /// `script.attach` with a `.rhai` path; empty removes the script.
    Script(Entity, String),
}

/// The Inspector's edit buffers.
#[derive(Debug, Default)]
pub struct Inspector {
    /// The entity the buffers describe.
    entity: Option<Entity>,
    name: String,
    transform: TransformDesc,
    /// `None` when the entity draws no mesh.
    color: Option<[f32; 4]>,
    mesh: String,
    script: String,
    /// The transform before the preview in progress, if any.
    transform_origin: Option<TransformDesc>,
    /// The colour before the preview in progress, if any.
    color_origin: Option<[f32; 4]>,
    /// Whether these widgets had keyboard focus last frame (their buffers
    /// are then not refreshed from the world).
    name_focused: bool,
    transform_focused: bool,
    mesh_focused: bool,
    script_focused: bool,
}

/// The values the inspector shows for `entity`, read from the world.
struct Current {
    name: String,
    transform: Option<TransformDesc>,
    color: Option<[f32; 4]>,
    mesh: String,
    /// The script path, or empty.
    script: String,
    /// The script's last error, as `path:line: message`.
    script_error: Option<String>,
}

impl Current {
    fn read(engine: &Headless, entity: Entity) -> Self {
        let world = &engine.world;
        let instance = world.get::<&MeshInstance>(entity).ok().map(|i| *i);
        let mesh = instance
            .and_then(|i| {
                let state = engine.resources.get::<RenderState>()?;
                state.registry.source(i.mesh).map(|s| s.to_string())
            })
            .unwrap_or_default();
        let script = world.get::<&Script>(entity).ok().map(|s| s.path.clone());
        let script_error = script
            .as_deref()
            .and_then(|path| script_error(engine, path));
        Self {
            script: script
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            script_error,
            name: world
                .get::<&Name>(entity)
                .map(|n| n.0.clone())
                .unwrap_or_default(),
            transform: world
                .get::<&Transform>(entity)
                .ok()
                .map(|t| TransformDesc::from_transform(&t)),
            color: instance.map(|i| i.color),
            mesh,
        }
    }
}

/// The "Scene camera" section: the scene's own [`Camera`] (the one Save
/// writes and Play renders from; the viewport otherwise shows the editor's
/// orbit camera), read-only, with **View from scene camera**
/// ([`Action::ViewFromSceneCamera`]) and **Set scene camera from view**
/// ([`Action::SetSceneCameraFromView`]).
pub fn scene_camera_ui(ui: &mut Ui, engine: &Headless, actions: &mut Vec<Action>) {
    ui.strong("Scene camera");
    let Some(camera) = engine.resources.get::<Camera>() else {
        ui.weak("No camera");
        return;
    };
    egui::Grid::new("scene_camera_grid")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            ui.label("Position");
            ui.label(vec3_text(camera.transform.translation));
            ui.end_row();
            ui.label("Looking");
            ui.label(vec3_text(camera.transform.forward()));
            ui.end_row();
            ui.label("FOV");
            ui.label(fov_text(&camera.projection));
            ui.end_row();
        });
    if ui
        .button("View from scene camera")
        .on_hover_text("Move the viewport camera to where the scene camera is")
        .clicked()
    {
        actions.push(Action::ViewFromSceneCamera);
    }
    if ui
        .button("Set scene camera from view")
        .on_hover_text(
            "Move the scene camera to the viewport camera (undoable; the scene is changed)",
        )
        .clicked()
    {
        actions.push(Action::SetSceneCameraFromView);
    }
}

/// `v` with two decimals, as `(x, y, z)`.
fn vec3_text(v: Vec3) -> String {
    format!("({:.2}, {:.2}, {:.2})", v.x, v.y, v.z)
}

/// A perspective projection's vertical field of view in degrees, or
/// `orthographic`.
fn fov_text(projection: &Projection) -> String {
    match projection {
        Projection::Perspective { fov_y_radians, .. } => {
            format!("{:.1}°", fov_y_radians.to_degrees())
        }
        Projection::Orthographic { .. } => "orthographic".to_string(),
    }
}

/// The latest error of the script at `path` (as written in its
/// [`Script`]), formatted `path:line: message`, if it has one.
pub fn script_error(engine: &Headless, path: &Path) -> Option<String> {
    let scripts = engine.resources.get::<ScriptEngine>()?;
    scripts.error(path).map(ToString::to_string)
}

impl Inspector {
    /// Commits a preview still in progress (as when the selection changes
    /// mid-drag) so it is not left unrecorded.
    fn flush(&mut self, actions: &mut Vec<Action>) {
        let Some(entity) = self.entity else {
            return;
        };
        if let Some(original) = self.transform_origin.take()
            && original != self.transform
        {
            actions.push(Action::Edit(Edit::Transform {
                entity,
                desc: self.transform,
                original,
            }));
        }
        if let (Some(original), Some(color)) = (self.color_origin.take(), self.color)
            && original != color
        {
            actions.push(Action::Edit(Edit::Color {
                entity,
                color,
                original,
            }));
        }
    }

    /// Draws the panel for `selected` and pushes previews and commits onto
    /// `actions`.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        engine: &Headless,
        selected: Option<Entity>,
        actions: &mut Vec<Action>,
    ) {
        let selected = selected.filter(|&e| engine.world.contains(e));
        if selected != self.entity {
            self.flush(actions);
            *self = Self {
                entity: selected,
                ..Self::default()
            };
        }
        let Some(entity) = selected else {
            ui.weak("Select an entity");
            return;
        };
        let current = Current::read(engine, entity);
        self.refresh(&current);

        ui.label(format!("Entity #{}", entity_id(entity)));
        egui::Grid::new("inspector_grid")
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                self.name_row(ui, entity, actions);
                self.transform_rows(ui, entity, actions);
                self.mesh_row(ui, engine, entity, actions);
                self.color_row(ui, entity, actions);
                self.script_row(ui, entity, &current, actions);
            });
        if let Some(error) = &current.script_error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }

        // A drag or colour pick ends when the pointer is released; a typed
        // number ends when its field loses focus.
        let pointer_down = ui.input(|i| i.pointer.any_down());
        if !pointer_down && !self.transform_focused {
            self.flush(actions);
        }
    }

    /// Takes the world's values into every buffer that is not being edited.
    fn refresh(&mut self, current: &Current) {
        if !self.name_focused {
            self.name.clone_from(&current.name);
        }
        if self.transform_origin.is_none()
            && !self.transform_focused
            && let Some(transform) = current.transform
        {
            self.transform = transform;
        }
        if self.color_origin.is_none() {
            self.color = current.color;
        }
        if !self.mesh_focused {
            self.mesh.clone_from(&current.mesh);
        }
        if !self.script_focused {
            self.script.clone_from(&current.script);
        }
    }

    /// The script path field: commits on Enter or when it loses focus, if
    /// the path changed. Clearing it removes the script.
    fn script_row(
        &mut self,
        ui: &mut Ui,
        entity: Entity,
        current: &Current,
        actions: &mut Vec<Action>,
    ) {
        ui.label("Script");
        let response = ui.add(
            egui::TextEdit::singleline(&mut self.script)
                .hint_text("scripts/name.rhai")
                .desired_width(160.0),
        );
        let path = self.script.trim();
        if response.lost_focus() && path != current.script {
            actions.push(Action::Edit(Edit::Script(entity, path.to_string())));
        }
        self.script_focused = response.has_focus();
        ui.end_row();
    }

    fn name_row(&mut self, ui: &mut Ui, entity: Entity, actions: &mut Vec<Action>) {
        ui.label("Name");
        let response = ui.text_edit_singleline(&mut self.name);
        if response.lost_focus() {
            actions.push(Action::Edit(Edit::Name(entity, self.name.clone())));
        }
        self.name_focused = response.has_focus();
        ui.end_row();
    }

    fn transform_rows(&mut self, ui: &mut Ui, entity: Entity, actions: &mut Vec<Action>) {
        let before = self.transform;
        let mut focused = false;
        let rows: [(&str, &mut [f32; 3], f64); 3] = [
            ("Translation", &mut self.transform.translation, 0.05),
            ("Rotation (°)", &mut self.transform.rotation_euler_deg, 1.0),
            ("Scale", &mut self.transform.scale, 0.01),
        ];
        for (label, values, speed) in rows {
            ui.label(label);
            ui.horizontal(|ui| {
                for value in values.iter_mut() {
                    let response = ui.add(
                        DragValue::new(value)
                            .speed(speed)
                            .max_decimals(3)
                            .update_while_editing(false),
                    );
                    focused |= response.has_focus();
                }
            });
            ui.end_row();
        }
        self.transform_focused = focused;
        if self.transform != before {
            self.transform_origin.get_or_insert(before);
            actions.push(Action::PreviewTransform(entity, self.transform));
        }
    }

    fn mesh_row(
        &mut self,
        ui: &mut Ui,
        engine: &Headless,
        entity: Entity,
        actions: &mut Vec<Action>,
    ) {
        ui.label("Mesh");
        ui.horizontal(|ui| {
            let mut sources: Vec<String> = engine
                .resources
                .get::<RenderState>()
                .map(|state| {
                    state
                        .registry
                        .iter()
                        .filter_map(|(handle, _)| state.registry.source(handle))
                        .map(|s| s.to_string())
                        .collect()
                })
                .unwrap_or_default();
            for builtin in ["builtin://cube", "builtin://sphere"] {
                if !sources.iter().any(|s| s == builtin) {
                    sources.push(builtin.to_string());
                }
            }
            sources.sort();
            sources.dedup();
            let current = self.mesh.clone();
            egui::ComboBox::from_id_salt("inspector_mesh")
                .selected_text(if current.is_empty() {
                    "(none)"
                } else {
                    &current
                })
                .show_ui(ui, |ui| {
                    for source in &sources {
                        if ui.selectable_label(*source == current, source).clicked()
                            && *source != current
                        {
                            actions.push(Action::Edit(Edit::Mesh(entity, source.clone())));
                        }
                    }
                });
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.mesh)
                    .hint_text("asset source")
                    .desired_width(120.0),
            );
            if response.lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                && !self.mesh.trim().is_empty()
            {
                actions.push(Action::Edit(Edit::Mesh(
                    entity,
                    self.mesh.trim().to_string(),
                )));
            }
            self.mesh_focused = response.has_focus();
        });
        ui.end_row();
    }

    fn color_row(&mut self, ui: &mut Ui, entity: Entity, actions: &mut Vec<Action>) {
        let Some(color) = self.color else {
            return;
        };
        ui.label("Colour");
        let [r, g, b, a] = color;
        // `Rgba` is linear, like the component.
        let mut rgba = egui::Rgba::from_rgba_unmultiplied(r, g, b, a);
        let response = egui::widgets::color_picker::color_edit_button_rgba(
            ui,
            &mut rgba,
            egui::widgets::color_picker::Alpha::OnlyBlend,
        );
        if response.changed() {
            let new = rgba.to_rgba_unmultiplied();
            if new != color {
                self.color_origin.get_or_insert(color);
                self.color = Some(new);
                actions.push(Action::PreviewColor(entity, new));
            }
        }
        ui.end_row();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_camera_text() {
        assert_eq!(
            vec3_text(Vec3::new(0.0, 2.0, -5.004)),
            "(0.00, 2.00, -5.00)"
        );
        assert_eq!(
            fov_text(&Projection::Perspective {
                fov_y_radians: 60f32.to_radians(),
                near: 0.1,
                far: 100.0,
            }),
            "60.0°"
        );
    }
}
