// SPDX-License-Identifier: MIT
// Generated from artifacts/FakeUniswapV2Pair.json by gen_interfaces.py — do not edit.
pragma solidity ^0.8.28;

import {PoolKey, SwapParams, ModifyLiquidityParams, ExecuteResult} from "./Types.sol";

interface IFakeUniswapV2Pair {
    event Sync(uint112 reserve0, uint112 reserve1);
    event Swap(address indexed sender, uint256 amount0In, uint256 amount1In, uint256 amount0Out, uint256 amount1Out, address indexed to);
    function getReserves() external view returns (uint112 r0, uint112 r1, uint32 r2);
    function sync() external;
    function reset() external;
    function set_next_swap(uint256 amount_in, uint256 amount_out, bool zero_for_one) external;
    function swap(uint256 amount0Out, uint256 amount1Out, address to, bytes calldata data) external;
    function token0() external view returns (address r0);
    function token1() external view returns (address r0);
    function callback_variant() external view returns (uint256 r0);
    function amount_in() external view returns (uint256 r0);
    function amount_out() external view returns (uint256 r0);
    function unlocked() external view returns (uint256 r0);
    function reserve0() external view returns (uint112 r0);
    function reserve1() external view returns (uint112 r0);
    function block_timestamp_last() external view returns (uint32 r0);
}
