# degenbot-decoders

Pure-Rust Uniswap V2/V3/V4 event-log decoders (alloy-only leaf; no pyo3/tokio/degenbot-core).

Event-log decoders for Uniswap V2/V3/V4 as an alloy-only leaf crate — deliberately tiny (no pyo3, tokio, or core dependency) so decoding stays fast anywhere it is needed.

## Usage

```toml
degenbot-decoders = "0.6.0-alpha.5"
```

Or: `cargo add degenbot-decoders` (the pre-release version must be pinned explicitly, e.g. "0.6.0-alpha.5").

Part of [degenbot](https://github.com/BowTiedDevil/degenbot) — a Rust-first MEV bot for EVM chains. The in-repo root README and `docs/` cover the full architecture; this crate is published standalone so you can depend on exactly the pieces you need.
