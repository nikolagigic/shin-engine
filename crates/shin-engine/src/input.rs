//! Keyboard and mouse state, plus the free-fly camera controller. Owned by
//! GE-007.
//!
//! [`InputState`] is a resource built from winit events. [`crate::app::EcsGame`]
//! inserts it, forwards window and device events to it, and calls
//! [`InputState::end_frame`] after the frame systems have run.
//!
//! [`fly_camera_system`] is a frame system: WASD (plus E/Q for up/down) moves
//! the [`Camera`] resource, and holding the right mouse button grabs the
//! cursor and turns mouse motion into yaw and pitch.
//!
//! Named actions (GE-033) live in [`actions`]: [`Actions`] is the resource
//! games and scripts read by action name (`jump`, `move_x`), bound in
//! `shin.toml` to keys, mouse buttons, and gamepads ([`GamepadState`],
//! fed by gilrs through [`Gamepads`]); [`actions_system`] derives it each
//! frame. Tools: [`register_tools`] adds `input.list` and `input.set`.

pub mod actions;
mod tools;

use std::collections::HashSet;

use winit::event::{DeviceEvent, ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::PhysicalKey;

pub use actions::{
    ActionBinding, ActionValue, Actions, Binding, GamepadAxis, GamepadButton, GamepadState,
    Gamepads, InputConfig, actions_system,
};
pub use tools::register_tools;
pub use winit::keyboard::KeyCode;

use crate::ecs::{Resources, Time, World};
use crate::math::{Camera, Quat, Vec2, Vec3};

/// Index of the left button in [`InputState::mouse_buttons`].
pub const MOUSE_LEFT: usize = 0;
/// Index of the right button in [`InputState::mouse_buttons`].
pub const MOUSE_RIGHT: usize = 1;
/// Index of the middle button in [`InputState::mouse_buttons`].
pub const MOUSE_MIDDLE: usize = 2;

/// Keyboard and mouse state for the current frame.
///
/// Keys are identified by physical position ([`KeyCode`]), so WASD stays in
/// place on non-QWERTY layouts.
#[derive(Debug, Default)]
pub struct InputState {
    pressed: HashSet<KeyCode>,
    just_pressed: HashSet<KeyCode>,
    just_released: HashSet<KeyCode>,
    /// Raw mouse motion in pixels accumulated this frame (+x right, +y down);
    /// reset by [`end_frame`](Self::end_frame).
    pub mouse_delta: Vec2,
    /// Held state of the left, right, and middle buttons (see [`MOUSE_LEFT`],
    /// [`MOUSE_RIGHT`], [`MOUSE_MIDDLE`]).
    pub mouse_buttons: [bool; 3],
    /// Whether the cursor should be grabbed and hidden for mouse look. Set
    /// while the right mouse button is held; the app applies it to the window.
    pub cursor_grabbed: bool,
}

impl InputState {
    /// Updates key and mouse-button state from a window event. Losing focus
    /// releases every key and button, so nothing stays stuck down.
    pub fn handle_window_event(&mut self, event: &WindowEvent) {
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(code),
                        state,
                        ..
                    },
                ..
            } => match state {
                ElementState::Pressed => self.press(*code),
                ElementState::Released => self.release(*code),
            },
            WindowEvent::MouseInput { state, button, .. } => {
                self.set_mouse_button(*button, *state == ElementState::Pressed);
            }
            WindowEvent::Focused(false) => self.release_all(),
            _ => {}
        }
    }

    /// Accumulates raw mouse motion into [`mouse_delta`](Self::mouse_delta).
    /// Raw motion keeps arriving while the cursor is grabbed, unlike cursor
    /// positions.
    pub fn handle_device_event(&mut self, event: &DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta: (dx, dy) } = event {
            self.mouse_delta += Vec2::new(*dx as f32, *dy as f32);
        }
    }

    /// Whether `key` is held down.
    pub fn pressed(&self, key: KeyCode) -> bool {
        self.pressed.contains(&key)
    }

    /// Whether `key` went down this frame. True for exactly one frame per
    /// press; key repeats do not count.
    pub fn just_pressed(&self, key: KeyCode) -> bool {
        self.just_pressed.contains(&key)
    }

    /// Whether `key` was released this frame.
    pub fn just_released(&self, key: KeyCode) -> bool {
        self.just_released.contains(&key)
    }

    /// Ends the frame: clears the `just_*` sets and the mouse delta. Held
    /// keys and buttons persist.
    pub fn end_frame(&mut self) {
        self.just_pressed.clear();
        self.just_released.clear();
        self.mouse_delta = Vec2::ZERO;
    }

    fn press(&mut self, key: KeyCode) {
        // `insert` is false for OS key repeats, which must not re-trigger.
        if self.pressed.insert(key) {
            self.just_pressed.insert(key);
        }
    }

    fn release(&mut self, key: KeyCode) {
        if self.pressed.remove(&key) {
            self.just_released.insert(key);
        }
    }

    fn set_mouse_button(&mut self, button: MouseButton, down: bool) {
        let index = match button {
            MouseButton::Left => MOUSE_LEFT,
            MouseButton::Right => MOUSE_RIGHT,
            MouseButton::Middle => MOUSE_MIDDLE,
            _ => return,
        };
        self.mouse_buttons[index] = down;
        if index == MOUSE_RIGHT {
            self.cursor_grabbed = down;
        }
    }

    fn release_all(&mut self) {
        let held: Vec<KeyCode> = self.pressed.iter().copied().collect();
        for key in held {
            self.release(key);
        }
        self.mouse_buttons = [false; 3];
        self.cursor_grabbed = false;
    }
}

