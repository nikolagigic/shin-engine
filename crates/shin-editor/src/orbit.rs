//! The editor's orbit camera: left-drag orbits around a target point,
//! right-drag pans the target, the wheel zooms.
//!
//! [`OrbitCamera`] holds only the orbit parameters; [`OrbitCamera::transform`]
//! is the resulting placement, which the editor swaps into the scene's
//! camera only for the viewport render, and [`OrbitCamera::look_from`]
//! takes a camera's placement back into orbit parameters (GE-030: the
//! scene's camera and this one stay apart and convert on request).

use shin::math::{Quat, Transform, Vec2, Vec3};

/// Radians of yaw or pitch per point of mouse drag.
const ORBIT_SPEED: f32 = 0.008;
/// Target movement per point of drag, as a fraction of the distance, so
/// panning feels the same at any zoom level.
const PAN_SPEED: f32 = 0.0015;
/// Zoom rate per point of scroll: the distance is multiplied by
/// `exp(-scroll * ZOOM_SPEED)`.
const ZOOM_SPEED: f32 = 0.0015;
/// Pitch stays strictly inside ±90° so the view never flips over the pole.
const MAX_PITCH: f32 = 89f32.to_radians();
/// Closest the camera may get to its target.
const MIN_DISTANCE: f32 = 0.1;
/// Farthest the camera may get from its target.
const MAX_DISTANCE: f32 = 500.0;
/// How far ahead of a camera [`OrbitCamera::look_from`] puts the target at
/// least, so a camera looking away from the old target still orbits
/// something in front of it (the same floor a scene file's `look_at` uses).
const MIN_LOOK_FROM_DISTANCE: f32 = 1.0;

/// A camera circling `target` at `distance`, described by yaw and pitch.
///
/// Yaw 0 and pitch 0 put the camera on the target's +Z side looking along
/// -Z; negative pitch lifts the camera so it looks down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OrbitCamera {
    /// The point the camera looks at and orbits around.
    pub target: Vec3,
    /// Rotation about +Y, in radians.
    pub yaw: f32,
    /// Rotation about the camera's right axis, in radians, within ±89°.
    pub pitch: f32,
    /// Distance from the target.
    pub distance: f32,
}

impl Default for OrbitCamera {
    /// Above and in front of the origin, looking down at a unit cube
    /// resting on y = 0.
    fn default() -> Self {
        Self {
            target: Vec3::new(0.0, 0.5, 0.0),
            yaw: 0.0,
            pitch: -25f32.to_radians(),
            distance: 6.0,
        }
    }
}

impl OrbitCamera {
    /// The camera's orientation.
    pub fn rotation(&self) -> Quat {
        Quat::from_rotation_y(self.yaw) * Quat::from_rotation_x(self.pitch)
    }

    /// Where the camera is and which way it faces; its forward points at
    /// [`Self::target`].
    pub fn transform(&self) -> Transform {
        let rotation = self.rotation();
        let translation = self.target + rotation * Vec3::Z * self.distance;
        Transform::from_trs(translation, rotation, Vec3::ONE)
    }

