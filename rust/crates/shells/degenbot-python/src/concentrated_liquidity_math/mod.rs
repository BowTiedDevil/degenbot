//! `PyO3` wrappers over the `degenbot-concentrated-liquidity-math` pure core.
//! Mirrors `crates/foundation/degenbot-math/src/cl/`.

pub mod lib;
pub mod tick_math;

use pyo3::prelude::*;

/// The `degenbot._ffi.concentrated_liquidity_math` Python submodule
/// (declarative `#[pymodule]`).
///
/// Combines the swap-path math functions in `lib.rs` (`BitMath`/`FullMath`/
/// `UnsafeMath`/`LiquidityMath`/`SqrtPriceMath`/`SwapMath`/`TickMath` helpers/
/// `LiquidityMapping`), the 2 `tick_math.rs` entry points
/// (`get_sqrt_ratio_at_tick` / `get_tick_at_sqrt_ratio`), and the 4
/// `TickMath` boundary constants (`MIN_TICK`/`MAX_TICK`/
/// `MIN_SQRT_RATIO`/`MAX_SQRT_RATIO`) — previously all flat on the root
/// `_ffi` namespace — onto one real submodule with un-prefixed names
/// (`muldiv`, not `cl_muldiv`). The parent module registers the submodule
/// itself; the `sys.modules` entry lives in the parent's shared helper.
///
/// The companion `src/degenbot/uniswap/math.py` re-exports these as the
/// stable import path, decoupling Python consumers from `degenbot._ffi`.
#[pymodule(submodule)]
#[pyo3(module = "degenbot._ffi")]
pub mod concentrated_liquidity_math {
    use pyo3::prelude::*;

    // lib.rs — the swap-path math functions.
    #[pymodule_export]
    use super::lib::{
        compute_swap_step_v3, compute_swap_step_v4, get_tick_word_and_bit_position,
        least_significant_bit, most_significant_bit, muldiv, muldiv_rounding_up,
    };

    // tick_math.rs — the 2 high-level entry points (previously flat on root
    // via c_api.rs).
    #[pymodule_export]
    use super::tick_math::{get_sqrt_ratio_at_tick, get_tick_at_sqrt_ratio};

    // ADR-005 single-source-of-truth: the canonical home is the
    // `degenbot-concentrated-liquidity-math` core; the `PyO3` seam surfaces
    // them so Python companions and a standalone Rust consumer share one
    // source.
    #[pymodule_export]
    const MIN_TICK: i32 = degenbot_math::cl::tick_math::MIN_TICK;

    #[pymodule_export]
    const MAX_TICK: i32 = degenbot_math::cl::tick_math::MAX_TICK;

    /// The sqrt-ratio boundary constants cannot be declarative `const`
    /// exports: they are `U160`s widened to `U256` for the alloy → Python-int
    /// conversion helper, which is a runtime call. They stay on the
    /// submodule's imperative island here.
    #[pymodule_init]
    fn init(m: &Bound<'_, PyModule>) -> PyResult<()> {
        use degenbot_math::cl::tick_math::{MAX_SQRT_RATIO, MIN_SQRT_RATIO};
        let py = m.py();
        m.add(
            "MIN_SQRT_RATIO",
            crate::conversion::alloy::u256_to_py(
                py,
                &alloy::primitives::U256::from(MIN_SQRT_RATIO),
            )?,
        )?;
        m.add(
            "MAX_SQRT_RATIO",
            crate::conversion::alloy::u256_to_py(
                py,
                &alloy::primitives::U256::from(MAX_SQRT_RATIO),
            )?,
        )
    }
}
