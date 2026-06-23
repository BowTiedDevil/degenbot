# AGENTS.md

## Planning
Use `ergo` for all feature planning. Discover usage with `ergo --help` and `ergo quickstart`. Include detailed implementation and planning notes in the body of each task.

## Refactoring & Feature Development
Use Red/Green TDD while refactoring and implementing new features.

## Commands
Uses `just` (see justfile) and `uv` as the package runner. Key commands:

### Python
- `just test-python` - Run Python tests
- `just test-rust-python` - Run Rust-wrapped Python tests

### Rust
- `just test-rust` - Run Rust tests
- `just lint-rust` - Run Rust linter (clippy)

**Important**: The Rust extension is automatically rebuilt on import by maturin. Do NOT manually rebuild the extension, recreate the virtual environment, or reinstall the package after making Rust code changes. Any `uv run ...` command will trigger a rebuild if needed.

### Combined
- `just test-all` - Run all tests (Rust + Python)
- `just lint` - Run lint and type checks (Rust + Python)
- `just format` - Run formatters (Rust + Python)

## Git Commits
Commit messages must follow the project convention enforced by `commitlint`. Run `just setup-git-hooks` once after cloning to enable the local hooks (`commit-msg` at commit time, `pre-push` that re-lints the push range as a safety net for `--no-verify` bypasses) and the editor template. For manual range checks: `just lint-commits` (default: unpushed commits) or `just lint-commits main..HEAD`.

## Backwards Compatibility
- Unless directed otherwise, design standalone features without a backwards compatibility layer.

## Architecture & Domain Knowledge
The codebase uses layered documentation. This file holds operational guidance only — read the relevant focused doc before naming, editing, or extending a module.
- **[`CONTEXT-MAP.md`](CONTEXT-MAP.md)** — ubiquitous-language index + per-module `CONTEXT.md` pointers. Read the relevant module context before naming variables, classes, or docstrings.
- **[ADR records](docs/adr/)** — ADR-001 I/O-free pools, ADR-002 pool-type registry singleton, ADR-003 Bot as state owner, ADR-004 CL tickmap typed boundary, ADR-005 Polars-inspired three-layer FFI, ADR-006 per-chain bot orchestrator
- **[`docs/architecture/`](docs/architecture/)** — long-form architecture
- **[`docs/migration-guides/`](docs/migration-guides/)** — completed refactors. Of note: [`three-layer-transition.md`](docs/migration-guides/three-layer-transition.md) — the rubric for evaluating a Python module against the ADR-005 three-layer architecture, moving responsibility to Rust, and transitioning tests.
