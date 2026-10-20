# Agent skills

Tools tell an agent *what* it can call. Skills tell it *how* to get a job
done with them: which tools to call in which order, with example
arguments, how to check the result, and the usual mistakes. Shin ships
both, from one source, to any MCP client and to Claude Code.

## The skills

| Skill | Use it to |
|-------|-----------|
| `shin-scene-building` | Turn a description into named entities with a camera and light, check it with a rendered frame, and save it as a scene file. |
| `shin-visual-debugging` | Work out why a frame looks wrong: list entities, check meshes, aim the camera, test the light, isolate, fix, re-render. |
| `shin-scripting` | Write a Rhai script, attach it, play and step the simulation, read script errors, iterate with hot reload, stop. |
| `shin-project` | Create, open, and run a project, learn its layout, serve it with `--mcp`, and add a `game.*` tool. |
| `shin-multiplayer` | Check what is replicated, host a server, join it from a second engine, watch the session, and debug a client that shows something different. |

Each skill is a short Markdown file with a goal, when to use it, the
exact tool sequence with example JSON, a verification step, and
pitfalls.

## Where agents find them

- **Over MCP, as prompts.** `shin-mcp` and every game run with `--mcp`
  list one prompt per skill (`prompts/list`). Getting a prompt
  (`prompts/get`) returns the skill as a user message; the optional
  `goal` argument is appended to it, so `shin-scene-building` with
  `goal: "a red crate on a green floor"` is a ready-made task. In Claude
  Code, MCP prompts appear as slash commands such as
  `/mcp__shin__shin-scene-building`.
- **Over MCP, as resources.** `shin://skills/<name>` returns a skill's
  `SKILL.md`. See [Agents and MCP](agents-mcp.md#prompts-and-resources).
- **In every new project, for Claude Code.** `shin new` writes each skill
  to `.claude/skills/<name>/SKILL.md`, where Claude Code discovers project
  skills, and a `CLAUDE.md` that points to them. See
  [Projects](projects.md#files-for-agents).

## Adding or changing a skill

The source of truth is `crates/shin-engine/skills/<name>/SKILL.md` in
the engine repository: Markdown with front matter. The folder lives
inside the engine crate so it ships with the published `shin-engine`.

```markdown
---
name: shin-example
description: One or two sentences saying what the skill does and when to use it.
---

# Doing the example thing

## Goal
## When to use
## Steps
## Verification
## Pitfalls
```

The `name` must match the folder, and the `description` is what an agent
reads to decide whether to use the skill, so make it say when. Keep a
skill under 150 lines. Then add it to `SKILLS` in
`crates/shin-engine/src/project/skills.rs`; the server and `shin new`
pick it up from there.

Two tests keep the skills honest. Every `` `x.y` `` name in a skill
that looks like a tool (`entity.spawn`, not `main.rs`) must exist in the
engine's tool registry, so a renamed tool fails the build until the
skill is updated; `game.*` names are project tools and are not checked.
And every folder in `crates/shin-engine/skills/` must be embedded, served as a prompt, and
written by `shin new`:

```sh
cargo test -p shin-cli skills
```

When a change alters how a task is done with the tools, update the
skill for it in the same change, as with the guide pages.
