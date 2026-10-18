//! GE-031: `assets/scenes/hierarchy.scene.ron`, a parent cube turned a
//! quarter turn about Y (and scaled 0.8) with two child spheres at local (±1.5, 0, 0),
//! renders the children where the parent's rotation carries them (in front
//! of and behind the cube), matching `tests/golden/hierarchy.png`.

mod common;

use std::path::Path;

use common::{GoldenOpts, assert_golden};
use shin::ecs::{Children, Entity, Name, Parent};
use shin::headless::Headless;
use shin::math::{Camera, Vec3};

const SIZE: u32 = 128;

/// The pixel world point `p` projects to.
fn to_pixel(engine: &Headless, p: Vec3) -> (u32, u32) {
    let ndc = engine
        .resources
        .expect::<Camera>()
        .view_projection(1.0)
        .project_point3(p);
    let x = (ndc.x * 0.5 + 0.5) * SIZE as f32;
    let y = (-ndc.y * 0.5 + 0.5) * SIZE as f32;
    (x.round() as u32, y.round() as u32)
}

#[test]
fn hierarchy_scene_matches_golden() -> anyhow::Result<()> {
    let mut engine = Headless::new(SIZE, SIZE)?;
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/scenes/hierarchy.scene.ron");
    let spawned = shin::scene::load(
        &mut engine.world,
        &mut engine.resources,
        &engine.gpu.device,
        &engine.gpu.queue,
        &path,
    )?;
    assert_eq!(spawned, 3);

    // The file's nesting became parent links.
    let named = |name: &str| -> Entity {
        engine
            .world
            .query::<(Entity, &Name)>()
            .iter()
            .find(|(_, n)| n.0 == name)
            .map(|(e, _)| e)
            .unwrap_or_else(|| panic!("no entity named {name}"))
    };
    let (body, front, back) = (named("body"), named("front"), named("back"));
    assert_eq!(engine.world.get::<&Parent>(front)?.0, body);
    assert_eq!(engine.world.get::<&Parent>(back)?.0, body);
    assert_eq!(engine.world.get::<&Children>(body)?.0, vec![front, back]);

    let frame = engine.render()?;
    assert_golden("hierarchy", &frame, GoldenOpts::default());

    // The spheres are drawn at their world positions: the parent's 0.8
    // scale and quarter turn take local (-1.5, 0, 0) to (0, 0, 1.2) in
    // front of the cube, and (1.5, 0, 0) to (0, 0, -1.2) behind it. The
    // background is black.
    let world_front = shin::ecs::world_transform(&engine.world, front).translation;
    assert!(
        world_front.abs_diff_eq(Vec3::new(0.0, 0.0, 1.2), 1e-5),
        "{world_front}"
    );
    let (x, y) = to_pixel(&engine, world_front);
    let pixel = frame.get_pixel(x, y).0;
    assert!(
        pixel[2] > pixel[0] + 40,
        "the blue front sphere is at ({x}, {y}): {pixel:?}"
    );
    // Where the spheres would be without propagation (beside the cube)
    // there is only background.
    for local in [Vec3::new(-1.5, 0.0, 0.0), Vec3::new(1.5, 0.0, 0.0)] {
        let (x, y) = to_pixel(&engine, local);
        assert_eq!(
            frame.get_pixel(x, y).0,
            [0, 0, 0, 255],
            "nothing at the unpropagated position ({x}, {y})"
        );
    }
    Ok(())
}
