//! GE-015: build a scene, step it, and look at it, entirely through the
//! tool registry, the way an agent connected to `shin-mcp` would.
//!
//! Also checks that every engine tool's schema describes every field.

mod common;

use common::{GoldenOpts, assert_golden};
use image::RgbaImage;
use serde_json::{Value, json};
use shin::headless::Headless;
use shin::tool::{ToolOutput, ToolRegistry};

const SIZE: u32 = 128;

fn call_json(reg: &ToolRegistry, engine: &mut Headless, name: &str, args: Value) -> Value {
    match reg.call(engine, name, args) {
        Ok(ToolOutput::Json(value)) => value,
        other => panic!("`{name}` did not return JSON: {other:?}"),
    }
}

/// Calls `render.frame` at `SIZE` x `SIZE` and decodes the PNG.
fn render_frame(reg: &ToolRegistry, engine: &mut Headless) -> RgbaImage {
    let args = json!({ "width": SIZE, "height": SIZE });
    match reg.call(engine, "render.frame", args) {
        Ok(ToolOutput::Image { png, width, height }) => {
            assert_eq!((width, height), (SIZE, SIZE));
            image::load_from_memory_with_format(&png, image::ImageFormat::Png)
                .expect("render.frame returns a valid PNG")
                .to_rgba8()
        }
        other => panic!("render.frame did not return an image: {other:?}"),
    }
}

#[test]
fn tools_scene_roundtrip() -> anyhow::Result<()> {
    let reg = ToolRegistry::with_engine_tools();
    // Start at a different size so render.frame's resize is exercised.
    let mut engine = Headless::new(32, 32)?;
    let clear = render_frame(&reg, &mut engine);

    let cube =
        call_json(&reg, &mut engine, "mesh.builtin", json!({ "kind": "cube" }))["handle"].clone();
    // Three cubes in a row: red tilted on the left, green in the middle,
    // blue scaled down on the right.
    let spawns = [
        json!({ "mesh": cube, "translation": [-1.5, 0, 0], "rotation_euler_deg": [0, 30, 0],
                "color": [0.8, 0.1, 0.1, 1] }),
        json!({ "mesh": cube, "translation": [0, 0, 0], "color": [0.1, 0.8, 0.1, 1] }),
        json!({ "mesh": cube, "translation": [1.5, 0, 0], "scale": [0.6, 0.6, 0.6],
                "color": [0.1, 0.1, 0.8, 1] }),
    ];
    let entities: Vec<Value> = spawns
        .into_iter()
        .map(|args| call_json(&reg, &mut engine, "entity.spawn", args)["entity"].clone())
        .collect();
    assert_eq!(engine.world.len(), 3);

    let time = call_json(&reg, &mut engine, "sim.step", json!({ "steps": 10 }));
    assert_eq!(time["fixed_steps"], 10);

    let frame = render_frame(&reg, &mut engine);
    let differing = frame
        .pixels()
        .zip(clear.pixels())
        .filter(|(a, b)| a != b)
        .count();
    assert!(
        differing > (SIZE * SIZE / 20) as usize,
        "only {differing} pixels differ from the clear frame"
    );
    assert_golden("tool_scene", &frame, GoldenOpts::default());

    for entity in entities {
        let out = call_json(
            &reg,
            &mut engine,
            "entity.despawn",
            json!({ "entity": entity }),
        );
        assert_eq!(out, json!({ "ok": true, "removed": 1 }));
    }
    let empty = render_frame(&reg, &mut engine);
    assert!(
        empty == clear,
        "frame after despawning everything is not the clear frame"
    );
    Ok(())
}

#[test]
fn every_tool_field_has_a_description() {
    let tools = ToolRegistry::with_engine_tools().list();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    for expected in [
        "scene.load_gltf",
        "scene.clear",
        "mesh.list",
        "mesh.builtin",
        "entity.spawn",
        "entity.list",
        "entity.set_transform",
        "entity.despawn",
        "camera.set",
        "light.set",
        "sim.step",
        "render.frame",
        "scene.save",
        "scene.load",
    ] {
        assert!(
            names.contains(&expected),
            "`{expected}` missing from {names:?}"
        );
    }

    for tool in &tools {
        assert!(
            !tool.description.is_empty(),
            "`{}` has no description",
            tool.name
        );
        let schema = &tool.input_schema;
        assert_eq!(schema["type"], "object", "`{}` schema", tool.name);
        let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
            continue; // no arguments
        };
        for (field, property) in properties {
            let description = property.get("description").and_then(Value::as_str);
            assert!(
                description.is_some_and(|d| !d.is_empty()),
                "`{}` field `{field}` has no description: {property}",
                tool.name
            );
        }
    }
}
