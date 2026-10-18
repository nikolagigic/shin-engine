//! The Inspector panel (GE-023): the selected entity's name, transform,
//! mesh, and colour, editable; (GE-011) its script path, with the
//! script's last compile or runtime error; and (GE-030) the scene's own
//! camera, read-only, with buttons to swap placements with the viewport's
//! orbit camera ([`scene_camera_ui`]). GE-031: the transform rows are
//! local to the entity's parent (shown read-only by name), followed by the
//! read-only world transform. GE-036: the entity's sound: source, a Play
//! button that previews it, volume, and the loop, spatial, and autoplay
//! flags. GE-034: a Physics section edits the entity's rigid body
//! (kind, mass) and collider (shape and size, friction, restitution);
//! the viewport draws the selected entity's collider as a translucent
//! shape.
//!
//! The panel keeps its own copy of the values being edited, refreshed from
//! the world every frame except for the field the user is working on.
//! Drags and colour picks push live previews and one commit when the
//! pointer is released; text fields commit on Enter or when they lose
//! focus. Commits become [`Edit`]s that [`crate::editor::EditorCore`] turns
//! into tool calls through the command history ([`crate::edit`]).

use std::path::Path;

use egui::{DragValue, Ui};
use shin::audio::AudioSource;
use shin::ecs::{Entity, Name, parent_of, world_transform};
use shin::headless::Headless;
use shin::math::{Camera, Projection, Transform, Vec3};
use shin::physics::{BodyKind, Collider, ColliderShape, RigidBody};
use shin::render::{MeshInstance, RenderState};
use shin::scene::{AudioDesc, TransformDesc};
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
    /// `entity.set_texture` with a texture asset source string, or `None`
    /// to remove the texture (GE-032, from the Assets panel).
    Texture(Entity, Option<String>),
    /// `script.attach` with a `.rhai` path; empty removes the script.
    Script(Entity, String),
    /// `entity.set_audio` with the whole sound; `None` removes it (GE-036).
    Audio(Entity, Option<AudioDesc>),
    /// `physics.set_body` (GE-034); `None` removes the body.
    Body(Entity, Option<RigidBody>),
    /// `physics.set_collider` (GE-034); `None` removes the collider.
    Collider(Entity, Option<Collider>),
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
    /// The sound source field.
    audio_source: String,
    /// The volume drag value.
    audio_volume: f32,
    body: Option<RigidBody>,
    collider: Option<Collider>,
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
    audio_source_focused: bool,
    /// Whether the volume is being dragged or typed in.
    audio_volume_editing: bool,
    /// Whether a physics number is being dragged or typed.
    physics_focused: bool,
}

/// The values the inspector shows for `entity`, read from the world.
struct Current {
    name: String,
    transform: Option<TransformDesc>,
    /// The world-space transform (GE-031), shown read-only.
    world: TransformDesc,
    /// The parent's label (`name` or `#id`), if it has a parent.
    parent: Option<String>,
    color: Option<[f32; 4]>,
    mesh: String,
    /// The script path, or empty.
    script: String,
    /// The script's last error, as `path:line: message`.
    script_error: Option<String>,
    /// The entity's sound, if any.
    audio: Option<AudioDesc>,
    body: Option<RigidBody>,
    collider: Option<Collider>,
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
        let world_space = TransformDesc::from_transform(&world_transform(world, entity));
        let parent = parent_of(world, entity).map(|parent| {
            let name = world.get::<&Name>(parent).ok().map(|n| (*n).clone());
            crate::ui::entity_label(parent, name.as_ref())
        });
        Self {
            world: world_space,
            parent,
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
            audio: world
                .get::<&AudioSource>(entity)
                .ok()
                .map(|a| AudioDesc::from_source(&a)),
            body: world.get::<&RigidBody>(entity).ok().map(|b| *b),
            collider: world.get::<&Collider>(entity).ok().map(|c| *c),
        }
    }
}

