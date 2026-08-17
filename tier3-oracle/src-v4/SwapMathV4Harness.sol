// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {SwapMath as V4SwapMath} from "v4-core/src/libraries/SwapMath.sol";

/// Tier-3a byte-exact oracle harness for V4 `SwapMath.computeSwapStep`
/// (ergo task OZRQS6, epic UP5NH6). A thin external wrapper exposing the
/// internal-pure library function so a Rust `#[test]` can call it via revm
/// and assert byte-for-byte equality with
/// `degenbot_concentrated_liquidity_math::compute_swap_step_v4`.
///
/// amountRemaining sign convention: NEGATIVE = exact input (V4 — opposite of
/// V3). The protocol-fee threading (`calculateSwapFee(protocolFeeDir, lpFee)`)
/// happens at the `Pool.swap` caller, NOT inside `computeSwapStep` — so this
/// single-step oracle takes the pre-computed COMBINED `feePips` directly.
contract SwapMathV4Harness {
    function computeSwapStep(
        uint160 sqrtPriceCurrentX96,
        uint160 sqrtPriceTargetX96,
        uint128 liquidity,
        int256 amountRemaining,
        uint24 feePips
    )
        external
        pure
        returns (uint160 sqrtPriceNextX96, uint256 amountIn, uint256 amountOut, uint256 feeAmount)
    {
        return V4SwapMath.computeSwapStep(
            sqrtPriceCurrentX96,
            sqrtPriceTargetX96,
            liquidity,
            amountRemaining,
            feePips
        );
    }
}
