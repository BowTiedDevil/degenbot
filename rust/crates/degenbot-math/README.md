# degenbot-math

Pure-Rust ports of canonical AMM invariant math — one crate, five family
modules (ADR-035).

| Module | Port of |
| --- | --- |
| `v2` | Uniswap V2 constant-product (x·y=k) swap math |
| `cl` | Uniswap V3/V4 concentrated-liquidity math libraries (`tick`, `sqrt_price_math`, `swap_math`, `liquidity_mapping`) |
| `curve` | Curve StableSwap invariant math (`CurveDyCalculator`, `calc_dy`, `calc_y`) |
| `balancer` | Balancer V2 `FixedPoint` / `LogExpMath` / `WeightedMath` / `StableMath` |
| `solidly` | Solidly / Aerodrome / Camelot stable-pool invariants |

Every port is cross-verified against its canonical source by the workspace's
tier-3 oracle suites (REVM-transacted Solidity harnesses and Python
crosscheck snapshots); this crate hosts those suites.

Part of the [degenbot](https://crates.io/crates/degenbot) workspace — see
`docs/adr/ADR-035-math-consolidation-and-umbrella-aliases.md` for the
consolidation rationale.
