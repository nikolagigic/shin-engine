//! Named input actions (GE-033): `jump`, `move_x`, ... bound in `shin.toml`
//! to keys, mouse buttons, and gamepad buttons and axes, and read by name
//! from game systems and scripts instead of by key.
//!
//! ```toml
//! [input.actions]
//! jump = ["Space", "Gamepad:South"]
//! move_x = { axis = ["KeyA", "KeyD"], gamepad_axis = "LeftStickX" }
//! ```
//!
//! A *button* action lists bindings, any of which holds it: a key by its
//! [`KeyCode`] name (`Space`, `KeyA`, `ArrowUp`), `Mouse:Left`,
//! `Mouse:Right`, `Mouse:Middle`, or `Gamepad:<button>` with a
//! [`GamepadButton`] name. An *axis* action has a value in `-1..=1`:
//! `axis` names a negative and a positive binding, `gamepad_axis` a
//! [`GamepadAxis`]; whichever source is further from zero wins. An axis
//! action counts as pressed from [`PRESS_THRESHOLD`]; a button action's
//! axis is `1.0` while it is held.
//!
//! [`Actions`] is the resource holding every action's binding and current
//! value ([`Actions::pressed`], [`Actions::just_pressed`],
//! [`Actions::axis`]). [`actions_system`] derives the values from
//! [`InputState`] and [`GamepadState`] once per frame: [`crate::app::EcsGame`]
//! polls the gamepads ([`Gamepads`]) into [`GamepadState`] and runs the
//! system before its other frame systems. A [`crate::headless::Headless`]
//! engine has no devices (no [`InputState`]), so its actions keep whatever
//! the `input.set` tool ([`super::register_tools`]) or
//! [`Actions::set_value`] injected.
//!
//! `just_pressed` is an edge: it becomes true when an action goes from
//! released to pressed and stays true until a fixed step has run
//! ([`Actions::end_step`], which the engine drivers call after each
//! step). A tap between two steps is never lost, and a script sees each
//! press in exactly one step.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::str::FromStr;

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use winit::event::MouseButton;

use super::{InputState, KeyCode, MOUSE_LEFT, MOUSE_MIDDLE, MOUSE_RIGHT};
use crate::ecs::{Resources, World};

/// An axis action counts as pressed when its value's magnitude reaches
/// this.
pub const PRESS_THRESHOLD: f32 = 0.5;

/// A gamepad button, named as gilrs names them and as `shin.toml` binds
/// them (`Gamepad:South`). `South` is A on an Xbox pad and Cross on a
/// PlayStation pad.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GamepadButton {
    /// Bottom face button (A / Cross).
    South,
    /// Right face button (B / Circle).
    East,
    /// Top face button (Y / Triangle).
    North,
    /// Left face button (X / Square).
    West,
    /// Extra face button on some pads.
    C,
    /// Extra face button on some pads.
    Z,
    /// Left bumper.
    LeftTrigger,
    /// Left trigger.
    LeftTrigger2,
    /// Right bumper.
    RightTrigger,
    /// Right trigger.
    RightTrigger2,
    /// Select / Back / Share.
    Select,
    /// Start / Options.
    Start,
    /// Guide / Home.
    Mode,
    /// Pressing the left stick.
    LeftThumb,
    /// Pressing the right stick.
    RightThumb,
    /// D-pad up.
    DPadUp,
    /// D-pad down.
    DPadDown,
    /// D-pad left.
    DPadLeft,
    /// D-pad right.
    DPadRight,
}

impl GamepadButton {
    /// Every button, in gilrs order.
    pub const ALL: [Self; 19] = [
        Self::South,
        Self::East,
        Self::North,
        Self::West,
        Self::C,
        Self::Z,
        Self::LeftTrigger,
        Self::LeftTrigger2,
        Self::RightTrigger,
        Self::RightTrigger2,
        Self::Select,
        Self::Start,
        Self::Mode,
        Self::LeftThumb,
        Self::RightThumb,
        Self::DPadUp,
        Self::DPadDown,
        Self::DPadLeft,
        Self::DPadRight,
    ];
}

impl fmt::Display for GamepadButton {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl FromStr for GamepadButton {
    type Err = String;

    /// Parses a variant name such as `South` or `DPadUp`.
    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|button| button.to_string() == name)
            .ok_or_else(|| {
                let names: Vec<String> = Self::ALL.iter().map(ToString::to_string).collect();
                format!(
                    "unknown gamepad button `{name}`; expected one of {}",
                    names.join(", ")
                )
            })
    }
}

/// A gamepad axis, named as gilrs names them. Sticks run from `-1` (left
/// or down) to `1` (right or up); the triggers (`LeftZ`, `RightZ`) from
/// `0` to `1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GamepadAxis {
    /// Left stick, horizontal.
    LeftStickX,
    /// Left stick, vertical (up is positive).
    LeftStickY,
    /// Left trigger as an axis.
    LeftZ,
    /// Right stick, horizontal.
    RightStickX,
    /// Right stick, vertical (up is positive).
    RightStickY,
    /// Right trigger as an axis.
    RightZ,
    /// D-pad, horizontal, on pads that report it as an axis.
    DPadX,
    /// D-pad, vertical, on pads that report it as an axis.
    DPadY,
}

