// SPDX-License-Identifier: MIT
// Generated from artifacts/FakeUniswapV3Pool.json by gen_interfaces.py — do not edit.
pragma solidity ^0.8.28;

import {PoolKey, SwapParams, ModifyLiquidityParams, ExecuteResult} from "./Types.sol";

interface IFakeUniswapV3Pool {
    event Swap(address indexed sender, address indexed recipient, int256 amount0, int256 amount1, uint160 sqrtPriceX96, uint128 liquidity, int24 tick);
    function initialize(uint160 sqrt_price_x96) external;
    function add_liquidity() external;
    function get_amount_out(uint256 amount_in, bool zero_for_one) external view returns (uint256 r0);
    function swap(address recipient, bool zero_for_one, int256 amount_specified, uint160 sqrt_price_limit_x96, bytes calldata data) external returns (int256 r0, int256 r1);
    function token0() external view returns (address r0);
    function token1() external view returns (address r0);
    function fee() external view returns (uint24 r0);
    function callback_variant() external view returns (uint256 r0);
    function sqrt_price_x96() external view returns (uint160 r0);
    function liquidity() external view returns (uint128 r0);
    function unlocked() external view returns (bool r0);
}
