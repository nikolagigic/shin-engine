//! Window, event loop, and fixed-timestep game loop. Owned by GE-001.
//!
//! User code implements [`Game`] and hands it to [`run`]. The loop ticks the
//! simulation at a fixed `dt` ([`FixedTimestep`]) and renders once per
//! frame, clearing the surface and depth buffer before calling
//! [`Game::render`].
//!
//! [`EcsGame`] is a ready-made [`Game`] for applications that are "a world
//! plus systems" (see [`crate::ecs`]). It keeps an
//! [`InputState`] resource up to date and records frame times in a
//! [`FrameStats`] resource.

use std::sync::Arc;
use std::time::Instant;

use anyhow::Context as _;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{DeviceEvent, DeviceId, ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{CursorGrabMode, Window, WindowId};

use crate::ecs::{self, ExitRequest, Resources, Schedule, Time, World};
use crate::gpu::{GpuContext, SurfaceTarget};
use crate::input::{Actions, GamepadState, Gamepads, InputState, actions_system};
use crate::stats::{FrameStats, frame_stats_system};

/// Longest frame time fed to the simulation, in seconds. Longer frames (a
/// breakpoint, a dragged window) are clamped so the loop never tries to
/// catch up with hundreds of steps.
pub const MAX_FRAME_TIME: f64 = 0.25;

/// Window and loop settings for [`run`].
#[derive(Clone, Debug)]
pub struct AppConfig {
    /// Window title.
    pub title: String,
    /// Initial window width in logical pixels.
    pub width: u32,
    /// Initial window height in logical pixels.
    pub height: u32,
    /// Simulation step in seconds; default `1.0 / 60.0`.
    pub fixed_dt: f64,
    /// Colour the surface is cleared to at the start of every frame, in
    /// linear RGB; the sRGB surface encodes it for display.
    pub clear_color: wgpu::Color,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            title: "engine".to_string(),
            width: 1280,
            height: 720,
            fixed_dt: 1.0 / 60.0,
            clear_color: wgpu::Color::BLACK,
        }
    }
}

/// User code implements this. [`run`] owns the loop and calls these hooks.
pub trait Game {
    /// Called once with the window, right after it is created and before
    /// [`init`](Self::init). Games that drive a windowing integration of
    /// their own (the editor's egui-winit state needs the window for its
    /// scale factor, size, and clipboard) keep a clone of the `Arc`.
    fn window_created(&mut self, window: &Arc<Window>) {
        let _ = window;
    }

    /// Called once after the GPU context exists.
    fn init(&mut self, ctx: &GpuContext) -> anyhow::Result<()> {
        let _ = ctx;
        Ok(())
    }

    /// Fixed-timestep simulation step. Called 0..n times per frame.
    fn fixed_update(&mut self, dt: f64) {
        let _ = dt;
    }

    /// Once per frame, after the surface (`view`) and its matching depth
    /// buffer (`depth`, [`crate::gpu::DEPTH_FORMAT`]) have been cleared.
    /// `alpha` in 0..1 is the fraction of a fixed step left in the
    /// accumulator, for interpolating between the last two simulation
    /// states.
    fn render(
        &mut self,
        ctx: &GpuContext,
        view: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        alpha: f64,
    ) {
        let _ = (ctx, view, depth, alpha);
    }

    /// Every window event, before the engine handles it.
    fn on_window_event(&mut self, event: &winit::event::WindowEvent) {
        let _ = event;
    }

    /// Every raw device event (for example unaccelerated mouse motion,
    /// which keeps arriving while the cursor is grabbed).
    fn on_device_event(&mut self, event: &DeviceEvent) {
        let _ = event;
    }

    /// Polled once per frame; returning `Some` sets the window title.
    fn title(&self) -> Option<String> {
        None
    }

    /// Polled once per frame; `true` grabs and hides the cursor, `false`
    /// releases and shows it. The cursor is always released on exit.
    fn cursor_grabbed(&self) -> bool {
        false
    }

    /// Polled once per frame after `render`; `true` closes the window and
    /// makes [`run`] return `Ok(())`.
    fn exit_requested(&self) -> bool {
        false
    }

