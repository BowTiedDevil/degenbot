# degenbot-price

Pure-Rust on-chain price readers — Chainlink aggregator + Aave oracle over degenbot-rpc eth_call.

Pure-Rust on-chain price readers: the Chainlink aggregator and the Aave oracle, driven over degenbot-rpc eth_call — no Python oracle hop.

## Usage

```toml
degenbot-price = "0.6.0-alpha.5"
```

Or: `cargo add degenbot-price` (the pre-release version must be pinned explicitly, e.g. "0.6.0-alpha.5").

Part of [degenbot](https://github.com/BowTiedDevil/degenbot) — a Rust-first MEV bot for EVM chains. The in-repo root README and `docs/` cover the full architecture; this crate is published standalone so you can depend on exactly the pieces you need.
