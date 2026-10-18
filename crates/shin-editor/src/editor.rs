//! The editor frame: run the egui UI, render the scene into the viewport's
//! [`OffscreenTarget`], and paint the UI over the output view.
//!
//! [`EditorCore`] owns a [`Headless`] engine (on a clone of the window's
//! GPU context) with the project open, and a [`ToolRegistry`]; every edit
//! (console line, scene panel, inspector, undo, redo) goes through the
//! engine's command history ([`shin::command`], via [`crate::edit`]).
//! Play and Stop (GE-024) use the engine's play mode
//! ([`shin::headless::PlayState`]); while playing, a [`PlayClock`] steps the
//! simulation every frame and edits are refused.
//! The scene's own [`Camera`] resource is left as authored (GE-030): the
//! editor's [`OrbitCamera`] is swapped in only around the viewport render
//! while editing, and Play renders from the scene camera as the game would.
//! It does one frame given egui input and an output view, so the
//! same code serves the window ([`EditorApp`], a [`Game`] for
//! [`shin::app::run`]) and `--screenshot` ([`screenshot`], which paints into
//! an offscreen target instead of the surface).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use egui_dock::DockState;
use serde_json::{Value, json};
use shin::app::{AppConfig, Game};
use shin::asset::AssetSource;
use shin::command::{self, History};
use shin::ecs::{Entity, Name};
use shin::gpu::{GpuContext, OFFSCREEN_COLOR_FORMAT, OFFSCREEN_GAMMA_VIEW_FORMAT, OffscreenTarget};
use shin::headless::Headless;
use shin::math::{Camera, Transform};
use shin::project::{Project, ProjectRoot};
use shin::render::{MeshInstance, begin_mesh_pass};
use shin::tool::{ToolOutput, ToolRegistry};
use winit::window::Window;

use crate::assets::{self, AssetBrowser};
use crate::edit;
use crate::inspector::{Edit, Inspector};
use crate::log_sink::LogBuffer;
use crate::orbit::OrbitCamera;
use crate::play::{self, PlayClock};
use crate::scene;
use crate::ui::{self, Action, Console, EditorUi, Fields, Selection, Tab, Viewport};

/// Editor window size in logical pixels, and the `--screenshot` size in
/// pixels (screenshots render at one pixel per point).
pub const WINDOW_SIZE: [u32; 2] = [1600, 900];

/// The egui texture showing the engine's offscreen target, and the target
/// size it was registered for.
struct ViewportTexture {
    size: [u32; 2],
    texture: egui::TextureId,
}

/// Editor state that does not depend on having a window.
pub struct EditorCore {
    egui: egui::Context,
    /// Created on the first frame, when the output format is known.
    renderer: Option<egui_wgpu::Renderer>,
    dock: DockState<Tab>,
    /// The scene: world, resources, command history, offscreen target.
    engine: Headless,
    /// The tools the console and menus call.
    registry: ToolRegistry,
    orbit: OrbitCamera,
    log: LogBuffer,
    viewport: Viewport,
    viewport_texture: Option<ViewportTexture>,
    console: Console,
    quit: bool,
    /// The open project's manifest.
    project: Project,
    /// The open project's directory (canonical), which the Assets panel
    /// scans and Import copies into.
    project_dir: PathBuf,
    selection: Selection,
    inspector: Inspector,
    /// The Assets panel (GE-032).
    assets: AssetBrowser,
    /// Whether the Assets tab was docked last frame; docking it again
    /// (View menu) rescans at once.
    assets_docked: bool,
    fields: Fields,
    /// The scene file Save writes, relative to the project.
    scene_path: String,
    /// The undo stack (`(tool, args)`) when the scene was last opened or
    /// saved; the scene is unsaved while the stack differs from it.
    saved: Vec<(String, Value)>,
    /// Steps the simulation from frame times while playing (GE-024).
    clock: PlayClock,
}

/// How much the selected entity's colour is mixed toward white in the
/// viewport (render only; the component keeps its colour).
const HIGHLIGHT: f32 = 0.5;

impl EditorCore {
    /// An editor on the project in `project_dir`, showing its entry scene,
    /// drawing its UI with `egui` and its Log panel from `log`.
    pub fn new(
        gpu: &GpuContext,
        egui: egui::Context,
        project_dir: &Path,
        log: LogBuffer,
    ) -> anyhow::Result<Self> {
        // The target is resized to the Viewport panel once it is laid out.
        let mut engine = Headless::from_gpu(gpu.clone(), 1, 1)?;
        let project = scene::open(&mut engine, project_dir)?;
        let project_dir = engine.resources.expect::<ProjectRoot>().0.clone();
        let clock = PlayClock::new(engine.fixed_dt);
        Ok(Self {
            clock,
            egui,
            renderer: None,
            dock: ui::default_layout(),
            engine,
            registry: ToolRegistry::with_engine_tools(),
            orbit: OrbitCamera::default(),
            log,
            viewport: Viewport::default(),
            viewport_texture: None,
            console: Console::default(),
            quit: false,
            scene_path: scene::default_path(&project),
            project,
            project_dir,
            selection: Selection::default(),
            inspector: Inspector::default(),
            assets: AssetBrowser::default(),
            assets_docked: false,
            fields: Fields::default(),
            saved: Vec::new(),
        })
    }

    /// Adds [`play::orbit_demo`] to the engine's fixed systems, so Play
    /// moves the entity named `moon` (`--demo-orbit`).
    pub fn add_orbit_demo(&mut self) {
        self.engine
            .schedule
            .add_fixed("orbit_demo", play::orbit_demo);
    }

