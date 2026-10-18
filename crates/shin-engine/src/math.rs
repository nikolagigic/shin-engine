//! glam re-exports, [`Transform`], and [`Camera`].
//!
//! Conventions used across the engine: right-handed, +Y up, -Z forward
//! (glTF), column-major matrices, and wgpu's clip-space depth range `0..1`.

pub use glam::{Mat3, Mat4, Quat, Vec2, Vec3, Vec4};

// glam 0.34 moved projections off `Mat4`. `directx` is the WebGPU convention:
// right-handed view space in, NDC with +Y up and depth 0..1 out.
use glam::camera::rh::proj::directx;

/// Position, orientation, and scale of an object in 3D space.
///
/// Applied in scale, then rotation, then translation order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    /// Position in parent (or world) space.
    pub translation: Vec3,
    /// Orientation; a unit quaternion.
    pub rotation: Quat,
    /// Per-axis scale factor.
    pub scale: Vec3,
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform {
    /// No translation, no rotation, unit scale.
    pub const IDENTITY: Self = Self {
        translation: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        scale: Vec3::ONE,
    };

    /// An identity transform moved to `t`.
    pub fn from_translation(t: Vec3) -> Self {
        Self {
            translation: t,
            ..Self::IDENTITY
        }
    }

    /// A transform from translation, rotation, and scale.
    pub fn from_trs(t: Vec3, r: Quat, s: Vec3) -> Self {
        Self {
            translation: t,
            rotation: r,
            scale: s,
        }
    }

    /// The local-to-parent matrix: `T * R * S`.
    pub fn to_matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }

    /// `self * local`: where something placed at `local` inside this
    /// transform's space ends up, as a parent and a child compose (GE-031).
    ///
    /// Translation, rotation, and scale compose separately (scale per
    /// axis). That equals multiplying the matrices unless `self` has a
    /// non-uniform scale *and* `local` is rotated; the shear that
    /// combination produces cannot be stored in a `Transform` and is
    /// dropped.
    pub fn mul_transform(&self, local: &Transform) -> Transform {
        Transform {
            translation: self.translation + self.rotation * (self.scale * local.translation),
            rotation: self.rotation * local.rotation,
            scale: self.scale * local.scale,
        }
    }

    /// The local transform that, placed inside `parent`
    /// ([`Self::mul_transform`]), gives `self`: the inverse of composing.
    /// Scale is divided per axis, so a zero component of `parent.scale`
    /// gives non-finite values.
    pub fn relative_to(&self, parent: &Transform) -> Transform {
        let inverse_rotation = parent.rotation.inverse();
        Transform {
            translation: (inverse_rotation * (self.translation - parent.translation))
                / parent.scale,
            rotation: inverse_rotation * self.rotation,
            scale: self.scale / parent.scale,
        }
    }

    /// Whether every component is finite.
    pub fn is_finite(&self) -> bool {
        self.translation.is_finite() && self.rotation.is_finite() && self.scale.is_finite()
    }

    /// The direction this transform faces (local -Z).
    pub fn forward(&self) -> Vec3 {
        self.rotation * Vec3::NEG_Z
    }

    /// The direction to this transform's right (local +X).
    pub fn right(&self) -> Vec3 {
        self.rotation * Vec3::X
    }

    /// The direction above this transform (local +Y).
    pub fn up(&self) -> Vec3 {
        self.rotation * Vec3::Y
    }

    /// Rotates in place so that [`forward`](Self::forward) points at `target`
    /// and [`up`](Self::up) is as close to `up` as possible.
    ///
    /// Leaves the rotation unchanged if `target` equals the translation or
    /// the view direction is parallel to `up`.
    pub fn looking_at(&mut self, target: Vec3, up: Vec3) {
        let Some(forward) = (target - self.translation).try_normalize() else {
            return;
        };
        let Some(right) = forward.cross(up).try_normalize() else {
            return;
        };
        let up = right.cross(forward);
        // Local +Z points backwards, away from the target.
        self.rotation = Quat::from_mat3(&Mat3::from_cols(right, up, -forward));
    }
}

/// A viewpoint into the scene: where it is and how it projects.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    /// Camera placement; the camera looks along `transform.forward()`.
    pub transform: Transform,
    /// How view space maps to clip space.
    pub projection: Projection,
}

/// Projection from view space to clip space, with depth in `0..1`.
#[derive(Clone, Copy, Debug)]
pub enum Projection {
    /// Perspective projection with a vertical field of view.
    Perspective {
        /// Vertical field of view in radians.
        fov_y_radians: f32,
        /// Distance to the near clip plane (maps to depth 0).
        near: f32,
        /// Distance to the far clip plane (maps to depth 1).
        far: f32,
    },
    /// Orthographic projection with a fixed visible height.
    Orthographic {
        /// Visible height in world units; width is `height * aspect`.
        height: f32,
        /// Distance to the near clip plane (maps to depth 0).
        near: f32,
        /// Distance to the far clip plane (maps to depth 1).
        far: f32,
    },
}

