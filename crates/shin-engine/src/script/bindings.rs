//! The types and functions scripts see, registered on the Rhai engine.
//!
//! - `Vec3`: `vec3(x, y, z)`, fields `x`, `y`, `z`, `+`, `-`, `* number`,
//!   `length()`.
//! - `Quat`: `quat_euler(x, y, z)` (radians, intrinsic X then Y then Z, as
//!   the `entity.*` tools use), fields `x`, `y`, `z`, `w` (read-only),
//!   `quat * quat`, `quat * vec3`.
//! - `Transform`: fields `translation`, `rotation`, `scale`.
//! - `Entity`: fields `id` and `name` (read-only), `transform` (local to
//!   the parent), and `world_transform` (read-only; a copy, as of the
//!   start of the step).
//! - `time()`: simulated seconds since play started.
//!
//! Numbers are Rhai's `FLOAT` (`f64`) and are stored as `f32`; setters
//! also accept integers, so `v.y = 1` works.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use glam::EulerRot;
use rhai::{Dynamic, Engine, FLOAT, INT};
use serde_json::{Value, json};

use crate::math::{Quat, Transform, Vec3};

/// The `transform` scripts edit: a shared handle, so a change made through
/// any copy of it (`entity.transform.translation.y = 1.0`, or
/// `let t = entity.transform; t.scale = ...`) reaches the entity.
#[derive(Clone, Debug)]
pub(crate) struct ScriptTransform(pub(crate) Rc<RefCell<Transform>>);

/// The `entity` argument of `fn update(entity, dt)`.
#[derive(Clone, Debug)]
pub(crate) struct ScriptEntity {
    /// The entity's tool id ([`crate::ecs::Entity::to_bits`]).
    pub(crate) id: INT,
    /// Its [`crate::ecs::Name`], or empty.
    pub(crate) name: String,
    /// Its transform (local to its parent), written back after the call.
    pub(crate) transform: ScriptTransform,
    /// Its world-space transform at the start of the step (GE-031);
    /// scripts read it as `world_transform` and cannot change it.
    pub(crate) world: Transform,
}

/// Registers every script type and function on `engine`; `time()` reads
/// `clock`.
pub(crate) fn register(engine: &mut Engine, clock: Rc<Cell<f64>>) {
    register_vec3(engine);
    register_quat(engine);
    register_transform(engine);
    register_entity(engine);
    engine.register_fn("time", move || clock.get() as FLOAT);
}

fn register_vec3(engine: &mut Engine) {
    engine
        .register_type_with_name::<Vec3>("Vec3")
        .register_fn("vec3", |x: FLOAT, y: FLOAT, z: FLOAT| {
            Vec3::new(x as f32, y as f32, z as f32)
        })
        .register_fn("+", |a: Vec3, b: Vec3| a + b)
        .register_fn("-", |a: Vec3, b: Vec3| a - b)
        .register_fn("-", |a: Vec3| -a)
        .register_fn("*", |a: Vec3, s: FLOAT| a * s as f32)
        .register_fn("*", |s: FLOAT, a: Vec3| a * s as f32)
        .register_fn("length", |a: &mut Vec3| a.length() as FLOAT)
        .register_fn("to_string", |a: &mut Vec3| a.to_string())
        .register_fn("to_debug", |a: &mut Vec3| a.to_string());
    macro_rules! field {
        ($name:literal, $field:ident) => {
            engine
                .register_get($name, |v: &mut Vec3| v.$field as FLOAT)
                .register_set($name, |v: &mut Vec3, x: FLOAT| v.$field = x as f32)
                .register_set($name, |v: &mut Vec3, x: INT| v.$field = x as f32);
        };
    }
    field!("x", x);
    field!("y", y);
    field!("z", z);
}

