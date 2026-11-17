// SPDX-License-Identifier: MIT
// Generated from the artifacts' tuple shapes by gen_interfaces.py — do not edit.
pragma solidity ^0.8.28;

struct ExecuteResult {
    uint256 gross_profit;
    uint256 bribe_paid;
}

struct PoolKey {
    address currency0;
    address currency1;
    uint24 fee;
    int24 tick_spacing;
    address hooks;
}

struct ModifyLiquidityParams {
    int24 tick_lower;
    int24 tick_upper;
    int256 liquidity_delta;
    bytes32 salt;
}

struct SwapParams {
    bool zero_for_one;
    int256 amount_specified;
    uint160 sqrt_price_limit_x96;
}