impl GamepadAxis {
    /// Every axis, in gilrs order.
    pub const ALL: [Self; 8] = [
        Self::LeftStickX,
        Self::LeftStickY,
        Self::LeftZ,
        Self::RightStickX,
        Self::RightStickY,
        Self::RightZ,
        Self::DPadX,
        Self::DPadY,
    ];
}

impl fmt::Display for GamepadAxis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl FromStr for GamepadAxis {
    type Err = String;

    /// Parses a variant name such as `LeftStickX`.
    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|axis| axis.to_string() == name)
            .ok_or_else(|| {
                let names: Vec<String> = Self::ALL.iter().map(ToString::to_string).collect();
                format!(
                    "unknown gamepad axis `{name}`; expected one of {}",
                    names.join(", ")
                )
            })
    }
}

/// The state of every connected gamepad, merged: a button is pressed if
/// it is down on any pad, an axis is the value last reported by any pad.
/// Free of gilrs, so tests and other drivers can fill it in directly.
///
/// [`Gamepads::poll`] feeds it from real devices; [`crate::app::EcsGame`]
/// calls [`Self::end_frame`] after its frame systems, as it does for
/// [`InputState`].
#[derive(Clone, Debug, Default)]
pub struct GamepadState {
    pressed: HashSet<GamepadButton>,
    just_pressed: HashSet<GamepadButton>,
    axes: HashMap<GamepadAxis, f32>,
}

impl GamepadState {
    /// Records `button` going down. A press while already down (another
    /// pad, or a repeat) does not count as a new edge.
    pub fn press(&mut self, button: GamepadButton) {
        if self.pressed.insert(button) {
            self.just_pressed.insert(button);
        }
    }

    /// Records `button` coming up.
    pub fn release(&mut self, button: GamepadButton) {
        self.pressed.remove(&button);
    }

    /// Records `axis` at `value`, clamped to `-1..=1`.
    pub fn set_axis(&mut self, axis: GamepadAxis, value: f32) {
        self.axes.insert(axis, value.clamp(-1.0, 1.0));
    }

    /// Whether `button` is held down.
    pub fn pressed(&self, button: GamepadButton) -> bool {
        self.pressed.contains(&button)
    }

    /// Whether `button` went down since the last [`Self::end_frame`].
    pub fn just_pressed(&self, button: GamepadButton) -> bool {
        self.just_pressed.contains(&button)
    }

    /// The last value of `axis`, or `0.0` if it never moved.
    pub fn axis(&self, axis: GamepadAxis) -> f32 {
        self.axes.get(&axis).copied().unwrap_or(0.0)
    }

    /// Ends the frame: clears the `just_pressed` set. Held buttons and
    /// axis values persist.
    pub fn end_frame(&mut self) {
        self.just_pressed.clear();
    }

    /// Releases every button and centres every axis, as when a pad is
    /// unplugged.
    pub fn release_all(&mut self) {
        self.pressed.clear();
        self.axes.clear();
    }
}

/// The connected gamepads, through gilrs. [`crate::app::EcsGame`] owns one
/// and polls it into the [`GamepadState`] resource once per frame.
/// Without gilrs (it failed to start, or [`Self::default`]) polling does
/// nothing.
#[derive(Default)]
pub struct Gamepads {
    gilrs: Option<gilrs::Gilrs>,
}

impl Gamepads {
    /// Starts gilrs. If it cannot start, the reason is logged and the
    /// result polls nothing; a game without gamepads still runs.
    pub fn new() -> Self {
        let gilrs = match gilrs::Gilrs::new() {
            Ok(gilrs) => Some(gilrs),
            // No gamepad support on this platform: a dummy that never
            // reports an event.
            Err(gilrs::Error::NotImplemented(gilrs)) => Some(gilrs),
            Err(err) => {
                log::warn!("gamepads unavailable: {err}");
                None
            }
        };
        if let Some(gilrs) = &gilrs {
            for (_, pad) in gilrs.gamepads() {
                log::info!("gamepad connected: {}", pad.name());
            }
        }
        Self { gilrs }
    }

    /// Applies every pending gamepad event to `state`.
    pub fn poll(&mut self, state: &mut GamepadState) {
        let Some(gilrs) = &mut self.gilrs else {
            return;
        };
        while let Some(event) = gilrs.next_event() {
            match event.event {
                gilrs::EventType::ButtonPressed(button, _) => {
                    if let Some(button) = gamepad_button(button) {
                        state.press(button);
                    }
                }
                gilrs::EventType::ButtonReleased(button, _) => {
                    if let Some(button) = gamepad_button(button) {
                        state.release(button);
                    }
                }
                gilrs::EventType::AxisChanged(axis, value, _) => {
                    if let Some(axis) = gamepad_axis(axis) {
                        state.set_axis(axis, value);
                    }
                }
                gilrs::EventType::Connected => {
                    log::info!("gamepad connected: {}", gilrs.gamepad(event.id).name());
                }
                gilrs::EventType::Disconnected => {
                    log::info!("gamepad disconnected");
                    state.release_all();
                }
                _ => {}
            }
        }
    }
}