    /// Runs one editor frame: the UI pass on `input`, the simulation steps
    /// due while playing ([`PlayClock::tick`]), then the scene into
    /// the viewport target (resized to the panel first), then the UI painted
    /// over `output` (loaded, not cleared), whose size is `size_px` and
    /// colour format `format`. Returns what egui wants the platform to do
    /// (cursor icon, clipboard, ...).
    pub fn frame(
        &mut self,
        gpu: &GpuContext,
        input: egui::RawInput,
        output: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        size_px: [u32; 2],
    ) -> egui::PlatformOutput {
        let egui_ctx = self.egui.clone();
        let mut actions = Vec::new();
        self.drop_dead_selection();
        // Opening the Assets panel (View menu) rescans the project at once;
        // while it is shown it checks for changes on its own.
        let assets_docked = self.dock.find_tab(&Tab::Assets).is_some();
        if assets_docked && !self.assets_docked {
            self.assets.invalidate();
        }
        self.assets_docked = assets_docked;
        let mut full = egui_ctx.run_ui(input, |ui| {
            EditorUi {
                engine: &self.engine,
                log: &self.log,
                orbit: &mut self.orbit,
                viewport: &mut self.viewport,
                console: &mut self.console,
                selection: &mut self.selection,
                inspector: &mut self.inspector,
                assets: &mut self.assets,
                project_dir: &self.project_dir,
                fields: &mut self.fields,
                scene_path: &self.scene_path,
                actions: &mut actions,
                quit: &mut self.quit,
            }
            .show(ui, &mut self.dock);
        });
        for action in actions {
            self.run(action);
        }
        self.clock.tick(&mut self.engine);

        let renderer = self.renderer.get_or_insert_with(|| {
            egui_wgpu::Renderer::new(&gpu.device, format, egui_wgpu::RendererOptions::default())
        });

        sync_viewport_texture(
            &mut self.viewport_texture,
            &mut self.viewport,
            &mut self.engine,
            renderer,
        );
        if self.viewport_texture.is_some() {
            // Editing: the viewport shows the orbit camera. Playing: the
            // scene's own camera, as the game would (GE-030).
            let view = (!self.engine.play_state().is_playing()).then(|| self.orbit.transform());
            render_scene(&mut self.engine, self.selection.0, view);
        }

        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: size_px,
            pixels_per_point: full.pixels_per_point,
        };
        let jobs = egui_ctx.tessellate(full.shapes, full.pixels_per_point);
        // Drained, not iterated: a `TexturesDelta` dropped with entries left
        // in it asserts in debug builds.
        let mut textures = std::mem::take(&mut full.textures_delta);
        for (id, deltas) in textures.set.drain() {
            for delta in &deltas {
                renderer.update_texture(&gpu.device, &gpu.queue, id, delta);
            }
        }
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("egui encoder"),
            });
        let mut commands =
            renderer.update_buffers(&gpu.device, &gpu.queue, &mut encoder, &jobs, &screen);
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: output,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            renderer.render(&mut pass.forget_lifetime(), &jobs, &screen);
        }
        commands.push(encoder.finish());
        gpu.queue.submit(commands);
        for id in textures.free.drain() {
            renderer.free_texture(&id);
        }
        full.platform_output
    }

    /// Clears the selection if its entity no longer exists (deleted, or
    /// its spawn undone).
    fn drop_dead_selection(&mut self) {
        if self
            .selection
            .0
            .is_some_and(|e| !self.engine.world.contains(e))
        {
            self.selection.0 = None;
        }
    }

    /// Carries out what the UI asked for. Results and errors go to the log
    /// and, for console lines, to the console transcript.
    fn run(&mut self, action: Action) {
        if let Err(err) = self.try_run(action) {
            log::error!("{err:#}");
        }
    }

    /// [`Self::run`], returning the error instead of logging it.
    fn try_run(&mut self, action: Action) -> anyhow::Result<()> {
        anyhow::ensure!(
            !(action.is_edit() && self.engine.play_state().is_playing()),
            "editing is disabled while playing; press Stop first"
        );
        let reg = &self.registry;
        let engine = &mut self.engine;
        match action {
            Action::Play => {
                engine.play()?;
                self.clock.reset();
            }
            Action::Pause => {
                engine.pause()?;
            }
            Action::Stop => {
                engine.stop()?;
                self.drop_dead_selection();
            }
            Action::Undo => match command::undo(engine)? {
                true => log::info!("undo"),
                false => log::info!("nothing to undo"),
            },
            Action::Redo => match command::redo(engine)? {
                true => log::info!("redo"),
                false => log::info!("nothing to redo"),
            },
            Action::Console(line) => {
                let reply = self.run_console_line(&line);
                match &reply {
                    Ok(text) => log::info!("{line} -> {text}"),
                    Err(text) => log::error!("{line} -> {text}"),
                }
                self.console.record(line, reply);
            }
            Action::Spawn(source) => {
                let name = spawn_name(&source);
                let entity = edit::spawn(reg, engine, &source, self.orbit.target, name.as_deref())?;
                self.selection.0 = Some(entity);
            }
            Action::Despawn(entity) => {
                edit::despawn(reg, engine, entity)?;
                self.drop_dead_selection();
            }
            Action::PreviewTransform(entity, desc) => {
                edit::preview_transform(engine, entity, &desc);
            }
            Action::PreviewColor(entity, color) => edit::preview_color(engine, entity, color),
            Action::Edit(Edit::Name(entity, name)) => {
                let current = engine
                    .world
                    .get::<&Name>(entity)
                    .map(|n| n.0.clone())
                    .unwrap_or_default();
                if name != current {
                    edit::commit_name(reg, engine, entity, &name)?;
                }
            }
            Action::Edit(Edit::Transform {
                entity,
                desc,
                original,
            }) => {
                edit::preview_transform(engine, entity, &original);
                edit::commit_transform(reg, engine, entity, &desc)?;
            }
            Action::Edit(Edit::Color {
                entity,
                color,
                original,
            }) => {
                edit::preview_color(engine, entity, original);
                edit::commit_color(reg, engine, entity, color)?;
            }
            Action::Edit(Edit::Mesh(entity, source)) => {
                edit::commit_mesh(reg, engine, entity, &source)?;
            }
            Action::Edit(Edit::Texture(entity, source)) => {
                edit::commit_texture(reg, engine, entity, source.as_deref())?;
            }
            Action::Edit(Edit::Script(entity, path)) => {
                // A compile error is logged by the engine and shown in the
                // inspector; the script stays attached.
                edit::commit_script(reg, engine, entity, &path)?;
            }
            Action::Open => self.open_scene(scene::default_path(&self.project))?,
            Action::OpenScene(path) => self.open_scene(path)?,
            Action::Save => self.save_as(self.scene_path.clone())?,
            Action::SaveAs(path) => self.save_as(path)?,
            Action::Import(path) => {
                let imported = assets::import(&self.project_dir, Path::new(&path))
                    .with_context(|| format!("importing {path}"))?;
                log::info!("imported {path} as {}", imported.display());
                self.assets.invalidate();
            }
            Action::ViewFromSceneCamera => {
                let camera = engine
                    .resources
                    .get::<Camera>()
                    .context("no Camera resource")?;
                self.orbit.look_from(&camera.transform);
            }
            Action::SetSceneCameraFromView => {
                let view = self.orbit.transform();
                edit::commit_camera(reg, engine, view.translation, self.orbit.target)?;
            }
        }
        Ok(())
    }

    /// File > Open and the Assets panel's Open: loads the scene file at
    /// `path` (relative to the project; not an undoable edit: the history
    /// starts over) and makes it the scene Save writes.
    fn open_scene(&mut self, path: String) -> anyhow::Result<()> {
        self.registry
            .call(&mut self.engine, "scene.load", json!({ "path": path }))
            .with_context(|| format!("opening {path}"))?;
        self.engine.resources.insert(History::default());
        self.selection.0 = None;
        self.saved.clear();
        log::info!("opened {path}");
        self.scene_path = path;
        Ok(())
    }

    /// File > Save and Save As: writes the scene to `path` (relative to the
    /// project) with `scene.save` and makes it the scene Save writes.
    fn save_as(&mut self, path: String) -> anyhow::Result<()> {
        self.registry
            .call(&mut self.engine, "scene.save", json!({ "path": path }))
            .with_context(|| format!("saving {path}"))?;
        self.saved = self.undo_stack();
        log::info!("saved {path}");
        self.scene_path = path;
        Ok(())
    }

    /// The command history's undo stack as `(tool, args)`.
    fn undo_stack(&self) -> Vec<(String, Value)> {
        self.engine
            .resources
            .get::<History>()
            .map(|h| {
                h.undo_entries()
                    .map(|(tool, args)| (tool.to_string(), args.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether the scene has edits that were not saved: the undo stack
    /// differs from what it was at the last open or save.
    pub fn is_dirty(&self) -> bool {
        let Some(history) = self.engine.resources.get::<History>() else {
            return !self.saved.is_empty();
        };
        history.undo_len() != self.saved.len()
            || history.undo_entries().zip(&self.saved).any(
                |((tool, args), (saved_tool, saved_args))| tool != saved_tool || args != saved_args,
            )
    }

    /// The window title: the project's, with `*` while there are unsaved
    /// edits.
    pub fn title(&self) -> String {
        let dirty = if self.is_dirty() { "*" } else { "" };
        format!("{}{dirty} — Shin Editor", self.project.window.title)
    }

    /// Runs `<tool.name> [json args]` through the command history and
    /// describes the result. While playing the history is frozen: the tool
    /// runs without being recorded (Stop discards what it changed), and
    /// `history.*` tools are refused.
    fn run_console_line(&mut self, line: &str) -> Result<String, String> {
        let (tool, args) = ui::parse_console_line(line)?;
        let output = if self.engine.play_state().is_playing() {
            if tool.starts_with("history.") {
                return Err("the history is frozen while playing; press Stop first".to_string());
            }
            self.registry.call(&mut self.engine, tool, args)
        } else {
            command::apply(&self.registry, &mut self.engine, tool, args)
        }
        .map_err(|err| format!("{err:#}"))?;
        Ok(match output {
            ToolOutput::Text(text) => text,
            ToolOutput::Json(value) => value.to_string(),
            ToolOutput::Image { width, height, .. } => format!("<{width}x{height} PNG>"),
        })
    }

    /// Whether File > Quit was clicked.
    pub fn quit_requested(&self) -> bool {
        self.quit
    }
}

/// Makes the engine's offscreen target match the size the Viewport panel
/// asked for and points the panel's egui texture id at its colour texture,
/// re-registering it whenever the target was recreated. Does nothing while
/// the panel has no area (before its first layout, or collapsed).
fn sync_viewport_texture(
    slot: &mut Option<ViewportTexture>,
    viewport: &mut Viewport,
    engine: &mut Headless,
    renderer: &mut egui_wgpu::Renderer,
) {
    let [width, height] = viewport.size_px;
    if width == 0 || height == 0 {
        return;
    }
    // Opening a project resizes the target behind the slot's back, so
    // compare against the target too.
    let target_matches = (engine.target.width, engine.target.height) == (width, height);
    if target_matches && slot.as_ref().is_some_and(|v| v.size == [width, height]) {
        return;
    }

    engine.resize(width, height);
    let gpu = &engine.gpu;
    // egui samples textures as gamma-encoded bytes: view the sRGB colour
    // texture as plain Rgba8Unorm so the stored bytes reach egui unchanged.
    let view = engine
        .target
        .color
        .create_view(&wgpu::TextureViewDescriptor {
            format: Some(OFFSCREEN_GAMMA_VIEW_FORMAT),
            ..Default::default()
        });
    let filter = wgpu::FilterMode::Linear;
    let texture = match slot.take() {
        Some(old) => {
            renderer.update_egui_texture_from_wgpu_texture(&gpu.device, &view, filter, old.texture);
            old.texture
        }
        None => renderer.register_native_texture(&gpu.device, &view, filter),
    };
    log::debug!("viewport target resized to {width}x{height}");
    viewport.texture = Some(texture);
    *slot = Some(ViewportTexture {
        size: [width, height],
        texture,
    });
}

/// Renders `engine`'s scene into its target with `selected` tinted toward
/// white and, with `view`, from that placement instead of the scene
/// camera's. The tint and the camera are put back right after the render,
/// so the component, the `Camera` resource, the history, and saved scenes
/// never see them.
fn render_scene(engine: &mut Headless, selected: Option<Entity>, view: Option<Transform>) {
    let tinted = selected.and_then(|entity| {
        let instance = engine
            .world
            .query_one_mut::<&mut MeshInstance>(entity)
            .ok()?;
        let original = instance.color;
        instance.color = highlight(original);
        Some((entity, original))
    });
    let authored = view.map(|view| {
        let camera = engine.resources.expect_mut::<Camera>();
        std::mem::replace(&mut camera.transform, view)
    });
    engine.render_to_target();
    if let Some(transform) = authored {
        engine.resources.expect_mut::<Camera>().transform = transform;
    }
    if let Some((entity, original)) = tinted {
        edit::preview_color(engine, entity, original);
    }
}

/// The window title for `project` before anything is edited.
fn title(project: &Project) -> String {
    format!("{} — Shin Editor", project.window.title)
}

/// `color` mixed [`HIGHLIGHT`] of the way toward white (alpha kept).
fn highlight([r, g, b, a]: [f32; 4]) -> [f32; 4] {
    let mix = |c: f32| c + (1.0 - c) * HIGHLIGHT;
    [mix(r), mix(g), mix(b), a]
}

/// The name an entity added from `source` gets: the built-in's name or the
/// file stem (`builtin://cube` gives `cube`, `assets/crate.gltf#1` gives
/// `crate`).
fn spawn_name(source: &str) -> Option<String> {
    match AssetSource::parse(source).ok()? {
        AssetSource::Builtin(name) => Some(name),
        AssetSource::File { path, .. }
        | AssetSource::Image { path }
        | AssetSource::GltfImage { path, .. } => {
            Some(path.file_stem()?.to_string_lossy().into_owned())
        }
    }
}

/// The windowed editor: an [`EditorCore`] fed by egui-winit.
pub struct EditorApp {
    project_dir: std::path::PathBuf,
    log: LogBuffer,
    egui: egui::Context,
    window: Option<Arc<Window>>,
    egui_winit: Option<egui_winit::State>,
    core: Option<EditorCore>,
    /// Whether to register [`play::orbit_demo`] (`--demo-orbit`).
    orbit_demo: bool,
}

impl EditorApp {
    /// An editor for the project in `project_dir`; the window, GPU, and
    /// scene are created when [`shin::app::run`] starts it. With
    /// `orbit_demo`, Play runs [`play::orbit_demo`].
    pub fn new(project_dir: &Path, log: LogBuffer, orbit_demo: bool) -> Self {
        Self {
            project_dir: project_dir.to_path_buf(),
            orbit_demo,
            log,
            egui: egui::Context::default(),
            window: None,
            egui_winit: None,
            core: None,
        }
    }
}

impl Game for EditorApp {
    fn window_created(&mut self, window: &Arc<Window>) {
        self.egui_winit = Some(egui_winit::State::new(
            self.egui.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            window.theme(),
            None,
        ));
        self.window = Some(window.clone());
    }

    fn init(&mut self, ctx: &GpuContext) -> anyhow::Result<()> {
        if let Some(state) = &mut self.egui_winit {
            state.set_max_texture_side(ctx.device.limits().max_texture_dimension_2d as usize);
        }
        let mut core =
            EditorCore::new(ctx, self.egui.clone(), &self.project_dir, self.log.clone())?;
        if self.orbit_demo {
            core.add_orbit_demo();
        }
        self.core = Some(core);
        Ok(())
    }

    fn render(
        &mut self,
        ctx: &GpuContext,
        view: &wgpu::TextureView,
        _depth: &wgpu::TextureView,
        _alpha: f64,
    ) {
        let (Some(window), Some(state), Some(core)) =
            (&self.window, &mut self.egui_winit, &mut self.core)
        else {
            return;
        };
        let input = state.take_egui_input(window);
        let texture = view.texture();
        let size = [texture.width(), texture.height()];
        let platform = core.frame(ctx, input, view, texture.format(), size);
        state.handle_platform_output(window, platform);
    }

    fn on_window_event(&mut self, event: &winit::event::WindowEvent) {
        if let (Some(window), Some(state)) = (&self.window, &mut self.egui_winit) {
            // The editor repaints every frame, so the repaint hint is unused;
            // whether egui consumed the event is decided per widget in the UI.
            let _ = state.on_window_event(window, event);
        }
    }

    fn exit_requested(&self) -> bool {
        self.core.as_ref().is_some_and(EditorCore::quit_requested)
    }

    /// Escape clears the selection instead of closing the editor.
    fn handles_escape(&self) -> bool {
        true
    }

    fn title(&self) -> Option<String> {
        self.core.as_ref().map(EditorCore::title)
    }
}

/// Opens the editor window on the project in `project_dir` and runs until
/// it is closed. With `orbit_demo`, Play runs [`play::orbit_demo`].
pub fn run(project_dir: &Path, log: LogBuffer, orbit_demo: bool) -> anyhow::Result<()> {
    let project = Project::load(project_dir)?;
    let config = AppConfig {
        title: title(&project),
        width: WINDOW_SIZE[0],
        height: WINDOW_SIZE[1],
        ..Default::default()
    };
    shin::app::run(config, EditorApp::new(project_dir, log, orbit_demo))
}

/// Renders one full editor frame of the project in `project_dir` without a
/// window, [`WINDOW_SIZE`] pixels at one pixel per point, and saves it as a
/// PNG at `out`.
///
/// The frame is run a few times first so egui and the dock settle their
/// layout and the viewport target exists at its final size.
pub fn screenshot(project_dir: &Path, out: &Path, log: LogBuffer) -> anyhow::Result<()> {
    /// Frames run before the one that is saved.
    const WARM_UP_FRAMES: u32 = 3;

    let project = Project::load(project_dir)?;
    log::info!("rendering a screenshot of `{}`", title(&project));
    let gpu = GpuContext::new(None)?;
    let mut core = EditorCore::new(&gpu, egui::Context::default(), project_dir, log)?;
    let [width, height] = WINDOW_SIZE;
    let target = OffscreenTarget::new(&gpu.device, width, height);
    let screen_rect =
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width as f32, height as f32));
    for frame in 0..=WARM_UP_FRAMES {
        let input = egui::RawInput {
            screen_rect: Some(screen_rect),
            time: Some(f64::from(frame) / 60.0),
            max_texture_side: Some(gpu.device.limits().max_texture_dimension_2d as usize),
            ..Default::default()
        };
        // Like the window loop, start every frame from black.
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("screenshot clear encoder"),
            });
        drop(begin_mesh_pass(
            &mut encoder,
            &target.color_view,
            &target.depth_view,
            wgpu::Color::BLACK,
        ));
        gpu.queue.submit([encoder.finish()]);
        core.frame(
            &gpu,
            input,
            &target.color_view,
            OFFSCREEN_COLOR_FORMAT,
            WINDOW_SIZE,
        );
    }
    target
        .read_pixels(&gpu)?
        .save(out)
        .with_context(|| format!("writing {}", out.display()))?;
    log::info!("screenshot written to {}", out.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shin::math::{Projection, Vec2, Vec3};
    use shin::project::{self, NewProjectOptions};
    use shin::scene::TransformDesc;

    /// One frame at [`WINDOW_SIZE`] with `events` as the input.
    fn frame(
        core: &mut EditorCore,
        gpu: &GpuContext,
        target: &OffscreenTarget,
        events: Vec<egui::Event>,
    ) {
        let [width, height] = WINDOW_SIZE;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width as f32, height as f32),
            )),
            events,
            ..Default::default()
        };
        core.frame(
            gpu,
            input,
            &target.color_view,
            OFFSCREEN_COLOR_FORMAT,
            WINDOW_SIZE,
        );
    }

    /// Ctrl+Z (Cmd+Z on macOS), with `shift` for redo.
    fn undo_key(shift: bool) -> egui::Event {
        let mut modifiers = egui::Modifiers::COMMAND;
        modifiers.shift = shift;
        egui::Event::Key {
            key: egui::Key::Z,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    /// The ticket's manual check, automated: `entity.spawn` from the console
    /// adds an entity, Ctrl+Z removes it, Ctrl+Shift+Z brings it back.
    #[test]
    fn ctrl_z_undoes_a_console_spawn() -> anyhow::Result<()> {
        let root =
            std::env::temp_dir().join(format!("shin-editor-undo-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let project_dir = root.join("demo");
        project::create(&NewProjectOptions {
            name: "demo".to_string(),
            dir: project_dir.clone(),
            engine: Default::default(),
        })?;

        let gpu = GpuContext::new(None)?;
        let mut core = EditorCore::new(
            &gpu,
            egui::Context::default(),
            &project_dir,
            LogBuffer::default(),
        )?;
        let [width, height] = WINDOW_SIZE;
        let target = OffscreenTarget::new(&gpu.device, width, height);
        frame(&mut core, &gpu, &target, Vec::new());
        let starter = core.engine.world.len();

        core.run(Action::Console(
            r#"entity.spawn {"mesh": 0, "translation": [0, 2, 0]}"#.to_string(),
        ));
        assert!(
            matches!(core.console.transcript.last(), Some((_, Ok(_)))),
            "{:?}",
            core.console.transcript
        );
        assert_eq!(core.engine.world.len(), starter + 1);

        frame(&mut core, &gpu, &target, vec![undo_key(false)]);
        assert_eq!(core.engine.world.len(), starter, "Ctrl+Z removes it");

        frame(&mut core, &gpu, &target, vec![undo_key(true)]);
        assert_eq!(
            core.engine.world.len(),
            starter + 1,
            "Ctrl+Shift+Z restores it"
        );

        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    /// A fresh demo project in a temp dir named after `label`; returns the
    /// temp root (to delete) and the project directory.
    fn demo_project(label: &str) -> anyhow::Result<(std::path::PathBuf, std::path::PathBuf)> {
        let root =
            std::env::temp_dir().join(format!("shin-editor-{label}-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let project_dir = root.join("demo");
        project::create(&NewProjectOptions {
            name: "demo".to_string(),
            dir: project_dir.clone(),
            engine: Default::default(),
        })?;
        Ok((root, project_dir))
    }

    /// A key press with no modifiers.
    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// `entity.list`, without the ids (which a reload changes).
    fn entities(core: &mut EditorCore) -> anyhow::Result<Vec<serde_json::Value>> {
        let ToolOutput::Json(serde_json::Value::Array(mut rows)) =
            core.registry
                .call(&mut core.engine, "entity.list", json!({}))?
        else {
            anyhow::bail!("entity.list returned no array");
        };
        for row in &mut rows {
            row.as_object_mut().unwrap().remove("entity");
        }
        Ok(rows)
    }

    /// The entity named `name`.
    fn named(core: &EditorCore, name: &str) -> Entity {
        core.engine
            .world
            .query::<(Entity, &Name)>()
            .iter()
            .find(|(_, n)| n.0 == name)
            .map(|(e, _)| e)
            .unwrap_or_else(|| panic!("no entity named {name}"))
    }

    /// The ticket's screenshot test: after `entity.spawn` through the
    /// console, the Scene panel region of the frame changes (a new row).
    #[test]
    fn scene_panel_shows_a_console_spawn() -> anyhow::Result<()> {
        let (root, project_dir) = demo_project("scene-panel")?;
        let gpu = GpuContext::new(None)?;
        let mut core = EditorCore::new(
            &gpu,
            egui::Context::default(),
            &project_dir,
            LogBuffer::default(),
        )?;
        let [width, height] = WINDOW_SIZE;
        let target = OffscreenTarget::new(&gpu.device, width, height);
        // The Scene panel is the left 18% below the menu bar.
        let scene_panel = |core: &mut EditorCore| -> anyhow::Result<image::RgbaImage> {
            for _ in 0..2 {
                frame(core, &gpu, &target, Vec::new());
            }
            let image = target.read_pixels(&gpu)?;
            Ok(image::imageops::crop_imm(&image, 0, 30, 270, 300).to_image())
        };
        let before = scene_panel(&mut core)?;

        core.run(Action::Console(
            r#"entity.spawn {"mesh": 0, "translation": [0, 2, 0]}"#.to_string(),
        ));
        assert!(matches!(core.console.transcript.last(), Some((_, Ok(_)))));
        let after = scene_panel(&mut core)?;
        let changed = before
            .pixels()
            .zip(after.pixels())
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 20, "the scene panel changed ({changed} pixels)");

        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    /// Runs `preview` (a drag in progress), if any, then `action`, and
    /// checks undo restores the entity list from before both and redo the
    /// one after.
    fn run_undo_redo(
        core: &mut EditorCore,
        preview: Option<Action>,
        action: Action,
    ) -> anyhow::Result<()> {
        let label = format!("{action:?}");
        let before = entities(core)?;
        if let Some(preview) = preview {
            core.try_run(preview)?;
        }
        core.try_run(action)?;
        let after = entities(core)?;
        assert_ne!(before, after, "{label} changed something");
        core.try_run(Action::Undo)?;
        assert_eq!(entities(core)?, before, "{label} is undone");
        core.try_run(Action::Redo)?;
        assert_eq!(entities(core)?, after, "{label} is redone");
        Ok(())
    }

    /// The ticket's round trip without a window: add, rename, move,
    /// rescale, recolour, change mesh, delete (each undone and redone),
    /// save, then edit and reopen.
    #[test]
    fn edit_save_reopen_round_trip() -> anyhow::Result<()> {
        let (root, project_dir) = demo_project("round-trip")?;
        let gpu = GpuContext::new(None)?;
        let mut core = EditorCore::new(
            &gpu,
            egui::Context::default(),
            &project_dir,
            LogBuffer::default(),
        )?;
        let scene_file = project_dir.join("scenes/main.scene.ron");
        let starter = std::fs::read_to_string(&scene_file)?;
        let starter_spheres = starter.matches("builtin://sphere").count();
        let starter_len = entities(&mut core)?.len();
        assert!(starter_len >= 2, "the entry scene is open");
        assert!(!core.is_dirty());
        assert!(!core.title().contains('*'));

        run_undo_redo(
            &mut core,
            None,
            Action::Spawn("builtin://sphere".to_string()),
        )?;
        let ball = named(&core, "sphere");
        assert_eq!(core.selection.0, Some(ball), "the new entity is selected");
        assert_eq!(
            core.engine.world.get::<&Transform>(ball)?.translation,
            core.orbit.target,
            "added at the orbit target"
        );
        assert!(core.is_dirty() && core.title().contains('*'));

        run_undo_redo(
            &mut core,
            None,
            Action::Edit(Edit::Name(ball, "ball".to_string())),
        )?;
        let original = TransformDesc::from_transform(&*core.engine.world.get::<&Transform>(ball)?);
        let moved = TransformDesc {
            translation: [2.0, 1.0, -1.0],
            scale: [0.5, 0.5, 0.5],
            ..original
        };
        // A drag: a preview, then one commit.
        run_undo_redo(
            &mut core,
            Some(Action::PreviewTransform(ball, moved)),
            Action::Edit(Edit::Transform {
                entity: ball,
                desc: moved,
                original,
            }),
        )?;
        let white = core.engine.world.get::<&MeshInstance>(ball)?.color;
        run_undo_redo(
            &mut core,
            Some(Action::PreviewColor(ball, [0.0, 0.0, 0.5, 1.0])),
            Action::Edit(Edit::Color {
                entity: ball,
                color: [0.0, 0.0, 1.0, 1.0],
                original: white,
            }),
        )?;
        let cube = named(&core, "cube");
        run_undo_redo(
            &mut core,
            None,
            Action::Edit(Edit::Mesh(cube, "builtin://sphere".to_string())),
        )?;
        let ground = named(&core, "ground");
        core.selection.0 = Some(ground);
        run_undo_redo(&mut core, None, Action::Despawn(ground))?;
        core.drop_dead_selection();
        assert_eq!(core.selection.0, None, "deleting clears the selection");

        let edited = entities(&mut core)?;
        core.try_run(Action::Save)?;
        assert!(!core.is_dirty() && !core.title().contains('*'));
        let saved = std::fs::read_to_string(&scene_file)?;
        assert_eq!(edited.len(), starter_len, "one added, one deleted");
        assert!(saved.contains(r#"name: "ball""#), "{saved}");
        assert!(saved.contains("translation: (2.0, 1.0, -1.0)"), "{saved}");
        assert!(saved.contains("scale: (0.5, 0.5, 0.5)"), "{saved}");
        assert!(saved.contains("color: (0.0, 0.0, 1.0, 1.0)"), "{saved}");
        // The new ball and the re-meshed cube.
        assert_eq!(
            saved.matches("builtin://sphere").count(),
            starter_spheres + 2,
            "{saved}"
        );
        assert!(!saved.contains("ground"), "{saved}");

        // An unsaved edit, then reopen: back to what was saved.
        core.try_run(Action::Despawn(named(&core, "ball")))?;
        assert!(core.is_dirty());
        core.try_run(Action::Open)?;
        assert_eq!(entities(&mut core)?, edited);
        assert!(!core.is_dirty());

        core.try_run(Action::SaveAs("scenes/copy.scene.ron".to_string()))?;
        assert_eq!(core.scene_path, "scenes/copy.scene.ron");
        assert!(project_dir.join("scenes/copy.scene.ron").exists());

        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    /// Escape clears the selection, Delete deletes the selected entity,
    /// and the selection highlight never reaches the component.
    #[test]
    fn keys_and_highlight() -> anyhow::Result<()> {
        let (root, project_dir) = demo_project("keys")?;
        let gpu = GpuContext::new(None)?;
        let mut core = EditorCore::new(
            &gpu,
            egui::Context::default(),
            &project_dir,
            LogBuffer::default(),
        )?;
        let [width, height] = WINDOW_SIZE;
        let target = OffscreenTarget::new(&gpu.device, width, height);
        frame(&mut core, &gpu, &target, Vec::new());

        let cube = named(&core, "cube");
        let color = core.engine.world.get::<&MeshInstance>(cube)?.color;
        core.selection.0 = Some(cube);
        frame(&mut core, &gpu, &target, Vec::new());
        assert_eq!(
            core.engine.world.get::<&MeshInstance>(cube)?.color,
            color,
            "the highlight is render-only"
        );
        frame(&mut core, &gpu, &target, vec![key(egui::Key::Escape)]);
        assert_eq!(core.selection.0, None, "Escape clears the selection");

        core.selection.0 = Some(cube);
        frame(&mut core, &gpu, &target, vec![key(egui::Key::Delete)]);
        assert!(!core.engine.world.contains(cube), "Delete deletes it");
        frame(&mut core, &gpu, &target, Vec::new());
        assert_eq!(core.selection.0, None);
        frame(&mut core, &gpu, &target, vec![undo_key(false)]);
        assert!(core.engine.world.contains(cube), "and is undoable");

        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    /// An editor on a fresh demo project with [`play::orbit_demo`] added,
    /// after one frame; returns it with the GPU, target, and temp root.
    fn play_editor(
        label: &str,
    ) -> anyhow::Result<(EditorCore, GpuContext, OffscreenTarget, std::path::PathBuf)> {
        let (root, project_dir) = demo_project(label)?;
        let gpu = GpuContext::new(None)?;
        let mut core = EditorCore::new(
            &gpu,
            egui::Context::default(),
            &project_dir,
            LogBuffer::default(),
        )?;
        core.add_orbit_demo();
        let [width, height] = WINDOW_SIZE;
        let target = OffscreenTarget::new(&gpu.device, width, height);
        frame(&mut core, &gpu, &target, Vec::new());
        Ok((core, gpu, target, root))
    }

    /// The ticket's acceptance test: with an orbit demo system registered,
    /// Play moves the moon and Stop restores its exact transform (and the
    /// whole scene, entity ids, history, and saved state).
    #[test]
    fn play_moves_the_moon_and_stop_restores_it() -> anyhow::Result<()> {
        let (mut core, gpu, target, root) = play_editor("play-stop")?;
        let moon = named(&core, play::ORBITER);
        let before = *core.engine.world.get::<&Transform>(moon)?;
        let scene_before =
            shin::scene::SceneFile::capture(&core.engine.world, &core.engine.resources)?;
        let list_before = entities(&mut core)?;
        core.selection.0 = Some(moon);

        core.try_run(Action::Play)?;
        assert!(core.engine.play_state().is_playing());
        // One second of frames: 60 fixed steps at 1/60 s.
        for _ in 0..4 {
            core.clock.advance(&mut core.engine, 0.25);
        }
        assert!((core.engine.play_time().unwrap() - 1.0).abs() < 1e-9);
        let moved = core.engine.world.get::<&Transform>(moon)?.translation;
        assert!(
            moved.distance(before.translation) > 0.5,
            "the moon moved from {} to {moved}",
            before.translation
        );
        frame(&mut core, &gpu, &target, Vec::new());

        core.try_run(Action::Stop)?;
        assert!(!core.engine.play_state().is_playing());
        assert_eq!(
            *core.engine.world.get::<&Transform>(moon)?,
            before,
            "the moon is back, exactly, under the same id"
        );
        assert_eq!(
            shin::scene::SceneFile::capture(&core.engine.world, &core.engine.resources)?,
            scene_before
        );
        assert_eq!(entities(&mut core)?, list_before);
        assert_eq!(core.selection.0, Some(moon), "the selection survives");
        assert!(!core.is_dirty(), "playing is not an edit");
        assert!(
            core.engine
                .resources
                .get::<History>()
                .is_none_or(|h| h.undo_len() == 0),
            "nothing recorded"
        );

        // Not playing: frames do not step.
        assert_eq!(core.clock.advance(&mut core.engine, 1.0), 0);
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    /// While playing, edit actions, undo, Delete, and recorded console
    /// edits are refused; pause stops stepping; console `sim.*` drives
    /// the same mode as the toolbar.
    #[test]
    fn play_disables_editing() -> anyhow::Result<()> {
        let (mut core, gpu, target, root) = play_editor("play-disabled")?;
        let cube = named(&core, "cube");
        core.try_run(Action::Despawn(named(&core, "ground")))?;
        assert_eq!(core.engine.resources.expect::<History>().undo_len(), 1);
        let starter = core.engine.world.len();

        core.run(Action::Console("sim.play".to_string()));
        assert!(core.engine.play_state().is_playing(), "console sim.play");
        for action in [
            Action::Spawn("builtin://cube".to_string()),
            Action::Despawn(cube),
            Action::Edit(Edit::Name(cube, "box".to_string())),
            Action::Undo,
            Action::Redo,
            Action::Save,
            Action::Open,
        ] {
            let err = core.try_run(action.clone()).expect_err("refused");
            assert!(
                err.to_string().contains("while playing"),
                "{action:?}: {err}"
            );
        }
        core.selection.0 = Some(cube);
        frame(&mut core, &gpu, &target, vec![key(egui::Key::Delete)]);
        frame(&mut core, &gpu, &target, vec![undo_key(false)]);
        assert!(core.engine.world.contains(cube), "Delete is ignored");
        assert_eq!(core.engine.world.len(), starter, "Ctrl+Z is ignored");

        core.run(Action::Console("history.undo".to_string()));
        assert!(
            matches!(core.console.transcript.last(), Some((_, Err(e))) if e.contains("frozen"))
        );
        core.run(Action::Console(
            r#"entity.spawn {"mesh": 0, "translation": [0, 3, 0]}"#.to_string(),
        ));
        assert_eq!(core.engine.world.len(), starter + 1, "console still works");
        assert_eq!(
            core.engine.resources.expect::<History>().undo_len(),
            1,
            "but is not recorded"
        );

        core.try_run(Action::Pause)?;
        assert_eq!(core.clock.advance(&mut core.engine, 1.0), 0, "paused");
        core.try_run(Action::Pause)?;
        assert_eq!(core.clock.advance(&mut core.engine, 0.25), 15, "resumed");

        core.try_run(Action::Stop)?;
        assert_eq!(
            core.engine.world.len(),
            starter,
            "the console spawn is gone"
        );
        core.try_run(Action::Undo)?;
        assert_eq!(
            core.engine.world.len(),
            starter + 1,
            "the history picks up where it was: the ground comes back"
        );
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    /// GE-011: the inspector's Script field attaches a script with one
    /// undoable edit, Play runs it, Stop puts the entity back, and a broken
    /// script's error is what the inspector shows.
    #[test]
    fn inspector_script_runs_on_play_and_shows_errors() -> anyhow::Result<()> {
        let (mut core, gpu, target, root) = play_editor("script")?;
        let project_dir = root.join("demo");
        std::fs::create_dir_all(project_dir.join("scripts"))?;
        std::fs::write(
            project_dir.join("scripts/lift.rhai"),
            "fn update(entity, dt) { entity.transform.translation.y += dt; }",
        )?;
        let cube = named(&core, "cube");
        let start = *core.engine.world.get::<&Transform>(cube)?;

        run_undo_redo(
            &mut core,
            None,
            Action::Edit(Edit::Script(cube, "scripts/lift.rhai".to_string())),
        )?;
        assert_eq!(entities(&mut core)?[1]["script"], "scripts/lift.rhai");
        assert_eq!(
            crate::inspector::script_error(&core.engine, "scripts/lift.rhai".as_ref()),
            None
        );
        frame(&mut core, &gpu, &target, Vec::new());

        core.try_run(Action::Play)?;
        // Two quarter-second frames (the clock clamps a frame to 0.25 s).
        for _ in 0..2 {
            core.clock.advance(&mut core.engine, 0.25);
        }
        let y = core.engine.world.get::<&Transform>(cube)?.translation.y;
        assert!(
            (y - (start.translation.y + 0.5)).abs() < 1e-4,
            "the script lifted the cube half a unit: {y} from {}",
            start.translation.y
        );
        frame(&mut core, &gpu, &target, Vec::new());
        core.try_run(Action::Stop)?;
        assert_eq!(*core.engine.world.get::<&Transform>(cube)?, start);

        std::fs::write(
            project_dir.join("scripts/broken.rhai"),
            "fn update(entity, dt) {\n  let = 1;\n}",
        )?;
        core.try_run(Action::Edit(Edit::Script(
            cube,
            "scripts/broken.rhai".to_string(),
        )))?;
        let error = crate::inspector::script_error(&core.engine, "scripts/broken.rhai".as_ref())
            .expect("the compile error is kept for the inspector");
        assert!(error.starts_with("scripts/broken.rhai:2: "), "{error}");
        frame(&mut core, &gpu, &target, Vec::new());

        core.try_run(Action::Edit(Edit::Script(cube, String::new())))?;
        assert!(entities(&mut core)?[1].get("script").is_none(), "cleared");
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    /// An editor on a fresh demo project after one frame, with the GPU,
    /// a window-sized target, the temp root, and the scene file's path.
    fn editor(
        label: &str,
    ) -> anyhow::Result<(
        EditorCore,
        GpuContext,
        OffscreenTarget,
        std::path::PathBuf,
        std::path::PathBuf,
    )> {
        let (root, project_dir) = demo_project(label)?;
        let gpu = GpuContext::new(None)?;
        let mut core = EditorCore::new(
            &gpu,
            egui::Context::default(),
            &project_dir,
            LogBuffer::default(),
        )?;
        let [width, height] = WINDOW_SIZE;
        let target = OffscreenTarget::new(&gpu.device, width, height);
        frame(&mut core, &gpu, &target, Vec::new());
        let scene_file = project_dir.join("scenes/main.scene.ron");
        Ok((core, gpu, target, root, scene_file))
    }

    /// The scene camera's placement.
    fn scene_camera(core: &EditorCore) -> Transform {
        core.engine.resources.expect::<Camera>().transform
    }

    /// The camera a scene file holds.
    fn file_camera(path: &Path) -> anyhow::Result<shin::scene::CameraDesc> {
        shin::scene::SceneFile::from_ron(&std::fs::read_to_string(path)?)?
            .camera
            .context("the scene file has no camera")
    }

    /// The middle of the Viewport panel after two frames.
    fn viewport(
        core: &mut EditorCore,
        gpu: &GpuContext,
        target: &OffscreenTarget,
    ) -> anyhow::Result<image::RgbaImage> {
        for _ in 0..2 {
            frame(core, gpu, target, Vec::new());
        }
        let image = target.read_pixels(gpu)?;
        Ok(image::imageops::crop_imm(&image, 400, 135, 800, 450).to_image())
    }

    /// How many pixels differ between two same-sized images.
    fn differing(a: &image::RgbaImage, b: &image::RgbaImage) -> usize {
        a.pixels().zip(b.pixels()).filter(|(a, b)| a != b).count()
    }

    /// GE-030: frames with the orbit camera never touch the scene camera,
    /// so Save after orbiting writes the camera that was authored.
    #[test]
    fn orbit_and_save_keep_the_scene_camera() -> anyhow::Result<()> {
        let (mut core, gpu, target, root, scene_file) = editor("scene-camera-save")?;
        let authored = scene_camera(&core);
        assert_eq!(
            authored.translation,
            Vec3::new(0.0, 2.0, 5.0),
            "the starter scene's camera is the scene camera"
        );
        core.try_run(Action::Save)?;
        let before = std::fs::read_to_string(&scene_file)?;

        core.orbit.orbit(Vec2::new(300.0, -80.0));
        core.orbit.pan(Vec2::new(40.0, 20.0));
        core.orbit.zoom(400.0);
        for _ in 0..3 {
            frame(&mut core, &gpu, &target, Vec::new());
        }
        assert_eq!(scene_camera(&core), authored, "frames leave it alone");
        assert!(
            core.orbit
                .transform()
                .translation
                .distance(authored.translation)
                > 1.0,
            "the view is somewhere else"
        );
        assert!(!core.is_dirty(), "orbiting is not an edit");

        core.try_run(Action::Save)?;
        let after = std::fs::read_to_string(&scene_file)?;
        assert_eq!(after, before, "the saved file is identical");
        assert_eq!(
            file_camera(&scene_file)?.position,
            authored.translation.to_array()
        );
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    /// GE-030: "Set scene camera from view" records one `camera.set` that
    /// moves the scene camera to the orbit camera, keeping the field of
    /// view; it dirties the scene, Save writes it, and Ctrl+Z puts the
    /// authored camera back exactly.
    #[test]
    fn set_scene_camera_from_view_is_undoable() -> anyhow::Result<()> {
        let (mut core, gpu, target, root, scene_file) = editor("scene-camera-set")?;
        core.try_run(Action::Save)?;
        let original_file = std::fs::read_to_string(&scene_file)?;
        let authored = *core.engine.resources.expect::<Camera>();
        let Projection::Perspective { fov_y_radians, .. } = authored.projection else {
            panic!("the starter camera is perspective");
        };

        core.orbit.orbit(Vec2::new(250.0, 60.0));
        core.orbit.zoom(-300.0);
        frame(&mut core, &gpu, &target, Vec::new());
        let view = core.orbit.transform();
        core.try_run(Action::SetSceneCameraFromView)?;
        let camera = *core.engine.resources.expect::<Camera>();
        assert!(
            camera.transform.translation.distance(view.translation) < 1e-4,
            "{camera:?}"
        );
        assert!(
            camera.transform.forward().distance(view.forward()) < 1e-4,
            "{camera:?}"
        );
        assert!(
            matches!(camera.projection, Projection::Perspective { fov_y_radians: f, .. } if f == fov_y_radians),
            "the field of view is kept: {:?}",
            camera.projection
        );
        assert!(core.is_dirty() && core.title().contains('*'));
        let stack = core.undo_stack();
        assert_eq!(stack.len(), 1, "{stack:?}");
        assert_eq!(stack[0].0, "camera.set", "{stack:?}");

        core.try_run(Action::Save)?;
        assert_ne!(std::fs::read_to_string(&scene_file)?, original_file);
        let saved = file_camera(&scene_file)?;
        assert!(
            Vec3::from(saved.position).distance(view.translation) < 1e-4,
            "{saved:?}"
        );
        // A scene file's look_at is the view ray's point nearest the origin,
        // so check the direction, not the point.
        let saved_forward = (Vec3::from(saved.look_at) - Vec3::from(saved.position)).normalize();
        assert!(
            saved_forward.distance(view.forward()) < 1e-3,
            "look_at is on the view ray: {saved:?}"
        );

        frame(&mut core, &gpu, &target, vec![undo_key(false)]);
        assert_eq!(
            scene_camera(&core),
            authored.transform,
            "Ctrl+Z puts the authored camera back"
        );
        assert!(core.is_dirty(), "undone past the save");
        core.try_run(Action::Save)?;
        assert_eq!(std::fs::read_to_string(&scene_file)?, original_file);
        frame(&mut core, &gpu, &target, vec![undo_key(true)]);
        assert!(
            scene_camera(&core).translation.distance(view.translation) < 1e-4,
            "Ctrl+Shift+Z redoes it"
        );
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    /// GE-030: the viewport renders from the orbit camera while editing and
    /// from the scene camera while playing; Stop returns to the orbit view,
    /// and "View from scene camera" moves the orbit camera to match.
    #[test]
    fn play_renders_from_the_scene_camera() -> anyhow::Result<()> {
        let (mut core, gpu, target, root, _) = editor("scene-camera-play")?;
        core.orbit.orbit(Vec2::new(400.0, -100.0));
        core.orbit.zoom(500.0);
        let orbit_view = viewport(&mut core, &gpu, &target)?;
        let total = orbit_view.pixels().count();

        core.try_run(Action::Play)?;
        let playing_view = viewport(&mut core, &gpu, &target)?;
        let changed = differing(&orbit_view, &playing_view);
        assert!(
            changed > total / 10,
            "playing shows the scene camera ({changed} of {total} pixels differ)"
        );

        core.try_run(Action::Stop)?;
        let stopped_view = viewport(&mut core, &gpu, &target)?;
        assert_eq!(
            differing(&orbit_view, &stopped_view),
            0,
            "Stop returns to the orbit view"
        );

        core.try_run(Action::ViewFromSceneCamera)?;
        let authored = scene_camera(&core);
        let view = core.orbit.transform();
        assert!(view.translation.distance(authored.translation) < 1e-4);
        assert!(view.forward().distance(authored.forward()) < 1e-4);
        assert!(!core.is_dirty(), "viewing is not an edit");
        let from_scene = viewport(&mut core, &gpu, &target)?;
        let changed = differing(&playing_view, &from_scene);
        assert!(
            changed < total / 100,
            "the orbit camera now shows the scene camera's view ({changed} pixels differ)"
        );
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    /// GE-032: the Assets panel's action paths. Spawn and Assign (mesh,
    /// texture, script) are each one undoable edit; Open loads a scene
    /// file and makes it the one Save writes; Import copies a file into
    /// `assets/` and the panel lists it.
    #[test]
    fn assets_panel_spawn_assign_open_and_import() -> anyhow::Result<()> {
        let (root, project_dir) = demo_project("assets-panel")?;
        let test_assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("../shin-engine/assets/test");
        // A second mesh whose image is the PNG next to it, and a script.
        std::fs::copy(
            test_assets.join("textured_cube.gltf"),
            project_dir.join("assets/crate.gltf"),
        )?;
        std::fs::copy(
            test_assets.join("checker.png"),
            project_dir.join("assets/checker.png"),
        )?;
        std::fs::create_dir_all(project_dir.join("scripts"))?;
        std::fs::write(
            project_dir.join("scripts/lift.rhai"),
            "fn update(entity, dt) { entity.transform.translation.y += dt; }",
        )?;
        let gpu = GpuContext::new(None)?;
        let mut core = EditorCore::new(
            &gpu,
            egui::Context::default(),
            &project_dir,
            LogBuffer::default(),
        )?;
        let [width, height] = WINDOW_SIZE;
        let target = OffscreenTarget::new(&gpu.device, width, height);
        // The panel is in the default layout, so one frame scans the project.
        frame(&mut core, &gpu, &target, Vec::new());
        let sources = |core: &EditorCore| -> Vec<String> {
            core.assets
                .entries()
                .iter()
                .map(|e| e.source.to_string())
                .collect()
        };
        assert_eq!(
            sources(&core),
            [
                "assets/checker.png",
                "assets/crate.gltf",
                "assets/crate.gltf#image=0",
                "assets/cube.gltf",
                "scenes/main.scene.ron",
                "scripts/lift.rhai",
            ]
        );
        let starter = core.engine.world.len();

        // Spawn: the mesh entry's source, at the orbit target, selected.
        run_undo_redo(
            &mut core,
            None,
            Action::Spawn("assets/crate.gltf".to_string()),
        )?;
        assert_eq!(core.engine.world.len(), starter + 1);
        let spawned = core.selection.0.expect("the new entity is selected");
        assert_eq!(
            core.engine.world.get::<&Name>(spawned)?.0,
            "crate",
            "named after the file"
        );
        assert_eq!(
            core.engine.world.get::<&Transform>(spawned)?.translation,
            core.orbit.target
        );

        // Assign to selection: mesh, texture (and its removal), script.
        let cube = named(&core, "cube");
        run_undo_redo(
            &mut core,
            None,
            Action::Edit(Edit::Mesh(cube, "assets/crate.gltf".to_string())),
        )?;
        run_undo_redo(
            &mut core,
            None,
            Action::Edit(Edit::Texture(
                cube,
                Some("assets/crate.gltf#image=0".to_string()),
            )),
        )?;
        let instance = *core.engine.world.get::<&MeshInstance>(cube)?;
        let state = core.engine.resources.expect::<shin::render::RenderState>();
        assert_eq!(
            state
                .textures
                .source(instance.texture.expect("textured"))
                .map(ToString::to_string)
                .as_deref(),
            Some("assets/crate.gltf#image=0")
        );
        run_undo_redo(&mut core, None, Action::Edit(Edit::Texture(cube, None)))?;
        assert_eq!(core.engine.world.get::<&MeshInstance>(cube)?.texture, None);
        run_undo_redo(
            &mut core,
            None,
            Action::Edit(Edit::Script(cube, "scripts/lift.rhai".to_string())),
        )?;
        assert_eq!(
            core.engine
                .world
                .get::<&shin::script::Script>(cube)?
                .path
                .to_string_lossy(),
            "scripts/lift.rhai"
        );
        // Five actions (spawn and four assigns), each one history entry.
        assert_eq!(core.engine.resources.expect::<History>().undo_len(), 5);

        // Open: save a copy, then open it from the panel; the history
        // starts over and Save targets the copy.
        core.try_run(Action::SaveAs("scenes/copy.scene.ron".to_string()))?;
        core.assets.invalidate();
        frame(&mut core, &gpu, &target, Vec::new());
        assert!(
            sources(&core).contains(&"scenes/copy.scene.ron".to_string()),
            "{:?}",
            sources(&core)
        );
        core.try_run(Action::Despawn(cube))?;
        assert!(core.is_dirty());
        core.try_run(Action::OpenScene("scenes/copy.scene.ron".to_string()))?;
        assert_eq!(core.scene_path, "scenes/copy.scene.ron");
        assert!(!core.is_dirty());
        assert_eq!(core.engine.resources.expect::<History>().undo_len(), 0);
        assert!(
            core.engine
                .world
                .query::<(Entity, &Name)>()
                .iter()
                .any(|(_, n)| n.0 == "cube"),
            "the cube is back from the saved copy"
        );
        let err = core
            .try_run(Action::OpenScene("scenes/none.scene.ron".to_string()))
            .expect_err("a missing scene fails");
        assert!(err.to_string().contains("scenes/none.scene.ron"), "{err}");

        // Import: an absolute path is copied into assets/ and listed (its
        // image is the checker.png already there).
        let gltf = test_assets.join("textured_cube.gltf").canonicalize()?;
        core.try_run(Action::Import(gltf.to_string_lossy().into_owned()))?;
        assert!(project_dir.join("assets/textured_cube.gltf").is_file());
        frame(&mut core, &gpu, &target, Vec::new());
        assert!(
            sources(&core).contains(&"assets/textured_cube.gltf#image=0".to_string()),
            "{:?}",
            sources(&core)
        );
        let err = core
            .try_run(Action::Import(gltf.to_string_lossy().into_owned()))
            .expect_err("importing the same name twice fails");
        assert!(format!("{err:#}").contains("already exists"), "{err:#}");
        let err = core
            .try_run(Action::Import("relative.png".to_string()))
            .expect_err("a relative path fails");
        assert!(format!("{err:#}").contains("absolute"), "{err:#}");

        // The imported file's entries can be used like any other.
        let cube = named(&core, "cube");
        run_undo_redo(
            &mut core,
            None,
            Action::Edit(Edit::Texture(
                cube,
                Some("assets/textured_cube.gltf#image=0".to_string()),
            )),
        )?;

        // While playing, the panel's edits are refused but Import is not.
        core.try_run(Action::Play)?;
        for action in [
            Action::Spawn("assets/crate.gltf".to_string()),
            Action::Edit(Edit::Texture(cube, None)),
            Action::OpenScene("scenes/copy.scene.ron".to_string()),
        ] {
            let err = core.try_run(action.clone()).expect_err("refused");
            assert!(
                err.to_string().contains("while playing"),
                "{action:?}: {err}"
            );
        }
        assert!(!Action::Import(String::new()).is_edit());
        core.try_run(Action::Stop)?;

        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn spawn_names_come_from_the_source() {
        assert_eq!(spawn_name("builtin://cube").as_deref(), Some("cube"));
        assert_eq!(spawn_name("assets/crate.gltf#1").as_deref(), Some("crate"));
        assert_eq!(spawn_name("bad://x"), None);
    }

    #[test]
    fn highlight_mixes_toward_white() {
        assert_eq!(highlight([0.0, 0.5, 1.0, 0.25]), [0.5, 0.75, 1.0, 0.25]);
    }
}
