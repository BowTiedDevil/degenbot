# degenbot-simulation

In-process revm simulation executor + dispatch fan-out (pyo3-free core; ADR-019 D4 fold).

The in-process revm simulation executor with dispatch fan-out (the ADR-019 D4 fold), and the home of the tier-3 on-chain parity oracles that byte-verify the engine against real pool bytecode.

## Usage

```toml
degenbot-simulation = "0.6.0-alpha.5"
```

Or: `cargo add degenbot-simulation` (the pre-release version must be pinned explicitly, e.g. "0.6.0-alpha.5").

Part of [degenbot](https://github.com/BowTiedDevil/degenbot) — a Rust-first MEV bot for EVM chains. The in-repo root README and `docs/` cover the full architecture; this crate is published standalone so you can depend on exactly the pieces you need.
