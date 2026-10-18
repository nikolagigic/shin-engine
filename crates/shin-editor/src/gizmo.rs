//! Transform gizmos (GE-035): handles on the selected entity that move,
//! rotate, or scale it by dragging in the viewport.
//!
//! [`TransformGizmo`] wraps a `transform-gizmo-egui` [`Gizmo`] and feeds it
//! the viewport camera and the pointer itself (not through the crate's
//! `GizmoExt::interact`, whose full-viewport drag sense would take every
//! drag away from the orbit camera). The gizmo works on the entity's world
//! transform; [`local_from_world`] turns the result back into the
//! entity's own (parent-relative) transform. While a handle is dragged
//! each frame pushes an unrecorded [`Action::PreviewTransform`]; releasing
//! pushes one [`Edit::Transform`] from the transform before the drag, so
//! the whole drag is one `entity.set_transform` in the history, exactly
//! as an Inspector drag is.
//!
//! Holding Ctrl (Cmd on macOS) snaps: translation in steps of
//! [`SNAP_DISTANCE`], rotation of [`SNAP_ANGLE_DEG`], scale of
//! [`SNAP_SCALE`], each counted from where the drag started.

use egui::{Key, Mesh, Painter, Pos2, Rect, Rgba, epaint::Vertex};
use shin::ecs::{Entity, World, parent_of, world_transform};
use shin::headless::Headless;
use shin::math::{Camera, Mat4, Quat, Transform, Vec3};
use shin::scene::TransformDesc;
use transform_gizmo_egui::math::{DMat4, DQuat, DVec3, Transform as GizmoTransform};
use transform_gizmo_egui::{
    Gizmo, GizmoConfig, GizmoDrawData, GizmoInteraction, GizmoMode, GizmoOrientation,
};

use crate::inspector::Edit;
use crate::ui::Action;

/// Translation snap step, in world units, while Ctrl is held.
pub const SNAP_DISTANCE: f32 = 0.5;
/// Rotation snap step, in degrees, while Ctrl is held.
pub const SNAP_ANGLE_DEG: f32 = 15.0;
/// Scale snap step while Ctrl is held.
pub const SNAP_SCALE: f32 = 0.1;

/// Which handles the gizmo shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Arrows and planes that move the entity (W).
    #[default]
    Translate,
    /// Rings that turn it (E).
    Rotate,
    /// Handles that resize it along its own axes (R).
    Scale,
}

impl Mode {
    /// Every mode, in toolbar order.
    pub const ALL: [Mode; 3] = [Mode::Translate, Mode::Rotate, Mode::Scale];

    /// The toolbar button's text.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Translate => "Move",
            Mode::Rotate => "Rotate",
            Mode::Scale => "Scale",
        }
    }

    /// The key that picks this mode while the pointer is over the viewport.
    pub fn key(self) -> Key {
        match self {
            Mode::Translate => Key::W,
            Mode::Rotate => Key::E,
            Mode::Scale => Key::R,
        }
    }
}

/// The primary pointer button as the gizmo sees it for one frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pointer {
    /// Where the pointer is, in screen points; `None` outside the window.
    pub pos: Option<Pos2>,
    /// Whether the pointer is over the viewport and not over another panel
    /// or popup.
    pub over_viewport: bool,
    /// Whether the primary button went down this frame.
    pub pressed: bool,
    /// Whether the primary button is held.
    pub down: bool,
}

/// A handle drag in progress.
#[derive(Clone, Copy, Debug)]
struct Drag {
    entity: Entity,
    /// The entity's transform before the drag, which undo goes back to.
    original: TransformDesc,
    /// The latest previewed transform, committed on release.
    last: TransformDesc,
}

/// The viewport's gizmo: its mode and the drag in progress.
#[derive(Default)]
pub struct TransformGizmo {
    gizmo: Gizmo,
    /// Which handles are shown.
    pub mode: Mode,
    drag: Option<Drag>,
    /// Whether the primary button went down on a handle and is still held;
    /// the orbit camera and click picking leave the pointer alone then.
    grabbed: bool,
}

impl TransformGizmo {
    /// Whether the pointer was pressed on a handle and is still held.
    pub fn has_pointer(&self) -> bool {
        self.grabbed
    }