fn register_quat(engine: &mut Engine) {
    engine
        .register_type_with_name::<Quat>("Quat")
        .register_fn("quat_euler", |x: FLOAT, y: FLOAT, z: FLOAT| {
            Quat::from_euler(EulerRot::XYZ, x as f32, y as f32, z as f32)
        })
        .register_fn("*", |a: Quat, b: Quat| a * b)
        .register_fn("*", |a: Quat, v: Vec3| a * v)
        .register_get("x", |q: &mut Quat| q.x as FLOAT)
        .register_get("y", |q: &mut Quat| q.y as FLOAT)
        .register_get("z", |q: &mut Quat| q.z as FLOAT)
        .register_get("w", |q: &mut Quat| q.w as FLOAT)
        .register_fn("to_string", |q: &mut Quat| q.to_string())
        .register_fn("to_debug", |q: &mut Quat| q.to_string());
}

fn register_transform(engine: &mut Engine) {
    engine
        .register_type_with_name::<ScriptTransform>("Transform")
        .register_get_set(
            "translation",
            |t: &mut ScriptTransform| t.0.borrow().translation,
            |t: &mut ScriptTransform, v: Vec3| t.0.borrow_mut().translation = v,
        )
        .register_get_set(
            "rotation",
            |t: &mut ScriptTransform| t.0.borrow().rotation,
            |t: &mut ScriptTransform, q: Quat| t.0.borrow_mut().rotation = q.normalize(),
        )
        .register_get_set(
            "scale",
            |t: &mut ScriptTransform| t.0.borrow().scale,
            |t: &mut ScriptTransform, v: Vec3| t.0.borrow_mut().scale = v,
        )
        .register_fn("to_string", |t: &mut ScriptTransform| {
            format!("{:?}", *t.0.borrow())
        });
}

fn register_entity(engine: &mut Engine) {
    engine
        .register_type_with_name::<ScriptEntity>("Entity")
        .register_get("id", |e: &mut ScriptEntity| e.id)
        .register_get("name", |e: &mut ScriptEntity| e.name.clone())
        // A fresh copy each read: writing to it changes nothing.
        .register_get("world_transform", |e: &mut ScriptEntity| {
            ScriptTransform(Rc::new(RefCell::new(e.world)))
        })
        .register_get_set(
            "transform",
            |e: &mut ScriptEntity| e.transform.clone(),
            |e: &mut ScriptEntity, t: ScriptTransform| {
                // `entity.transform.x = ...` writes the handle back to
                // itself; copying a cell onto itself would double-borrow.
                if !Rc::ptr_eq(&e.transform.0, &t.0) {
                    let value = *t.0.borrow();
                    *e.transform.0.borrow_mut() = value;
                }
            },
        );
}

/// `value` as JSON: unit is `null`, numbers, strings, arrays, and maps map
/// directly, a `Vec3` is `[x, y, z]`, a `Quat` is `[x, y, z, w]`, a
/// `Transform` is `{ translation, rotation, scale }`, and anything else is
/// its string form. Non-finite floats are `null`.
pub(crate) fn to_json(value: &Dynamic) -> Value {
    if value.is_unit() {
        return Value::Null;
    }
    if let Some(b) = value.clone().try_cast::<bool>() {
        return json!(b);
    }
    if let Some(i) = value.clone().try_cast::<INT>() {
        return json!(i);
    }
    if let Some(f) = value.clone().try_cast::<FLOAT>() {
        return json!(f);
    }
    if value.is_string() || value.is_char() {
        return json!(value.to_string());
    }
    if let Some(array) = value.clone().try_cast::<rhai::Array>() {
        return Value::Array(array.iter().map(to_json).collect());
    }
    if let Some(map) = value.clone().try_cast::<rhai::Map>() {
        return Value::Object(
            map.iter()
                .map(|(k, v)| (k.to_string(), to_json(v)))
                .collect(),
        );
    }
    if let Some(v) = value.clone().try_cast::<Vec3>() {
        return json!(v.to_array());
    }
    if let Some(q) = value.clone().try_cast::<Quat>() {
        return json!(q.to_array());
    }
    if let Some(t) = value.clone().try_cast::<ScriptTransform>() {
        let t = *t.0.borrow();
        return json!({
            "translation": t.translation.to_array(),
            "rotation": t.rotation.to_array(),
            "scale": t.scale.to_array(),
        });
    }
    json!(value.to_string())
}
