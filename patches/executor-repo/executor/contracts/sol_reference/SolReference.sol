// SPDX-License-Identifier: MIT
pragma solidity =0.7.6;

import './FullMath.sol';
import './SqrtPriceMath.sol';
import './SwapMath.sol';
import './TickMath.sol';
import './SafeCast.sol';
import './UnsafeMath.sol';

/// @dev Reference harness: exposes original Solidity library functions
///      with the same signatures as the Vyper test_harness.
contract SolReference {

    // ── FullMath ──

    function test_mul_div(uint256 a, uint256 b, uint256 d) external pure returns (uint256) {
        return FullMath.mulDiv(a, b, d);
    }

    function test_mul_div_rounding_up(uint256 a, uint256 b, uint256 d) external pure returns (uint256) {
        return FullMath.mulDivRoundingUp(a, b, d);
    }

    // ── UnsafeMath ──

    function test_div_rounding_up(uint256 x, uint256 y) external pure returns (uint256) {
        return UnsafeMath.divRoundingUp(x, y);
    }

    // ── SafeCast ──

    function test_to_uint160(uint256 y) external pure returns (uint160) {
        return SafeCast.toUint160(y);
    }

    function test_to_int128(int256 y) external pure returns (int128) {
        return SafeCast.toInt128(y);
    }

    function test_to_int256(uint256 y) external pure returns (int256) {
        return SafeCast.toInt256(y);
    }

    // ── SqrtPriceMath ──

    function test_get_next_sqrt_price_from_input(
        uint160 sqrtPX96, uint128 liquidity, uint256 amountIn, bool zeroForOne
    ) external pure returns (uint160) {
        return SqrtPriceMath.getNextSqrtPriceFromInput(sqrtPX96, liquidity, amountIn, zeroForOne);
    }

    function test_get_next_sqrt_price_from_output(
        uint160 sqrtPX96, uint128 liquidity, uint256 amountOut, bool zeroForOne
    ) external pure returns (uint160) {
        return SqrtPriceMath.getNextSqrtPriceFromOutput(sqrtPX96, liquidity, amountOut, zeroForOne);
    }

    function test_get_amount0_delta(
        uint160 sqrtRatioAX96, uint160 sqrtRatioBX96, uint128 liquidity, bool roundUp
    ) external pure returns (uint256) {
        return SqrtPriceMath.getAmount0Delta(sqrtRatioAX96, sqrtRatioBX96, liquidity, roundUp);
    }

    function test_get_amount1_delta(
        uint160 sqrtRatioAX96, uint160 sqrtRatioBX96, uint128 liquidity, bool roundUp
    ) external pure returns (uint256) {
        return SqrtPriceMath.getAmount1Delta(sqrtRatioAX96, sqrtRatioBX96, liquidity, roundUp);
    }

    // ── TickMath ──

    function test_get_sqrt_ratio_at_tick(int24 tick) external pure returns (uint160) {
        return TickMath.getSqrtRatioAtTick(tick);
    }

    function test_get_tick_at_sqrt_ratio(uint160 sqrtPriceX96) external pure returns (int24) {
        return TickMath.getTickAtSqrtRatio(sqrtPriceX96);
    }

    // ── SwapMath ──

    function test_compute_swap_step(
        uint160 sqrtRatioCurrentX96,
        uint160 sqrtRatioTargetX96,
        uint128 liquidity,
        int256 amountRemaining,
        uint24 feePips
    ) external pure returns (uint160 sqrtRatioNextX96, uint256 amountIn, uint256 amountOut, uint256 feeAmount) {
        return SwapMath.computeSwapStep(
            sqrtRatioCurrentX96, sqrtRatioTargetX96, liquidity, amountRemaining, feePips
        );
    }
}