    /// Whether a handle is under `pos` (screen points), as of the last
    /// [`Self::update`].
    #[cfg(test)]
    pub fn handle_at(&self, pos: Pos2) -> bool {
        self.gizmo.pick_preview((pos.x, pos.y))
    }

    /// Runs the gizmo for one frame on `entity` (the selection, `None` to
    /// hide the gizmo) as seen through `camera` in the viewport `rect`
    /// (screen points, `pixels_per_point` pixels each), with Ctrl `snap`.
    /// Pushes a preview while a handle is dragged and the edit when it is
    /// released. Returns what to draw, if anything.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        engine: &Headless,
        entity: Option<Entity>,
        camera: &Camera,
        rect: Rect,
        pixels_per_point: f32,
        pointer: Pointer,
        snap: bool,
        actions: &mut Vec<Action>,
    ) -> Option<GizmoDrawData> {
        let entity = entity.filter(|&e| engine.world.get::<&Transform>(e).is_ok());
        if self.drag.is_some_and(|drag| Some(drag.entity) != entity) {
            self.finish(engine, actions);
        }
        let Some(entity) = entity.filter(|_| rect.is_positive()) else {
            self.grabbed = false;
            return None;
        };

        let aspect = rect.width() / rect.height();
        self.gizmo.update_config(GizmoConfig {
            view_matrix: dmat4(camera.view_matrix()).into(),
            projection_matrix: dmat4(camera.projection_matrix(aspect)).into(),
            viewport: rect,
            modes: match self.mode {
                Mode::Translate => GizmoMode::all_translate(),
                Mode::Rotate => GizmoMode::all_rotate(),
                Mode::Scale => GizmoMode::all_scale(),
            },
            // Scale along the entity's own axes: a world-axis scale of a
            // turned entity cannot be stored in a `Transform`.
            orientation: match self.mode {
                Mode::Scale => GizmoOrientation::Local,
                Mode::Translate | Mode::Rotate => GizmoOrientation::Global,
            },
            snapping: snap,
            snap_distance: SNAP_DISTANCE,
            snap_angle: SNAP_ANGLE_DEG.to_radians(),
            snap_scale: SNAP_SCALE,
            pixels_per_point,
            ..*self.gizmo.config()
        });

        let target = to_gizmo(&world_transform(&engine.world, entity));
        let cursor = pointer.pos.unwrap_or(Pos2::ZERO);
        let result = self.gizmo.update(
            GizmoInteraction {
                cursor_pos: (cursor.x, cursor.y),
                hovered: pointer.pos.is_some() && (pointer.over_viewport || self.grabbed),
                drag_started: pointer.pressed && pointer.over_viewport,
                dragging: pointer.down,
            },
            &[target],
        );
        if pointer.pressed && pointer.over_viewport && self.gizmo.is_focused() {
            self.grabbed = true;
        }
        if self.grabbed
            && let Some((_, moved)) = result
            && let Some(local) = local_from_world(&engine.world, entity, from_gizmo(&moved[0]))
        {
            let desc = TransformDesc::from_transform(&local);
            let drag = self.drag.get_or_insert_with(|| Drag {
                entity,
                original: current_desc(engine, entity),
                last: desc,
            });
            drag.last = desc;
            actions.push(Action::PreviewTransform(entity, desc));
        }
        if !pointer.down {
            self.grabbed = false;
            self.finish(engine, actions);
        }
        Some(self.gizmo.draw())
    }

    /// Ends the drag in progress, if any, with the edit that records it.
    /// A drag that changed nothing, or whose entity is gone, records
    /// nothing.
    fn finish(&mut self, engine: &Headless, actions: &mut Vec<Action>) {
        if let Some(drag) = self.drag.take()
            && drag.last != drag.original
            && engine.world.contains(drag.entity)
        {
            actions.push(Action::Edit(Edit::Transform {
                entity: drag.entity,
                desc: drag.last,
                original: drag.original,
            }));
        }
    }
}

/// `entity`'s own transform as the Inspector shows it.
fn current_desc(engine: &Headless, entity: Entity) -> TransformDesc {
    engine
        .world
        .get::<&Transform>(entity)
        .map(|t| TransformDesc::from_transform(&t))
        .unwrap_or_default()
}

