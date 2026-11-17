// SPDX-License-Identifier: MIT
// Generated from artifacts/UniswapV2Factory.json by gen_interfaces.py — do not edit.
pragma solidity ^0.8.28;

import {PoolKey, SwapParams, ModifyLiquidityParams, ExecuteResult} from "./Types.sol";

interface IUniswapV2Factory {
    event PairCreated(address indexed token0, address indexed token1, address pair, uint256 a3);
    function allPairs(uint256 a0) external view returns (address r0);
    function allPairsLength() external view returns (uint256 r0);
    function createPair(address tokenA, address tokenB) external returns (address pair);
    function feeTo() external view returns (address r0);
    function feeToSetter() external view returns (address r0);
    function getPair(address a0, address a1) external view returns (address r0);
    function setFeeTo(address _feeTo) external;
    function setFeeToSetter(address _feeToSetter) external;
}
