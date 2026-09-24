# degenbot-pools

Value-only pool identity/state structs + stateless swap simulation for the degenbot pool families.

Value-only pool identity and state structs for every supported AMM topology (Uniswap V2/V3/V4, Curve, Balancer, Solidly family), plus the stateless swap-simulation entry points the solvers and engine drive.

## Usage

```toml
degenbot-pools = "0.6.0-alpha.5"
```

Or: `cargo add degenbot-pools` (the pre-release version must be pinned explicitly, e.g. "0.6.0-alpha.5").

Part of [degenbot](https://github.com/BowTiedDevil/degenbot) — a Rust-first MEV bot for EVM chains. The in-repo root README and `docs/` cover the full architecture; this crate is published standalone so you can depend on exactly the pieces you need.