/// The local transform that puts `entity` at `world` (world space): `world`
/// itself for a root, `world` relative to the parent's world transform for
/// a child. `None` when that is not finite (a parent with a zero scale).
pub fn local_from_world(world: &World, entity: Entity, placement: Transform) -> Option<Transform> {
    let local = match parent_of(world, entity) {
        Some(parent) => placement.relative_to(&world_transform(world, parent)),
        None => placement,
    };
    local.is_finite().then_some(local)
}

/// Draws the gizmo's triangles, clipped to the viewport.
pub fn paint(painter: &Painter, data: GizmoDrawData) {
    let vertices = data
        .vertices
        .into_iter()
        .zip(data.colors)
        .map(|(pos, [r, g, b, a])| Vertex {
            pos: pos.into(),
            uv: Pos2::ZERO,
            color: Rgba::from_rgba_premultiplied(r, g, b, a).into(),
        })
        .collect();
    painter.add(Mesh {
        indices: data.indices,
        vertices,
        ..Default::default()
    });
}

/// A glam 0.34 matrix as the gizmo's (glam 0.32, f64) type.
fn dmat4(m: Mat4) -> DMat4 {
    DMat4::from_cols_array(&m.to_cols_array().map(f64::from))
}

/// A [`Transform`] as the gizmo's.
fn to_gizmo(t: &Transform) -> GizmoTransform {
    let v = |v: Vec3| DVec3::from_array(v.to_array().map(f64::from));
    let r = t.rotation;
    GizmoTransform::from_scale_rotation_translation(
        v(t.scale),
        DQuat::from_xyzw(r.x.into(), r.y.into(), r.z.into(), r.w.into()),
        v(t.translation),
    )
}