/// Pitch limit for [`FlyCamera`]: 89°, just short of straight up or down
/// so the view never flips.
pub const MAX_PITCH: f32 = 89.0 * std::f32::consts::PI / 180.0;

/// Free-fly camera settings and orientation, stored as a resource next to
/// the [`Camera`] it drives.
#[derive(Clone, Copy, Debug)]
pub struct FlyCamera {
    /// Movement speed in metres per second.
    pub speed: f32,
    /// Mouse-look sensitivity in radians per pixel.
    pub sensitivity: f32,
    /// Heading in radians. 0 looks down -Z; positive turns right, from -Z
    /// toward +X.
    pub yaw: f32,
    /// Elevation in radians. Positive looks up; kept within ±[`MAX_PITCH`].
    pub pitch: f32,
}

impl Default for FlyCamera {
    fn default() -> Self {
        Self {
            speed: 5.0,
            sensitivity: 0.002,
            yaw: 0.0,
            pitch: 0.0,
        }
    }
}

impl FlyCamera {
    /// The camera orientation for the current yaw and pitch: pitch about
    /// local +X, then yaw about world +Y.
    pub fn rotation(&self) -> Quat {
        // A positive rotation about +Y turns -Z toward -X, so yawing right
        // (toward +X) is a negative angle.
        Quat::from_rotation_y(-self.yaw) * Quat::from_rotation_x(self.pitch)
    }
}

