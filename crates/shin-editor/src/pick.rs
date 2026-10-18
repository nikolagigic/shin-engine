//! Viewport picking (GE-035): which entity is under the pointer.
//!
//! The pointer becomes a [`Ray`] through the viewport camera
//! ([`Ray::through`]); every entity that draws a mesh is tested as an
//! oriented box, its mesh's object-space [`Aabb`]
//! ([`shin::render::MeshRegistry::bounds`]) placed by its world transform
//! ([`world_transform`]), and the nearest hit wins ([`pick`]).

use shin::ecs::{Entity, World, world_transform};
use shin::headless::Headless;
use shin::math::{Camera, Transform, Vec2, Vec3};
use shin::render::{Aabb, MeshInstance, MeshRegistry, RenderState};

use crate::orbit::OrbitCamera;

/// A segment from the camera's near plane to its far plane along the
/// pointer: points are `origin + t * dir` for `t` in `0..=1`, so `t`
/// orders hits by distance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ray {
    /// The point on the near plane.
    pub origin: Vec3,
    /// From the near-plane point to the far-plane point (not normalised).
    pub dir: Vec3,
}

impl Ray {
    /// The ray through `ndc` (normalised device coordinates: -1..1, +y up)
    /// of `camera` drawing into a viewport of `aspect` (width / height).
    pub fn through(camera: &Camera, aspect: f32, ndc: Vec2) -> Self {
        let inverse = camera.view_projection(aspect).inverse();
        // wgpu depth runs 0 (near) to 1 (far).
        let near = inverse.project_point3(ndc.extend(0.0));
        let far = inverse.project_point3(ndc.extend(1.0));
        Self {
            origin: near,
            dir: far - near,
        }
    }
}

/// `pos` in a viewport covering `rect` (`[min, max]` in screen points, +y
/// down) as normalised device coordinates (+y up).
pub fn ndc(rect: [Vec2; 2], pos: Vec2) -> Vec2 {
    let [min, max] = rect;
    let unit = (pos - min) / (max - min);
    Vec2::new(unit.x * 2.0 - 1.0, 1.0 - unit.y * 2.0)
}

/// Where along `ray` (its `t`, `0..=1`) it first meets `bounds` placed by
/// `placement`, if it does. A ray starting inside the box hits at 0. A
/// placement that cannot be inverted (a zero scale) is never hit.
pub fn hit_box(ray: &Ray, placement: &Transform, bounds: &Aabb) -> Option<f32> {
    let to_local = placement.to_matrix().inverse();
    if !to_local.is_finite() {
        return None;
    }
    // An affine map keeps `t`, so the box can be tested in object space.
    let origin = to_local.transform_point3(ray.origin);
    let dir = to_local.transform_vector3(ray.dir);
    let (mut enter, mut exit) = (0.0f32, 1.0f32);
    for axis in 0..3 {
        let (o, d) = (origin[axis], dir[axis]);
        let (lo, hi) = (bounds.min[axis], bounds.max[axis]);
        if d.abs() < f32::EPSILON {
            // Parallel to this pair of faces: inside the slab or never.
            if o < lo || o > hi {
                return None;
            }
            continue;
        }
        let (a, b) = ((lo - o) / d, (hi - o) / d);
        enter = enter.max(a.min(b));
        exit = exit.min(a.max(b));
        if enter > exit {
            return None;
        }
    }
    Some(enter)
}

/// The entity drawing a mesh that `ray` hits first, if any. Entities whose
/// mesh is not in `meshes` are skipped.
pub fn pick(world: &World, meshes: &MeshRegistry, ray: &Ray) -> Option<Entity> {
    world
        .query::<(Entity, &MeshInstance)>()
        .iter()
        .filter_map(|(entity, instance)| {
            let bounds = meshes.bounds(instance.mesh)?;
            let t = hit_box(ray, &world_transform(world, entity), &bounds)?;
            Some((t, entity))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, entity)| entity)
}

/// The camera the viewport shows: the scene's [`Camera`] (its projection
/// always, its placement while playing) with the orbit camera's placement
/// while editing (GE-030). `None` when the scene has no camera.
pub fn viewport_camera(engine: &Headless, orbit: &OrbitCamera) -> Option<Camera> {
    let mut camera = *engine.resources.get::<Camera>()?;
    if !engine.play_state().is_playing() {
        camera.transform = orbit.transform();
    }
    Some(camera)
}