    /// Whether the game uses Escape itself. By default (`false`) pressing
    /// Escape closes the window; a game returning `true` (the editor, where
    /// Escape clears the selection) still sees the key in
    /// [`on_window_event`](Self::on_window_event) and quits through
    /// [`exit_requested`](Self::exit_requested) instead.
    fn handles_escape(&self) -> bool {
        false
    }
}

/// Draws the world each frame for [`EcsGame`]; called after the frame
/// systems with the colour view and the matching depth view, both already
/// cleared. Arguments: `(world, resources, ctx, color_view, depth_view)`.
///
/// To draw [`crate::render::MeshInstance`] entities, call
/// [`crate::render::render_world`] from here.
pub type RenderFn =
    Box<dyn FnMut(&mut World, &mut Resources, &GpuContext, &wgpu::TextureView, &wgpu::TextureView)>;

/// A [`Game`] that is a [`World`] plus a [`Schedule`] of systems.
///
/// The loop keeps the [`Time`] resource up to date: each
/// [`fixed_update`](Game::fixed_update) advances `Time` by one step and
/// then runs the schedule's fixed systems; each [`render`](Game::render)
/// records `frame_dt` and `alpha`, runs the frame systems, and finally
/// calls [`render_fn`](Self::render_fn) if one is set.
///
/// It also owns input: an [`InputState`] resource receives every window and
/// device event, is reset with [`InputState::end_frame`] after the frame
/// systems, and its `cursor_grabbed` flag drives the window cursor. Gamepads
/// (gilrs, started in [`Game::init`]) are polled into a [`GamepadState`]
/// resource at the start of every frame, and [`actions_system`] then
/// derives the [`Actions`] resource (empty until the game inserts its
/// project's, see [`Actions::from_config`]) before the schedule's frame
/// systems run. Each fixed step ends with [`Actions::end_step`].
///
/// Frame times go into a [`FrameStats`] resource: [`frame_stats_system`]
/// runs after every frame system in the schedule, and once per second the
/// window title becomes `"{title} | {summary}"` (see [`Self::title`]).
///
/// Any system can end the app by setting the [`ExitRequest`] resource to
/// `true`; the window closes after the current frame.
pub struct EcsGame {
    /// Entities and components.
    pub world: World,
    /// Singletons shared by systems; always contains a [`Time`].
    pub resources: Resources,
    /// Systems to run each fixed step and each frame.
    pub schedule: Schedule,
    /// Optional drawing callback, run last in every frame.
    pub render_fn: Option<RenderFn>,
    /// Base window title, shown before the [`FrameStats::summary`] once
    /// per second. Empty by default, which shows the summary alone; set it
    /// to [`AppConfig::title`] to keep the name visible.
    pub title: String,
    /// The gamepads, polled every frame; started by [`Game::init`] so
    /// building a game touches no devices.
    gamepads: Gamepads,
    last_frame: Option<Instant>,
    /// Frame time accumulated since the window title was last rebuilt.
    title_age: f64,
    /// The title [`Game::title`] reports; `None` until one second of
    /// frames has been measured, so the configured title shows first.
    stats_title: Option<String>,
}

impl Default for EcsGame {
    fn default() -> Self {
        Self::new(World::new(), Schedule::new())
    }
}

impl EcsGame {
    /// A game over `world` driven by `schedule`, with default [`Time`],
    /// [`InputState`], [`GamepadState`], [`Actions`] (none defined) and
    /// [`FrameStats`] resources, `ExitRequest(false)`, and no render
    /// callback.
    pub fn new(world: World, schedule: Schedule) -> Self {
        let mut resources = Resources::new();
        resources.insert(Time::default());
        resources.insert(InputState::default());
        resources.insert(GamepadState::default());
        resources.insert(Actions::new());
        resources.insert(FrameStats::default());
        resources.insert(ExitRequest(false));
        Self {
            world,
            resources,
            schedule,
            render_fn: None,
            title: String::new(),
            gamepads: Gamepads::default(),
            last_frame: None,
            title_age: 0.0,
            stats_title: None,
        }
    }

    /// The [`Time`] resource, re-inserted if user code removed or replaced
    /// the resource map.
    fn time_mut(&mut self) -> &mut Time {
        if self.resources.get::<Time>().is_none() {
            self.resources.insert(Time::default());
        }
        self.resources.expect_mut::<Time>()
    }