/// A gilrs button as a [`GamepadButton`]; `None` for `Unknown`.
fn gamepad_button(button: gilrs::Button) -> Option<GamepadButton> {
    use gilrs::Button as B;
    Some(match button {
        B::South => GamepadButton::South,
        B::East => GamepadButton::East,
        B::North => GamepadButton::North,
        B::West => GamepadButton::West,
        B::C => GamepadButton::C,
        B::Z => GamepadButton::Z,
        B::LeftTrigger => GamepadButton::LeftTrigger,
        B::LeftTrigger2 => GamepadButton::LeftTrigger2,
        B::RightTrigger => GamepadButton::RightTrigger,
        B::RightTrigger2 => GamepadButton::RightTrigger2,
        B::Select => GamepadButton::Select,
        B::Start => GamepadButton::Start,
        B::Mode => GamepadButton::Mode,
        B::LeftThumb => GamepadButton::LeftThumb,
        B::RightThumb => GamepadButton::RightThumb,
        B::DPadUp => GamepadButton::DPadUp,
        B::DPadDown => GamepadButton::DPadDown,
        B::DPadLeft => GamepadButton::DPadLeft,
        B::DPadRight => GamepadButton::DPadRight,
        B::Unknown => return None,
    })
}

/// A gilrs axis as a [`GamepadAxis`]; `None` for `Unknown`.
fn gamepad_axis(axis: gilrs::Axis) -> Option<GamepadAxis> {
    use gilrs::Axis as A;
    Some(match axis {
        A::LeftStickX => GamepadAxis::LeftStickX,
        A::LeftStickY => GamepadAxis::LeftStickY,
        A::LeftZ => GamepadAxis::LeftZ,
        A::RightStickX => GamepadAxis::RightStickX,
        A::RightStickY => GamepadAxis::RightStickY,
        A::RightZ => GamepadAxis::RightZ,
        A::DPadX => GamepadAxis::DPadX,
        A::DPadY => GamepadAxis::DPadY,
        A::Unknown => return None,
    })
}

/// Every [`KeyCode`] a binding can name, so names parse without a serde
/// feature on winit. Generated from winit 0.30's `KeyCode`.
#[rustfmt::skip]
const KEY_CODES: &[KeyCode] = &[
    KeyCode::Backquote, KeyCode::Backslash, KeyCode::BracketLeft, KeyCode::BracketRight, KeyCode::Comma, KeyCode::Digit0,
    KeyCode::Digit1, KeyCode::Digit2, KeyCode::Digit3, KeyCode::Digit4, KeyCode::Digit5, KeyCode::Digit6,
    KeyCode::Digit7, KeyCode::Digit8, KeyCode::Digit9, KeyCode::Equal, KeyCode::IntlBackslash, KeyCode::IntlRo,
    KeyCode::IntlYen, KeyCode::KeyA, KeyCode::KeyB, KeyCode::KeyC, KeyCode::KeyD, KeyCode::KeyE,
    KeyCode::KeyF, KeyCode::KeyG, KeyCode::KeyH, KeyCode::KeyI, KeyCode::KeyJ, KeyCode::KeyK,
    KeyCode::KeyL, KeyCode::KeyM, KeyCode::KeyN, KeyCode::KeyO, KeyCode::KeyP, KeyCode::KeyQ,
    KeyCode::KeyR, KeyCode::KeyS, KeyCode::KeyT, KeyCode::KeyU, KeyCode::KeyV, KeyCode::KeyW,
    KeyCode::KeyX, KeyCode::KeyY, KeyCode::KeyZ, KeyCode::Minus, KeyCode::Period, KeyCode::Quote,
    KeyCode::Semicolon, KeyCode::Slash, KeyCode::AltLeft, KeyCode::AltRight, KeyCode::Backspace, KeyCode::CapsLock,
    KeyCode::ContextMenu, KeyCode::ControlLeft, KeyCode::ControlRight, KeyCode::Enter, KeyCode::SuperLeft, KeyCode::SuperRight,
    KeyCode::ShiftLeft, KeyCode::ShiftRight, KeyCode::Space, KeyCode::Tab, KeyCode::Convert, KeyCode::KanaMode,
    KeyCode::Lang1, KeyCode::Lang2, KeyCode::Lang3, KeyCode::Lang4, KeyCode::Lang5, KeyCode::NonConvert,
    KeyCode::Delete, KeyCode::End, KeyCode::Help, KeyCode::Home, KeyCode::Insert, KeyCode::PageDown,
    KeyCode::PageUp, KeyCode::ArrowDown, KeyCode::ArrowLeft, KeyCode::ArrowRight, KeyCode::ArrowUp, KeyCode::NumLock,
    KeyCode::Numpad0, KeyCode::Numpad1, KeyCode::Numpad2, KeyCode::Numpad3, KeyCode::Numpad4, KeyCode::Numpad5,
    KeyCode::Numpad6, KeyCode::Numpad7, KeyCode::Numpad8, KeyCode::Numpad9, KeyCode::NumpadAdd, KeyCode::NumpadBackspace,
    KeyCode::NumpadClear, KeyCode::NumpadClearEntry, KeyCode::NumpadComma, KeyCode::NumpadDecimal, KeyCode::NumpadDivide, KeyCode::NumpadEnter,
    KeyCode::NumpadEqual, KeyCode::NumpadHash, KeyCode::NumpadMemoryAdd, KeyCode::NumpadMemoryClear, KeyCode::NumpadMemoryRecall, KeyCode::NumpadMemoryStore,
    KeyCode::NumpadMemorySubtract, KeyCode::NumpadMultiply, KeyCode::NumpadParenLeft, KeyCode::NumpadParenRight, KeyCode::NumpadStar, KeyCode::NumpadSubtract,
    KeyCode::Escape, KeyCode::Fn, KeyCode::FnLock, KeyCode::PrintScreen, KeyCode::ScrollLock, KeyCode::Pause,
    KeyCode::BrowserBack, KeyCode::BrowserFavorites, KeyCode::BrowserForward, KeyCode::BrowserHome, KeyCode::BrowserRefresh, KeyCode::BrowserSearch,
    KeyCode::BrowserStop, KeyCode::Eject, KeyCode::LaunchApp1, KeyCode::LaunchApp2, KeyCode::LaunchMail, KeyCode::MediaPlayPause,
    KeyCode::MediaSelect, KeyCode::MediaStop, KeyCode::MediaTrackNext, KeyCode::MediaTrackPrevious, KeyCode::Power, KeyCode::Sleep,
    KeyCode::AudioVolumeDown, KeyCode::AudioVolumeMute, KeyCode::AudioVolumeUp, KeyCode::WakeUp, KeyCode::Meta, KeyCode::Hyper,
    KeyCode::Turbo, KeyCode::Abort, KeyCode::Resume, KeyCode::Suspend, KeyCode::Again, KeyCode::Copy,
    KeyCode::Cut, KeyCode::Find, KeyCode::Open, KeyCode::Paste, KeyCode::Props, KeyCode::Select,
    KeyCode::Undo, KeyCode::Hiragana, KeyCode::Katakana, KeyCode::F1, KeyCode::F2, KeyCode::F3,
    KeyCode::F4, KeyCode::F5, KeyCode::F6, KeyCode::F7, KeyCode::F8, KeyCode::F9,
    KeyCode::F10, KeyCode::F11, KeyCode::F12, KeyCode::F13, KeyCode::F14, KeyCode::F15,
    KeyCode::F16, KeyCode::F17, KeyCode::F18, KeyCode::F19, KeyCode::F20, KeyCode::F21,
    KeyCode::F22, KeyCode::F23, KeyCode::F24, KeyCode::F25, KeyCode::F26, KeyCode::F27,
    KeyCode::F28, KeyCode::F29, KeyCode::F30, KeyCode::F31, KeyCode::F32, KeyCode::F33,
    KeyCode::F34, KeyCode::F35,
];

