//! Five crates drop onto a ground slab and settle into a stack (GE-034).
//! Each crate is a unit cube with a dynamic [`RigidBody`] and a box
//! [`Collider`]; the ground has a fixed body. `physics_system` steps the
//! rapier3d simulation every fixed step and writes the crates' transforms
//! back.
//!
//! Run with `cargo run --example physics_stack`. Escape quits. The golden
//! test `golden_physics_settled` renders this scene after 3 s.

use shin::app::{self, AppConfig, EcsGame};
use shin::asset::{self, AssetSource};
use shin::ecs::{Name, Resources, Schedule, World};
use shin::gpu::GpuContext;
use shin::math::{Camera, Projection, Quat, Transform, Vec3};
use shin::physics::{BodyKind, Collider, ColliderShape, RigidBody, physics_system};
use shin::render::{LightUniform, MeshInstance, RenderState, render_world};

/// Number of crates in the stack.
pub const CRATES: usize = 5;
/// Height of the lowest crate's centre when it is dropped; each crate
/// starts [`DROP_SPACING`] above the one below.
pub const DROP_HEIGHT: f32 = 1.0;
/// Vertical distance between the crates' centres when they are dropped
/// (they are 1 m tall, so 0.2 m apart).
pub const DROP_SPACING: f32 = 1.2;
/// Linear clear colour: a dark blue-grey.
pub const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.03,
    g: 0.04,
    b: 0.06,
    a: 1.0,
};

/// The camera: back, up, and to the right of the stack, looking at its
/// middle.
pub fn camera() -> Camera {
    let mut transform = Transform::from_translation(Vec3::new(4.0, 4.0, 9.0));
    transform.looking_at(Vec3::new(0.0, 2.0, 0.0), Vec3::Y);
    Camera {
        transform,
        projection: Projection::Perspective {
            fov_y_radians: 45f32.to_radians(),
            near: 0.1,
            far: 100.0,
        },
    }
}

/// Builds the [`RenderState`] for colour targets of `format` and spawns
/// the ground (top face at y = 0) and the crates, hanging apart above it.
pub fn setup(
    world: &mut World,
    res: &mut Resources,
    ctx: &GpuContext,
    format: wgpu::TextureFormat,
) {
    let mut state = RenderState::new(&ctx.device, &ctx.queue, format);
    state.light = LightUniform {
        direction: [-1.0, -2.0, -1.5, 0.0],
        color: [1.0, 1.0, 1.0, 1.0],
        ambient: [0.12, 0.12, 0.14, 1.0],
    };
    // Registered under its built-in source, so play mode can snapshot the
    // scene (the golden test plays it).
    let cube = state.registry.add_with_source(
        &ctx.device,
        &asset::cube(),
        AssetSource::Builtin("cube".into()),
    );
    res.insert(state);

    // The unit cube's collider; each entity's scale sizes it.
    let unit_box = Collider::new(ColliderShape::Cuboid(Vec3::splat(0.5)));
    world.spawn((
        Transform::from_trs(
            Vec3::new(0.0, -0.25, 0.0),
            Quat::IDENTITY,
            Vec3::new(12.0, 0.5, 12.0),
        ),
        MeshInstance {
            mesh: cube,
            color: [0.2, 0.24, 0.2, 1.0],
            texture: None,
        },
        Name("ground".into()),
        RigidBody::new(BodyKind::Fixed),
        unit_box,
    ));
    for i in 0..CRATES {
        let shade = i as f32 / CRATES as f32;
        world.spawn((
            Transform::from_trs(
                Vec3::new(0.0, DROP_HEIGHT + DROP_SPACING * i as f32, 0.0),
                Quat::from_rotation_y((15.0 * i as f32).to_radians()),
                Vec3::ONE,
            ),
            MeshInstance {
                mesh: cube,
                color: [0.75 - 0.35 * shade, 0.45, 0.2 + 0.6 * shade, 1.0],
                texture: None,
            },
            Name(format!("crate {}", i + 1)),
            RigidBody::new(BodyKind::Dynamic),
            unit_box,
        ));
    }
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let mut schedule = Schedule::new();
    schedule.add_fixed("physics", physics_system);
    let mut game = EcsGame::new(World::new(), schedule);
    game.resources.insert(camera());
    game.render_fn = Some(Box::new(|world, res, ctx, color, depth| {
        let texture = color.texture();
        if res.get::<RenderState>().is_none() {
            setup(world, res, ctx, texture.format());
        }
        let aspect = texture.width() as f32 / texture.height() as f32;
        render_world(world, res, ctx, color, depth, aspect);
    }));

    let config = AppConfig {
        title: "physics_stack".into(),
        clear_color: CLEAR,
        ..Default::default()
    };
    game.title = config.title.clone();
    app::run(config, game)
}
