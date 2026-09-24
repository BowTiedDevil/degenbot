# degenbot-db

SQLite persistence substrate for the degenbot Rust core — pyo3-free read handle with an Alembic-aware schema gate.

The SQLite persistence substrate for the Rust core: a pyo3-free read handle plus the schema gate that understands the Alembic-owned transition, so Rust owns durability end to end.

## Usage

```toml
degenbot-db = "0.6.0-alpha.5"
```

Or: `cargo add degenbot-db` (the pre-release version must be pinned explicitly, e.g. "0.6.0-alpha.5").

Part of [degenbot](https://github.com/BowTiedDevil/degenbot) — a Rust-first MEV bot for EVM chains. The in-repo root README and `docs/` cover the full architecture; this crate is published standalone so you can depend on exactly the pieces you need.