    /// The GPU-independent half of [`Game::render`]: updates `frame_dt`
    /// and `alpha` in [`Time`], polls the gamepads and derives the
    /// [`Actions`] ([`actions_system`]), runs the frame systems and then
    /// [`frame_stats_system`], ends the input frame, and rebuilds the
    /// window title if a second has passed since the last rebuild.
    fn begin_frame(&mut self, alpha: f64) {
        let now = Instant::now();
        let frame_dt = self
            .last_frame
            .map_or(0.0, |last| (now - last).as_secs_f64());
        self.last_frame = Some(now);

        let time = self.time_mut();
        time.frame_dt = frame_dt;
        time.alpha = alpha;
        if let Some(gamepads) = self.resources.get_mut::<GamepadState>() {
            self.gamepads.poll(gamepads);
        }
        // Before the schedule, so every frame system sees this frame's
        // actions.
        actions_system(&mut self.world, &mut self.resources);
        self.schedule
            .run_frame(&mut self.world, &mut self.resources);
        // Run directly rather than via the schedule so it stays last even
        // when user code adds frame systems after `new`.
        frame_stats_system(&mut self.world, &mut self.resources);
        if let Some(input) = self.resources.get_mut::<InputState>() {
            input.end_frame();
        }
        if let Some(gamepads) = self.resources.get_mut::<GamepadState>() {
            gamepads.end_frame();
        }

        self.title_age += frame_dt;
        if self.title_age >= 1.0 {
            self.title_age = 0.0;
            let summary = self
                .resources
                .get::<FrameStats>()
                .map_or_else(String::new, FrameStats::summary);
            self.stats_title = Some(if self.title.is_empty() {
                summary
            } else {
                format!("{} | {summary}", self.title)
            });
        }
    }
}

impl Game for EcsGame {
    /// Starts the gamepads ([`Gamepads::new`]).
    fn init(&mut self, _ctx: &GpuContext) -> anyhow::Result<()> {
        self.gamepads = Gamepads::new();
        Ok(())
    }

    fn fixed_update(&mut self, dt: f64) {
        ecs::fixed_step(&mut self.world, &mut self.resources, &mut self.schedule, dt);
        if let Some(actions) = self.resources.get_mut::<Actions>() {
            actions.end_step();
        }
    }

    fn render(
        &mut self,
        ctx: &GpuContext,
        view: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        alpha: f64,
    ) {
        self.begin_frame(alpha);
        if let Some(render_fn) = &mut self.render_fn {
            render_fn(&mut self.world, &mut self.resources, ctx, view, depth);
        }
    }

    fn on_window_event(&mut self, event: &WindowEvent) {
        if let Some(input) = self.resources.get_mut::<InputState>() {
            input.handle_window_event(event);
        }
    }

    fn on_device_event(&mut self, event: &DeviceEvent) {
        if let Some(input) = self.resources.get_mut::<InputState>() {
            input.handle_device_event(event);
        }
    }

    /// `"{title} | {summary}"`, rebuilt once per second of frames; `None`
    /// during the first second.
    fn title(&self) -> Option<String> {
        self.stats_title.clone()
    }

    fn cursor_grabbed(&self) -> bool {
        self.resources
            .get::<InputState>()
            .is_some_and(|input| input.cursor_grabbed)
    }

    /// `true` once a system sets the [`ExitRequest`] resource to `true`.
    fn exit_requested(&self) -> bool {
        self.resources
            .get::<ExitRequest>()
            .is_some_and(|exit| exit.0)
    }
}

/// Result of [`FixedTimestep::advance`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StepResult {
    /// How many fixed steps to run this frame.
    pub steps: u32,
    /// Leftover fraction of a step, in `[0, 1)`.
    pub alpha: f64,
}

/// Accumulator for a fixed-timestep simulation with variable-rate rendering.
#[derive(Clone, Debug)]
pub struct FixedTimestep {
    fixed_dt: f64,
    accumulator: f64,
}

impl FixedTimestep {
    /// Tolerance so a frame that is an exact multiple of `dt` is not short a
    /// step because of floating-point rounding in the accumulator.
    const EPSILON: f64 = 1e-9;