    /// Orbits by a drag of `delta` points (x right, y down): dragging
    /// right swings the camera left around the target, dragging down
    /// raises it.
    pub fn orbit(&mut self, delta: Vec2) {
        self.yaw -= delta.x * ORBIT_SPEED;
        self.pitch = (self.pitch - delta.y * ORBIT_SPEED).clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// Pans by a drag of `delta` points (x right, y down): the scene follows
    /// the pointer, so the target moves the opposite way in view space.
    pub fn pan(&mut self, delta: Vec2) {
        let t = self.transform();
        let scale = self.distance * PAN_SPEED;
        self.target += (-t.right() * delta.x + t.up() * delta.y) * scale;
    }

    /// Zooms by `scroll` points of wheel movement; positive (wheel up)
    /// moves closer.
    pub fn zoom(&mut self, scroll: f32) {
        self.distance =
            (self.distance * (-scroll * ZOOM_SPEED).exp()).clamp(MIN_DISTANCE, MAX_DISTANCE);
    }

    /// Moves this orbit so its camera stands where `placement` is and faces
    /// the same way (its scale is ignored). The new target is the point on
    /// the view ray nearest the old target, at least one unit ahead, so
    /// orbiting afterwards circles what the camera was looking at. A view
    /// straight up or down is clamped to ±89° of pitch like a drag.
    pub fn look_from(&mut self, placement: &Transform) {
        let position = placement.translation;
        let forward = placement.forward();
        self.pitch = forward
            .y
            .clamp(-1.0, 1.0)
            .asin()
            .clamp(-MAX_PITCH, MAX_PITCH);
        self.yaw = (-forward.x).atan2(-forward.z);
        self.distance = (self.target - position)
            .dot(forward)
            .clamp(MIN_LOOK_FROM_DISTANCE, MAX_DISTANCE);
        self.target = position + forward * self.distance;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        a.distance(b) < 1e-4
    }

    #[test]
    fn camera_faces_the_target_at_the_distance() {
        let orbit = OrbitCamera::default();
        let t = orbit.transform();
        assert!((t.translation.distance(orbit.target) - orbit.distance).abs() < 1e-4);
        let to_target = (orbit.target - t.translation).normalize();
        assert!(close(t.forward(), to_target), "forward {}", t.forward());
        assert!(
            t.translation.y > orbit.target.y,
            "negative pitch looks down"
        );
    }

    #[test]
    fn orbit_keeps_target_and_distance() {
        let mut orbit = OrbitCamera::default();
        let before = orbit.transform().translation;
        orbit.orbit(Vec2::new(120.0, -40.0));
        let t = orbit.transform();
        assert!(!close(t.translation, before), "the camera moved");
        assert!((t.translation.distance(orbit.target) - orbit.distance).abs() < 1e-4);
        assert!(close(
            t.forward(),
            (orbit.target - t.translation).normalize()
        ));
    }

    #[test]
    fn pitch_is_clamped() {
        let mut orbit = OrbitCamera::default();
        orbit.orbit(Vec2::new(0.0, 1e6));
        assert_eq!(orbit.pitch, -MAX_PITCH);
        orbit.orbit(Vec2::new(0.0, -1e6));
        assert_eq!(orbit.pitch, MAX_PITCH);
    }

    #[test]
    fn pan_moves_target_in_the_view_plane() {
        let mut orbit = OrbitCamera::default();
        let forward = orbit.transform().forward();
        let start = orbit.target;
        orbit.pan(Vec2::new(50.0, 0.0));
        let moved = orbit.target - start;
        assert!(moved.length() > 0.0);
        assert!(
            moved.dot(forward).abs() < 1e-5,
            "no movement along the view"
        );
        let right = orbit.transform().right();
        assert!(
            moved.dot(right) < 0.0,
            "dragging right moves the target left"
        );

        let start = orbit.target;
        orbit.pan(Vec2::new(0.0, 50.0));
        let up = orbit.transform().up();
        assert!(
            (orbit.target - start).dot(up) > 0.0,
            "dragging down moves it up"
        );
    }

    #[test]
    fn zoom_scales_distance_within_limits() {
        let mut orbit = OrbitCamera::default();
        orbit.zoom(100.0);
        assert!(orbit.distance < 6.0, "wheel up moves closer");
        orbit.zoom(-200.0);
        assert!(orbit.distance > 6.0, "wheel down moves away");
        orbit.zoom(1e6);
        assert_eq!(orbit.distance, MIN_DISTANCE);
        orbit.zoom(-1e6);
        assert_eq!(orbit.distance, MAX_DISTANCE);
    }

    #[test]
    fn look_from_round_trips_the_placement() {
        let from = OrbitCamera {
            target: Vec3::new(1.0, 2.0, -3.0),
            yaw: 2.5,
            pitch: -0.7,
            distance: 4.0,
        };
        let placement = from.transform();
        let mut orbit = OrbitCamera::default();
        orbit.look_from(&placement);
        let back = orbit.transform();
        assert!(close(back.translation, placement.translation), "{back:?}");
        assert!(close(back.forward(), placement.forward()), "{back:?}");
        // The target is where the view ray passes nearest the old target
        // (0, 0.5, 0), not the original orbit's target.
        let to_target = orbit.target - placement.translation;
        assert!(close(to_target.normalize(), placement.forward()));
        assert!(
            (orbit.target - OrbitCamera::default().target)
                .dot(placement.forward())
                .abs()
                < 1e-4,
            "nearest point on the ray"
        );
    }

    #[test]
    fn look_from_the_starter_camera_targets_the_cube() {
        // The starter scene's camera: at (0, 2, 5) looking at (0, 0.5, 0),
        // which is the default orbit target, so it is kept.
        let mut placement = Transform::from_translation(Vec3::new(0.0, 2.0, 5.0));
        placement.looking_at(Vec3::new(0.0, 0.5, 0.0), Vec3::Y);
        let mut orbit = OrbitCamera::default();
        orbit.look_from(&placement);
        assert!(close(orbit.target, Vec3::new(0.0, 0.5, 0.0)), "{orbit:?}");
        assert!(close(orbit.transform().translation, placement.translation));
        assert!(orbit.pitch < 0.0, "looking down");
    }

    #[test]
    fn look_from_keeps_at_least_one_unit_ahead() {
        // Facing away from the old target: the target moves in front.
        let mut placement = Transform::from_translation(Vec3::new(0.0, 0.5, -5.0));
        placement.looking_at(Vec3::new(0.0, 0.5, -10.0), Vec3::Y);
        let mut orbit = OrbitCamera::default();
        orbit.look_from(&placement);
        assert_eq!(orbit.distance, MIN_LOOK_FROM_DISTANCE);
        assert!(close(orbit.target, Vec3::new(0.0, 0.5, -6.0)), "{orbit:?}");
        assert!(close(orbit.transform().translation, placement.translation));
    }
}
