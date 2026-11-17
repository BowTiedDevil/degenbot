// SPDX-License-Identifier: MIT
// Generated from artifacts/UniswapV3Factory.json by gen_interfaces.py — do not edit.
pragma solidity ^0.8.28;

import {PoolKey, SwapParams, ModifyLiquidityParams, ExecuteResult} from "./Types.sol";

interface IUniswapV3Factory {
    event FeeAmountEnabled(uint24 indexed fee, int24 indexed tickSpacing);
    event OwnerChanged(address indexed oldOwner, address indexed newOwner);
    event PoolCreated(address indexed token0, address indexed token1, uint24 indexed fee, int24 tickSpacing, address pool);
    function createPool(address tokenA, address tokenB, uint24 fee) external returns (address pool);
    function enableFeeAmount(uint24 fee, int24 tickSpacing) external;
    function feeAmountTickSpacing(uint24 a0) external view returns (int24 r0);
    function getPool(address a0, address a1, uint24 a2) external view returns (address r0);
    function owner() external view returns (address r0);
    function parameters() external view returns (address factory, address token0, address token1, uint24 fee, int24 tickSpacing);
    function setOwner(address _owner) external;
}
