# degenbot-arbitrage

The settlement-arbitrage searcher strategy — the 7-call pre/post-balance bundle, gross/net + priority-fee sizing, dispatch fan-out + categorization policy. One example strategy over the degenbot-simulation engine (ADR-019 D4/D7; pyo3-free Rust core).

The settlement-arbitrage searcher strategy as a concrete example over the simulation engine: the 7-call pre/post-balance bundle, gross/net and priority-fee sizing, and the dispatch fan-out with categorization policy.

## Usage

```toml
degenbot-arbitrage = "0.6.0-alpha.5"
```

Or: `cargo add degenbot-arbitrage` (the pre-release version must be pinned explicitly, e.g. "0.6.0-alpha.5").

Part of [degenbot](https://github.com/BowTiedDevil/degenbot) — a Rust-first MEV bot for EVM chains. The in-repo root README and `docs/` cover the full architecture; this crate is published standalone so you can depend on exactly the pieces you need.
