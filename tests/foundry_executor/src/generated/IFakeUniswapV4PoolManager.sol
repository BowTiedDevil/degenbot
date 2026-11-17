// SPDX-License-Identifier: MIT
// Generated from artifacts/FakeUniswapV4PoolManager.json by gen_interfaces.py — do not edit.
pragma solidity ^0.8.28;

import {PoolKey, SwapParams, ModifyLiquidityParams, ExecuteResult} from "./Types.sol";

interface IFakeUniswapV4PoolManager {
    event Transfer(address caller, address indexed from_, address indexed to_, uint256 indexed id, uint256 amount);
    event Approval(address indexed owner, address indexed spender, uint256 indexed id, uint256 amount);
    event OperatorSet(address indexed owner, address indexed operator, bool approved);
    event Initialize(bytes32 indexed id, address indexed currency0, address indexed currency1, uint24 fee, int24 tick_spacing, address hooks, uint160 sqrt_price_x96, int24 tick);
    event ModifyLiquidity(bytes32 indexed id, address indexed sender, int24 tick_lower, int24 tick_upper, int256 liquidity_delta, bytes32 salt);
    event Donate(bytes32 indexed id, address indexed sender, uint256 amount0, uint256 amount1);
    event Swap(bytes32 indexed id, address indexed sender, int128 amount0, int128 amount1, uint160 sqrtPriceX96, uint128 liquidity, int24 tick, uint24 fee);
    function balanceOf(address owner, uint256 id) external view returns (uint256 r0);
    function allowance(address owner, address spender, uint256 id) external view returns (uint256 r0);
    function isOperator(address owner, address operator) external view returns (bool r0);
    function transfer(address receiver, uint256 id, uint256 amount) external returns (bool r0);
    function transferFrom(address sender, address receiver, uint256 id, uint256 amount) external returns (bool r0);
    function approve(address spender, uint256 id, uint256 amount) external returns (bool r0);
    function setOperator(address operator, bool approved) external returns (bool r0);
    function initialize(PoolKey calldata key, uint160 sqrt_price_x96) external returns (int24 r0);
    function set_next_modify_liquidity(PoolKey calldata pool_key, ModifyLiquidityParams calldata params, int128 amount0, int128 amount1) external;
    function modifyLiquidity(PoolKey calldata key, ModifyLiquidityParams calldata params, bytes calldata hook_data) external returns (int256 r0, int256 r1);
    function set_next_donate(PoolKey calldata pool_key, uint256 amount0, uint256 amount1) external;
    function donate(PoolKey calldata key, uint256 amount0, uint256 amount1, bytes calldata hook_data) external returns (int256 r0);
    function mint(address to, uint256 id, uint256 amount) external;
    function burn(address from_, uint256 id, uint256 amount) external;
    function sync(address currency) external;
    function settle() external payable returns (uint256 r0);
    function settleFor(address recipient) external payable returns (uint256 r0);
    function take(address currency, address to, uint256 amount) external;
    function clear(address currency, uint256 amount) external;
    function unlock(bytes calldata data) external returns (bytes memory r0);
    function set_next_swap(PoolKey calldata pool_key, uint256 amount_in, uint256 amount_out, bool zero_for_one, bytes calldata hook_data) external;
    function swap(PoolKey calldata key, SwapParams calldata params, bytes calldata hookData) external returns (int256 r0);
    function updateDynamicLPFee(PoolKey calldata key, uint24 new_dynamic_lp_fee) external;
    function exttload(bytes32 slot) external view returns (bytes32 r0);
    function pools(bytes32 arg0) external view returns (uint160 r0);
    function dynamic_lp_fees(bytes32 arg0) external view returns (uint24 r0);
    function swap_amounts(bytes32 arg0, address arg1) external view returns (uint256 r0);
    function modify_liquidity_delta0(bytes32 arg0, bytes32 arg1) external view returns (int128 r0);
    function modify_liquidity_delta1(bytes32 arg0, bytes32 arg1) external view returns (int128 r0);
    function donate_amount0(bytes32 arg0) external view returns (uint256 r0);
    function donate_amount1(bytes32 arg0) external view returns (uint256 r0);
    function erc6909_balance_of(address arg0, uint256 arg1) external view returns (uint256 r0);
    function erc6909_allowance(address arg0, address arg1, uint256 arg2) external view returns (uint256 r0);
    function erc6909_operator(address arg0, address arg1) external view returns (bool r0);
}
