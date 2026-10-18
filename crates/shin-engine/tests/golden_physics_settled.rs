//! GE-034: the five-crate stack of `examples/physics_stack.rs` after 3 s of
//! physics has settled into a column on the ground, matching
//! `tests/golden/physics_settled.png`.
//!
//! Includes the example so the test simulates exactly the scene it builds
//! (its `main` is unused here), through `Headless` in play mode, whose
//! default schedule runs `physics_system`.

mod common;

#[path = "../examples/physics_stack.rs"]
#[allow(dead_code)]
mod physics_stack;

use common::{GoldenOpts, assert_golden};
use shin::ecs::Name;
use shin::gpu::OFFSCREEN_COLOR_FORMAT;
use shin::headless::Headless;
use shin::math::Transform;

const WIDTH: u32 = 192;
const HEIGHT: u32 = 192;

#[test]
fn physics_stack_settles_and_matches_golden() -> anyhow::Result<()> {
    let mut engine = Headless::new(WIDTH, HEIGHT)?;
    engine.clear_color = physics_stack::CLEAR;
    physics_stack::setup(
        &mut engine.world,
        &mut engine.resources,
        &engine.gpu,
        OFFSCREEN_COLOR_FORMAT,
    );
    engine.resources.insert(physics_stack::camera());

    engine.play()?;
    engine.step(180);
    assert!((engine.play_time().unwrap() - 3.0).abs() < 1e-9);

    // Crate i rests on crate i - 1: centre at y = i + 0.5, within a few
    // centimetres of straight above where it was dropped (the twisted
    // crates nudge each other sideways as they land).
    let mut crates: Vec<(String, Transform)> = engine
        .world
        .query::<(&Name, &Transform)>()
        .iter()
        .filter(|(name, _)| name.0.starts_with("crate"))
        .map(|(name, t)| (name.0.clone(), *t))
        .collect();
    crates.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(crates.len(), physics_stack::CRATES);
    for (i, (name, t)) in crates.iter().enumerate() {
        let expected = i as f32 + 0.5;
        assert!(
            (t.translation.y - expected).abs() < 0.02,
            "{name} rests at y = {expected}: {}",
            t.translation
        );
        assert!(
            t.translation.x.abs() < 0.05 && t.translation.z.abs() < 0.05,
            "{name} fell straight down: {}",
            t.translation
        );
    }

    // The reference comes from `cargo test-all`. Other builds (this crate
    // alone, or without `--all-targets`) unify dependency features
    // differently, which nudges rapier's floating-point results: the
    // crates settle a fraction of a pixel apart and a few tenths of a
    // percent of pixels, all on crate edges, differ. The positions are
    // checked above, so allow that here.
    let image = engine.render()?;
    let opts = GoldenOpts {
        max_diff_fraction: 0.005,
        ..GoldenOpts::default()
    };
    assert_golden("physics_settled", &image, opts);
    Ok(())
}
