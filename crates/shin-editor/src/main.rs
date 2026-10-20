//! `shin-editor`, the Shin editor: an egui shell around a live viewport.
//!
//! ```text
//! shin-editor <project-dir> [--screenshot <png> [--select <name>]] [--demo-orbit]
//! ```
//!
//! Opens the project in `<project-dir>` (its `shin.toml` names the window)
//! with docked Viewport, Scene, Assets, Inspector, History, Multiplayer,
//! Log, and Console panels. In the viewport, left-drag orbits, right-drag pans, the
//! wheel zooms, a click selects the entity under the pointer, and the
//! selection's gizmo moves, rotates, or scales it (W / E / R). The
//! toolbar's Play runs the simulation and Stop puts the scene back.
//!
//! `--screenshot <png>` renders one full editor frame without a window,
//! writes it to `<png>`, and exits; tests use it. `--select <name>` selects
//! the entity with that name first, so the frame shows its gizmo.
//!
//! `--demo-orbit` registers a demo system that moves the entity named
//! `moon` around the origin while playing, as the starter game does, since
//! the editor does not run the game's own systems yet.

mod assets;
mod edit;
mod editor;
mod gizmo;
mod inspector;
mod log_sink;
mod multiplayer;
mod orbit;
mod pick;
mod play;
mod scene;
mod ui;

use std::path::PathBuf;

use clap::Parser;

use crate::log_sink::LogBuffer;

/// The Shin editor.
#[derive(Parser)]
#[command(name = "shin-editor", version, about)]
struct Cli {
    /// The project directory (the one containing `shin.toml`).
    project_dir: PathBuf,
    /// Render one editor frame offscreen to this PNG and exit.
    #[arg(long, value_name = "PNG")]
    screenshot: Option<PathBuf>,
    /// With `--screenshot`: select the entity with this name first, so the
    /// image shows its highlight and gizmo.
    #[arg(long, value_name = "NAME", requires = "screenshot")]
    select: Option<String>,
    /// Register a demo system that orbits the entity named `moon` while
    /// playing.
    #[arg(long)]
    demo_orbit: bool,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let log = LogBuffer::default();
    log_sink::init(log.clone())?;

    match cli.screenshot {
        Some(out) => editor::screenshot(&cli.project_dir, &out, cli.select.as_deref(), log),
        None => editor::run(&cli.project_dir, log, cli.demo_orbit),
    }
}
