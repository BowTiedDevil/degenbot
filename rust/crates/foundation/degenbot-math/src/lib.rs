//! degenbot-math — consolidated AMM invariant-math ports (ADR-035).
//!
//! Five family modules, each a byte-exact (or documented-deviation) port of
//! the canonical Solidity math for its pool family:
//!
//! - [`v2`] — Uniswap V2 constant-product (x·y=k) swap math
//! - [`concentrated_liquidity`] — Uniswap V3/V4 concentrated-liquidity math libraries (`tick`, `sqrt_price_math`, `swap_math`, `liquidity_mapping`)
//! - [`curve`] — `Curve StableSwap invariant math` (`CurveDyCalculator`, `calc_dy`, `calc_y`)
//! - [`balancer`] — Balancer V2 `FixedPoint` / `LogExpMath` / `WeightedMath` / `StableMath`
//! - [`solidly`] — Solidly / Aerodrome / Camelot stable-pool invariants
//!
//! One shared primitive module sits beside the families: [`bigint`] — the
//! 2048-bit width with its widening/narrowing conversions and the floor
//! integer square roots, owned here because the family math and its solver
//! consumers both need them.
//!
//! Consolidated from five workspace crates that shared provenance, the ADR-009
//! lockstep version, and the same consumer set (ADR-035).

pub mod balancer;
pub mod bigint;
pub mod concentrated_liquidity;
pub mod curve;
pub mod solidly;
pub mod v2;
