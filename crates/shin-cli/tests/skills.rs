//! GE-039: the agent skills in the repository's `skills/` folder stay in
//! sync with the engine. Every tool a skill names exists, and every skill
//! folder is embedded (so `shin-mcp` serves it as a prompt) and written by
//! `shin new`.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use shin::project::skills::SKILLS;
use shin::tool::ToolRegistry;

/// The repository's `skills/` folder.
fn skills_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../skills")
}

/// Extensions that make an `x.y` token a file name rather than a tool.
const FILE_EXTENSIONS: [&str; 13] = [
    "rs", "toml", "ron", "json", "md", "gltf", "glb", "bin", "png", "jpg", "jpeg", "rhai", "lock",
];

/// Namespace of a project's own tools, which the engine does not register.
const PROJECT_NAMESPACE: &str = "game";

/// `name` has the shape of a tool name: `<subsystem>.<verb>`, lowercase
/// ASCII letters, digits, and `_`, each part starting with a letter.
fn looks_like_tool(name: &str) -> bool {
    let part = |s: &str| {
        s.starts_with(|c: char| c.is_ascii_lowercase())
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    match name.split_once('.') {
        Some((subsystem, verb)) => {
            part(subsystem) && part(verb) && !FILE_EXTENSIONS.contains(&verb)
        }
        None => false,
    }
}

/// Every `` `x.y` `` inline-code token in `markdown` that looks like a tool
/// name, outside the project's own `game.*` namespace. Fenced code blocks
/// are skipped (they hold Rust and Rhai, not tool references).
fn tool_names(markdown: &str) -> BTreeSet<&str> {
    let mut names = BTreeSet::new();
    let mut in_fence = false;
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        for code in line.split('`').skip(1).step_by(2) {
            if looks_like_tool(code) && !code.starts_with(&format!("{PROJECT_NAMESPACE}.")) {
                names.insert(code);
            }
        }
    }
    names
}

#[test]
fn skills_name_only_registered_tools() {
    let registered: BTreeSet<String> = ToolRegistry::with_engine_tools()
        .list()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    let mut checked = 0;
    let mut missing = Vec::new();
    for skill in SKILLS {
        let path = skills_dir().join(skill.name).join("SKILL.md");
        let text = fs::read_to_string(&path).unwrap();
        for name in tool_names(&text) {
            checked += 1;
            if !registered.contains(name) {
                missing.push(format!("{}: `{name}`", path.display()));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "skills name tools that do not exist:\n{}",
        missing.join("\n")
    );
    assert!(checked > 40, "only {checked} tool references found");
}

#[test]
fn skills_tool_name_extraction() {
    let text = "Call `entity.spawn`, edit `main.rs` and `shin.toml`, try `game.hello`.\n\
                ```rust\nlet x = `not.checked`;\n```\n`Cargo.toml` `a.b.c` `render.frame`";
    let names: Vec<_> = tool_names(text).into_iter().collect();
    assert_eq!(names, ["entity.spawn", "render.frame"]);
}

#[test]
fn every_skills_folder_is_embedded_and_scaffolded() -> anyhow::Result<()> {
    let on_disk: BTreeSet<String> = fs::read_dir(skills_dir())?
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    let embedded: BTreeSet<String> = SKILLS.iter().map(|s| s.name.to_string()).collect();
    assert_eq!(
        on_disk, embedded,
        "add new skill folders to SKILLS in shin-engine/src/project/skills.rs"
    );

    let parent = std::env::temp_dir().join(format!("shin-cli-test-skills-{}", std::process::id()));
    let _ = fs::remove_dir_all(&parent);
    let out = Command::new(env!("CARGO_BIN_EXE_shin"))
        .args(["new", "skilled", "--dir", parent.to_str().unwrap()])
        .output()?;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listed = String::from_utf8(out.stdout)?;
    let project = parent.join("skilled");
    for skill in SKILLS {
        let written = project
            .join(".claude/skills")
            .join(skill.name)
            .join("SKILL.md");
        assert!(
            listed.contains(&written.display().to_string()),
            "`shin new` did not list {}:\n{listed}",
            written.display()
        );
        let source = fs::read_to_string(skills_dir().join(skill.name).join("SKILL.md"))?;
        assert_eq!(fs::read_to_string(&written)?, source, "{}", skill.name);
    }
    let claude = fs::read_to_string(project.join("CLAUDE.md"))?;
    assert!(claude.starts_with("# skilled\n"), "{claude}");
    assert!(listed.contains("CLAUDE.md"), "{listed}");
    let _ = fs::remove_dir_all(&parent);
    Ok(())
}
