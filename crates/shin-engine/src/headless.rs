//! Window-less engine handle: GPU context, world, schedule, and an offscreen
//! target, driven by tools and tests instead of an event loop. Owned by GE-013.
//!
//! [`Headless`] is what every [`crate::tool::Tool`] operates on. Simulation
//! is advanced explicitly with [`Headless::step`] (no wall clock), and
//! frames are produced on demand with [`Headless::render`], so results are
//! deterministic.
//!
//! Play mode (GE-024): [`Headless::play`], [`Headless::pause`], and
//! [`Headless::stop`] snapshot the scene and restore it, with the
//! [`PlayState`] resource saying which mode the engine is in; see [`play`].
//! Tools: [`register_tools`] adds `sim.play`, `sim.pause`, and `sim.stop`.

pub mod play;

use std::path::Path;

use anyhow::Context as _;

use crate::app::RenderFn;
use crate::asset::Assets;
use crate::audio::audio_system;
use crate::ecs::{self, Resources, Schedule, Time, World};
use crate::gpu::{GpuContext, OFFSCREEN_COLOR_FORMAT, OffscreenTarget};
use crate::input::Actions;
use crate::math::{Camera, Projection, Transform, Vec3};
use crate::net;
use crate::physics::{PhysicsWorld, physics_system};
use crate::project::{Project, ProjectRoot};
use crate::render::{LightUniform, RenderState, render_world};
use crate::scene;
use crate::script::{self, script_system};
use crate::stats::{FrameStats, frame_stats_system};

pub use play::{PlaySnapshot, PlayState, register_tools};

/// A window-less engine: everything [`crate::app::EcsGame`] has, plus a GPU
/// context and an offscreen target instead of a window.
pub struct Headless {
    /// GPU context created without a surface.
    pub gpu: GpuContext,
    /// Entities and components.
    pub world: World,
    /// Singletons shared by systems; always contains a [`Time`].
    pub resources: Resources,
    /// Systems run by [`Self::step`] (fixed) and [`Self::render`] (frame).
    pub schedule: Schedule,
    /// The colour + depth target [`Self::render`] draws into.
    pub target: OffscreenTarget,
    /// Length of one simulation step in seconds; default `1.0 / 60.0`.
    pub fixed_dt: f64,
    /// Colour the target is cleared to before drawing, in linear RGB.
    /// Default black.
    pub clear_color: wgpu::Color,
    /// Optional drawing callback, called by [`Self::render`] after the
    /// clear with the target's colour and depth views. [`Self::new`]
    /// installs one that calls [`render_world`]; with none installed a
    /// render is just the clear.
    pub render_fn: Option<RenderFn>,
}

impl Headless {
    /// Creates a GPU context without a surface and a `width` x `height`
    /// offscreen target, with an empty world and a schedule holding
    /// [`frame_stats_system`] and [`audio_system`] (frame), then
    /// [`script_system`] and [`physics_system`] (fixed):
    /// [`Self::from_parts`] with empty parts and that schedule.
    ///
    /// Resources: default [`Time`] and [`FrameStats`]; a [`RenderState`] with no meshes and
    /// [`default_light`]; [`default_camera`]; [`PlayState::Edit`]; an [`Assets`] cache
    /// rooted at the current working directory; an empty [`PhysicsWorld`]; and empty [`Actions`]
    /// (a project's are installed by [`Self::load_project`]). [`Self::render_fn`]
    /// draws the world with [`render_world`], so spawning an entity with a
    /// `Transform` and a `MeshInstance` is enough to see it.
    ///
    /// Fails if no GPU adapter is available or a dimension is zero.
    pub fn new(width: u32, height: u32) -> anyhow::Result<Self> {
        Self::from_parts(
            World::new(),
            Resources::new(),
            default_schedule(),
            (width, height),
        )
    }

    /// A headless engine over a world, resources, and schedule the caller
    /// built, with a new GPU context and a `size.0` x `size.1` offscreen
    /// target. This is how a game serves its own world (GE-027): it builds
    /// the parts it would give [`crate::app::EcsGame`], its own systems in
    /// `schedule`, and wraps them here.
    ///
    /// `world` and `schedule` are used as given; nothing is added to the
    /// schedule (add [`frame_stats_system`] for frame statistics,
    /// [`script_system`] for [`crate::script::Script`]s,
    /// [`audio_system`] for spatial sounds, and [`physics_system`] for
    /// rigid bodies). Each
    /// resource [`Self::new`] installs that `resources` lacks is inserted
    /// with the same default: [`Time`], [`FrameStats`], a [`RenderState`]
    /// with [`default_light`], [`default_camera`], [`PlayState::Edit`],
    /// [`Assets`] rooted at the working directory, and a [`PhysicsWorld`].
    /// Resources already present are kept.
    /// `resources` should not hold a [`RenderState`]: GPU objects made on
    /// another device cannot be drawn here. [`Self::render_fn`] draws the
    /// world with [`render_world`].
    ///
    /// Fails if a dimension is zero (checked before the GPU is touched) or
    /// no GPU adapter is available.
    pub fn from_parts(
        world: World,
        resources: Resources,
        schedule: Schedule,
        size: (u32, u32),
    ) -> anyhow::Result<Self> {
        check_size(size)?;
        Self::assemble(GpuContext::new(None)?, world, resources, schedule, size)
    }

