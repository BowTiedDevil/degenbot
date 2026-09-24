# degenbot-solvers

Value-only multi-hop arbitrage solver math — Möbius closed-form (V2/CL) + golden-section (Solidly/Curve/Balancer) + QuantAMM basket, with the hop-state intake contract (ADR-015).

The multi-hop arbitrage solver math: Möbius closed-form (V2/CL), golden-section (Solidly/Curve/Balancer), and QuantAMM baskets, with the hop-state intake contract (ADR-015).

## Usage

```toml
degenbot-solvers = "0.6.0-alpha.5"
```

Or: `cargo add degenbot-solvers` (the pre-release version must be pinned explicitly, e.g. "0.6.0-alpha.5").

Part of [degenbot](https://github.com/BowTiedDevil/degenbot) — a Rust-first MEV bot for EVM chains. The in-repo root README and `docs/` cover the full architecture; this crate is published standalone so you can depend on exactly the pieces you need.
