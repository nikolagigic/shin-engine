//! Spawns the `shin-mcp` binary and speaks MCP to it over stdio with
//! hand-written JSON-RPC lines: handshake, `tools/list`, an unknown tool
//! (must be a JSON-RPC error, not a crash), then `engine.info`.
//!
//! Runs with `RUST_LOG=debug` and checks that every stdout line is JSON, so
//! no log output leaks into the protocol channel.

mod common;

use serde_json::{Value, json};

use common::{Server, first_text, parse_json_line};

#[test]
fn stdio_roundtrip() {
    let mut server = Server::spawn(&["--width", "64", "--height", "36"]);

    let init = server.handshake("stdio_roundtrip");
    assert_eq!(init["result"]["serverInfo"]["name"], "shin-mcp", "{init}");

    let list = server.request(2, "tools/list", json!({}));
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("no tools in {list}"))
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"engine.info"), "{names:?}");
    assert!(names.contains(&"engine.list_tools"), "{names:?}");
    // GE-046: the multiplayer tools.
    for net in [
        "net.host",
        "net.join",
        "net.disconnect",
        "net.status",
        "net.players",
    ] {
        assert!(names.contains(&net), "{net} in {names:?}");
    }

    let unknown = server.request(
        3,
        "tools/call",
        json!({ "name": "no.such_tool", "arguments": {} }),
    );
    let message = unknown["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a JSON-RPC error, got {unknown}"));
    assert!(message.contains("no.such_tool"), "{message}");
    // Invalid params, per the MCP spec for an unknown tool.
    assert_eq!(unknown["error"]["code"], json!(-32602), "{unknown}");

    // The server keeps serving after the error.
    let info = server.request(
        4,
        "tools/call",
        json!({ "name": "engine.info", "arguments": {} }),
    );
    let info: Value = serde_json::from_str(first_text(&info)).unwrap();
    assert!(info.get("entities").is_some(), "{info}");
    assert_eq!(info["target"], json!({ "width": 64, "height": 36 }));

    let lines = server.shutdown();
    assert!(lines.len() >= 4, "{lines:?}");
    for line in &lines {
        parse_json_line(line);
    }
}
