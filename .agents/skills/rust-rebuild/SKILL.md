---
name: rust-rebuild
description: How degenbot's installed Python extension (.so) is kept in sync with the Rust sources, and how to diagnose a stale build. Use after a Rust edit, when a Python run behaves as if recent Rust edits are absent, or before trusting any maturin/uv rebuild.
---

# Rust Rebuild Mechanics

Maturin and Cargo can serve cached artifacts, so an apparently successful
rebuild can still ship a **stale `.so`**. There is no build receipt or runtime
staleness gate; freshness comes from making the rebuild a mandatory, incremental
step of the entrypoints.

## The workflow

After any Rust edit, run `just dev` (incremental `maturin develop`) before
trusting a bot run or a pytest suite. `just test-python` / `just test` already
depend on it, so the Python suite never tests a stale `.so`. A plain `uv sync`
rebuilds it too - with the `dev-features` set - so it cannot clobber the
extension with a default-features wheel.

Only the recipes carry the full contract (`--locked`, the canonical feature
aliases, `cfg(tokio_unstable)` via `.cargo/config.toml`). A hand-typed
`cargo build` or `maturin develop` can serve the wrong feature set.

## Why there is no receipt

The old build-receipt subsystem (`.build-number`, `just verify-build-fresh`,
`degenbot.build_info`) was removed: it added a second source of truth about build
identity, and the editable install already has exactly one - the `.so` itself,
rebuilt on run. See `AGENTS.md` -> "Building the Rust extension".

## Failure-mode notes

- A stale `.so` after editing Rust is expected until you rebuild; run `just dev`,
  do not skip.
- `cargo clean` / `just gc-target` can roll back build artifacts, but the source
  tree is the only input - rebuild and the extension is correct again.