/// The [`KeyCode`] called `name` (its variant name: `Space`, `KeyA`,
/// `ArrowUp`, `Digit1`, `ShiftLeft`), if there is one.
pub fn key_code(name: &str) -> Option<KeyCode> {
    KEY_CODES
        .iter()
        .copied()
        .find(|code| format!("{code:?}") == name)
}

/// One source that can hold a button action or one end of an axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Binding {
    /// A key, by physical position; written as its [`KeyCode`] name.
    Key(KeyCode),
    /// A mouse button; written `Mouse:Left`, `Mouse:Right`, or
    /// `Mouse:Middle`.
    Mouse(MouseButton),
    /// A gamepad button on any pad; written `Gamepad:<button>`.
    Gamepad(GamepadButton),
}

impl fmt::Display for Binding {
    /// The form `shin.toml` uses.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Key(code) => write!(f, "{code:?}"),
            Self::Mouse(button) => write!(f, "Mouse:{button:?}"),
            Self::Gamepad(button) => write!(f, "Gamepad:{button}"),
        }
    }
}

impl FromStr for Binding {
    type Err = String;

    /// Parses the `shin.toml` form; the error names the unknown part.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if let Some(name) = text.strip_prefix("Mouse:") {
            return match name {
                "Left" => Ok(Self::Mouse(MouseButton::Left)),
                "Right" => Ok(Self::Mouse(MouseButton::Right)),
                "Middle" => Ok(Self::Mouse(MouseButton::Middle)),
                _ => Err(format!(
                    "unknown mouse button `{name}`; expected Mouse:Left, Mouse:Right, or \
                     Mouse:Middle"
                )),
            };
        }
        if let Some(name) = text.strip_prefix("Gamepad:") {
            return name.parse().map(Self::Gamepad);
        }
        key_code(text).map(Self::Key).ok_or_else(|| {
            format!(
                "unknown key `{text}`; expected a key name such as Space, KeyA, ArrowUp, or \
                 Digit1, a mouse button (Mouse:Left), or a gamepad button (Gamepad:South)"
            )
        })
    }
}

