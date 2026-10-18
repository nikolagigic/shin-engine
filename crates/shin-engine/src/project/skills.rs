//! Agent skills (GE-039): how-to guides for AI agents driving Shin through
//! its tools, one per task.
//!
//! The source of truth is `skills/<name>/SKILL.md` in the engine
//! repository: Markdown with YAML front matter giving `name` and
//! `description`. They are embedded here so that [`create`](super::create)
//! writes them into every new project as `.claude/skills/<name>/SKILL.md`
//! and `shin-mcp` serves them as MCP prompts and `shin://skills/<name>`
//! resources.
//!
//! ```
//! let skill = shin::project::skills::get("shin-scene-building").unwrap();
//! assert!(skill.description().contains("scene"));
//! assert!(skill.body().starts_with("# "));
//! ```

/// One skill: its name and the full text of its `SKILL.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Skill {
    /// The skill's name, which is also its folder name in `skills/`.
    pub name: &'static str,
    /// The whole `SKILL.md`, front matter included.
    pub text: &'static str,
}

impl Skill {
    /// The `description` from the front matter: what the skill does and
    /// when to use it. Empty if the front matter has none.
    pub fn description(&self) -> &'static str {
        self.front_matter()
            .lines()
            .find_map(|line| line.strip_prefix("description:"))
            .map(str::trim)
            .unwrap_or_default()
    }

    /// The Markdown after the front matter, without leading blank lines.
    pub fn body(&self) -> &'static str {
        split_front_matter(self.text).1.trim_start()
    }

    /// The lines between the opening and closing `---`.
    fn front_matter(&self) -> &'static str {
        split_front_matter(self.text).0
    }
}

/// Splits `text` into its front matter (without the `---` lines) and the
/// rest. Text without front matter has an empty one.
fn split_front_matter(text: &str) -> (&str, &str) {
    text.strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .unwrap_or(("", text))
}

/// Embeds `skills/<name>/SKILL.md` from the repository root.
macro_rules! skill {
    ($name:literal) => {
        Skill {
            name: $name,
            text: include_str!(concat!("../../../../skills/", $name, "/SKILL.md")),
        }
    };
}

/// Every skill, in the order they are listed and written.
pub const SKILLS: [Skill; 4] = [
    skill!("shin-scene-building"),
    skill!("shin-visual-debugging"),
    skill!("shin-scripting"),
    skill!("shin-project"),
];

/// The skill called `name`, if there is one.
pub fn get(name: &str) -> Option<&'static Skill> {
    SKILLS.iter().find(|skill| skill.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn front_matter_names_match_folders_and_describe_the_skill() {
        for skill in SKILLS {
            let name = skill
                .front_matter()
                .lines()
                .find_map(|line| line.strip_prefix("name:"))
                .map(str::trim);
            assert_eq!(name, Some(skill.name), "{}", skill.name);
            assert!(
                skill.description().len() > 40,
                "{} has no real description",
                skill.name
            );
            assert!(skill.body().starts_with("# "), "{}", skill.name);
        }
    }

    #[test]
    fn skills_stay_short() {
        for skill in SKILLS {
            let lines = skill.text.lines().count();
            assert!(lines < 150, "{} has {lines} lines", skill.name);
        }
    }

    #[test]
    fn text_without_front_matter_is_all_body() {
        assert_eq!(split_front_matter("# Hi\n"), ("", "# Hi\n"));
        assert_eq!(
            split_front_matter("---\nname: a\n---\n# A\n"),
            ("name: a", "# A\n")
        );
    }
}
