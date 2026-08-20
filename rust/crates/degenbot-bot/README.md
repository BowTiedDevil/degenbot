# degenbot-bot

Rust-owned bot state (BotState) + Möbius solvers + unified Uniswap V2/V3/V4 engine.

The Rust-owned heart of the bot: BotState (the ADR-003 state owner), the Möbius solvers, and the unified Uniswap V2/V3/V4 swap engine that drives pool state and solve results.

## Usage

```toml
degenbot-bot = "0.6.0-alpha.5"
```

Or: `cargo add degenbot-bot` (the pre-release version must be pinned explicitly, e.g. "0.6.0-alpha.5").

Part of [degenbot](https://github.com/BowTiedDevil/degenbot) — a Rust-first MEV bot for EVM chains. The in-repo root README and `docs/` cover the full architecture; this crate is published standalone so you can depend on exactly the pieces you need.
