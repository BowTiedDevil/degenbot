// SPDX-License-Identifier: MIT
// Generated from artifacts/UniswapV2Pair.json by gen_interfaces.py — do not edit.
pragma solidity ^0.8.28;

import {PoolKey, SwapParams, ModifyLiquidityParams, ExecuteResult} from "./Types.sol";

interface IUniswapV2Pair {
    event Approval(address indexed owner, address indexed spender, uint256 value);
    event Burn(address indexed sender, uint256 amount0, uint256 amount1, address indexed to);
    event Mint(address indexed sender, uint256 amount0, uint256 amount1);
    event Swap(address indexed sender, uint256 amount0In, uint256 amount1In, uint256 amount0Out, uint256 amount1Out, address indexed to);
    event Sync(uint112 reserve0, uint112 reserve1);
    event Transfer(address indexed from, address indexed to, uint256 value);
    function DOMAIN_SEPARATOR() external view returns (bytes32 r0);
    function MINIMUM_LIQUIDITY() external view returns (uint256 r0);
    function PERMIT_TYPEHASH() external view returns (bytes32 r0);
    function allowance(address a0, address a1) external view returns (uint256 r0);
    function approve(address spender, uint256 value) external returns (bool r0);
    function balanceOf(address a0) external view returns (uint256 r0);
    function burn(address to) external returns (uint256 amount0, uint256 amount1);
    function decimals() external view returns (uint8 r0);
    function factory() external view returns (address r0);
    function getReserves() external view returns (uint112 _reserve0, uint112 _reserve1, uint32 _blockTimestampLast);
    function initialize(address _token0, address _token1) external;
    function kLast() external view returns (uint256 r0);
    function mint(address to) external returns (uint256 liquidity);
    function name() external view returns (string memory r0);
    function nonces(address a0) external view returns (uint256 r0);
    function permit(address owner, address spender, uint256 value, uint256 deadline, uint8 v, bytes32 r, bytes32 s) external;
    function price0CumulativeLast() external view returns (uint256 r0);
    function price1CumulativeLast() external view returns (uint256 r0);
    function skim(address to) external;
    function swap(uint256 amount0Out, uint256 amount1Out, address to, bytes calldata data) external;
    function symbol() external view returns (string memory r0);
    function sync() external;
    function token0() external view returns (address r0);
    function token1() external view returns (address r0);
    function totalSupply() external view returns (uint256 r0);
    function transfer(address to, uint256 value) external returns (bool r0);
    function transferFrom(address from, address to, uint256 value) external returns (bool r0);
}
