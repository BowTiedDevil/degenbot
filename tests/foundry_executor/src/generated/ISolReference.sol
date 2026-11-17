// SPDX-License-Identifier: MIT
// Generated from artifacts/SolReference.json by gen_interfaces.py — do not edit.
pragma solidity ^0.8.28;

import {PoolKey, SwapParams, ModifyLiquidityParams, ExecuteResult} from "./Types.sol";

interface ISolReference {
    function test_compute_swap_step(uint160 sqrtRatioCurrentX96, uint160 sqrtRatioTargetX96, uint128 liquidity, int256 amountRemaining, uint24 feePips) external pure returns (uint160 sqrtRatioNextX96, uint256 amountIn, uint256 amountOut, uint256 feeAmount);
    function test_div_rounding_up(uint256 x, uint256 y) external pure returns (uint256 r0);
    function test_get_amount0_delta(uint160 sqrtRatioAX96, uint160 sqrtRatioBX96, uint128 liquidity, bool roundUp) external pure returns (uint256 r0);
    function test_get_amount1_delta(uint160 sqrtRatioAX96, uint160 sqrtRatioBX96, uint128 liquidity, bool roundUp) external pure returns (uint256 r0);
    function test_get_next_sqrt_price_from_input(uint160 sqrtPX96, uint128 liquidity, uint256 amountIn, bool zeroForOne) external pure returns (uint160 r0);
    function test_get_next_sqrt_price_from_output(uint160 sqrtPX96, uint128 liquidity, uint256 amountOut, bool zeroForOne) external pure returns (uint160 r0);
    function test_get_sqrt_ratio_at_tick(int24 tick) external pure returns (uint160 r0);
    function test_get_tick_at_sqrt_ratio(uint160 sqrtPriceX96) external pure returns (int24 r0);
    function test_mul_div(uint256 a, uint256 b, uint256 d) external pure returns (uint256 r0);
    function test_mul_div_rounding_up(uint256 a, uint256 b, uint256 d) external pure returns (uint256 r0);
    function test_to_int128(int256 y) external pure returns (int128 r0);
    function test_to_int256(uint256 y) external pure returns (int256 r0);
    function test_to_uint160(uint256 y) external pure returns (uint160 r0);
}