    /// Creates an accumulator stepping at `fixed_dt` seconds.
    ///
    /// # Panics
    ///
    /// If `fixed_dt` is not positive.
    pub fn new(fixed_dt: f64) -> Self {
        assert!(fixed_dt > 0.0, "fixed_dt must be positive, got {fixed_dt}");
        Self {
            fixed_dt,
            accumulator: 0.0,
        }
    }

    /// The fixed step in seconds.
    pub fn fixed_dt(&self) -> f64 {
        self.fixed_dt
    }

    /// Adds `frame_time` seconds (clamped to `0..=`[`MAX_FRAME_TIME`]) and
    /// returns how many fixed steps are due and the interpolation alpha.
    pub fn advance(&mut self, frame_time: f64) -> StepResult {
        self.accumulator += frame_time.clamp(0.0, MAX_FRAME_TIME);
        let mut steps = 0;
        while self.accumulator + Self::EPSILON >= self.fixed_dt {
            self.accumulator -= self.fixed_dt;
            steps += 1;
        }
        self.accumulator = self.accumulator.max(0.0);
        let alpha = self.accumulator / self.fixed_dt;
        StepResult { steps, alpha }
    }
}

/// Opens a window and runs `game` until the window is closed, Escape is
/// pressed (unless [`Game::handles_escape`]), or [`Game::exit_requested`]
/// returns `true`.
pub fn run<G: Game + 'static>(config: AppConfig, game: G) -> anyhow::Result<()> {
    let event_loop = EventLoop::new().context("failed to create event loop")?;
    let mut runner = Runner {
        timestep: FixedTimestep::new(config.fixed_dt),
        config,
        game,
        state: None,
        last_frame: None,
        title: None,
        error: None,
    };
    event_loop
        .run_app(&mut runner)
        .context("event loop failed")?;
    match runner.error {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// Everything that only exists while a window is open.
struct Running {
    window: Arc<Window>,
    gpu: GpuContext,
    target: SurfaceTarget<'static>,
    /// Cursor grab state last applied to the window.
    cursor_grabbed: bool,
}

impl Running {
    /// Grabs and hides the cursor, or releases and shows it. Tries
    /// `Confined` first and falls back to `Locked` (macOS only supports
    /// `Locked`); a failed grab is logged and the cursor stays visible.
    fn set_cursor_grabbed(&mut self, grabbed: bool) {
        if grabbed == self.cursor_grabbed {
            return;
        }
        self.cursor_grabbed = grabbed;
        if grabbed {
            let result = self
                .window
                .set_cursor_grab(CursorGrabMode::Confined)
                .or_else(|_| self.window.set_cursor_grab(CursorGrabMode::Locked));
            match result {
                Ok(()) => self.window.set_cursor_visible(false),
                Err(err) => log::warn!("could not grab the cursor: {err}"),
            }
        } else {
            if let Err(err) = self.window.set_cursor_grab(CursorGrabMode::None) {
                log::warn!("could not release the cursor: {err}");
            }
            self.window.set_cursor_visible(true);
        }
    }
}

struct Runner<G> {
    config: AppConfig,
    game: G,
    timestep: FixedTimestep,
    state: Option<Running>,
    last_frame: Option<Instant>,
    title: Option<String>,
    error: Option<anyhow::Error>,
}

impl<G: Game> Runner<G> {
    fn start(&mut self, event_loop: &ActiveEventLoop) -> anyhow::Result<()> {
        let attributes = Window::default_attributes()
            .with_title(self.config.title.clone())
            .with_inner_size(LogicalSize::new(self.config.width, self.config.height))
            .with_resizable(true);
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .context("failed to create window")?,
        );

        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle_from_env(
                Box::new(event_loop.owned_display_handle()),
            ));
        let surface = instance
            .create_surface(window.clone())
            .context("failed to create surface")?;
        let gpu = GpuContext::with_instance(instance, Some(&surface))?;
        let size = window.inner_size();
        let target = SurfaceTarget::new(&gpu, surface, size.width, size.height)?;

        self.game.window_created(&window);
        self.game.init(&gpu)?;
        window.request_redraw();
        self.state = Some(Running {
            window,
            gpu,
            target,
            cursor_grabbed: false,
        });
        Ok(())
    }

    fn frame(&mut self) -> anyhow::Result<()> {
        let Some(state) = &mut self.state else {
            return Ok(());
        };

        let now = Instant::now();
        let frame_time = self
            .last_frame
            .map_or(0.0, |last| (now - last).as_secs_f64());
        self.last_frame = Some(now);

        let step = self.timestep.advance(frame_time);
        for _ in 0..step.steps {
            self.game.fixed_update(self.timestep.fixed_dt());
        }

        let (surface_texture, suboptimal) = match state.target.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => (t, false),
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => (t, true),
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                state.window.request_redraw();
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                let (w, h) = (state.target.config.width, state.target.config.height);
                state.target.resize(&state.gpu.device, w, h);
                state.window.request_redraw();
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                log::warn!("surface lost; recreating it");
                let surface = state
                    .gpu
                    .instance
                    .create_surface(state.window.clone())
                    .context("failed to recreate surface")?;
                let size = state.window.inner_size();
                state.target = SurfaceTarget::new(&state.gpu, surface, size.width, size.height)?;
                state.window.request_redraw();
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                anyhow::bail!("validation error while acquiring the surface texture")
            }
        };
        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        clear(
            &state.gpu,
            &view,
            &state.target.depth_view,
            self.config.clear_color,
        );
        self.game
            .render(&state.gpu, &view, &state.target.depth_view, step.alpha);

        state.window.pre_present_notify();
        state.gpu.queue.present(surface_texture);
        if suboptimal {
            let (w, h) = (state.target.config.width, state.target.config.height);
            state.target.resize(&state.gpu.device, w, h);
        }

        if let Some(title) = self.game.title()
            && self.title.as_ref() != Some(&title)
        {
            state.window.set_title(&title);
            self.title = Some(title);
        }
        state.set_cursor_grabbed(self.game.cursor_grabbed());
        state.window.request_redraw();
        Ok(())
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, err: anyhow::Error) {
        log::error!("{err:#}");
        self.error = Some(err);
        event_loop.exit();
    }
}

