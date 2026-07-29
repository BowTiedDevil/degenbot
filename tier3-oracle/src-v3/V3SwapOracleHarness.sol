// SPDX-License-Identifier: MIT
pragma solidity 0.7.6;

import {UniswapV3Pool} from "v3-core/contracts/UniswapV3Pool.sol";
import {IUniswapV3PoolDeployer} from "v3-core/contracts/interfaces/IUniswapV3PoolDeployer.sol";
import {IUniswapV3Pool} from "v3-core/contracts/interfaces/IUniswapV3Pool.sol";
import {IUniswapV3SwapCallback} from "v3-core/contracts/interfaces/callback/IUniswapV3SwapCallback.sol";

/// Minimal ERC20 with `balances` at storage slot 0 so a revm `CacheDB` test
/// oracle can seed/deploy without a full token implementation. Arithmetic is
/// Solidity 0.7 wrapping (unchecked); the pool's swap math is computed BEFORE
/// any transfer, and the balance invariant (`IIA`) is satisfied by the
/// callback minting exactly the input the pool expects — so no balance
/// pre-seeding is required for the oracle to pass.
contract MockERC20V3 {
    mapping(address => uint256) public balanceOf; // slot 0

    function transfer(address to, uint256 amount) external returns (bool) {
        balanceOf[msg.sender] -= amount; // 0.7 wrapping — never reverts
        balanceOf[to] += amount;
        return true;
    }

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }
}

/// Tier-3b end-to-end V3 `Pool.swap` oracle harness (ergo task 2LTKVO, epic
/// UP5NH6). Deploys the canonical `UniswapV3Pool` from v3-core as real
/// bytecode, implements the deployer + swap-callback roles, and exposes a
/// single `swap` entry the Rust `#[test]` drives via revm. The Rust side
/// seeds the pool's `slot0`/`liquidity`/`ticks`/`tickBitmap` storage slots
/// directly (see `degenbot_pools::v3_storage_slots`) — NO `initialize`/`mint`
/// is needed because the swap math reads only those slots.
contract V3SwapOracleHarness is IUniswapV3PoolDeployer, IUniswapV3SwapCallback {
    address public pool;
    MockERC20V3 public token0;
    MockERC20V3 public token1;
    uint24 public fee;
    int24 public tickSpacing;

    struct Parameters {
        address factory;
        address token0;
        address token1;
        uint24 fee;
        int24 tickSpacing;
    }
    Parameters private params;

    constructor(uint24 _fee, int24 _tickSpacing) {
        fee = _fee;
        tickSpacing = _tickSpacing;
        token0 = new MockERC20V3();
        token1 = new MockERC20V3();
        params = Parameters({
            factory: address(this),
            token0: address(token0),
            token1: address(token1),
            fee: _fee,
            tickSpacing: _tickSpacing
        });
        // The pool CREATE is deferred to `setupPool()` so it runs as a CALL
        // with the FULL transaction gas forwarded (63/64 of ~16M), not the
        // 63/64 fraction remaining inside the constructor — the 4.4M gas for
        // the 22KB code deposit would otherwise be starved.
    }

    /// Deploys the real UniswapV3Pool (reads `params`). Call after construction.
    function setupPool() external {
        require(params.token0 != address(0), "already setup");
        pool = address(new UniswapV3Pool());
        delete params;
    }

    function parameters()
        external
        view
        override
        returns (
            address factory,
            address token0_,
            address token1_,
            uint24 fee_,
            int24 tickSpacing_
        )
    {
        Parameters memory p = params;
        return (p.factory, p.token0, p.token1, p.fee, p.tickSpacing);
    }

    /// Drives `pool.swap`. Recipient is the harness itself so the callback
    /// (which holds the token references) can mint the required input.
    function swap(bool zeroForOne, int256 amountSpecified, uint160 sqrtPriceLimitX96)
        external
        returns (int256 amount0, int256 amount1)
    {
        (amount0, amount1) = IUniswapV3Pool(pool).swap(
            address(this),
            zeroForOne,
            amountSpecified,
            sqrtPriceLimitX96,
            ""
        );
    }

    /// Pays the pool whatever it is owed (a positive delta). Minting to the
    /// pool satisfies `IIA` regardless of the seeded balances.
    function uniswapV3SwapCallback(
        int256 amount0Delta,
        int256 amount1Delta,
        bytes calldata
    ) external override {
        if (amount0Delta > 0) token0.mint(msg.sender, uint256(amount0Delta));
        if (amount1Delta > 0) token1.mint(msg.sender, uint256(amount1Delta));
    }
}
