//! What the server offers besides tools (GE-039): every agent skill as an
//! MCP prompt, and the skills and user-guide pages as MCP resources.
//!
//! - Prompt `<skill>`, with an optional `goal` argument: the skill's
//!   Markdown, followed by the goal when one is given.
//! - Resource `shin://skills/<skill>`: the skill's whole `SKILL.md`.
//! - Resource `shin://guide/<page>`: a page of the user guide, where
//!   `<page>` is its path under `guide/src/` without `.md`, e.g.
//!   `shin://guide/agents-mcp` or `shin://guide/reference/tools`.
//!
//! Guide pages are the mdBook sources, so `{{#include}}` lines appear as
//! written rather than expanded.

use rmcp::ErrorData;
use rmcp::model::{
    GetPromptResult, JsonObject, Prompt, PromptArgument, PromptMessage, ReadResourceResult,
    Resource, ResourceContents, Role,
};
use shin::project::skills::{self, SKILLS, Skill};

/// URI prefix of the skill resources.
const SKILL_URI: &str = "shin://skills/";
/// URI prefix of the guide-page resources.
const GUIDE_URI: &str = "shin://guide/";
/// MIME type of every resource.
const MARKDOWN: &str = "text/markdown";

/// Embeds `guide/src/<page>.md` from the repository root.
macro_rules! page {
    ($page:literal) => {
        (
            $page,
            include_str!(concat!("../../../guide/src/", $page, ".md")),
        )
    };
}

/// Every page of the user guide, `(page, Markdown)`, in book order.
pub(crate) const GUIDE_PAGES: [(&str, &str); 12] = [
    page!("SUMMARY"),
    page!("introduction"),
    page!("getting-started"),
    page!("projects"),
    page!("editor"),
    page!("scenes"),
    page!("scripting"),
    page!("agents-mcp"),
    page!("agents-skills"),
    page!("reference/tools"),
    page!("architecture"),
    page!("contributing"),
];

/// One prompt per skill.
pub(crate) fn prompts() -> Vec<Prompt> {
    SKILLS
        .iter()
        .map(|skill| {
            let goal = PromptArgument::new("goal")
                .with_description("What you want done, e.g. \"a red crate on a green floor\".")
                .with_required(false);
            Prompt::new(skill.name, Some(skill.description()), Some(vec![goal]))
        })
        .collect()
}

/// The prompt called `name`: the skill's Markdown as one user message,
/// with `## Goal` and the `goal` argument appended when it is given.
pub(crate) fn get_prompt(
    name: &str,
    arguments: Option<&JsonObject>,
) -> Result<GetPromptResult, ErrorData> {
    let skill = skills::get(name)
        .ok_or_else(|| ErrorData::invalid_params(format!("no prompt named `{name}`"), None))?;
    let mut text = skill.body().to_string();
    match arguments.and_then(|args| args.get("goal")) {
        None => {}
        Some(serde_json::Value::String(goal)) => {
            text.push_str("\n## Your goal\n\n");
            text.push_str(goal);
            text.push('\n');
        }
        Some(other) => {
            return Err(ErrorData::invalid_params(
                format!("prompt argument `goal` must be a string, got {other}"),
                None,
            ));
        }
    }
    Ok(
        GetPromptResult::new(vec![PromptMessage::new_text(Role::User, text)])
            .with_description(skill.description()),
    )
}

/// Every skill, then every guide page.
pub(crate) fn resources() -> Vec<Resource> {
    let skills = SKILLS.iter().map(|skill| {
        Resource::new(skill_uri(skill), skill.name)
            .with_description(skill.description())
            .with_mime_type(MARKDOWN)
    });
    let pages = GUIDE_PAGES.iter().map(|(page, _)| {
        Resource::new(format!("{GUIDE_URI}{page}"), format!("guide/{page}"))
            .with_description(format!("The Shin user guide: {page}.md"))
            .with_mime_type(MARKDOWN)
    });
    skills.chain(pages).collect()
}

/// The resource at `uri`, or a resource-not-found error naming it.
pub(crate) fn read_resource(uri: &str) -> Result<ReadResourceResult, ErrorData> {
    let text = if let Some(name) = uri.strip_prefix(SKILL_URI) {
        skills::get(name).map(|skill| skill.text)
    } else if let Some(page) = uri.strip_prefix(GUIDE_URI) {
        GUIDE_PAGES
            .iter()
            .find(|(name, _)| *name == page)
            .map(|(_, text)| *text)
    } else {
        None
    };
    let text =
        text.ok_or_else(|| ErrorData::resource_not_found(format!("no resource `{uri}`"), None))?;
    Ok(ReadResourceResult::new(vec![
        ResourceContents::text(text, uri).with_mime_type(MARKDOWN),
    ]))
}

/// `shin://skills/<name>`.
fn skill_uri(skill: &Skill) -> String {
    format!("{SKILL_URI}{}", skill.name)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    /// Every `.md` file under `dir`, as a page name relative to `root`.
    fn md_pages(root: &Path, dir: &Path, pages: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                md_pages(root, &path, pages);
            } else if path.extension().is_some_and(|ext| ext == "md") {
                let page = path.strip_prefix(root).unwrap().with_extension("");
                pages.push(page.to_str().unwrap().replace('\\', "/"));
            }
        }
    }

    #[test]
    fn every_guide_page_is_a_resource() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../guide/src");
        let mut on_disk = Vec::new();
        md_pages(&root, &root, &mut on_disk);
        on_disk.sort();
        let mut served: Vec<String> = GUIDE_PAGES.iter().map(|(p, _)| p.to_string()).collect();
        served.sort();
        assert_eq!(served, on_disk, "add new guide pages to GUIDE_PAGES");
    }

    #[test]
    fn prompts_take_an_optional_goal() {
        let result = get_prompt(
            "shin-scripting",
            Some(
                &serde_json::json!({ "goal": "make the moon spin" })
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .unwrap();
        let rmcp::model::ContentBlock::Text(text) = &result.messages[0].content else {
            panic!("expected text");
        };
        assert!(text.text.starts_with("# Scripting"), "{}", text.text);
        assert!(text.text.ends_with("## Your goal\n\nmake the moon spin\n"));

        let bad = serde_json::json!({ "goal": 3 })
            .as_object()
            .unwrap()
            .clone();
        assert!(get_prompt("shin-scripting", Some(&bad)).is_err());
        assert!(
            get_prompt("nope", None)
                .unwrap_err()
                .message
                .contains("nope")
        );
    }

    #[test]
    fn unknown_resources_are_not_found() {
        for uri in [
            "shin://skills/nope",
            "shin://guide/nope",
            "file:///etc/passwd",
        ] {
            let err = read_resource(uri).unwrap_err();
            assert_eq!(
                err.code,
                rmcp::model::ErrorCode::RESOURCE_NOT_FOUND,
                "{uri}"
            );
            assert!(err.message.contains(uri), "{}", err.message);
        }
    }
}
