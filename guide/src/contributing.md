# Contributing

Shin is developed in the open at
<https://github.com/nikolagigic/shin-engine>, dual-licensed under MIT and
Apache 2.0. Contributions are accepted under the same terms. The
repository's `CLAUDE.md` is the engineering handbook: the conventions,
the module map, and the definition of done, in full. This page is the
short version.

## How work is organised

Work is organised as **tickets**, each with an ID (`GE-NNN`), a T-shirt
size, acceptance criteria, and a test command that proves them. Each
branch implements exactly one ticket, in its own worktree, and is
reviewed and merged by the tech lead. Tickets map to modules, so parallel
branches rarely touch the same file.

Some conventions you will meet in the code:

- Edition 2024, Rust 1.99, pinned in `rust-toolchain.toml`.
- `cargo fmt` with default settings; `cargo clippy` with warnings denied.
- `anyhow::Result` at API boundaries and in examples and tests;
  `thiserror` enums inside a module when callers need to match on them.
- No new dependencies unless a ticket lists them; versions are pinned in
  the workspace `Cargo.toml`.
- No `unsafe` without a `// SAFETY:` comment and a ticket that asks for
  it.
- Every public item has a doc comment. Readability beats cleverness, and
  small public APIs beat flexible ones.
- Every user-facing capability registers a tool and is tested through
  the registry; every rendering change updates or adds an example and is
  proven by a golden image.

## Running the suites

From the repository root:

```sh
cargo fmt --check        # formatting
cargo check-all          # clippy on every target, warnings denied
cargo test-all           # unit, integration, and golden-image tests
cargo doc --no-deps      # rustdoc, must be warning-free
```

`check-all` and `test-all` are Cargo aliases defined in
`.cargo/config.toml`; CI runs exactly these.

Golden-image tests need a GPU or a software driver. When one fails, look
at `crates/shin-engine/tests/golden/_diff/`; if the new output is the
intended one, accept it with:

```sh
UPDATE_GOLDEN=1 cargo test -p shin-engine
```

Do not share a `CARGO_TARGET_DIR` between checkouts or worktrees: Cargo
judges freshness by file timestamps, and a shared target directory can
hand one checkout a stale library.

## The user guide

This guide is an [mdBook](https://rust-lang.github.io/mdBook/) in
`guide/` (source in `guide/src`, output in `guide/book`, which is
ignored). Build and preview it with:

```sh
mdbook build guide
mdbook serve guide
```

The [Tools reference](reference/tools.md) is generated from the tool
registry and checked by a test. Whenever a tool or its arguments change,
regenerate it and commit the result:

```sh
cargo run -p shin-cli -- docs tools > guide/src/reference/tools.md
cargo test -p shin-cli docs
```

`shin-mcp` serves the guide's pages to agents, so it embeds a copy of
`guide/src/*.md` in `crates/shin-mcp/guide/` (a crate can only ship
files inside its own folder). `guide/src/` is the source of truth; after
changing any page, refresh the copy and commit both:

```sh
UPDATE_GUIDE=1 cargo test -p shin-mcp guide
```

A test fails while the two differ. Agent skills have no copy: their only
source is `crates/shin-engine/skills/`.

CI builds the guide, runs that test, and publishes the book to GitHub
Pages on every push to `main`.

## Releasing

All four crates (`shin-engine`, `shin-mcp`, `shin-cli`, `shin-editor`)
share the workspace version and are released together. Shin is
pre-alpha, so a 0.x minor release may break any API.

1. On a branch: set `version` in `[workspace.package]` and the
   `version` of the `shin-engine` and `shin-mcp` entries in
   `[workspace.dependencies]` of the root `Cargo.toml`, then move the
   `[Unreleased]` notes in `CHANGELOG.md` into a section for the new
   version, dated. Merge it to `main`.
2. Tag the merge and push the tag:

   ```sh
   git tag v0.2.0
   git push origin v0.2.0
   ```

The `Release` workflow (`.github/workflows/release.yml`) then runs the
full CI suite, builds `shin` and `shin-editor` for macOS (arm64) and
Linux (x86_64), checks that the tag matches the workspace version,
publishes the crates to crates.io in dependency order, and creates a
GitHub Release with that version's changelog section as its notes and the
binaries attached. Publishing uses the repository secret
`CARGO_REGISTRY_TOKEN`.

CI also runs `cargo publish --dry-run` for every crate on every push, so
packaging problems show up long before a release. To run it locally:

```sh
cargo publish --dry-run -p shin-engine -p shin-mcp -p shin-cli -p shin-editor
```

Cargo packages the crates together and resolves them against each other,
so this works before a version is on crates.io. A crate ships only the
files its `include` list names: sources, plus whatever `include_str!` or
`include_bytes!` needs; tests, goldens, and examples stay in the
repository.

## Definition of done

Every ticket, no exceptions:

- Every acceptance criterion in the ticket is met and demonstrated.
- `cargo test-all`, `cargo check-all`, `cargo fmt --check`, and
  `cargo doc --no-deps` pass.
- New public items have a doc comment.
- A user-facing capability registers a tool and tests it through
  `ToolRegistry::call`.
- A user-facing change updates or adds the relevant page of this guide
  in the same branch, and regenerates the tools reference if any tool
  changed.
- The review request maps each acceptance criterion to its evidence.

Commits are plain descriptions of the change.
