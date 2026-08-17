// SPDX-License-Identifier: MIT
pragma solidity 0.7.6;

import {SwapMath as V3SwapMath} from "v3-core/contracts/libraries/SwapMath.sol";

/// Tier-3a byte-exact oracle harness for V3 `SwapMath.computeSwapStep`
/// (ergo task OZRQS6, epic UP5NH6). A thin external wrapper exposing the
/// internal-pure library function so a Rust `#[test]` can call it via revm
/// and assert byte-for-byte equality with
/// `degenbot_concentrated_liquidity_math::compute_swap_step_v3`.
///
/// amountRemaining sign convention: POSITIVE = exact input (V3).
contract SwapMathV3Harness {
    function computeSwapStep(
        uint160 sqrtRatioCurrentX96,
        uint160 sqrtRatioTargetX96,
        uint128 liquidity,
        int256 amountRemaining,
        uint24 feePips
    )
        external
        pure
        returns (uint160 sqrtRatioNextX96, uint256 amountIn, uint256 amountOut, uint256 feeAmount)
    {
        return V3SwapMath.computeSwapStep(
            sqrtRatioCurrentX96,
            sqrtRatioTargetX96,
            liquidity,
            amountRemaining,
            feePips
        );
    }
}
