// SPDX-License-Identifier: MIT
// Generated from artifacts/CmdExecutor.json by gen_interfaces.py — do not edit.
pragma solidity ^0.8.28;

import {PoolKey, SwapParams, ModifyLiquidityParams, ExecuteResult} from "./Types.sol";

interface ICmdExecutor {
    function initialize() external payable;
    function release_id() external view returns (bytes32 r0);
    function protocol_major() external view returns (uint32 r0);
    function call_encoding_version() external view returns (uint32 r0);
    function capabilities_hash() external view returns (bytes32 r0);
    function withdraw(uint256 amount, address destination) external;
    function sweep(address destination) external;
    function execute(bytes calldata commands) external payable returns (ExecuteResult memory r0);
    function execute(bytes calldata commands, uint256 config) external payable returns (ExecuteResult memory r0);
    function uniswapV2Call(address sender, uint256 amount0Out, uint256 amount1Out, bytes calldata data) external payable;
    function hook(address sender, uint256 amount0Out, uint256 amount1Out, bytes calldata data) external payable;
    function pancakeCall(address sender, uint256 amount0Out, uint256 amount1Out, bytes calldata data) external payable;
    function uniswapV3SwapCallback(int256 amount0_delta, int256 amount1_delta, bytes calldata data) external payable;
    function pancakeV3SwapCallback(int256 amount0_delta, int256 amount1_delta, bytes calldata data) external payable;
    function unlockCallback(bytes calldata data) external payable returns (bytes memory r0);
}
