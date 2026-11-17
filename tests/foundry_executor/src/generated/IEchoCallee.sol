// SPDX-License-Identifier: MIT
// Generated from artifacts/EchoCallee.json by gen_interfaces.py — do not edit.
pragma solidity ^0.8.28;

import {PoolKey, SwapParams, ModifyLiquidityParams, ExecuteResult} from "./Types.sol";

interface IEchoCallee {
    function unlockCallback(bytes calldata data) external returns (bytes memory r0);
    function do_unlock(address pool_manager, bytes calldata data) external returns (bytes memory r0);
    function do_swap_v3(address pool, address recipient, bool zero_for_one, int256 amount_specified, uint160 sqrt_price_limit_x96, bytes calldata data) external;
    function uniswapV2Call(address sender, uint256 amount0Out, uint256 amount1Out, bytes calldata data) external;
    function hook(address sender, uint256 amount0Out, uint256 amount1Out, bytes calldata data) external;
    function pancakeCall(address sender, uint256 amount0Out, uint256 amount1Out, bytes calldata data) external;
    function uniswapV3SwapCallback(int256 amount0, int256 amount1, bytes calldata data) external;
    function pancakeV3SwapCallback(int256 amount0, int256 amount1, bytes calldata data) external;
    function last_len() external view returns (uint256 r0);
    function last_digest() external view returns (bytes32 r0);
}