    /// [`Self::new`] on an existing GPU context, for example a clone of a
    /// window's, so meshes the engine uploads can be drawn by the window
    /// too.
    ///
    /// Fails if a dimension is zero.
    pub fn from_gpu(gpu: GpuContext, width: u32, height: u32) -> anyhow::Result<Self> {
        Self::assemble(
            gpu,
            World::new(),
            Resources::new(),
            default_schedule(),
            (width, height),
        )
    }

    /// Shared body of [`Self::from_parts`] and [`Self::from_gpu`]: inserts
    /// missing default resources and creates the target on `gpu`.
    fn assemble(
        gpu: GpuContext,
        world: World,
        mut resources: Resources,
        schedule: Schedule,
        size: (u32, u32),
    ) -> anyhow::Result<Self> {
        check_size(size)?;
        let target = OffscreenTarget::new(&gpu.device, size.0, size.1);
        if resources.get::<Time>().is_none() {
            resources.insert(Time::default());
        }
        if resources.get::<FrameStats>().is_none() {
            resources.insert(FrameStats::default());
        }
        if resources.get::<RenderState>().is_none() {
            let mut state = RenderState::new(&gpu.device, &gpu.queue, OFFSCREEN_COLOR_FORMAT);
            state.light = default_light();
            resources.insert(state);
        }
        if resources.get::<Camera>().is_none() {
            resources.insert(default_camera());
        }
        if resources.get::<PlayState>().is_none() {
            resources.insert(PlayState::Edit);
        }
        if resources.get::<Assets>().is_none() {
            let cwd = std::env::current_dir().context("failed to read the working directory")?;
            resources.insert(Assets::new(cwd));
        }
        if resources.get::<Actions>().is_none() {
            resources.insert(Actions::new());
        }
        if resources.get::<PhysicsWorld>().is_none() {
            resources.insert(PhysicsWorld::default());
        }
        let render_fn: RenderFn = Box::new(|world, res, ctx, color, depth| {
            let size = color.texture().size();
            let aspect = size.width as f32 / size.height as f32;
            render_world(world, res, ctx, color, depth, aspect);
        });
        Ok(Self {
            gpu,
            world,
            resources,
            schedule,
            target,
            fixed_dt: 1.0 / 60.0,
            clear_color: wgpu::Color::BLACK,
            render_fn: Some(render_fn),
        })
    }

    /// [`Self::new`] at the window size from `<dir>/shin.toml`, then
    /// [`Self::load_project`] on `dir`. What `shin-mcp --project` and the
    /// tests start from.
    pub fn open_project(dir: &Path) -> anyhow::Result<Self> {
        let project = Project::load(dir)?;
        let mut engine = Self::new(project.window.width, project.window.height)?;
        engine.load_project(dir)?;
        Ok(engine)
    }

    /// Opens the project in `dir` on this engine and returns its manifest.
    ///
    /// Reads `<dir>/shin.toml`, replaces the [`Assets`] cache with one
    /// rooted at the project, inserts the [`ProjectRoot`] resource (the
    /// canonical `dir`), installs the project's input [`Actions`] (all
    /// released), resizes the target to the project's window size,
    /// and replaces every entity: with the entry scene if the project has
    /// one (which also sets the camera and light it describes), otherwise
    /// with nothing.
    ///
    /// Fails if `dir` or its manifest cannot be read, or the entry scene
    /// does not load; the engine is then left as it was.
    pub fn load_project(&mut self, dir: &Path) -> anyhow::Result<Project> {
        let root = dir
            .canonicalize()
            .with_context(|| format!("cannot open project directory {}", dir.display()))?;
        let project = Project::load(&root)?;
        let root = ProjectRoot(root);
        let entry_scene = project
            .entry_scene
            .as_deref()
            .map(|scene| root.resolve(scene))
            .transpose()
            .context("invalid entry_scene in shin.toml")?;

        // Scene loading resolves meshes through the `Assets` resource, so
        // swap in the project's cache first and put the old one back if
        // the scene fails to load.
        let assets = Assets::new(root.0.clone());
        let previous = match self.resources.get_mut::<Assets>() {
            Some(current) => Some(std::mem::replace(current, assets)),
            None => {
                self.resources.insert(assets);
                None
            }
        };
        if let Some(path) = &entry_scene {
            let loaded = scene::load(
                &mut self.world,
                &mut self.resources,
                &self.gpu.device,
                &self.gpu.queue,
                path,
            );
            if let Err(err) = loaded {
                if let Some(previous) = previous {
                    self.resources.insert(previous);
                }
                return Err(err.context(format!("failed to load project `{}`", project.name)));
            }
        } else {
            self.world.clear();
        }

        log::info!("opened project `{}` in {}", project.name, root.0.display());
        self.resources.insert(root);
        self.resources.insert(Actions::from_config(&project.input));
        self.resize(project.window.width, project.window.height);
        Ok(project)
    }

