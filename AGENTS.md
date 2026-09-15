## Architecture

`degenbot` has migrated from a pure-Python library to a Rust core composed of standalone crates. The end state has two equally first-class consumers:

1. **Pure-Rust MEV bot.** Someone should be able to `cargo add degenbot` (the umbrella crate re-exporting the cores) and build a fully functional MEV bot using Rust components ONLY without involving Python. That core must own **everything** a functional MEV bot needs. The Rust core must be capable of performing every action the bot requires. **Rust is the engine; Python is a driver shell, not a co-implementation.**
2. **Python-driven MEV bot.** Someone in Python should be able to build a functional MEV bot using the Python interface as a **driver** over the same Rust core, via a thin PyO3 layer that translates Python calls into Rust calls.

## Concurrent Work Coordination
Check for other agents working concurrently before you begin work and any time you notice any edits, files, or changes in the working tree that are unrelated to your work. Use `/skill:pi-intercom` for instructions on using the inter-agent communication system. If another agent sends you a message, use `/skill:pi-intercom` to learn how to respond.

## Backwards Compatibility
Design standalone features without a backwards compatibility layer. Implement add a feature flag to allow parallel implementations if necessary, followed by a hard cutover.

## Planning
Use `ergo` for all planning. Discover usage with `ergo --help` and `ergo quickstart`. Include detailed implementation and planning notes in the body of each task.

## Refactoring & Feature Development
Use red/green test-driven development when refactoring and adding new features. Use `/skill:tdd` for guidelines.

## Complex System State
Prefer enum-based finite state machines to manage transitions within systems. When you encounter an existing system with ad-hoc rules and detailed comments meant to clarify complex interactions, propose a refactor to encapsulate that logic into a state machine.

## Comment Hygiene
Comments carry the *why* only if it outlives its lookup: no task/epic IDs (commits carry those), no "RED/merged/post-fix" narration, no refactor provenance. Sequencing rules belong in types, acceptance criteria in named tests, history in ADRs. Full rules and the first-home test: `docs/comments.md`.

## Commands
See the justfile.

## Rebuilding the Rust `.so` after edits
`uv run maturin develop` and even `cargo clean -p <crate>` do **not** reliably force a from-source recompile of the PyO3 `.so` — maturin uses cached artifacts across different feature-flag hash variants and `uv sync` installs a pre-built wheel in milliseconds. An apparently successful rebuild (~0.3–6s compile, no errors) silently ships a **stale `.so`** that doesn't contain the changes. This has bitten multiple sessions.

The only reliable way to force the `.so` to pick up Rust source changes:

```bash
uv sync --reinstall-package degenbot
```

Workflow after any Rust edit — verify, don't guess:

1. `just verify-build-fresh`. Exit 0 ⇒ the installed extension already
   contains your edits; no rebuild needed.
2. Exit 1 ⇒ run the reinstall above, then verify again. Only trust a bot run
   (or a pytest suite) once the check exits 0.

### Verifying freshness with the build receipt

Do not trust a silent "successful" rebuild — verify it. Every compile of
`degenbot_rs` runs `rust/crates/degenbot-python/build.rs`, which fingerprints
the crate's sources and writes `<count> <fingerprint>` to a receipt file
(`.build-number`, gitignored, at the repo root), embedding both values in the
compiled library. The counter advances only when the fingerprint (source
content) changes, so test/feature-variant rebuilds never false-positive.

```bash
uv run --no-sync python -m degenbot.build_info   # exit 1 if stale
# or:
just verify-build-fresh
# or from Python:
from degenbot.build_info import verify_build_fresh; verify_build_fresh()
# raw values: degenbot._ffi.build_number() / degenbot._ffi.build_fingerprint()
```

The check compares the installed fingerprint against the repo receipt, so any
material built from different sources than the installed artifact (the
cached-wheel failure mode) is caught, and no-change recompiles stay fresh.
`pytest tests/test_build_info.py` gates on this too — a stale `.so` fails the
suite. The receipt lives outside `rust/target` so `cargo clean` and
`just gc-target` can never roll it back. After Rust edits expect the gate to
flag staleness until you rebuild the wheel (`uv sync --reinstall-package
degenbot`) — that is the detector working, so run the rebuild, not a skip. A
reported number of 0 (or a missing fingerprint) means `build.rs` did not run —
investigate before trusting the build.

## Python Environment
Use `uv`.