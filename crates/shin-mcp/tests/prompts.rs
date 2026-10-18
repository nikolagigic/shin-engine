//! Spawns `shin-mcp` and checks the skills and guide over stdio (GE-039):
//! `initialize` advertises prompts and resources, `prompts/list` shows
//! every skill, `prompts/get` returns its text, `resources/list` has every
//! skill and guide page, and `resources/read` returns one.

mod common;

use serde_json::{Value, json};
use shin::project::skills::SKILLS;

use common::{Server, parse_json_line};

/// The `result` of a response, panicking on a JSON-RPC error.
fn result(response: &Value) -> &Value {
    assert!(response.get("error").is_none(), "{response}");
    &response["result"]
}

#[test]
fn prompts_and_resources_over_stdio() {
    let mut server = Server::spawn(&["--width", "16", "--height", "16"]);
    let init = server.handshake("prompts");
    let capabilities = &result(&init)["capabilities"];
    assert!(capabilities.get("prompts").is_some(), "{init}");
    assert!(capabilities.get("resources").is_some(), "{init}");

    // prompts/list: one prompt per skill, with its description and an
    // optional `goal` argument.
    let list = server.request(2, "prompts/list", json!({}));
    let prompts = result(&list)["prompts"].as_array().unwrap();
    let names: Vec<&str> = prompts
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    let skill_names: Vec<&str> = SKILLS.iter().map(|s| s.name).collect();
    assert_eq!(names, skill_names);
    for (prompt, skill) in prompts.iter().zip(SKILLS) {
        assert_eq!(prompt["description"], skill.description());
        assert_eq!(prompt["arguments"][0]["name"], "goal");
        assert_eq!(prompt["arguments"][0]["required"], false);
    }

    // prompts/get: the SKILL.md text (after its front matter) as a user
    // message, with the goal appended.
    for (id, skill) in (10..).zip(SKILLS) {
        let get = server.request(id, "prompts/get", json!({ "name": skill.name }));
        let message = &result(&get)["messages"][0];
        assert_eq!(message["role"], "user");
        assert_eq!(message["content"]["text"], skill.body(), "{}", skill.name);
    }
    let get = server.request(
        20,
        "prompts/get",
        json!({ "name": "shin-scene-building", "arguments": { "goal": "a red crate" } }),
    );
    let text = result(&get)["messages"][0]["content"]["text"]
        .as_str()
        .unwrap();
    assert!(text.ends_with("## Your goal\n\na red crate\n"), "{text}");
    let unknown = server.request(21, "prompts/get", json!({ "name": "no-such-skill" }));
    assert_eq!(unknown["error"]["code"], json!(-32602), "{unknown}");

    // resources/list: every skill and every guide page.
    let list = server.request(30, "resources/list", json!({}));
    let uris: Vec<&str> = result(&list)["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["uri"].as_str().unwrap())
        .collect();
    for skill in SKILLS {
        assert!(
            uris.contains(&format!("shin://skills/{}", skill.name).as_str()),
            "{uris:?}"
        );
    }
    for page in [
        "introduction",
        "agents-mcp",
        "agents-skills",
        "reference/tools",
    ] {
        assert!(
            uris.contains(&format!("shin://guide/{page}").as_str()),
            "{uris:?}"
        );
    }

    // resources/read: a guide page and a skill, as Markdown.
    let read = server.request(
        31,
        "resources/read",
        json!({ "uri": "shin://guide/agents-mcp" }),
    );
    let contents = &result(&read)["contents"][0];
    assert_eq!(contents["uri"], "shin://guide/agents-mcp");
    assert_eq!(contents["mimeType"], "text/markdown");
    assert!(
        contents["text"]
            .as_str()
            .unwrap()
            .starts_with("# Agents and MCP"),
        "{contents}"
    );
    let read = server.request(
        32,
        "resources/read",
        json!({ "uri": "shin://skills/shin-scripting" }),
    );
    assert_eq!(
        result(&read)["contents"][0]["text"],
        shin::project::skills::get("shin-scripting").unwrap().text
    );
    let missing = server.request(33, "resources/read", json!({ "uri": "shin://guide/nope" }));
    assert!(missing.get("error").is_some(), "{missing}");

    for line in &server.shutdown() {
        parse_json_line(line);
    }
}