/// The gizmo's transform as a [`Transform`].
fn from_gizmo(t: &GizmoTransform) -> Transform {
    let v = |v: DVec3| Vec3::new(v.x as f32, v.y as f32, v.z as f32);
    let r = DQuat::from(t.rotation);
    Transform::from_trs(
        v(t.translation.into()),
        Quat::from_xyzw(r.x as f32, r.y as f32, r.z as f32, r.w as f32).normalize(),
        v(t.scale.into()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use shin::command::{self, History};
    use shin::tool::ToolRegistry;

    use crate::edit;
    use crate::orbit::OrbitCamera;

    /// An 800×600 viewport at the screen origin.
    const RECT: Rect = Rect {
        min: Pos2::ZERO,
        max: Pos2::new(800.0, 600.0),
    };

    /// The default orbit view of the default camera.
    fn camera(engine: &Headless) -> Camera {
        let mut camera = *engine.resources.expect::<Camera>();
        camera.transform = OrbitCamera::default().transform();
        camera
    }

    /// Where `point` appears in [`RECT`].
    fn screen(camera: &Camera, point: Vec3) -> Pos2 {
        let ndc = camera
            .view_projection(RECT.width() / RECT.height())
            .project_point3(point);
        Pos2::new(
            (ndc.x + 1.0) / 2.0 * RECT.width(),
            (1.0 - ndc.y) / 2.0 * RECT.height(),
        )
    }

    /// Runs what the editor would for the gizmo's actions.
    fn apply(
        reg: &ToolRegistry,
        engine: &mut Headless,
        actions: Vec<Action>,
    ) -> anyhow::Result<()> {
        for action in actions {
            match action {
                Action::PreviewTransform(entity, desc) => {
                    edit::preview_transform(engine, entity, &desc)
                }
                Action::Edit(Edit::Transform {
                    entity,
                    desc,
                    original,
                }) => {
                    edit::preview_transform(engine, entity, &original);
                    edit::commit_transform(reg, engine, entity, &desc)?;
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        Ok(())
    }

    /// One frame of the gizmo on `entity`; returns its actions.
    fn step(
        gizmo: &mut TransformGizmo,
        engine: &Headless,
        entity: Entity,
        pointer: Pointer,
        snap: bool,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        let camera = camera(engine);
        gizmo.update(
            engine,
            Some(entity),
            &camera,
            RECT,
            1.0,
            pointer,
            snap,
            &mut actions,
        );
        actions
    }

    /// A point on the X arrow of the translate gizmo on `entity`: walks
    /// along the projected +X axis away from the centre until the gizmo
    /// would pick a handle (past the centre's view-plane circle).
    fn x_handle(gizmo: &mut TransformGizmo, engine: &Headless, entity: Entity) -> Pos2 {
        let camera = camera(engine);
        let centre = world_transform(&engine.world, entity).translation;
        let from = screen(&camera, centre);
        let along = (screen(&camera, centre + Vec3::X) - from).normalized();
        let _ = step(gizmo, engine, entity, Pointer::default(), false);
        (30..150)
            .map(|d| from + along * d as f32)
            .find(|&p| gizmo.handle_at(p))
            .expect("the X arrow is on the projected X axis")
    }

    /// Presses on `from`, drags `by` points in `frames` steps, releases,
    /// and applies every frame's actions; returns all the actions.
    fn drag(
        gizmo: &mut TransformGizmo,
        reg: &ToolRegistry,
        engine: &mut Headless,
        entity: Entity,
        from: Pos2,
        by: egui::Vec2,
        snap: bool,
    ) -> anyhow::Result<Vec<Action>> {
        const FRAMES: usize = 4;
        let mut all = Vec::new();
        let mut run = |engine: &mut Headless, pointer: Pointer| -> anyhow::Result<()> {
            let actions = step(gizmo, engine, entity, pointer, snap);
            all.extend(actions.clone());
            apply(reg, engine, actions)
        };
        let at = |pos: Pos2, pressed, down| Pointer {
            pos: Some(pos),
            over_viewport: true,
            pressed,
            down,
        };
        run(engine, at(from, true, true))?;
        for i in 1..=FRAMES {
            run(
                engine,
                at(from + by * (i as f32 / FRAMES as f32), false, true),
            )?;
        }
        run(engine, at(from + by, false, false))?;
        Ok(all)
    }

    fn setup() -> anyhow::Result<(ToolRegistry, Headless, Entity)> {
        let reg = ToolRegistry::with_engine_tools();
        let mut engine = Headless::new(8, 8)?;
        let cube = edit::spawn(&reg, &mut engine, "builtin://cube", Vec3::ZERO, None, None)?;
        Ok((reg, engine, cube))
    }

    fn undo_len(engine: &Headless) -> usize {
        engine
            .resources
            .get::<History>()
            .map_or(0, History::undo_len)
    }

    /// The ticket's gizmo criterion without a window: dragging the X
    /// arrow previews every frame, records one `entity.set_transform` on
    /// release, moves only along X, and one undo puts the entity back.
    #[test]
    fn gizmo_drag_commits_one_history_entry() -> anyhow::Result<()> {
        let (reg, mut engine, cube) = setup()?;
        let before = *engine.world.get::<&Transform>(cube)?;
        let entries = undo_len(&engine);
        let mut gizmo = TransformGizmo::default();
        let handle = x_handle(&mut gizmo, &engine, cube);
        let actions = drag(
            &mut gizmo,
            &reg,
            &mut engine,
            cube,
            handle,
            egui::vec2(120.0, 0.0),
            false,
        )?;

        let previews = actions
            .iter()
            .filter(|a| matches!(a, Action::PreviewTransform(..)))
            .count();
        let edits: Vec<_> = actions
            .iter()
            .filter(|a| matches!(a, Action::Edit(_)))
            .collect();
        assert!(previews >= 4, "{actions:?}");
        assert_eq!(edits.len(), 1, "{actions:?}");
        assert!(!gizmo.has_pointer(), "released");
        assert_eq!(undo_len(&engine), entries + 1, "one entry for the drag");
        let after = *engine.world.get::<&Transform>(cube)?;
        assert!(after.translation.x > 0.5, "moved along +X: {after:?}");
        assert_eq!((after.translation.y, after.translation.z), (0.0, 0.0));

        command::undo(&mut engine)?;
        assert_eq!(*engine.world.get::<&Transform>(cube)?, before);
        Ok(())
    }

    /// Ctrl snaps the move to whole steps of [`SNAP_DISTANCE`].
    #[test]
    fn gizmo_ctrl_snaps_translation() -> anyhow::Result<()> {
        let (reg, mut engine, cube) = setup()?;
        let mut gizmo = TransformGizmo::default();
        let handle = x_handle(&mut gizmo, &engine, cube);
        drag(
            &mut gizmo,
            &reg,
            &mut engine,
            cube,
            handle,
            egui::vec2(97.0, 0.0),
            true,
        )?;
        let x = engine.world.get::<&Transform>(cube)?.translation.x;
        let steps = x / SNAP_DISTANCE;
        assert!(x > 0.0 && (steps - steps.round()).abs() < 1e-4, "{x}");
        Ok(())
    }

    /// A press away from the handles neither grabs the pointer nor edits,
    /// and nothing is shown without a selection.
    #[test]
    fn gizmo_ignores_presses_off_the_handles() -> anyhow::Result<()> {
        let (reg, mut engine, cube) = setup()?;
        let mut gizmo = TransformGizmo::default();
        let corner = Pos2::new(20.0, 20.0);
        let actions = drag(
            &mut gizmo,
            &reg,
            &mut engine,
            cube,
            corner,
            egui::vec2(50.0, 30.0),
            false,
        )?;
        assert!(actions.is_empty(), "{actions:?}");
        assert!(!gizmo.has_pointer());

        let camera = camera(&engine);
        let mut actions = Vec::new();
        let shown = gizmo.update(
            &engine,
            None,
            &camera,
            RECT,
            1.0,
            Pointer::default(),
            false,
            &mut actions,
        );
        assert!(shown.is_none());
        Ok(())
    }

    /// The gizmo's world-space result becomes the child's local transform
    /// through a turned, scaled parent, and composing it back gives the
    /// world placement.
    #[test]
    fn gizmo_world_to_local_under_a_rotated_parent() -> anyhow::Result<()> {
        let (reg, mut engine, parent) = setup()?;
        edit::commit_transform(
            &reg,
            &mut engine,
            parent,
            &TransformDesc {
                translation: [1.0, 2.0, 3.0],
                rotation_euler_deg: [0.0, 90.0, 0.0],
                scale: [2.0; 3],
            },
        )?;
        let child = edit::spawn(
            &reg,
            &mut engine,
            "builtin://cube",
            Vec3::ZERO,
            None,
            Some(parent),
        )?;
        let target = Transform::from_trs(
            Vec3::new(1.0, 2.0, 1.0),
            Quat::from_rotation_y(90f32.to_radians()),
            Vec3::splat(2.0),
        );
        let local = local_from_world(&engine.world, child, target).expect("finite");
        // Two units along world -Z from the parent is one unit along its
        // local +X (turned 90° about Y, scaled 2).
        assert!(local.translation.abs_diff_eq(Vec3::X, 1e-5), "{local:?}");
        assert!(
            local.rotation.abs_diff_eq(Quat::IDENTITY, 1e-5),
            "{local:?}"
        );
        assert!(local.scale.abs_diff_eq(Vec3::ONE, 1e-5), "{local:?}");
        let parent_world = world_transform(&engine.world, parent);
        let back = parent_world.mul_transform(&local);
        assert!(back.translation.abs_diff_eq(target.translation, 1e-5));

        // A root's local transform is the world one.
        assert_eq!(
            local_from_world(&engine.world, parent, target),
            Some(target)
        );
        Ok(())
    }

    /// Dragging the X arrow of a child of a turned parent moves the child
    /// along world X, which is the parent's local Z.
    #[test]
    fn gizmo_drag_moves_a_child_along_world_x() -> anyhow::Result<()> {
        let (reg, mut engine, parent) = setup()?;
        edit::commit_transform(
            &reg,
            &mut engine,
            parent,
            &TransformDesc {
                translation: [0.0, 0.0, -3.0],
                rotation_euler_deg: [0.0, 90.0, 0.0],
                scale: [1.0; 3],
            },
        )?;
        let child = edit::spawn(
            &reg,
            &mut engine,
            "builtin://cube",
            Vec3::ZERO,
            None,
            Some(parent),
        )?;
        let mut gizmo = TransformGizmo::default();
        let handle = x_handle(&mut gizmo, &engine, child);
        drag(
            &mut gizmo,
            &reg,
            &mut engine,
            child,
            handle,
            egui::vec2(100.0, 0.0),
            false,
        )?;
        let world = world_transform(&engine.world, child).translation;
        assert!(world.x > 0.5, "{world}");
        assert!(world.y.abs() < 1e-4 && world.z.abs() < 1e-4, "{world}");
        let local = engine.world.get::<&Transform>(child)?.translation;
        // It started three units along the parent's local -X.
        assert!((local.x + 3.0).abs() < 1e-4 && local.z > 0.5, "{local}");
        Ok(())
    }
}
