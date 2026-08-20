# degenbot-core

Foundational utilities (errors, hex, addresses, runtime) for the degenbot Rust crates.

The foundational leaf the rest of the core builds on: shared error types, hex and address helpers, and runtime utilities.

## Usage

```toml
degenbot-core = "0.6.0-alpha.5"
```

Or: `cargo add degenbot-core` (the pre-release version must be pinned explicitly, e.g. "0.6.0-alpha.5").

Part of [degenbot](https://github.com/BowTiedDevil/degenbot) — a Rust-first MEV bot for EVM chains. The in-repo root README and `docs/` cover the full architecture; this crate is published standalone so you can depend on exactly the pieces you need.
