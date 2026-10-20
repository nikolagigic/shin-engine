//! Entity-component-system: a thin [`World`] wrapper over hecs, a
//! [`Resources`] map for singletons, a [`Schedule`] of plain-function
//! systems, and the [`Time`] resource. ADR 0003.
//!
//! Components are plain structs (for example [`crate::math::Transform`]).
//! Systems are plain functions `fn(&mut World, &mut Resources)`; a
//! [`Schedule`] runs them in insertion order, either once per fixed
//! simulation step or once per rendered frame.
//!
//! [`crate::app::EcsGame`] wires a world, resources, and schedule into the
//! game loop.
//!
//! Hierarchy (GE-031, [`hierarchy`]): a [`Parent`] makes an entity's
//! [`Transform`](crate::math::Transform) local to another entity;
//! [`propagate_transforms`] writes the resulting [`GlobalTransform`]s at
//! the end of every fixed step and before every render.
//!
//! Tools (GE-015): [`register_tools`] adds the `entity.*` tools,
//! `scene.clear`, and `sim.step`.

pub mod hierarchy;
mod tools;

use std::any::{Any, TypeId, type_name};
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};

pub use hecs::{Entity, Query, QueryBorrow, With, Without};
pub use hierarchy::{
    Children, GlobalTransform, Parent, child_map, despawn_recursive, parent_of,
    propagate_transforms, set_parent, subtree, world_transform,
};
pub use tools::register_tools;

/// The set of entities and their components.
///
/// Dereferences to [`hecs::World`], so `spawn`, `query`, `query_mut`,
/// `despawn`, and the rest of the hecs API are available directly.
#[derive(Default)]
pub struct World {
    inner: hecs::World,
}

impl World {
    /// An empty world.
    pub fn new() -> Self {
        Self::default()
    }
}

impl Deref for World {
    type Target = hecs::World;

    fn deref(&self) -> &hecs::World {
        &self.inner
    }
}

impl DerefMut for World {
    fn deref_mut(&mut self) -> &mut hecs::World {
        &mut self.inner
    }
}

/// Singleton values shared by systems, keyed by type: at most one value of
/// each type.
#[derive(Default)]
pub struct Resources {
    map: HashMap<TypeId, Box<dyn Any>>,
}

impl Resources {
    /// An empty resource map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores `value`, replacing any existing value of the same type.
    pub fn insert<T: 'static>(&mut self, value: T) {
        self.map.insert(TypeId::of::<T>(), Box::new(value));
    }

    /// Takes the value of type `T` out, if one was inserted.
    pub fn remove<T: 'static>(&mut self) -> Option<T> {
        self.map
            .remove(&TypeId::of::<T>())
            .and_then(|value| value.downcast().ok())
            .map(|value| *value)
    }

    /// The value of type `T`, if one was inserted.
    pub fn get<T: 'static>(&self) -> Option<&T> {
        self.map
            .get(&TypeId::of::<T>())
            .and_then(|value| value.downcast_ref())
    }

    /// The value of type `T` mutably, if one was inserted.
    pub fn get_mut<T: 'static>(&mut self) -> Option<&mut T> {
        self.map
            .get_mut(&TypeId::of::<T>())
            .and_then(|value| value.downcast_mut())
    }

    /// The value of type `T`.
    ///
    /// # Panics
    ///
    /// If no `T` was inserted; the message names the type.
    pub fn expect<T: 'static>(&self) -> &T {
        self.get()
            .unwrap_or_else(|| panic!("missing resource `{}`", type_name::<T>()))
    }

    /// The value of type `T` mutably.
    ///
    /// # Panics
    ///
    /// If no `T` was inserted; the message names the type.
    pub fn expect_mut<T: 'static>(&mut self) -> &mut T {
        self.get_mut()
            .unwrap_or_else(|| panic!("missing resource `{}`", type_name::<T>()))
    }
}

/// Simulation and frame timing. [`crate::app::EcsGame`] keeps this resource
/// up to date; systems read it from [`Resources`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Time {
    /// Length of one fixed simulation step, in seconds.
    pub fixed_dt: f64,
    /// Simulation seconds elapsed: `fixed_steps * fixed_dt`.
    pub elapsed: f64,
    /// Number of fixed steps run so far.
    pub fixed_steps: u64,
    /// Wall-clock seconds between the last two rendered frames.
    pub frame_dt: f64,
    /// Fraction of a fixed step left over at the last rendered frame, in
    /// `[0, 1)`, for interpolating between simulation states.
    pub alpha: f64,
}

