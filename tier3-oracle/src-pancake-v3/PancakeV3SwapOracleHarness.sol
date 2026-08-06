// SPDX-License-Identifier: MIT
pragma solidity 0.7.6;

import {PancakeV3Pool} from "pancake-v3-core/contracts/PancakeV3Pool.sol";
import {IPancakeV3PoolDeployer} from "pancake-v3-core/contracts/interfaces/IPancakeV3PoolDeployer.sol";
import {IPancakeV3Pool} from "pancake-v3-core/contracts/interfaces/IPancakeV3Pool.sol";
import {IPancakeV3SwapCallback} from "pancake-v3-core/contracts/interfaces/callback/IPancakeV3SwapCallback.sol";

/// Minimal ERC20 with `balances` at storage slot 0 (same shape as the V3
/// oracle's `MockERC20V3` — the pool's swap math runs before any transfer and
/// the callback mints exactly the input owed, so no balance pre-seeding is
/// required). Arithmetic is Solidity 0.7 wrapping (unchecked).
contract MockERC20Pancake {
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

/// Tier-3 PancakeSwap V3 `PancakeV3Pool.swap` oracle harness (task: build a
/// PancakeSwap V3 variant harness). Deploys the real `PancakeV3Pool` (the
/// Etherscan-verified deployment at 0x1445F32D1A74872bA41f3D8cF4022E9996120b31,
/// solc 0.7.6, vendored under `lib/pancake-src/`) as real bytecode, implements
/// the deployer + swap-callback roles, and exposes a single `swap` entry the
/// Rust `#[test]` drives via revm. The Rust side seeds the pool's
/// `slot0`/`liquidity`/`ticks`/`tickBitmap` storage slots directly (the same
/// `degenbot_pools::v3_storage_slots` encoders — the PancakeSwap fork shares
/// the Uniswap V3 storage layout), NO `initialize`/`mint` needed.
///
/// The swap MATH is byte-identical to Uniswap V3 (same CL step walk); the
/// variant differs in the emitted `Swap` event, which appends
/// `uint128 protocolFeesToken0/1` (topic0 `0x19b47279…` vs Uniswap's
/// `0xc42079f9…`, +2 data words) — exactly the divergence
/// `degenbot-decoders::v3_pancakeswap_swap_decoder` handles. This harness lets
/// the oracle assert the Rust `v3_simulate_swap` output === the PancakeSwap
/// pool's swap state byte-exact AND that the emitted event decodes through the
/// PancakeSwap variant decoder with the extra protocol-fee words (seeded
/// protocolFee = 0 ⇒ those words are 0, indistinguishable from a Uniswap
/// swap's state while still carrying the 9-field layout).
contract PancakeV3SwapOracleHarness is IPancakeV3PoolDeployer, IPancakeV3SwapCallback {
    address public pool;
    MockERC20Pancake public token0;
    MockERC20Pancake public token1;
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
        token0 = new MockERC20Pancake();
        token1 = new MockERC20Pancake();
        params = Parameters({
            factory: address(this),
            token0: address(token0),
            token1: address(token1),
            fee: _fee,
            tickSpacing: _tickSpacing
        });
        // The pool CREATE is deferred to `setupPool()` so it runs as a CALL
        // with the FULL transaction gas forwarded (63/64 of ~16M), not the
        // 63/64 fraction remaining inside the constructor.
    }

    /// Deploys the real `PancakeV3Pool` (reads `params`). Call after construction.
    function setupPool() external {
        require(params.token0 != address(0), "already setup");
        pool = address(new PancakeV3Pool());
        delete params;
    }

    /// Implements `IPancakeV3PoolDeployer.deploy` (the factory's create path).
    /// The harness uses `setupPool()` instead, but the interface requires this
    /// method; it creates the pool from the transient `params` and returns the
    /// address. Args are accepted for interface conformance (params already set
    /// in the constructor).
    function deploy(
        address,
        address,
        address,
        uint24,
        int24
    ) external override returns (address pool_) {
        require(params.token0 != address(0), "already setup");
        pool_ = address(new PancakeV3Pool());
        pool = pool_;
        delete params;
        return pool_;
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
        (amount0, amount1) = IPancakeV3Pool(pool).swap(
            address(this),
            zeroForOne,
            amountSpecified,
            sqrtPriceLimitX96,
            ""
        );
    }

    /// PancakeSwap V3 swap callback (note: `pancakeV3SwapCallback`, not
    /// `uniswapV3SwapCallback`). Pays the pool whatever it is owed.
    function pancakeV3SwapCallback(
        int256 amount0Delta,
        int256 amount1Delta,
        bytes calldata
    ) external override {
        if (amount0Delta > 0) token0.mint(msg.sender, uint256(amount0Delta));
        if (amount1Delta > 0) token1.mint(msg.sender, uint256(amount1Delta));
    }
}
