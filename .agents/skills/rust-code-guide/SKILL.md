---
name: rust-code-guide
description: >-
  Enforces the project's Rust quality gates via make format, make lint, and
  make test, plus per-module unit tests, and applies project Rust code
  preferences. Use when writing, editing, or reviewing Rust code, Cargo.toml,
  or when the user mentions Rust, cargo, clippy, rustfmt, unit tests, or code
  style.
---

# Rust Code Guide

Apply these rules when creating or changing Rust sources or `Cargo.toml`.
Skip the quality gates for docs-only or skill-only turns.

## Requirements

- Add unit tests for every logic-bearing module you add or change.
- Split each tested module into `<module>/mod.rs` and `<module>/tests.rs`.
- Do not require unit tests for crate-root `main` or `lib`.
- Treat `tests/` integration tests as optional extras, not substitutes for
  per-module unit tests.

## Preferences

Read and apply [code-preferences.md](references/code-preferences.md) on Rust
you write or are already changing. These are mandatory for the agent. They do
not fail `check.sh`. Do not expand a patch to rewrite unrelated existing code
just to apply a preference.

On review-only turns, flag violations in the changed hunks. Do not open a fix
patch unless asked. Do not request cleanup outside the diff.

## Checks

From the repo root run:

```bash
bash .agents/skills/rust-code-guide/scripts/check.sh
```

The script finds the nearest `Makefile` upward and runs `make format`,
`make lint`, then `make test`. If no `Makefile` exists, bootstrap the project
first; do not skip the checks. Fix failures and re-run until the script exits 0.
Do not claim completion if it fails.
