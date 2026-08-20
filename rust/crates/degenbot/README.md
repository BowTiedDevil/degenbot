# degenbot

Umbrella Rust crate: re-exports the pyo3-free degenbot core surface (Bot state, DexIdentity presets, calc math) for standalone Rust consumers.

The umbrella for the standalone Rust core: one `cargo add degenbot` gives a pure-Rust consumer the pyo3-free surface end to end (bot state, DEX identity presets, calc math, solvers, simulation) for building a complete MEV bot with no Python in the build or the runtime.

## Usage

```toml
degenbot = "0.6.0-alpha.5"
```

Or: `cargo add degenbot` (the pre-release version must be pinned explicitly, e.g. "0.6.0-alpha.5").

## Quickstart

The in-package example `examples/standalone_consumer.rs` (in-repo: `rust/crates/degenbot/examples/standalone_consumer.rs`) is a complete pure-Rust consumer of this crate — start there to see the full surface in one file.

Part of [degenbot](https://github.com/BowTiedDevil/degenbot) — a Rust-first MEV bot for EVM chains. The in-repo root README and `docs/` cover the full architecture; this crate is published standalone so you can depend on exactly the pieces you need.