impl<G: Game> ApplicationHandler for Runner<G> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_none()
            && let Err(err) = self.start(event_loop)
        {
            self.fail(event_loop, err);
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        self.game.on_window_event(&event);
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if !self.game.handles_escape() => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(state) = &mut self.state {
                    state
                        .target
                        .resize(&state.gpu.device, size.width, size.height);
                    state.window.request_redraw();
                }
            }
            // A redraw already queued when `exit` was called still arrives;
            // skip it so the game sees no frames after it asked to stop.
            WindowEvent::RedrawRequested if event_loop.exiting() => {}
            WindowEvent::RedrawRequested => {
                if let Err(err) = self.frame() {
                    self.fail(event_loop, err);
                } else if self.game.exit_requested() {
                    log::info!("exit requested");
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: DeviceId,
        event: DeviceEvent,
    ) {
        self.game.on_device_event(&event);
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(state) = &mut self.state {
            state.set_cursor_grabbed(false);
        }
        self.state = None;
    }
}

/// Clears `color` to `clear_color` and `depth` to 1.0 (the far plane).
fn clear(
    gpu: &GpuContext,
    color: &wgpu::TextureView,
    depth: &wgpu::TextureView,
    clear_color: wgpu::Color,
) {
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("clear encoder"),
        });
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("clear pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: color,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(clear_color),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: depth,
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
    gpu.queue.submit([encoder.finish()]);
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f64 = 1.0 / 60.0;

    #[test]
    fn two_steps_for_double_dt() {
        let mut ts = FixedTimestep::new(DT);
        let r = ts.advance(1.0 / 30.0);
        assert_eq!(r.steps, 2);
        assert!(r.alpha.abs() < 1e-6, "alpha = {}", r.alpha);
    }

    #[test]
    fn long_frames_are_clamped() {
        let mut ts = FixedTimestep::new(DT);
        let r = ts.advance(0.4);
        assert_eq!(r.steps, 15);
        assert!((0.0..1.0).contains(&r.alpha), "alpha = {}", r.alpha);
    }

    #[test]
    fn partial_steps_accumulate() {
        let mut ts = FixedTimestep::new(DT);
        let r = ts.advance(DT / 2.0);
        assert_eq!(r.steps, 0);
        assert!((r.alpha - 0.5).abs() < 1e-9);
        let r = ts.advance(DT / 2.0);
        assert_eq!(r.steps, 1);
    }

    fn count_frames(_: &mut World, res: &mut Resources) {
        *res.expect_mut::<u32>() += 1;
    }

    #[test]
    fn ecs_game_frame_updates_time_and_runs_frame_systems() {
        let mut schedule = Schedule::new();
        schedule.add_frame("count_frames", count_frames);
        let mut game = EcsGame::new(World::new(), schedule);
        game.resources.insert(0_u32);

        game.begin_frame(0.25);
        let time = *game.resources.expect::<Time>();
        assert_eq!(time.alpha, 0.25);
        assert_eq!(time.frame_dt, 0.0, "first frame has no predecessor");

        std::thread::sleep(std::time::Duration::from_millis(5));
        game.begin_frame(0.5);
        let time = *game.resources.expect::<Time>();
        assert_eq!(time.alpha, 0.5);
        assert!(time.frame_dt >= 0.005, "frame_dt = {}", time.frame_dt);
        assert_eq!(time.fixed_steps, 0, "frames do not step the simulation");
        assert_eq!(*game.resources.expect::<u32>(), 2);
    }

    /// Records the mouse delta the frame systems saw.
    fn record_mouse(_: &mut World, res: &mut Resources) {
        let delta = res.expect::<InputState>().mouse_delta;
        res.insert(Some(delta));
    }

    #[test]
    fn ecs_game_forwards_input_and_ends_frame_after_frame_systems() {
        use crate::math::Vec2;

        let mut schedule = Schedule::new();
        schedule.add_frame("record_mouse", record_mouse);
        let mut game = EcsGame::new(World::new(), schedule);
        game.on_device_event(&DeviceEvent::MouseMotion { delta: (4.0, -2.0) });
        assert!(!game.cursor_grabbed());

        game.begin_frame(0.0);
        assert_eq!(
            game.resources.expect::<Option<Vec2>>(),
            &Some(Vec2::new(4.0, -2.0)),
            "frame systems see this frame's input"
        );
        assert_eq!(
            game.resources.expect::<InputState>().mouse_delta,
            Vec2::ZERO,
            "end_frame ran after the frame systems"
        );

        game.resources.expect_mut::<InputState>().cursor_grabbed = true;
        assert!(game.cursor_grabbed());
    }

    #[test]
    fn ecs_game_records_frame_stats_after_frame_systems() {
        let mut game = EcsGame {
            title: "demo".to_string(),
            ..Default::default()
        };
        assert!(game.resources.get::<FrameStats>().is_some());

        game.begin_frame(0.0);
        assert!(
            game.resources.expect::<FrameStats>().is_empty(),
            "the first frame has no frame_dt to record"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
        game.begin_frame(0.0);
        let stats = game.resources.expect::<FrameStats>();
        assert_eq!(stats.len(), 1);
        assert!(stats.mean_ms() >= 5.0, "mean {}", stats.mean_ms());
        assert_eq!(game.title(), None, "no title before a second has passed");

        // A second's worth of frame time rebuilds the title.
        game.title_age = 1.0;
        game.begin_frame(0.0);
        let title = game.title().expect("title after one second");
        let summary = game.resources.expect::<FrameStats>().summary();
        assert_eq!(title, format!("demo | {summary}"));
    }

    #[test]
    fn ecs_game_reports_exit_request() {
        let mut game = EcsGame::default();
        assert!(!game.exit_requested(), "new games do not request exit");
        game.resources.insert(ExitRequest(true));
        assert!(game.exit_requested());
    }

    #[test]
    fn negative_frame_time_is_ignored() {
        let mut ts = FixedTimestep::new(DT);
        assert_eq!(ts.advance(-1.0).steps, 0);
    }

    #[test]
    fn alpha_always_in_unit_range() {
        let mut ts = FixedTimestep::new(DT);
        let frame_times = [0.0, 0.001, 0.007, 1.0 / 144.0, DT, 0.0213, 0.05, 0.3, 1.0];
        for i in 0..1000 {
            let r = ts.advance(frame_times[i % frame_times.len()]);
            assert!(
                (0.0..1.0).contains(&r.alpha),
                "alpha {} out of range",
                r.alpha
            );
        }
    }
}
