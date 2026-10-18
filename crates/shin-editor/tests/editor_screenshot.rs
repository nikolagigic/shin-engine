//! GE-020: `shin-editor <project> --screenshot <png>` renders one full
//! editor frame: the image is the window size, the viewport shows the scene,
//! and the panels drew UI. There is no UI golden; egui text rendering varies
//! between machines.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use image::{Rgba, RgbaImage};
use shin::project::{self, NewProjectOptions};

/// The editor window size, `editor::WINDOW_SIZE` in the binary.
const WINDOW_SIZE: (u32, u32) = (1600, 900);

/// The viewport's linear clear colour, `scene::SKY` in the binary.
const SKY_LINEAR: [f64; 3] = [0.05, 0.08, 0.15];

/// A fresh, missing directory under the system temp dir.
fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("shin-editor-test-{label}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir
}

/// Linear to sRGB-encoded byte, as the sRGB target stores it.
fn srgb_byte(linear: f64) -> u8 {
    let encoded = if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round() as u8
}

/// The `w` x `h` crop of `image` whose top-left corner is at the fractions
/// `(fx, fy)` of the image size.
fn crop(image: &RgbaImage, fx: f32, fy: f32, w: u32, h: u32) -> RgbaImage {
    let x = (fx * image.width() as f32) as u32;
    let y = (fy * image.height() as f32) as u32;
    image::imageops::crop_imm(image, x, y, w, h).to_image()
}

/// Whether every channel of `a` and `b` is within 2 of each other.
fn near(a: &Rgba<u8>, b: &Rgba<u8>) -> bool {
    a.0.iter().zip(b.0).all(|(x, y)| x.abs_diff(y) <= 2)
}

#[test]
fn screenshot_shows_scene_and_panels() -> anyhow::Result<()> {
    let root = temp_dir("screenshot");
    let project_dir = root.join("demo");
    project::create(&NewProjectOptions {
        name: "demo".to_string(),
        dir: project_dir.clone(),
        engine: Default::default(),
    })?;
    let png = root.join("editor.png");

    let output = Command::new(env!("CARGO_BIN_EXE_shin-editor"))
        .arg(&project_dir)
        .arg("--screenshot")
        .arg(&png)
        .output()?;
    assert!(
        output.status.success(),
        "shin-editor --screenshot failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let image = image::open(&png)?.to_rgba8();
    assert_eq!(
        image.dimensions(),
        WINDOW_SIZE,
        "the image is the window size"
    );

    // The viewport (centre) is not just the clear colour: the scene drew.
    let sky = Rgba([
        srgb_byte(SKY_LINEAR[0]),
        srgb_byte(SKY_LINEAR[1]),
        srgb_byte(SKY_LINEAR[2]),
        255,
    ]);
    let viewport = crop(&image, 0.25, 0.15, 800, 450);
    let sky_pixels = viewport.pixels().filter(|p| near(p, &sky)).count();
    let total = viewport.pixels().count();
    assert!(sky_pixels > 0, "the sky shows above the ground");
    assert!(
        sky_pixels < total * 3 / 4,
        "the viewport is mostly not sky ({sky_pixels} of {total} pixels)"
    );

    // Same-size crops of the four panels, each including its first content.
    let panels = [
        ("scene", crop(&image, 0.005, 0.045, 160, 90)),
        ("viewport", crop(&image, 0.45, 0.35, 160, 90)),
        ("inspector", crop(&image, 0.825, 0.045, 160, 90)),
        ("log", crop(&image, 0.19, 0.79, 160, 90)),
    ];
    for (name, panel) in &panels {
        let first = panel.get_pixel(0, 0);
        assert!(
            panel.pixels().any(|p| p != first),
            "the {name} panel is a single colour"
        );
    }
    for (i, (a_name, a)) in panels.iter().enumerate() {
        for (b_name, b) in &panels[i + 1..] {
            assert_ne!(a, b, "the {a_name} and {b_name} panels are identical");
        }
    }

    let _ = fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn missing_project_fails_with_the_manifest_path() {
    let dir = temp_dir("missing");
    let output = Command::new(env!("CARGO_BIN_EXE_shin-editor"))
        .arg(&dir)
        .arg("--screenshot")
        .arg(dir.join("never.png"))
        .output()
        .expect("run shin-editor");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("shin.toml"), "stderr: {stderr}");
}

/// Pixels of the viewport that are clearly green: the gizmo's Y handle
/// and plane handles. Nothing in the starter scene is green.
fn green_pixels(image: &RgbaImage) -> usize {
    crop(image, 0.25, 0.15, 800, 450)
        .pixels()
        .filter(|p| {
            let [r, g, b, _] = p.0.map(i32::from);
            g > r + 60 && g > b + 30
        })
        .count()
}

/// GE-035: `--select <name>` screenshots show the selection's gizmo
/// (the guide's `images/editor-gizmo.png` is made this way); an unknown
/// name fails.
#[test]
fn screenshot_with_a_selection_shows_its_gizmo() -> anyhow::Result<()> {
    let root = temp_dir("gizmo");
    let project_dir = root.join("demo");
    project::create(&NewProjectOptions {
        name: "demo".to_string(),
        dir: project_dir.clone(),
        engine: Default::default(),
    })?;
    let shoot = |png: &PathBuf, select: Option<&str>| -> anyhow::Result<std::process::Output> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_shin-editor"));
        command.arg(&project_dir).arg("--screenshot").arg(png);
        if let Some(name) = select {
            command.arg("--select").arg(name);
        }
        Ok(command.output()?)
    };
    let plain = root.join("plain.png");
    let selected = root.join("selected.png");
    assert!(shoot(&plain, None)?.status.success());
    let output = shoot(&selected, Some("cube"))?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let without = green_pixels(&image::open(&plain)?.to_rgba8());
    let with = green_pixels(&image::open(&selected)?.to_rgba8());
    assert_eq!(without, 0, "no gizmo without a selection");
    assert!(with > 50, "the gizmo's green handles show ({with} pixels)");

    let output = shoot(&root.join("none.png"), Some("no-such-entity"))?;
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no-such-entity"), "stderr: {stderr}");

    let _ = fs::remove_dir_all(&root);
    Ok(())
}
