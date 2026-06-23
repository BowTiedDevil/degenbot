//! Concentrated-liquidity math library — pure Rust implementations of
//! Uniswap V3 and V4 Solidity libraries.
//!
//! # Architecture
//!
//! Each submodule mirrors a Solidity library file and exposes pure functions
//! operating on native Alloy integer types (`U256`, `U160`, `I256`, etc.).
//! No `PyO3` dependency — the Python binding layer lives in the root `degenbot_rs`.
//!
//! # V3 / V4 convergence
//!
//! The Python implementations diverge because they port Solidity's different
//! overflow-detection patterns to Python's arbitrary-precision integers.
//! In Rust with proper fixed-width types, most of those divergences collapse:
//!
//! | Solidity pattern | Python port (V3) | Python port (V4) | Rust |
//! |---|---|---|---|
//! | `unchecked` overflow detection | `if product < MAX_UINT256` | `if product <= MAX_UINT256` | `U256::checked_mul` |
//! | `type(uint160)` downcast | `to_uint160()` helper | manual `> MAX_UINT160` check | `U160::try_from` |
//! | `mulmod` Yul builtin | Python `(x*y)%k` + `EVMRevertError` on `k==0` | Python `(x*y)%k` returning 0 on `k==0` | native `U256::mul_mod` |
//!
//! The one **genuine** divergence is [`swap_math::compute_swap_step`], where
//! V3 and V4 use opposite sign conventions for `amountSpecified`. This is
//! handled by two function variants sharing a common core.
//!
//! # Module layout
//!
//! ```text
//! cl_lib/
//! ├── mod.rs             — this file; re-exports
//! ├── bit_math.rs        — most_significant_bit, least_significant_bit
//! ├── full_math.rs       — muldiv, muldiv_rounding_up
//! ├── unsafe_math.rs     — div_rounding_up
//! ├── tick_math.rs       — sqrt-ratio ↔ tick conversions
//! ├── liquidity_math.rs  — add_delta (uint128 overflow check)
//! ├── sqrt_price_math.rs — amount deltas, next-price calculations
//! ├── swap_math.rs       — compute_swap_step (V3 + V4 variants)
//! └── functions.rs       — mulmod, compress, tick_position, safe casts
//! ```

pub mod bit_math;
pub mod full_math;
pub mod functions;
pub mod liquidity_math;
pub mod sqrt_price_math;
pub mod swap_math;
pub mod tick_math;
pub mod unsafe_math;
