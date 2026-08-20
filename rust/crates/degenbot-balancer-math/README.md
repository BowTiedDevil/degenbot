# degenbot-balancer-math

Pure-Rust ports of Balancer V2 FixedPoint / LogExpMath / WeightedMath / StableMath.

Reference ports of the Balancer V2 math libraries (FixedPoint, LogExpMath, WeightedMath, StableMath), byte-consistent with the canonical Solidity for weighted and stable pool computations.

## Usage

```toml
degenbot-balancer-math = "0.6.0-alpha.5"
```

Or: `cargo add degenbot-balancer-math` (the pre-release version must be pinned explicitly, e.g. "0.6.0-alpha.5").

Part of [degenbot](https://github.com/BowTiedDevil/degenbot) — a Rust-first MEV bot for EVM chains. The in-repo root README and `docs/` cover the full architecture; this crate is published standalone so you can depend on exactly the pieces you need.
