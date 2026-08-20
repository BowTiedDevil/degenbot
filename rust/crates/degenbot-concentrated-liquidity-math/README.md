# degenbot-concentrated-liquidity-math

Pure-Rust ports of Uniswap V3/V4 concentrated-liquidity math libraries.

Reference ports of the Uniswap V3/V4 concentrated-liquidity math libraries: tick, sqrt-price, and swap-step computations exactly as the canonical core contracts execute them.

## Usage

```toml
degenbot-concentrated-liquidity-math = "0.6.0-alpha.5"
```

Or: `cargo add degenbot-concentrated-liquidity-math` (the pre-release version must be pinned explicitly, e.g. "0.6.0-alpha.5").

Part of [degenbot](https://github.com/BowTiedDevil/degenbot) — a Rust-first MEV bot for EVM chains. The in-repo root README and `docs/` cover the full architecture; this crate is published standalone so you can depend on exactly the pieces you need.