impl Camera {
    /// World-to-view matrix: the inverse of the camera's transform matrix.
    pub fn view_matrix(&self) -> Mat4 {
        self.transform.to_matrix().inverse()
    }

    /// View-to-clip matrix for a viewport of the given `aspect` (width / height).
    pub fn projection_matrix(&self, aspect: f32) -> Mat4 {
        match self.projection {
            Projection::Perspective {
                fov_y_radians,
                near,
                far,
            } => directx::perspective(fov_y_radians, aspect, near, far),
            Projection::Orthographic { height, near, far } => {
                let half_h = height * 0.5;
                let half_w = half_h * aspect;
                directx::orthographic(-half_w, half_w, -half_h, half_h, near, far)
            }
        }
    }

    /// World-to-clip matrix: `projection_matrix(aspect) * view_matrix()`.
    pub fn view_projection(&self, aspect: f32) -> Mat4 {
        self.projection_matrix(aspect) * self.view_matrix()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1e-5;

    fn camera_at_z5(near: f32, far: f32) -> Camera {
        let mut transform = Transform::from_translation(Vec3::new(0.0, 0.0, 5.0));
        transform.looking_at(Vec3::ZERO, Vec3::Y);
        Camera {
            transform,
            projection: Projection::Perspective {
                fov_y_radians: 60f32.to_radians(),
                near,
                far,
            },
        }
    }

    fn ndc_depth(m: Mat4, p: Vec3) -> Vec4 {
        let clip = m * p.extend(1.0);
        clip / clip.w
    }

    #[test]
    fn default_is_identity_matrix() {
        assert_eq!(Transform::default().to_matrix(), Mat4::IDENTITY);
        assert_eq!(Transform::default(), Transform::IDENTITY);
    }

    #[test]
    fn translation_moves_origin() {
        let m = Transform::from_translation(Vec3::new(1.0, 2.0, 3.0)).to_matrix();
        assert_eq!(m.transform_point3(Vec3::ZERO), Vec3::new(1.0, 2.0, 3.0));
    }

    #[test]
    fn rotation_about_y_turns_forward_to_neg_x() {
        let t = Transform::from_trs(
            Vec3::ZERO,
            Quat::from_rotation_y(90f32.to_radians()),
            Vec3::ONE,
        );
        assert!(t.forward().abs_diff_eq(Vec3::NEG_X, EPS), "{}", t.forward());
        assert!(t.right().abs_diff_eq(Vec3::NEG_Z, EPS), "{}", t.right());
        assert!(t.up().abs_diff_eq(Vec3::Y, EPS), "{}", t.up());
    }

    #[test]
    fn looking_at_origin_from_pos_z_faces_neg_z() {
        let mut t = Transform::from_translation(Vec3::new(0.0, 0.0, 5.0));
        t.looking_at(Vec3::ZERO, Vec3::Y);
        assert!(t.forward().abs_diff_eq(Vec3::NEG_Z, EPS), "{}", t.forward());
        assert!(t.up().abs_diff_eq(Vec3::Y, EPS), "{}", t.up());
    }

    #[test]
    fn looking_at_arbitrary_target_points_forward_at_it() {
        let mut t = Transform::from_translation(Vec3::new(3.0, 4.0, -2.0));
        let target = Vec3::new(-1.0, 0.5, 7.0);
        t.looking_at(target, Vec3::Y);
        let expected = (target - t.translation).normalize();
        assert!(t.forward().abs_diff_eq(expected, EPS), "{}", t.forward());
        assert!(t.right().y.abs() < EPS, "right should stay horizontal");
    }

    #[test]
    fn looking_at_degenerate_keeps_rotation() {
        let mut t = Transform::from_translation(Vec3::new(0.0, 5.0, 0.0));
        t.looking_at(Vec3::ZERO, Vec3::Y);
        assert_eq!(t.rotation, Quat::IDENTITY);
        t.looking_at(t.translation, Vec3::Y);
        assert_eq!(t.rotation, Quat::IDENTITY);
    }

    #[test]
    fn mul_transform_places_a_child_in_the_parent_space() {
        // The GE-031 case: a parent at (1, 0, 0) turned a quarter turn
        // about Y carries a child at local (1, 0, 0) to world (1, 0, -1).
        let parent = Transform::from_trs(
            Vec3::X,
            Quat::from_rotation_y(90f32.to_radians()),
            Vec3::ONE,
        );
        let child = Transform::from_translation(Vec3::X);
        let world = parent.mul_transform(&child);
        assert!(
            world
                .translation
                .abs_diff_eq(Vec3::new(1.0, 0.0, -1.0), EPS),
            "{}",
            world.translation
        );
        // Composing agrees with the matrices when the scale is uniform.
        let parent = Transform::from_trs(
            Vec3::new(1.0, 2.0, 3.0),
            Quat::from_euler(glam::EulerRot::XYZ, 0.3, -1.1, 2.0),
            Vec3::splat(2.0),
        );
        let child = Transform::from_trs(
            Vec3::new(-0.5, 4.0, 1.5),
            Quat::from_rotation_x(0.9),
            Vec3::new(0.5, 1.5, 3.0),
        );
        let composed = parent.mul_transform(&child).to_matrix();
        let product = parent.to_matrix() * child.to_matrix();
        assert!(
            composed.abs_diff_eq(product, 1e-4),
            "{composed} vs {product}"
        );
        assert_eq!(Transform::IDENTITY.mul_transform(&child), child);
    }

    #[test]
    fn relative_to_inverts_mul_transform() {
        let parent = Transform::from_trs(
            Vec3::new(1.0, 2.0, 3.0),
            Quat::from_euler(glam::EulerRot::XYZ, 0.3, -1.1, 2.0),
            Vec3::new(2.0, 0.5, 4.0),
        );
        let child = Transform::from_trs(
            Vec3::new(-0.5, 4.0, 1.5),
            Quat::from_rotation_x(0.9),
            Vec3::new(0.5, 1.5, 3.0),
        );
        let back = parent.mul_transform(&child).relative_to(&parent);
        assert!(
            back.translation.abs_diff_eq(child.translation, EPS),
            "{back:?}"
        );
        assert!(back.rotation.abs_diff_eq(child.rotation, EPS), "{back:?}");
        assert!(back.scale.abs_diff_eq(child.scale, EPS), "{back:?}");
        assert!(back.is_finite());
        let zero_scale = Transform::from_trs(Vec3::ZERO, Quat::IDENTITY, Vec3::ZERO);
        assert!(!child.relative_to(&zero_scale).is_finite());
    }

    #[test]
    fn perspective_depth_range_is_zero_to_one() {
        let (near, far) = (0.1, 100.0);
        let vp = camera_at_z5(near, far).view_projection(1.0);

        let origin = ndc_depth(vp, Vec3::ZERO);
        assert!(origin.x.abs() < EPS && origin.y.abs() < EPS, "{origin}");
        assert!((0.0..=1.0).contains(&origin.z), "{origin}");

        let at_near = ndc_depth(vp, Vec3::new(0.0, 0.0, 5.0 - near));
        assert!(at_near.z.abs() < EPS, "{at_near}");

        let past_far = ndc_depth(vp, Vec3::new(0.0, 0.0, 5.0 - far * 1.5));
        assert!(past_far.z > 1.0, "{past_far}");
    }

    #[test]
    fn orthographic_maps_box_edges_to_ndc_edges() {
        let camera = Camera {
            transform: Transform::IDENTITY,
            projection: Projection::Orthographic {
                height: 4.0,
                near: 1.0,
                far: 11.0,
            },
        };
        let vp = camera.view_projection(2.0);
        let corner = ndc_depth(vp, Vec3::new(4.0, 2.0, -1.0));
        assert!(
            corner.abs_diff_eq(Vec4::new(1.0, 1.0, 0.0, 1.0), EPS),
            "{corner}"
        );
        let far = ndc_depth(vp, Vec3::new(0.0, 0.0, -11.0));
        assert!((far.z - 1.0).abs() < EPS, "{far}");
    }

    #[test]
    fn view_matrix_inverts_transform() {
        let samples = [
            Transform::from_trs(
                Vec3::new(1.5, -2.0, 7.25),
                Quat::from_euler(glam::EulerRot::YXZ, 0.7, -1.2, 2.9),
                Vec3::new(0.5, 2.0, 1.25),
            ),
            Transform::from_trs(
                Vec3::new(-30.0, 0.1, -4.0),
                Quat::from_axis_angle(Vec3::new(1.0, 2.0, 3.0).normalize(), 4.0),
                Vec3::splat(3.0),
            ),
        ];
        for transform in samples {
            let camera = Camera {
                transform,
                projection: Projection::Perspective {
                    fov_y_radians: 1.0,
                    near: 0.1,
                    far: 10.0,
                },
            };
            let product = camera.view_matrix() * transform.to_matrix();
            assert!(product.abs_diff_eq(Mat4::IDENTITY, EPS), "{product}");
        }
    }
}