impl Time {
    /// Records one fixed step of `dt` seconds: sets `fixed_dt`, increments
    /// `fixed_steps`, and adds `dt` to `elapsed`.
    pub fn advance_fixed(&mut self, dt: f64) {
        self.fixed_dt = dt;
        self.fixed_steps += 1;
        self.elapsed += dt;
    }
}

/// Component: a human-readable entity name, shown by `entity.list` and
/// saved in scene files. Names need not be unique.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Name(pub String);

/// Set to `ExitRequest(true)` from any system to close the window and make
/// [`crate::app::run`] return `Ok(())` after the current frame.
/// [`crate::app::EcsGame`] inserts `ExitRequest(false)` and reports it
/// through [`crate::app::Game::exit_requested`].
#[derive(Clone, Copy, Debug, Default)]
pub struct ExitRequest(pub bool);

/// Runs one fixed simulation step: advances the [`Time`] resource by `dt`
/// (inserting a default `Time` first if it is missing), runs the
/// schedule's fixed systems, so step `k` sees `fixed_steps == k`, and then
/// [`propagate_transforms`], so every [`GlobalTransform`] reflects what the
/// systems did.
///
/// This is the single fixed-step code path shared by
/// [`crate::app::EcsGame`] and [`crate::headless::Headless`].
pub fn fixed_step(world: &mut World, res: &mut Resources, schedule: &mut Schedule, dt: f64) {
    if res.get::<Time>().is_none() {
        res.insert(Time::default());
    }
    res.expect_mut::<Time>().advance_fixed(dt);
    schedule.run_fixed(world, res);
    propagate_transforms(world, res);
}

/// A system: a plain function over the world and resources.
pub type System = fn(&mut World, &mut Resources);