    /// Recreates the offscreen target at the new size. A zero width or
    /// height is ignored, as with a minimised window.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        if (width, height) != (self.target.width, self.target.height) {
            self.target = OffscreenTarget::new(&self.gpu.device, width, height);
        }
    }

    /// Runs `n` fixed steps of [`Self::fixed_dt`], advancing [`Time`]
    /// exactly as [`crate::app::EcsGame`] does (both use
    /// [`ecs::fixed_step`]). After each step the [`Actions`] resource's
    /// `just_pressed` edges are cleared ([`Actions::end_step`]), and on a
    /// multiplayer client the snapshot that step made due is applied
    /// ([`net::apply_pending`]), and the entities scripts spawned or
    /// despawned are ([`script::apply_commands`]); failures are logged.
    pub fn step(&mut self, n: u32) {
        for _ in 0..n {
            ecs::fixed_step(
                &mut self.world,
                &mut self.resources,
                &mut self.schedule,
                self.fixed_dt,
            );
            if let Some(actions) = self.resources.get_mut::<Actions>() {
                actions.end_step();
            }
            if let Err(err) = net::apply_pending(
                &mut self.world,
                &mut self.resources,
                &self.gpu.device,
                &self.gpu.queue,
            ) {
                log::error!("{err:#}");
            }
            if let Err(err) =
                script::apply_commands(&mut self.world, &mut self.resources, &self.gpu.device)
            {
                log::error!("{err:#}");
            }
        }
    }

    /// Renders one frame ([`Self::render_to_target`]) and reads it back.
    pub fn render(&mut self) -> anyhow::Result<image::RgbaImage> {
        self.render_to_target();
        self.target.read_pixels(&self.gpu)
    }

    /// Renders one frame into [`Self::target`] without reading it back.
    ///
    /// Sets `Time::frame_dt` and `Time::alpha` to zero (headless frames
    /// have no wall-clock duration and always land on a whole step), runs
    /// the frame systems and then [`ecs::propagate_transforms`], clears the
    /// target to [`Self::clear_color`] (depth to 1.0), then calls
    /// [`Self::render_fn`] if one is installed.
    pub fn render_to_target(&mut self) {
        if self.resources.get::<Time>().is_none() {
            self.resources.insert(Time::default());
        }
        let time = self.resources.expect_mut::<Time>();
        time.frame_dt = 0.0;
        time.alpha = 0.0;
        self.schedule
            .run_frame(&mut self.world, &mut self.resources);
        ecs::propagate_transforms(&mut self.world, &mut self.resources);

        self.clear();
        if let Some(render_fn) = &mut self.render_fn {
            render_fn(
                &mut self.world,
                &mut self.resources,
                &self.gpu,
                &self.target.color_view,
                &self.target.depth_view,
            );
        }
    }

    /// [`Self::render`], encoded as a PNG.
    pub fn render_png(&mut self) -> anyhow::Result<Vec<u8>> {
        let image = self.render()?;
        let mut png = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .context("failed to encode the frame as PNG")?;
        Ok(png)
    }

    /// Clears the colour target to [`Self::clear_color`] and depth to 1.0.
    fn clear(&self) {
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("headless clear encoder"),
            });
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("headless clear pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.target.color_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(self.clear_color),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.target.depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        self.gpu.queue.submit([encoder.finish()]);
    }
}

/// Fails unless both dimensions of `size` are non-zero.
fn check_size((width, height): (u32, u32)) -> anyhow::Result<()> {
    anyhow::ensure!(
        width > 0 && height > 0,
        "headless target must be at least 1x1, got {width}x{height}"
    );
    Ok(())
}

