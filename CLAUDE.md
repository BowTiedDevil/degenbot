# CLAUDE.md

This file is the Claude entrypoint for this vendored degenbot checkout. `AGENTS.md` remains the
source of truth for repository conventions; `UBIQUITOUS_LANGUAGE.md` pins domain terminology (with
per-module sub-glossaries under `src/degenbot/*/UBIQUITOUS_LANGUAGE.md`). Read both first, then
apply the project-specific guidance below.

## Project Role

This repository is the MEV-Arbitrum home for Python market intelligence and Rust latency helpers:

- Python protocol adapters, market analysis, source provenance, signal emitters, and candidate
  scoring live under `src/degenbot`.
- Rust deterministic hot-path helpers live under `rust/src` and are exposed through the maturin
  `degenbot_rs` binding.
- Stylus (Rust→WASM, Arbitrum onchain) ports live under `stylus/` (`core` plus the `*_adapter`
  crates); track migration state in `stylus/PORTING_STATUS.md`. This is a distinct lane from the
  `rust/` PyO3 helpers — do not conflate the two.
- `rust/crates/contract_bindings` is GENERATED from the parent `../../contracts` Foundry artifacts
  via `just gen-contract-bindings`. Never hand-edit it; run `just check-contract-bindings` after
  parent contract changes to catch drift.
- Parent-repo TypeScript orchestration stays in `coordinator/`.
- Parent-repo Solidity settlement and enforcement stays in `contracts/`.

See `docs/architecture/mev-arbitrum-code-home.md` before moving logic across those boundaries.

## Operating Rules

- NEVER manually rebuild, recreate, or reinstall the `rust/` PyO3 extension after Rust edits, and do
  not recreate the venv — running a Python or Rust test triggers the maturin rebuild on import. (This
  rule is `rust/`-only; the `stylus/` lane has its own explicit build recipes — see below.)
- Preserve unrelated local changes. Check `git status --short` before editing.
- Use red/green TDD for new behavior and focused regression tests for bug fixes. Build a `Fake`-
  prefixed test double instead of mocking.
- Prefer frozen dataclasses for value objects and `TypedDict` for known structured dictionaries.
- Keep arithmetic deterministic and integer-exact. Do not add tolerances to Aave or accounting
  verification paths. When porting Solidity, use Python `//` to match the EVM's `/`.
- Use repo-local errors derived from `DegenbotError` (`src/degenbot/exceptions/base.py`); catch
  specific subtypes, not broad `Exception`.
- Use `from degenbot.logging import logger` for logging.
- For DB access use the `with db_session() as session:` context manager
  (`from degenbot.database import db_session`); ORM models live in `src/degenbot/database/models/`.

## Verification Commands

Whole-suite (via `just`):

- Python: `just test-python` (runs `compile-test-contracts` via Forge first)
- Rust (PyO3): `just test-rust` · Stylus: `just test-stylus`
- Rust lint: `just lint-rust` · Combined tests: `just test-all` · Full lint: `just lint` · Format:
  `just format`

Single test / single file — `just test-python` runs the ENTIRE suite plus a contract recompile, so
for narrow work use the native runners directly, from the repo root (the env prefixes are not
optional):

- Python: `env -u RUST_LOG uv run pytest tests/<path>::<test> -x -q` (run `just
  compile-test-contracts` once if the test needs compiled fixtures)
- Rust (PyO3): `env PYO3_PYTHON="$PWD/.venv/bin/python" cargo test --features auto-initialize
  --manifest-path rust/Cargo.toml -- --test-threads=1 <substr>` (single-thread is required)
- Stylus: `cargo test --manifest-path stylus/Cargo.toml --locked --offline --lib --features
  native-test <substr>`

Run the smallest relevant test first, then a broader command when risk warrants it.

## Claude Workflow Layer

Adapted Claude agents and commands live under `.claude/`. They are intentionally scoped to degenbot:

- `.claude/agents/` contains read-only codebase and EVM investigation roles.
- `.claude/commands/` contains research, planning, implementation, validation, debug, worktree, and
  commit workflows.
- `scripts/claude_spec_metadata.sh` gathers deterministic metadata for research and plan documents.
- `scripts/defillama_reference_checkout.sh` creates sparse DefiLlama reference checkouts for
  protocol-source intelligence.

Do not copy generic boilerplate instructions into this repository without adapting them to the
Python/Rust/Aave/MEV architecture above.