/// Ordered lists of systems: one run every fixed step, one run every frame.
#[derive(Default)]
pub struct Schedule {
    fixed: Vec<(&'static str, System)>,
    frame: Vec<(&'static str, System)>,
}

impl Schedule {
    /// An empty schedule.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends `system` to the fixed-step list. `name` is used in logs.
    pub fn add_fixed(&mut self, name: &'static str, system: System) -> &mut Self {
        self.fixed.push((name, system));
        self
    }

    /// Appends `system` to the per-frame list. `name` is used in logs.
    pub fn add_frame(&mut self, name: &'static str, system: System) -> &mut Self {
        self.frame.push((name, system));
        self
    }

    /// Puts `system` first in the fixed-step list.
    pub fn prepend_fixed(&mut self, name: &'static str, system: System) -> &mut Self {
        self.fixed.insert(0, (name, system));
        self
    }

    /// Whether a fixed-step system named `name` is in the schedule.
    pub fn has_fixed(&self, name: &str) -> bool {
        self.fixed.iter().any(|(n, _)| *n == name)
    }

    /// Whether a per-frame system named `name` is in the schedule.
    pub fn has_frame(&self, name: &str) -> bool {
        self.frame.iter().any(|(n, _)| *n == name)
    }

    /// Runs every fixed-step system once, in insertion order.
    pub fn run_fixed(&mut self, world: &mut World, res: &mut Resources) {
        run_all(&self.fixed, world, res);
    }

    /// Runs every per-frame system once, in insertion order.
    pub fn run_frame(&mut self, world: &mut World, res: &mut Resources) {
        run_all(&self.frame, world, res);
    }
}

fn run_all(systems: &[(&'static str, System)], world: &mut World, res: &mut Resources) {
    for (name, system) in systems {
        log::trace!("running system `{name}`");
        system(world, res);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::{Quat, Transform, Vec3};

    #[derive(Debug, PartialEq)]
    struct Score(u32);

    #[test]
    fn resources_round_trip() {
        let mut res = Resources::new();
        assert!(res.get::<Score>().is_none());

        res.insert(Score(1));
        assert_eq!(res.get::<Score>(), Some(&Score(1)));

        res.get_mut::<Score>().unwrap().0 += 1;
        assert_eq!(res.expect::<Score>(), &Score(2));

        res.expect_mut::<Score>().0 = 7;
        res.insert(5_u8);
        assert_eq!(res.expect::<Score>(), &Score(7));
        assert_eq!(res.expect::<u8>(), &5);

        res.insert(Score(9));
        assert_eq!(res.expect::<Score>(), &Score(9), "insert replaces");
    }

    #[test]
    fn remove_takes_the_resource_out() {
        let mut res = Resources::new();
        res.insert(Score(3));
        assert_eq!(res.remove::<Score>(), Some(Score(3)));
        assert_eq!(res.remove::<Score>(), None);
    }

    fn noop(_: &mut World, _: &mut Resources) {}

    #[test]
    fn prepend_and_has() {
        let mut schedule = Schedule::new();
        schedule.add_fixed("b", noop).prepend_fixed("a", noop);
        schedule.add_frame("f", noop);
        let names: Vec<_> = schedule.fixed.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["a", "b"]);
        assert!(schedule.has_fixed("a") && !schedule.has_fixed("f"));
        assert!(schedule.has_frame("f") && !schedule.has_frame("a"));
    }

    #[test]
    #[should_panic(expected = "Score")]
    fn expect_panics_with_type_name() {
        Resources::new().expect::<Score>();
    }

    #[test]
    #[should_panic(expected = "Score")]
    fn expect_mut_panics_with_type_name() {
        Resources::new().expect_mut::<Score>();
    }

    type Log = Vec<&'static str>;

    fn first(_: &mut World, res: &mut Resources) {
        res.expect_mut::<Log>().push("first");
    }

    fn second(_: &mut World, res: &mut Resources) {
        res.expect_mut::<Log>().push("second");
    }

    fn third(_: &mut World, res: &mut Resources) {
        res.expect_mut::<Log>().push("third");
    }

    #[test]
    fn run_fixed_runs_in_insertion_order() {
        let mut schedule = Schedule::new();
        schedule
            .add_fixed("second", second)
            .add_fixed("first", first)
            .add_fixed("third", third)
            .add_frame("frame-only", first);
        let mut world = World::new();
        let mut res = Resources::new();
        res.insert(Log::new());

        schedule.run_fixed(&mut world, &mut res);
        assert_eq!(res.expect::<Log>(), &["second", "first", "third"]);

        schedule.run_fixed(&mut world, &mut res);
        assert_eq!(res.expect::<Log>().len(), 6);
    }

    #[test]
    fn run_frame_runs_in_insertion_order() {
        let mut schedule = Schedule::new();
        schedule
            .add_frame("third", third)
            .add_frame("first", first)
            .add_fixed("fixed-only", second);
        let mut world = World::new();
        let mut res = Resources::new();
        res.insert(Log::new());

        schedule.run_frame(&mut world, &mut res);
        assert_eq!(res.expect::<Log>(), &["third", "first"]);
    }

    /// Rotates every `Transform` a quarter turn about +Y, through `Deref`.
    fn quarter_turn(world: &mut World, _: &mut Resources) {
        for transform in world.query_mut::<&mut Transform>() {
            transform.rotation = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        }
    }

    #[test]
    fn systems_query_components_through_deref() {
        let mut world = World::new();
        for i in 0..100 {
            world.spawn((Transform::from_translation(Vec3::X * i as f32),));
        }
        let mut schedule = Schedule::new();
        schedule.add_fixed("quarter_turn", quarter_turn);
        schedule.run_fixed(&mut world, &mut Resources::new());

        let mut count = 0;
        for transform in world.query::<&Transform>().iter() {
            assert!((transform.forward() - Vec3::NEG_X).length() < 1e-5);
            count += 1;
        }
        assert_eq!(count, 100);
    }

    /// Counts fixed steps and records the step number each system saw.
    fn record_step(_: &mut World, res: &mut Resources) {
        let step = res.expect::<Time>().fixed_steps;
        res.expect_mut::<Vec<u64>>().push(step);
    }

    #[test]
    fn ecs_game_fixed_update_advances_time() {
        use crate::app::{EcsGame, Game};

        const DT: f64 = 1.0 / 60.0;
        let mut schedule = Schedule::new();
        schedule.add_fixed("record_step", record_step);
        let mut game = EcsGame::new(World::new(), schedule);
        game.resources.insert(Vec::<u64>::new());

        let n = 250;
        for _ in 0..n {
            game.fixed_update(DT);
        }

        let time = game.resources.expect::<Time>();
        assert_eq!(time.fixed_steps, n);
        assert!(
            (time.elapsed - n as f64 * DT).abs() < 1e-9,
            "elapsed = {}",
            time.elapsed
        );
        assert_eq!(time.fixed_dt, DT);
        // Time is advanced before the systems run, so step k sees k.
        let seen = game.resources.expect::<Vec<u64>>();
        assert_eq!(seen, &(1..=n).collect::<Vec<_>>());
    }

    #[test]
    fn advance_fixed_counts_steps_and_time() {
        let mut time = Time::default();
        time.advance_fixed(0.5);
        time.advance_fixed(0.25);
        assert_eq!(time.fixed_steps, 2);
        assert_eq!(time.fixed_dt, 0.25);
        assert_eq!(time.elapsed, 0.75);
    }

    #[test]
    fn ecs_game_restores_missing_time() {
        use crate::app::{EcsGame, Game};

        let mut game = EcsGame::default();
        game.resources = Resources::new();
        game.fixed_update(0.5);
        assert_eq!(game.resources.expect::<Time>().fixed_steps, 1);
    }
}