impl Serialize for Binding {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Binding {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

/// How one action is bound: the value of an `[input.actions]` entry.
#[derive(Clone, Debug, PartialEq)]
pub enum ActionBinding {
    /// `jump = ["Space", "Gamepad:South"]`: held while any binding is.
    Buttons(Vec<Binding>),
    /// `move_x = { axis = ["KeyA", "KeyD"], gamepad_axis = "LeftStickX" }`:
    /// a value in `-1..=1`.
    Axis {
        /// The binding that pulls the value to `-1` and the one that
        /// pushes it to `1`.
        axis: Option<[Binding; 2]>,
        /// A gamepad axis read directly.
        gamepad_axis: Option<GamepadAxis>,
    },
}

impl Serialize for ActionBinding {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct AxisForm<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            axis: Option<&'a [Binding; 2]>,
            #[serde(skip_serializing_if = "Option::is_none")]
            gamepad_axis: Option<&'a GamepadAxis>,
        }
        match self {
            Self::Buttons(bindings) => bindings.serialize(serializer),
            Self::Axis { axis, gamepad_axis } => AxisForm {
                axis: axis.as_ref(),
                gamepad_axis: gamepad_axis.as_ref(),
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for ActionBinding {
    /// An array of binding names is [`Self::Buttons`]; a table with `axis`
    /// and/or `gamepad_axis` is [`Self::Axis`]. Errors name the bad
    /// binding or field.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(BindingVisitor)
    }
}

struct BindingVisitor;

impl<'de> Visitor<'de> for BindingVisitor {
    type Value = ActionBinding;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "an array of bindings such as [\"Space\", \"Gamepad:South\"] or a table \
             { axis = [\"KeyA\", \"KeyD\"], gamepad_axis = \"LeftStickX\" }",
        )
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut bindings = Vec::new();
        while let Some(binding) = seq.next_element::<Binding>()? {
            bindings.push(binding);
        }
        Ok(ActionBinding::Buttons(bindings))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut axis = None;
        let mut gamepad_axis = None;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "axis" => {
                    let ends: Vec<Binding> = map.next_value()?;
                    let [negative, positive] = <[Binding; 2]>::try_from(ends).map_err(|ends| {
                        de::Error::custom(format!(
                            "`axis` needs exactly two bindings, negative then positive, got {}",
                            ends.len()
                        ))
                    })?;
                    axis = Some([negative, positive]);
                }
                "gamepad_axis" => {
                    let name: String = map.next_value()?;
                    gamepad_axis = Some(name.parse().map_err(de::Error::custom)?);
                }
                other => {
                    return Err(de::Error::unknown_field(other, &["axis", "gamepad_axis"]));
                }
            }
        }
        if axis.is_none() && gamepad_axis.is_none() {
            return Err(de::Error::custom(
                "an axis action needs `axis` (two bindings) or `gamepad_axis`",
            ));
        }
        Ok(ActionBinding::Axis { axis, gamepad_axis })
    }
}

/// The `[input]` table of `shin.toml`: named actions and their bindings.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputConfig {
    /// The `[input.actions]` table, by action name.
    #[serde(default)]
    pub actions: BTreeMap<String, ActionBinding>,
}

impl InputConfig {
    /// Whether no action is defined (the table is then left out of the
    /// manifest).
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }
}

/// An action's current value.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ActionValue {
    /// Held: a button action with a binding down, or an axis action at
    /// or beyond [`PRESS_THRESHOLD`].
    pub pressed: bool,
    /// Went from released to pressed, and no fixed step has run since
    /// (see the [module docs](self)).
    pub just_pressed: bool,
    /// `-1..=1` for an axis action; `0.0` or `1.0` for a button action.
    pub axis: f32,
}

/// One named action: its binding and value.
#[derive(Clone, Debug)]
struct Action {
    binding: ActionBinding,
    value: ActionValue,
}

/// Resource: every action's binding and current value. Built from the
/// project's [`InputConfig`] ([`Self::from_config`]) or by
/// [`Self::define`]. Queries for an unknown action are false or zero.
#[derive(Clone, Debug, Default)]
pub struct Actions {
    actions: BTreeMap<String, Action>,
}

impl Actions {
    /// No actions.
    pub fn new() -> Self {
        Self::default()
    }

    /// One action per entry of `config.actions`, all released.
    pub fn from_config(config: &InputConfig) -> Self {
        let mut actions = Self::new();
        for (name, binding) in &config.actions {
            actions.define(name.clone(), binding.clone());
        }
        actions
    }

    /// Adds `name` bound as `binding`, released; replaces an action of
    /// the same name.
    pub fn define(&mut self, name: impl Into<String>, binding: ActionBinding) {
        self.actions.insert(
            name.into(),
            Action {
                binding,
                value: ActionValue::default(),
            },
        );
    }

    /// Whether an action called `name` is defined.
    pub fn contains(&self, name: &str) -> bool {
        self.actions.contains_key(name)
    }