/// The schedule [`Headless::new`] starts with: [`frame_stats_system`] and
/// [`audio_system`] per frame, and per fixed step [`script_system`] then
/// [`physics_system`] (so kinematic bodies follow what scripts did in the
/// same step).
fn default_schedule() -> Schedule {
    let mut schedule = Schedule::new();
    schedule.add_frame("frame_stats", frame_stats_system);
    schedule.add_frame("audio", audio_system);
    schedule.add_fixed("script", script_system);
    schedule.add_fixed("physics", physics_system);
    schedule
}

/// The camera [`Headless::new`] installs: at `(0, 2, 5)` looking at the
/// origin, 45° vertical field of view, clip planes 0.1..100.
pub fn default_camera() -> Camera {
    let mut transform = Transform::from_translation(Vec3::new(0.0, 2.0, 5.0));
    transform.looking_at(Vec3::ZERO, Vec3::Y);
    Camera {
        transform,
        projection: Projection::Perspective {
            fov_y_radians: 45f32.to_radians(),
            near: 0.1,
            far: 100.0,
        },
    }
}

/// The light [`Headless::new`] installs: white, travelling along
/// `(1, -2, -1)` (from above, front-left of the default camera), with 0.1
/// ambient. The same light the golden tests use.
pub fn default_light() -> LightUniform {
    LightUniform {
        direction: [1.0, -2.0, -1.0, 0.0],
        color: [1.0, 1.0, 1.0, 1.0],
        ambient: [0.1, 0.1, 0.1, 1.0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_recreates_target_and_ignores_zero() -> anyhow::Result<()> {
        let mut engine = Headless::new(8, 8)?;
        engine.resize(16, 4);
        assert_eq!((engine.target.width, engine.target.height), (16, 4));
        engine.resize(0, 10);
        assert_eq!((engine.target.width, engine.target.height), (16, 4));
        assert_eq!(engine.render()?.dimensions(), (16, 4));
        Ok(())
    }

    #[test]
    fn new_rejects_zero_size() {
        assert!(Headless::new(0, 4).is_err());
    }

    fn mark_frame(_: &mut World, res: &mut Resources) {
        res.insert(true);
    }

    #[test]
    fn render_runs_frame_systems_and_render_fn() -> anyhow::Result<()> {
        let mut engine = Headless::new(4, 4)?;
        engine.schedule.add_frame("mark_frame", mark_frame);
        engine.render_fn = Some(Box::new(|_, res, _, _, _| res.insert(7_u8)));
        engine.render()?;
        assert_eq!(engine.resources.get::<bool>(), Some(&true));
        assert_eq!(engine.resources.get::<u8>(), Some(&7));
        Ok(())
    }

    /// Fixed system: counts steps in a `u32` resource.
    fn count_steps(_: &mut World, res: &mut Resources) {
        *res.expect_mut::<u32>() += 1;
    }

    #[test]
    fn from_parts_keeps_the_parts_and_fills_in_missing_resources() -> anyhow::Result<()> {
        let mut world = World::new();
        let entity = world.spawn((Transform::from_translation(Vec3::X),));
        let mut resources = Resources::new();
        resources.insert(0_u32);
        let mut camera = default_camera();
        camera.transform.translation = Vec3::new(0.0, 9.0, 0.0);
        resources.insert(camera);
        let mut schedule = Schedule::new();
        schedule.add_fixed("count_steps", count_steps);

        let mut engine = Headless::from_parts(world, resources, schedule, (12, 6))?;
        assert_eq!((engine.target.width, engine.target.height), (12, 6));
        // The caller's world, resources, and systems are used as given...
        assert!(engine.world.contains(entity));
        assert_eq!(
            engine.resources.expect::<Camera>().transform.translation,
            Vec3::new(0.0, 9.0, 0.0)
        );
        engine.step(3);
        assert_eq!(engine.resources.get::<u32>(), Some(&3));
        assert_eq!(engine.resources.expect::<Time>().fixed_steps, 3);
        // ...and what `new` installs is filled in where missing.
        assert!(engine.resources.get::<FrameStats>().is_some());
        assert_eq!(
            engine.resources.expect::<RenderState>().light.direction,
            default_light().direction
        );
        assert!(engine.resources.get::<Assets>().is_some());
        assert_eq!(engine.render()?.dimensions(), (12, 6));
        Ok(())
    }

    #[test]
    fn from_parts_rejects_zero_size() {
        let err = Headless::from_parts(World::new(), Resources::new(), Schedule::new(), (4, 0))
            .err()
            .expect("zero height must fail");
        assert!(err.to_string().contains("4x0"), "{err}");
    }
}
