// SPDX-License-Identifier: MIT
// Generated from artifacts/TestHarness.json by gen_interfaces.py — do not edit.
pragma solidity ^0.8.28;

import {PoolKey, SwapParams, ModifyLiquidityParams, ExecuteResult} from "./Types.sol";

interface ITestHarness {
    function test_mul_div(uint256 a, uint256 b, uint256 d) external pure returns (uint256 r0);
    function test_mul_div_rounding_up(uint256 a, uint256 b, uint256 d) external pure returns (uint256 r0);
    function test_div_rounding_up(uint256 x, uint256 y) external pure returns (uint256 r0);
    function test_to_uint160(uint256 y) external pure returns (uint160 r0);
    function test_to_int128(int256 y) external pure returns (int128 r0);
    function test_to_int256(uint256 y) external pure returns (int256 r0);
    function test_get_next_sqrt_price_from_input(uint160 sqrt_px96, uint128 liquidity, uint256 amount_in, bool zero_for_one) external pure returns (uint160 r0);
    function test_get_next_sqrt_price_from_output(uint160 sqrt_px96, uint128 liquidity, uint256 amount_out, bool zero_for_one) external pure returns (uint160 r0);
    function test_get_amount0_delta(uint160 sqrt_ratio_ax96, uint160 sqrt_ratio_bx96, uint128 liquidity, bool round_up) external pure returns (uint256 r0);
    function test_get_amount1_delta(uint160 sqrt_ratio_ax96, uint160 sqrt_ratio_bx96, uint128 liquidity, bool round_up) external pure returns (uint256 r0);
    function test_get_tick_at_sqrt_ratio(uint160 sqrt_price_x96) external pure returns (int24 r0);
    function test_get_sqrt_ratio_at_tick(int24 tick) external pure returns (uint160 r0);
    function test_compute_swap_step(uint160 sqrt_ratio_current_x96, uint160 sqrt_ratio_target_x96, uint128 liquidity, int256 amount_remaining, uint24 fee_pips) external pure returns (uint160 r0, uint256 r1, uint256 r2, uint256 r3);
}