/// The read-only world transform rows (GE-031): the editable rows above
/// them are local to the parent.
fn world_rows(ui: &mut Ui, world: &TransformDesc) {
    let fmt = |v: [f32; 3]| format!("{:.3}  {:.3}  {:.3}", v[0], v[1], v[2]);
    for (label, values) in [
        ("World position", world.translation),
        ("World rotation (°)", world.rotation_euler_deg),
        ("World scale", world.scale),
    ] {
        ui.label(label)
            .on_hover_text("Read-only: the transform above is local to the parent");
        ui.weak(fmt(values));
        ui.end_row();
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

/// The Inspector's name for a collider shape.
fn shape_label(shape: &ColliderShape) -> &'static str {
    match shape {
        ColliderShape::Cuboid(_) => "cuboid",
        ColliderShape::Ball(_) => "ball",
        ColliderShape::Capsule { .. } => "capsule",
    }
}

/// The shape the Inspector gives a new collider of kind `label`: sized to
/// fit the unit cube.
fn default_shape(label: &str) -> ColliderShape {
    match label {
        "ball" => ColliderShape::Ball(0.5),
        "capsule" => ColliderShape::Capsule {
            half_height: 0.25,
            radius: 0.25,
        },
        _ => ColliderShape::Cuboid(Vec3::splat(0.5)),
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
                ui.label("Parent");
                ui.label(current.parent.as_deref().unwrap_or("(none)"));
                ui.end_row();
                self.transform_rows(ui, entity, actions);
                world_rows(ui, &current.world);
                self.mesh_row(ui, engine, entity, actions);
                self.color_row(ui, entity, actions);
                self.script_row(ui, entity, &current, actions);
                self.audio_rows(ui, entity, &current, actions);
            });
        if let Some(error) = &current.script_error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        ui.separator();
        ui.strong("Physics");
        egui::Grid::new("inspector_physics_grid")
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| self.physics_rows(ui, entity, &current, actions));

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
        if !self.audio_source_focused {
            self.audio_source = current
                .audio
                .as_ref()
                .map(|a| a.source.clone())
                .unwrap_or_default();
        }
        if !self.audio_volume_editing {
            self.audio_volume = current.audio.as_ref().map_or(1.0, |a| a.volume);
        }
        if !self.physics_focused {
            self.body = current.body;
            self.collider = current.collider;
        }
    }

    /// The Audio rows (GE-036): the sound's source with a Play button that
    /// previews it, then, for an entity that has a sound, its volume and
    /// the Loop, Spatial, and Autoplay flags. The source commits on Enter
    /// or when it loses focus (empty removes the sound), the volume when
    /// the drag ends, and each flag when clicked.
    fn audio_rows(
        &mut self,
        ui: &mut Ui,
        entity: Entity,
        current: &Current,
        actions: &mut Vec<Action>,
    ) {
        ui.label("Audio");
        ui.horizontal(|ui| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.audio_source)
                    .hint_text("assets/sounds/x.wav")
                    .desired_width(130.0),
            );
            let source = self.audio_source.trim().to_string();
            let current_source = current.audio.as_ref().map_or("", |a| a.source.as_str());
            if response.lost_focus() && source != current_source {
                let audio = (!source.is_empty()).then(|| AudioDesc {
                    source: source.clone(),
                    ..current.audio.clone().unwrap_or_default()
                });
                actions.push(Action::Edit(Edit::Audio(entity, audio)));
            }
            self.audio_source_focused = response.has_focus();
            let play = ui
                .add_enabled(!source.is_empty(), egui::Button::new("▶ Play"))
                .on_hover_text("Preview the sound");
            if play.clicked() {
                actions.push(Action::PreviewSound(source));
            }
        });
        ui.end_row();

        let Some(audio) = &current.audio else {
            return;
        };
        ui.label("Volume");
        let response = ui.add(
            DragValue::new(&mut self.audio_volume)
                .speed(0.01)
                .range(0.0..=4.0)
                .max_decimals(2)
                .update_while_editing(false),
        );
        self.audio_volume_editing = response.dragged() || response.has_focus();
        let finished = response.drag_stopped() || response.lost_focus();
        if finished && self.audio_volume != audio.volume {
            actions.push(Action::Edit(Edit::Audio(
                entity,
                Some(AudioDesc {
                    volume: self.audio_volume,
                    ..audio.clone()
                }),
            )));
        }
        ui.end_row();

        ui.label("");
        ui.horizontal(|ui| {
            let mut flags = audio.clone();
            let mut changed = ui.checkbox(&mut flags.looping, "Loop").changed();
            changed |= ui
                .checkbox(&mut flags.spatial, "Spatial")
                .on_hover_text("Fade with the distance from the camera")
                .changed();
            changed |= ui
                .checkbox(&mut flags.autoplay, "Autoplay")
                .on_hover_text("Start when Play is pressed")
                .changed();
            if changed {
                actions.push(Action::Edit(Edit::Audio(entity, Some(flags))));
            }
        });
        ui.end_row();
    }

    /// The Physics rows (GE-034): body kind and, for a dynamic body, mass;
    /// collider shape, its size, friction, and restitution. Choosing a kind
    /// or shape commits at once; a number commits when its drag ends or
    /// its field loses focus.
    fn physics_rows(
        &mut self,
        ui: &mut Ui,
        entity: Entity,
        current: &Current,
        actions: &mut Vec<Action>,
    ) {
        let mut focused = false;
        let mut done = false;
        // Sizes and masses must stay positive.
        const SIZE: std::ops::RangeInclusive<f32> = 0.001..=1e4;
        let mut number = |ui: &mut Ui, value: &mut f32, speed: f64, range| {
            let response = ui.add(
                DragValue::new(value)
                    .speed(speed)
                    .range(range)
                    .max_decimals(3)
                    .update_while_editing(false),
            );
            focused |= response.dragged() || response.has_focus();
            done |= response.drag_stopped() || response.lost_focus();
        };

        ui.label("Body");
        let kind = self.body.map(|b| b.kind);
        let mut picked = kind;
        egui::ComboBox::from_id_salt("inspector_body")
            .selected_text(kind.map_or("(none)", BodyKind::label))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut picked, None, "(none)");
                for kind in [BodyKind::Dynamic, BodyKind::Kinematic, BodyKind::Fixed] {
                    ui.selectable_value(&mut picked, Some(kind), kind.label());
                }
            });
        if picked != kind {
            let mass = self.body.map_or(RigidBody::DEFAULT_MASS, |b| b.mass);
            let body = picked.map(|kind| RigidBody { kind, mass });
            actions.push(Action::Edit(Edit::Body(entity, body)));
        }
        ui.end_row();
        if let Some(body) = &mut self.body
            && body.kind == BodyKind::Dynamic
        {
            ui.label("Mass (kg)");
            number(ui, &mut body.mass, 0.05, 0.001..=1e6);
            ui.end_row();
        }

        ui.label("Collider");
        let shape = self.collider.map(|c| shape_label(&c.shape));
        let mut picked = shape;
        egui::ComboBox::from_id_salt("inspector_collider")
            .selected_text(shape.unwrap_or("(none)"))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut picked, None, "(none)");
                for label in ["cuboid", "ball", "capsule"] {
                    ui.selectable_value(&mut picked, Some(label), label);
                }
            });
        if picked != shape {
            let collider = picked.map(|label| Collider {
                shape: default_shape(label),
                ..self.collider.unwrap_or(Collider::new(default_shape(label)))
            });
            actions.push(Action::Edit(Edit::Collider(entity, collider)));
        }
        ui.end_row();
        if let Some(collider) = &mut self.collider {
            match &mut collider.shape {
                ColliderShape::Cuboid(half) => {
                    ui.label("Half extents");
                    ui.horizontal(|ui| {
                        number(ui, &mut half.x, 0.01, SIZE);
                        number(ui, &mut half.y, 0.01, SIZE);
                        number(ui, &mut half.z, 0.01, SIZE);
                    });
                    ui.end_row();
                }
                ColliderShape::Ball(radius) => {
                    ui.label("Radius");
                    number(ui, radius, 0.01, SIZE);
                    ui.end_row();
                }
                ColliderShape::Capsule {
                    half_height,
                    radius,
                } => {
                    ui.label("Half height");
                    number(ui, half_height, 0.01, SIZE);
                    ui.end_row();
                    ui.label("Radius");
                    number(ui, radius, 0.01, SIZE);
                    ui.end_row();
                }
            }
            ui.label("Friction");
            number(ui, &mut collider.friction, 0.01, 0.0..=100.0);
            ui.end_row();
            ui.label("Restitution");
            number(ui, &mut collider.restitution, 0.01, 0.0..=1.0);
            ui.end_row();
        }

        self.physics_focused = focused;
        if done && !focused {
            if self.body != current.body
                && let Some(body) = self.body
            {
                actions.push(Action::Edit(Edit::Body(entity, Some(body))));
            }
            if self.collider != current.collider
                && let Some(collider) = self.collider
            {
                actions.push(Action::Edit(Edit::Collider(entity, Some(collider))));
            }
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
