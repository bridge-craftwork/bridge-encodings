# CLAUDE.md

This file provides guidance to Claude Code when working with this repository.

## Project Overview

<!-- TODO: Add project description -->

## Build & Test Commands

**Use `./dev-build.sh` for local development builds, not bare cargo.** This repo depends on sibling bridge crates as git dependencies, with gitignored `[patch]` overrides in `.cargo/config.toml` redirecting them to the local checkouts in `../`. Cargo never lets a `[patch]` override an existing `Cargo.lock` pin, so bare `cargo build` silently compiles the GitHub revisions of those crates instead of your local edits — and if the patches do take effect, they rewrite `Cargo.lock` with local-path entries that must never be committed (CI has no sibling checkouts). The script keeps a separate local lock (`.cargo/dev.lock`), swaps it in around the cargo call, verifies each patched crate resolved to a local checkout, and leaves the committed `Cargo.lock` untouched.

```bash
./dev-build.sh                    # cargo build, against local sibling checkouts
./dev-build.sh build --release    # any cargo subcommand + args pass through
./dev-build.sh test               # cargo test
./dev-build.sh clippy -- -D warnings   # lint
cargo fmt --check                 # no dependency resolution; bare cargo is fine
```

Bare cargo is correct only when you *want* the committed lock's git pins — i.e. reproducing exactly what CI builds (pre-commit checks, release verification). The committed `Cargo.lock` must always pin `git+https://` sources for the internal crates; never commit a lock where those entries have lost their `source =` lines.

## Pre-commit Requirements

Before committing, always run and fix:
1. `cargo fmt --all` - Format all code
2. `cargo clippy --all-targets -- -D warnings` - Fix all clippy warnings
3. `cargo test` - Ensure all tests pass

## Code Standards

- No `unwrap()` or `expect()` outside test code - use proper error handling
- No `println!()` in library code (CLI binaries are OK)
- All public functions must have doc comments (`///`)
- All `unsafe` blocks must have a comment explaining why they're safe
- Prefer editing existing files over creating new ones

## Git Configuration

Use SSH for all GitHub operations:
- Clone/push/pull: `git@github.com:bridge-craftwork/repo.git` (not `https://`)
- Remote URLs should use SSH format

## Related Projects

All located at `/Users/rick/Development/GitHub/`:

| Project | Description | Relationship |
|---------|-------------|--------------|
| [bridge-types](../bridge-types) | Core bridge types | sibling |
| [Bridge-Parsers](../Bridge-Parsers) | PBN/LIN file parsing | sibling |
| [bridge-wrangler](../bridge-wrangler) | CLI tool | sibling |

## Notifications

Send Pushover notifications when work is blocked or completed:

```bash
pushover "message" "title"    # title defaults to "Claude Code"
```

**When to notify:**
- Waiting for user input or permission
- Task completed after extended work
- Build/test failures that need attention
- Any situation where work is paused and user may not notice
