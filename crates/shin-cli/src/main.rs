//! `shin`, the Shin command-line tool.
//!
//! ```text
//! shin new <name> [--dir <path>] [--engine-path <path>]
//! shin edit <dir> [-- <editor args>...]
//! shin docs tools
//! ```
//!
//! `shin new` creates a runnable game project in `<dir>/<name>` (the
//! current directory by default) and prints every file it wrote, one per
//! line. It is the same operation as the engine's `project.new` tool.
//!
//! `shin edit` opens the project in `<dir>` in the editor by running the
//! `shin-editor` binary: the one named by `$SHIN_EDITOR`, else the one next
//! to `shin`, else the one on `PATH`. Arguments after `--` are passed on
//! (for example `-- --screenshot out.png`).
//!
//! `shin docs tools` prints the user guide's tools reference (GE-029) as
//! Markdown, generated from the engine's tool registry; see [`docs`].

mod docs;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command as Process;

use anyhow::Context as _;

use clap::{Parser, Subcommand};
use shin::project::{self, EngineDep, NewProjectOptions};

/// The Shin game engine's command-line tool.
#[derive(Parser)]
#[command(name = "shin", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new game project in `<dir>/<name>`.
    New {
        /// Project and Cargo package name (ASCII letters, digits, `-`, `_`;
        /// not starting with a digit).
        name: String,
        /// Parent directory of the new project.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Depend on a local `crates/shin-engine` instead of the published
        /// `shin-engine` on crates.io.
        #[arg(long)]
        engine_path: Option<PathBuf>,
    },
    /// Open the project in `<dir>` in the editor (`shin-editor`).
    Edit {
        /// The project directory (the one containing `shin.toml`).
        dir: PathBuf,
        /// Extra arguments for `shin-editor`, after `--`.
        #[arg(last = true)]
        editor_args: Vec<OsString>,
    },
    /// Print generated documentation for the user guide (`guide/`).
    Docs {
        #[command(subcommand)]
        what: Docs,
    },
}

/// What `shin docs` can generate.
#[derive(Subcommand)]
enum Docs {
    /// The tools reference as Markdown: every engine tool with its
    /// description and arguments. Redirect it to
    /// `guide/src/reference/tools.md`.
    Tools,
}

/// Environment variable that overrides which editor binary `shin edit` runs.
const EDITOR_ENV: &str = "SHIN_EDITOR";

/// The `shin-editor` binary to run: `$SHIN_EDITOR` if set, else the
/// `shin-editor` next to `shin_exe` if it exists, else plain `shin-editor`
/// for a `PATH` lookup.
fn editor_binary(env_override: Option<OsString>, shin_exe: Option<&Path>) -> PathBuf {
    if let Some(path) = env_override {
        return path.into();
    }
    let name = format!("shin-editor{}", std::env::consts::EXE_SUFFIX);
    shin_exe
        .map(|exe| exe.with_file_name(&name))
        .filter(|sibling| sibling.is_file())
        .unwrap_or_else(|| name.into())
}

/// Runs the editor on `dir` with `extra` arguments and waits for it.
fn edit(dir: &Path, extra: &[OsString]) -> anyhow::Result<()> {
    let exe = std::env::current_exe().ok();
    let editor = editor_binary(std::env::var_os(EDITOR_ENV), exe.as_deref());
    let status = Process::new(&editor)
        .arg(dir)
        .args(extra)
        .status()
        .with_context(|| {
            format!(
                "could not run the editor `{}`; build it with `cargo build -p shin-editor` \
                 or point ${EDITOR_ENV} at it",
                editor.display()
            )
        })?;
    anyhow::ensure!(status.success(), "the editor exited with {status}");
    Ok(())
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    match Cli::parse().command {
        Command::New {
            name,
            dir,
            engine_path,
        } => {
            let opts = NewProjectOptions {
                dir: dir.join(&name),
                name,
                engine: engine_path.map(EngineDep::Path).unwrap_or_default(),
            };
            for file in project::create(&opts)? {
                println!("{}", file.display());
            }
            eprintln!(
                "Created `{}`. Run it with:\n\n    cd {} && cargo run",
                opts.name,
                opts.dir.display()
            );
        }
        Command::Edit { dir, editor_args } => edit(&dir, &editor_args)?,
        Command::Docs { what: Docs::Tools } => print!("{}", docs::tools_markdown()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_wins() {
        let exe = std::env::current_exe().unwrap();
        assert_eq!(
            editor_binary(Some("/opt/editor".into()), Some(&exe)),
            PathBuf::from("/opt/editor")
        );
    }

    #[test]
    fn falls_back_to_path_lookup_without_a_sibling() {
        let name = format!("shin-editor{}", std::env::consts::EXE_SUFFIX);
        let missing = Path::new("/definitely/not/here/shin");
        assert_eq!(editor_binary(None, Some(missing)), PathBuf::from(&name));
        assert_eq!(editor_binary(None, None), PathBuf::from(&name));
    }
}