    /// Every action's name, binding, and value, by name.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &ActionBinding, ActionValue)> {
        self.actions
            .iter()
            .map(|(name, action)| (name.as_str(), &action.binding, action.value))
    }

    /// The value of `name`, or `None` if it is not defined.
    pub fn value(&self, name: &str) -> Option<ActionValue> {
        self.actions.get(name).map(|action| action.value)
    }

    /// Whether `name` is held.
    pub fn pressed(&self, name: &str) -> bool {
        self.value(name).is_some_and(|value| value.pressed)
    }

    /// Whether `name` was pressed and no fixed step has run since.
    pub fn just_pressed(&self, name: &str) -> bool {
        self.value(name).is_some_and(|value| value.just_pressed)
    }

    /// The value of `name` in `-1..=1` (`0.0` or `1.0` for a button
    /// action).
    pub fn axis(&self, name: &str) -> f32 {
        self.value(name).map_or(0.0, |value| value.axis)
    }

    /// Injects a value for `name`, as `input.set` does: the axis becomes
    /// `axis` (clamped to `-1..=1`), `pressed` follows
    /// [`PRESS_THRESHOLD`], and going from released to pressed sets
    /// `just_pressed`. Returns the new value.
    ///
    /// On an engine with devices the next [`actions_system`] run replaces
    /// it; on a headless engine it stays until the next injection.
    ///
    /// Fails if `name` is not defined, listing the actions that are.
    pub fn set_value(&mut self, name: &str, axis: f32) -> anyhow::Result<ActionValue> {
        if !self.contains(name) {
            let names: Vec<&str> = self.actions.keys().map(String::as_str).collect();
            anyhow::bail!(
                "unknown action `{name}`; actions: {}",
                if names.is_empty() {
                    "none defined (add [input.actions] to shin.toml)".to_string()
                } else {
                    names.join(", ")
                }
            );
        }
        self.apply(name, axis.clamp(-1.0, 1.0), false);
        Ok(self.value(name).unwrap_or_default())
    }

    /// [`Self::set_value`] with `1.0` or `0.0`.
    pub fn set_pressed(&mut self, name: &str, pressed: bool) -> anyhow::Result<ActionValue> {
        self.set_value(name, if pressed { 1.0 } else { 0.0 })
    }

    /// Sets `name` to `axis`; `edge` forces `just_pressed` for a tap that
    /// began and ended within the frame.
    fn apply(&mut self, name: &str, axis: f32, edge: bool) {
        let Some(action) = self.actions.get_mut(name) else {
            return;
        };
        let pressed = axis.abs() >= PRESS_THRESHOLD;
        if edge || (pressed && !action.value.pressed) {
            action.value.just_pressed = true;
        }
        action.value.pressed = pressed;
        action.value.axis = axis;
    }

    /// Derives every action's value from the devices: what
    /// [`actions_system`] does each frame.
    pub fn update(&mut self, input: &InputState, gamepads: &GamepadState) {
        let samples: Vec<(String, f32, bool)> = self
            .actions
            .iter()
            .map(|(name, action)| {
                let (axis, edge) = sample(&action.binding, input, gamepads);
                (name.clone(), axis, edge)
            })
            .collect();
        for (name, axis, edge) in samples {
            self.apply(&name, axis, edge);
        }
    }

    /// Ends a fixed step: clears every `just_pressed`. The engine drivers
    /// ([`crate::app::EcsGame`], [`crate::headless::Headless::step`]) call
    /// this after each step.
    pub fn end_step(&mut self) {
        for action in self.actions.values_mut() {
            action.value.just_pressed = false;
        }
    }
}

/// The value a binding produces from the devices, and whether it saw a
/// press edge this frame.
fn sample(binding: &ActionBinding, input: &InputState, gamepads: &GamepadState) -> (f32, bool) {
    match binding {
        ActionBinding::Buttons(bindings) => {
            let mut held = false;
            let mut edge = false;
            for binding in bindings {
                let (h, e) = sample_binding(*binding, input, gamepads);
                held |= h;
                edge |= e;
            }
            (if held { 1.0 } else { 0.0 }, edge)
        }
        ActionBinding::Axis { axis, gamepad_axis } => {
            let keys = axis.map_or(0.0, |[negative, positive]| {
                let held = |binding: Binding| sample_binding(binding, input, gamepads).0;
                held(positive) as i32 as f32 - held(negative) as i32 as f32
            });
            let pad = gamepad_axis.map_or(0.0, |axis| gamepads.axis(axis));
            // Whichever source is further from centre wins, so a resting
            // stick does not cancel the keys and vice versa.
            let value = if pad.abs() > keys.abs() { pad } else { keys };
            (value, false)
        }
    }
}

/// Whether `binding` is held, and whether it went down this frame.
fn sample_binding(binding: Binding, input: &InputState, gamepads: &GamepadState) -> (bool, bool) {
    match binding {
        Binding::Key(code) => (input.pressed(code), input.just_pressed(code)),
        Binding::Mouse(button) => {
            let held = mouse_index(button).is_some_and(|index| input.mouse_buttons[index]);
            (held, false)
        }
        Binding::Gamepad(button) => (gamepads.pressed(button), gamepads.just_pressed(button)),
    }
}

/// The [`InputState::mouse_buttons`] index of `button`, for the three it
/// tracks.
fn mouse_index(button: MouseButton) -> Option<usize> {
    match button {
        MouseButton::Left => Some(MOUSE_LEFT),
        MouseButton::Right => Some(MOUSE_RIGHT),
        MouseButton::Middle => Some(MOUSE_MIDDLE),
        _ => None,
    }
}