/// Frame system: reads [`InputState`], [`Time`] and [`FlyCamera`], and writes
/// the [`Camera`] resource's transform.
///
/// While the cursor is grabbed, mouse motion turns the camera (+x right,
/// +y down). W/S move along the view direction, A/D strafe, and E/Q move
/// along world up and down, at `speed` metres per second of `frame_dt`.
///
/// # Panics
///
/// If any of the four resources is missing.
pub fn fly_camera_system(_world: &mut World, res: &mut Resources) {
    let dt = res.expect::<Time>().frame_dt as f32;
    let input = res.expect::<InputState>();
    let look = if input.cursor_grabbed {
        input.mouse_delta
    } else {
        Vec2::ZERO
    };
    let axis = |positive: KeyCode, negative: KeyCode| {
        input.pressed(positive) as i32 as f32 - input.pressed(negative) as i32 as f32
    };
    let forward_input = axis(KeyCode::KeyW, KeyCode::KeyS);
    let right_input = axis(KeyCode::KeyD, KeyCode::KeyA);
    let up_input = axis(KeyCode::KeyE, KeyCode::KeyQ);

    let fly = res.expect_mut::<FlyCamera>();
    fly.yaw += look.x * fly.sensitivity;
    fly.pitch = (fly.pitch - look.y * fly.sensitivity).clamp(-MAX_PITCH, MAX_PITCH);
    let rotation = fly.rotation();
    let speed = fly.speed;

    let transform = &mut res.expect_mut::<Camera>().transform;
    transform.rotation = rotation;
    let direction =
        transform.forward() * forward_input + transform.right() * right_input + Vec3::Y * up_input;
    // Normalised so diagonal movement is not faster than straight movement.
    transform.translation += direction.normalize_or_zero() * speed * dt;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::{Projection, Transform};

    const EPS: f32 = 1e-4;

    #[test]
    fn just_pressed_lasts_one_frame_and_pressed_until_release() {
        let mut input = InputState::default();
        input.press(KeyCode::KeyW);
        assert!(input.pressed(KeyCode::KeyW));
        assert!(input.just_pressed(KeyCode::KeyW));

        input.end_frame();
        assert!(input.pressed(KeyCode::KeyW), "held across frames");
        assert!(!input.just_pressed(KeyCode::KeyW), "only for one frame");

        // An OS key repeat while held must not re-trigger `just_pressed`.
        input.press(KeyCode::KeyW);
        assert!(!input.just_pressed(KeyCode::KeyW));
        input.end_frame();
        assert!(input.pressed(KeyCode::KeyW));

        input.release(KeyCode::KeyW);
        assert!(!input.pressed(KeyCode::KeyW));
        assert!(input.just_released(KeyCode::KeyW));
        input.end_frame();
        assert!(!input.just_released(KeyCode::KeyW));
    }

    #[test]
    fn press_and_release_in_one_frame_is_seen() {
        let mut input = InputState::default();
        input.press(KeyCode::Space);
        input.release(KeyCode::Space);
        assert!(input.just_pressed(KeyCode::Space));
        assert!(input.just_released(KeyCode::Space));
        assert!(!input.pressed(KeyCode::Space));
    }

    #[test]
    fn mouse_motion_accumulates_until_end_frame() {
        let mut input = InputState::default();
        input.handle_device_event(&DeviceEvent::MouseMotion { delta: (3.0, -1.0) });
        input.handle_device_event(&DeviceEvent::MouseMotion { delta: (2.0, 4.0) });
        assert_eq!(input.mouse_delta, Vec2::new(5.0, 3.0));
        input.end_frame();
        assert_eq!(input.mouse_delta, Vec2::ZERO);
    }

    #[test]
    fn right_button_grabs_cursor_and_focus_loss_releases_everything() {
        let mut input = InputState::default();
        input.set_mouse_button(MouseButton::Right, true);
        assert!(input.cursor_grabbed);
        assert!(input.mouse_buttons[MOUSE_RIGHT]);
        input.set_mouse_button(MouseButton::Right, false);
        assert!(!input.cursor_grabbed);

        input.set_mouse_button(MouseButton::Right, true);
        input.press(KeyCode::KeyA);
        input.handle_window_event(&WindowEvent::Focused(false));
        assert!(!input.cursor_grabbed);
        assert_eq!(input.mouse_buttons, [false; 3]);
        assert!(!input.pressed(KeyCode::KeyA));
        assert!(input.just_released(KeyCode::KeyA));
    }

    /// Resources for the fly camera: a camera at the origin facing -Z.
    fn fly_resources(frame_dt: f64) -> Resources {
        let mut res = Resources::new();
        res.insert(Time {
            frame_dt,
            ..Time::default()
        });
        res.insert(InputState::default());
        res.insert(FlyCamera::default());
        res.insert(Camera {
            transform: Transform::IDENTITY,
            projection: Projection::Perspective {
                fov_y_radians: 1.0,
                near: 0.1,
                far: 100.0,
            },
        });
        res
    }

    #[test]
    fn holding_w_for_one_second_moves_five_metres_forward() {
        let mut res = fly_resources(1.0 / 60.0);
        // Look somewhere off-axis so "forward" is not trivially -Z.
        {
            let fly = res.expect_mut::<FlyCamera>();
            fly.yaw = 0.7;
            fly.pitch = 0.3;
        }
        res.expect_mut::<InputState>().press(KeyCode::KeyW);
        let mut world = World::new();
        for _ in 0..60 {
            fly_camera_system(&mut world, &mut res);
            res.expect_mut::<InputState>().end_frame();
        }

        let transform = res.expect::<Camera>().transform;
        let expected = transform.forward() * 5.0;
        assert!(
            transform.translation.abs_diff_eq(expected, EPS),
            "moved to {}, expected {expected}",
            transform.translation
        );
        assert!((transform.translation.length() - 5.0).abs() < EPS);
    }

    #[test]
    fn mouse_right_yaws_toward_positive_x() {
        let mut res = fly_resources(0.0);
        {
            let input = res.expect_mut::<InputState>();
            input.cursor_grabbed = true;
            input.mouse_delta = Vec2::new(100.0, 0.0);
        }
        fly_camera_system(&mut World::new(), &mut res);

        let fly = *res.expect::<FlyCamera>();
        assert!((fly.yaw - 0.2).abs() < EPS, "yaw = {}", fly.yaw);
        assert_eq!(fly.pitch, 0.0);
        // Forward rotated 0.2 rad from -Z toward +X.
        let forward = res.expect::<Camera>().transform.forward();
        let expected = Vec3::new(0.2f32.sin(), 0.0, -0.2f32.cos());
        assert!(forward.abs_diff_eq(expected, EPS), "forward = {forward}");
        assert!(forward.x > 0.0);
    }

    #[test]
    fn mouse_motion_without_grab_does_not_turn() {
        let mut res = fly_resources(0.0);
        res.expect_mut::<InputState>().mouse_delta = Vec2::new(100.0, 50.0);
        fly_camera_system(&mut World::new(), &mut res);
        let fly = *res.expect::<FlyCamera>();
        assert_eq!((fly.yaw, fly.pitch), (0.0, 0.0));
    }

    #[test]
    fn pitch_is_clamped_to_89_degrees() {
        let mut res = fly_resources(0.0);
        let mut world = World::new();
        for dy in [-1.0e9, 1.0e9, -1.0e6] {
            {
                let input = res.expect_mut::<InputState>();
                input.cursor_grabbed = true;
                input.mouse_delta = Vec2::new(0.0, dy);
            }
            fly_camera_system(&mut world, &mut res);
            let pitch = res.expect::<FlyCamera>().pitch;
            assert!(pitch.abs() <= MAX_PITCH, "pitch = {pitch}");
            assert!((pitch.abs() - MAX_PITCH).abs() < EPS, "pinned at the limit");
            // Mouse up (negative y) looks up.
            assert_eq!(pitch > 0.0, dy < 0.0);
            let forward = res.expect::<Camera>().transform.forward();
            assert!(forward.y.abs() < 1.0, "never straight up or down");
        }
    }

    #[test]
    fn strafe_and_vertical_keys_move_along_right_and_up() {
        let mut res = fly_resources(1.0);
        res.expect_mut::<InputState>().press(KeyCode::KeyD);
        fly_camera_system(&mut World::new(), &mut res);
        let t = res.expect::<Camera>().transform.translation;
        assert!(t.abs_diff_eq(Vec3::new(5.0, 0.0, 0.0), EPS), "{t}");

        let mut res = fly_resources(1.0);
        res.expect_mut::<InputState>().press(KeyCode::KeyE);
        fly_camera_system(&mut World::new(), &mut res);
        let t = res.expect::<Camera>().transform.translation;
        assert!(t.abs_diff_eq(Vec3::new(0.0, 5.0, 0.0), EPS), "{t}");
    }
}