/// The entity under the point `ndc` of the viewport (`aspect` = width /
/// height), seen through `camera`.
pub fn pick_at(engine: &Headless, camera: &Camera, aspect: f32, ndc: Vec2) -> Option<Entity> {
    let state = engine.resources.get::<RenderState>()?;
    pick(
        &engine.world,
        &state.registry,
        &Ray::through(camera, aspect, ndc),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use shin::math::{Projection, Quat};
    use shin::tool::ToolRegistry;

    use crate::edit;

    /// At (0, 0, 5) looking down -Z with a 60° field of view.
    fn camera() -> Camera {
        Camera {
            transform: Transform::from_translation(Vec3::new(0.0, 0.0, 5.0)),
            projection: Projection::Perspective {
                fov_y_radians: 60f32.to_radians(),
                near: 0.1,
                far: 100.0,
            },
        }
    }

    fn unit_box() -> Aabb {
        Aabb {
            min: Vec3::splat(-0.5),
            max: Vec3::splat(0.5),
        }
    }

    /// `point` projected to normalised device coordinates.
    fn project(camera: &Camera, point: Vec3) -> Vec2 {
        camera.view_projection(1.0).project_point3(point).truncate()
    }

    #[test]
    fn pick_ray_runs_from_near_to_far_through_the_pointer() {
        let camera = camera();
        let ray = Ray::through(&camera, 1.0, Vec2::ZERO);
        assert!(ray.origin.abs_diff_eq(Vec3::new(0.0, 0.0, 4.9), 1e-4));
        assert!(ray.dir.normalize().abs_diff_eq(Vec3::NEG_Z, 1e-5));
        let target = Vec3::new(1.0, -0.5, 0.0);
        let ray = Ray::through(&camera, 1.0, project(&camera, target));
        let to_target = (target - ray.origin).normalize();
        assert!(to_target.abs_diff_eq(ray.dir.normalize(), 1e-4));
    }

    #[test]
    fn pick_ndc_maps_the_rect_corners() {
        let rect = [Vec2::new(100.0, 50.0), Vec2::new(300.0, 150.0)];
        assert_eq!(ndc(rect, Vec2::new(100.0, 50.0)), Vec2::new(-1.0, 1.0));
        assert_eq!(ndc(rect, Vec2::new(300.0, 150.0)), Vec2::new(1.0, -1.0));
        assert_eq!(ndc(rect, Vec2::new(200.0, 100.0)), Vec2::ZERO);
    }

    #[test]
    fn pick_box_hits_rotated_and_scaled_boxes() {
        let camera = camera();
        let ray = Ray::through(&camera, 1.0, Vec2::ZERO);
        let t = hit_box(&ray, &Transform::IDENTITY, &unit_box()).expect("hit");
        let hit = ray.origin + ray.dir * t;
        assert!(hit.abs_diff_eq(Vec3::new(0.0, 0.0, 0.5), 1e-3), "{hit}");

        // Rotated 45° about Y, the nearest corner sticks out to z = √½.
        let turned = Transform::from_trs(
            Vec3::ZERO,
            Quat::from_rotation_y(45f32.to_radians()),
            Vec3::ONE,
        );
        let t = hit_box(&ray, &turned, &unit_box()).expect("hit");
        let z = (ray.origin + ray.dir * t).z;
        assert!((z - 0.5f32.sqrt()).abs() < 1e-3, "{z}");

        // Off to the side misses; stretched along X it hits.
        let side = Ray::through(&camera, 1.0, project(&camera, Vec3::new(1.5, 0.0, 0.0)));
        assert_eq!(hit_box(&side, &Transform::IDENTITY, &unit_box()), None);
        let wide = Transform::from_trs(Vec3::ZERO, Quat::IDENTITY, Vec3::new(4.0, 1.0, 1.0));
        assert!(hit_box(&side, &wide, &unit_box()).is_some());

        // A zero scale cannot be hit; behind the camera is not hit.
        let flat = Transform::from_trs(Vec3::ZERO, Quat::IDENTITY, Vec3::ZERO);
        assert_eq!(hit_box(&ray, &flat, &unit_box()), None);
        let behind = Transform::from_translation(Vec3::new(0.0, 0.0, 8.0));
        assert_eq!(hit_box(&ray, &behind, &unit_box()), None);
    }

    /// The ticket's picking maths: the pointer at a cube's projected centre
    /// selects it, off to the side selects nothing, and of two overlapping
    /// cubes the nearer one is selected, also as the child of a parent.
    #[test]
    fn pick_selects_the_nearest_cube_under_the_pointer() -> anyhow::Result<()> {
        let reg = ToolRegistry::with_engine_tools();
        let mut engine = Headless::new(8, 8)?;
        let camera = camera();
        let back = edit::spawn(&reg, &mut engine, "builtin://cube", Vec3::ZERO, None, None)?;
        let at = |point: Vec3| project(&camera, point);
        assert_eq!(pick_at(&engine, &camera, 1.0, at(Vec3::ZERO)), Some(back));
        let beside = at(Vec3::new(1.5, 0.0, 0.0));
        assert_eq!(pick_at(&engine, &camera, 1.0, beside), None);

        let front = edit::spawn(
            &reg,
            &mut engine,
            "builtin://cube",
            Vec3::new(0.4, 0.0, 2.0),
            None,
            None,
        )?;
        assert_eq!(pick_at(&engine, &camera, 1.0, at(Vec3::ZERO)), Some(front));
        // The front cube spans x -0.1..0.9, so the back cube's left edge
        // is still visible beside it.
        let edge = at(Vec3::new(-0.45, 0.0, 0.5));
        assert_eq!(pick_at(&engine, &camera, 1.0, edge), Some(back));

        // A child is picked where its world transform puts it: the parent
        // is turned 90° about Y, so the child's local +X is world -Z.
        let parent = edit::spawn(
            &reg,
            &mut engine,
            "builtin://cube",
            Vec3::new(0.0, 3.0, -10.0),
            None,
            None,
        )?;
        edit::commit_transform(
            &reg,
            &mut engine,
            parent,
            &shin::scene::TransformDesc {
                translation: [0.0, 3.0, -10.0],
                rotation_euler_deg: [0.0, 90.0, 0.0],
                scale: [1.0; 3],
            },
        )?;
        let child_at = Vec3::new(0.0, 3.0, -7.0);
        let child = edit::spawn(
            &reg,
            &mut engine,
            "builtin://cube",
            child_at,
            None,
            Some(parent),
        )?;
        assert_eq!(pick_at(&engine, &camera, 1.0, at(child_at)), Some(child));
        Ok(())
    }
}