/// Frame system: derives the [`Actions`] resource from [`InputState`] and
/// [`GamepadState`] ([`Actions::update`]).
///
/// Does nothing without an [`Actions`] resource, or without an
/// [`InputState`] (a headless engine), whose actions then keep their
/// injected values. A missing [`GamepadState`] counts as no gamepads.
pub fn actions_system(_world: &mut World, res: &mut Resources) {
    let Some(mut actions) = res.get_mut::<Actions>().map(std::mem::take) else {
        return;
    };
    if let Some(input) = res.get::<InputState>() {
        let no_gamepads = GamepadState::default();
        let gamepads = res.get::<GamepadState>().unwrap_or(&no_gamepads);
        actions.update(input, gamepads);
    }
    res.insert(actions);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ecs::Time;

    /// `jump`, `move_x`, and `fire` as a starter project would bind them.
    fn config() -> InputConfig {
        toml::from_str(
            r#"
            [actions]
            jump = ["Space", "Gamepad:South"]
            move_x = { axis = ["KeyA", "KeyD"], gamepad_axis = "LeftStickX" }
            fire = ["Mouse:Left"]
            "#,
        )
        .expect("config parses")
    }

    /// `InputState` with `keys` held (pressed this frame).
    fn keys(keys: &[KeyCode]) -> InputState {
        let mut input = InputState::default();
        for key in keys {
            input.press(*key);
        }
        input
    }

    #[test]
    fn bindings_parse_and_print_in_manifest_form() {
        for (text, binding) in [
            ("Space", Binding::Key(KeyCode::Space)),
            ("KeyA", Binding::Key(KeyCode::KeyA)),
            ("ArrowUp", Binding::Key(KeyCode::ArrowUp)),
            ("F35", Binding::Key(KeyCode::F35)),
            ("Mouse:Middle", Binding::Mouse(MouseButton::Middle)),
            (
                "Gamepad:DPadLeft",
                Binding::Gamepad(GamepadButton::DPadLeft),
            ),
        ] {
            assert_eq!(text.parse::<Binding>(), Ok(binding), "{text}");
            assert_eq!(binding.to_string(), text);
        }
        assert_eq!(KEY_CODES.len(), 194, "one entry per winit 0.30 KeyCode");
    }

    #[test]
    fn bad_key_names_error_naming_the_key() {
        let err = "Spce".parse::<Binding>().unwrap_err();
        assert!(err.contains("unknown key `Spce`"), "{err}");
        let err = "Mouse:Back".parse::<Binding>().unwrap_err();
        assert!(err.contains("unknown mouse button `Back`"), "{err}");
        let err = "Gamepad:Cross".parse::<Binding>().unwrap_err();
        assert!(err.contains("unknown gamepad button `Cross`"), "{err}");
        assert!(err.contains("South"), "lists the valid names: {err}");

        // Through the manifest: the error still names the key.
        let err = toml::from_str::<InputConfig>("[actions]\njump = [\"Spce\"]\n").unwrap_err();
        assert!(err.to_string().contains("unknown key `Spce`"), "{err}");
        let err = toml::from_str::<InputConfig>(
            "[actions]\nmove = { axis = [\"KeyA\"], gamepad_axis = \"LeftStickX\" }\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("exactly two"), "{err}");
        let err =
            toml::from_str::<InputConfig>("[actions]\nmove = { gamepad_axes = \"LeftStickX\" }\n")
                .unwrap_err();
        assert!(err.to_string().contains("gamepad_axes"), "{err}");
        let err = toml::from_str::<InputConfig>("[actions]\nmove = {}\n").unwrap_err();
        assert!(err.to_string().contains("needs `axis`"), "{err}");
        let err = toml::from_str::<InputConfig>("[actions]\nmove = { gamepad_axis = \"Stick\" }\n")
            .unwrap_err();
        assert!(
            err.to_string().contains("unknown gamepad axis `Stick`"),
            "{err}"
        );
    }

    #[test]
    fn config_round_trips_through_toml() {
        let config = config();
        assert_eq!(
            config.actions["jump"],
            ActionBinding::Buttons(vec![
                Binding::Key(KeyCode::Space),
                Binding::Gamepad(GamepadButton::South)
            ])
        );
        assert_eq!(
            config.actions["move_x"],
            ActionBinding::Axis {
                axis: Some([Binding::Key(KeyCode::KeyA), Binding::Key(KeyCode::KeyD)]),
                gamepad_axis: Some(GamepadAxis::LeftStickX),
            }
        );
        let text = toml::to_string(&config).expect("serializes");
        assert!(
            text.contains("jump = [\"Space\", \"Gamepad:South\"]"),
            "{text}"
        );
        assert!(text.contains("gamepad_axis = \"LeftStickX\""), "{text}");
        assert_eq!(
            toml::from_str::<InputConfig>(&text).expect("parses"),
            config
        );
        assert!(InputConfig::default().is_empty());
        assert!(
            toml::from_str::<InputConfig>("")
                .expect("empty table")
                .is_empty()
        );
    }

    #[test]
    fn axis_from_two_keys() {
        let mut actions = Actions::from_config(&config());
        let pads = GamepadState::default();
        for (held, expected) in [
            (&[][..], 0.0),
            (&[KeyCode::KeyA][..], -1.0),
            (&[KeyCode::KeyD][..], 1.0),
            (&[KeyCode::KeyA, KeyCode::KeyD][..], 0.0),
        ] {
            actions.update(&keys(held), &pads);
            assert_eq!(actions.axis("move_x"), expected, "{held:?}");
            assert_eq!(actions.pressed("move_x"), expected != 0.0, "{held:?}");
            actions.end_step();
        }
        assert_eq!(actions.axis("jump"), 0.0, "button actions read 0 or 1");
        actions.update(&keys(&[KeyCode::Space]), &pads);
        assert_eq!(actions.axis("jump"), 1.0);
        assert_eq!(actions.axis("nope"), 0.0, "unknown actions are zero");
        assert!(!actions.pressed("nope"));
    }

    /// The gilrs-free path: `GamepadState` filled in by hand.
    #[test]
    fn gamepad_buttons_and_axes_drive_actions() {
        let mut actions = Actions::from_config(&config());
        let mut pads = GamepadState::default();
        pads.press(GamepadButton::South);
        pads.set_axis(GamepadAxis::LeftStickX, 0.7);
        actions.update(&InputState::default(), &pads);
        assert!(actions.pressed("jump"));
        assert!(actions.just_pressed("jump"));
        assert_eq!(actions.axis("move_x"), 0.7);
        assert!(actions.pressed("move_x"), "beyond the threshold");

        // The source further from centre wins.
        pads.set_axis(GamepadAxis::LeftStickX, -0.3);
        actions.update(&keys(&[KeyCode::KeyD]), &pads);
        assert_eq!(actions.axis("move_x"), 1.0);
        actions.update(&InputState::default(), &pads);
        assert_eq!(actions.axis("move_x"), -0.3);
        assert!(!actions.pressed("move_x"), "below the threshold");

        pads.release(GamepadButton::South);
        actions.update(&InputState::default(), &pads);
        assert!(!actions.pressed("jump"));
        pads.set_axis(GamepadAxis::LeftStickX, 5.0);
        assert_eq!(pads.axis(GamepadAxis::LeftStickX), 1.0, "clamped");
        pads.release_all();
        assert_eq!(pads.axis(GamepadAxis::LeftStickX), 0.0);
    }

    #[test]
    fn mouse_buttons_hold_actions() {
        let mut actions = Actions::from_config(&config());
        let mut input = InputState::default();
        input.mouse_buttons[MOUSE_LEFT] = true;
        actions.update(&input, &GamepadState::default());
        assert!(actions.pressed("fire") && actions.just_pressed("fire"));
        input.mouse_buttons[MOUSE_LEFT] = false;
        actions.update(&input, &GamepadState::default());
        assert!(!actions.pressed("fire"));
    }

    #[test]
    fn just_pressed_lasts_until_a_step_has_run() {
        let mut actions = Actions::from_config(&config());
        let pads = GamepadState::default();
        let mut input = keys(&[KeyCode::Space]);
        actions.update(&input, &pads);
        assert!(actions.just_pressed("jump"));
        input.end_frame();
        // Another frame with no step in between keeps the edge.
        actions.update(&input, &pads);
        assert!(actions.just_pressed("jump"), "latched until a step runs");
        actions.end_step();
        assert!(!actions.just_pressed("jump"));
        assert!(actions.pressed("jump"), "still held");
        actions.update(&input, &pads);
        assert!(!actions.just_pressed("jump"), "holding is not a new press");

        // A tap that begins and ends within one frame is still a press.
        input.release(KeyCode::Space);
        input.end_frame();
        input.press(KeyCode::Space);
        input.release(KeyCode::Space);
        actions.update(&input, &pads);
        assert!(!actions.pressed("jump"));
        assert!(actions.just_pressed("jump"), "the tap is not lost");
    }

    #[test]
    fn set_value_injects_and_rejects_unknown_actions() {
        let mut actions = Actions::from_config(&config());
        let value = actions.set_pressed("jump", true).unwrap();
        assert_eq!(
            value,
            ActionValue {
                pressed: true,
                just_pressed: true,
                axis: 1.0
            }
        );
        actions.end_step();
        let value = actions.set_pressed("jump", true).unwrap();
        assert!(!value.just_pressed, "already held: no new edge");
        let value = actions.set_value("move_x", -2.0).unwrap();
        assert_eq!(value.axis, -1.0, "clamped");
        assert!(value.pressed && value.just_pressed);
        let value = actions.set_value("move_x", 0.2).unwrap();
        assert!(!value.pressed, "below the threshold");

        let err = actions.set_pressed("fly", true).unwrap_err().to_string();
        assert!(err.contains("unknown action `fly`"), "{err}");
        assert!(err.contains("fire, jump, move_x"), "{err}");
        let err = Actions::new()
            .set_pressed("fly", true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("none defined"), "{err}");
    }

    #[test]
    fn actions_system_derives_with_devices_and_keeps_injections_without() {
        // Headless: no InputState, so injected values survive the system.
        let mut res = Resources::new();
        res.insert(Time::default());
        let mut actions = Actions::from_config(&config());
        actions.set_pressed("jump", true).unwrap();
        res.insert(actions);
        actions_system(&mut World::new(), &mut res);
        assert!(res.expect::<Actions>().pressed("jump"));

        // With devices the system derives from them.
        res.insert(keys(&[KeyCode::KeyD]));
        let mut pads = GamepadState::default();
        pads.set_axis(GamepadAxis::LeftStickX, -0.2);
        res.insert(pads);
        actions_system(&mut World::new(), &mut res);
        let actions = res.expect::<Actions>();
        assert!(
            !actions.pressed("jump"),
            "no key held: the injection is replaced"
        );
        assert_eq!(actions.axis("move_x"), 1.0);

        // No Actions resource at all: nothing to do.
        let mut empty = Resources::new();
        actions_system(&mut World::new(), &mut empty);
        assert!(empty.get::<Actions>().is_none());
    }

    #[test]
    fn iter_lists_actions_by_name() {
        let actions = Actions::from_config(&config());
        let names: Vec<&str> = actions.iter().map(|(name, ..)| name).collect();
        assert_eq!(names, ["fire", "jump", "move_x"]);
        assert!(actions.contains("jump") && !actions.contains("run"));
    }
}
