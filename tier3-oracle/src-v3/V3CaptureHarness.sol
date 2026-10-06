// SPDX-License-Identifier: MIT
pragma solidity 0.7.6;

import {UniswapV3Pool} from "v3-core/contracts/UniswapV3Pool.sol";
import {IUniswapV3PoolDeployer} from "v3-core/contracts/interfaces/IUniswapV3PoolDeployer.sol";
import {IUniswapV3Pool} from "v3-core/contracts/interfaces/IUniswapV3Pool.sol";
import {IUniswapV3SwapCallback} from "v3-core/contracts/interfaces/callback/IUniswapV3SwapCallback.sol";
import {IUniswapV3MintCallback} from "v3-core/contracts/interfaces/callback/IUniswapV3MintCallback.sol";

/// The canonical `PoolCreated` event (V3 factory signature) — the capture
/// harness announces its deployed pool through the REAL event the pool
/// updater's creation fetch filters on.
contract V3CaptureEvents {
    event PoolCreated(
        address indexed token0,
        address indexed token1,
        uint24 indexed fee,
        int24 tickSpacing,
        address pool
    );
}

/// Minimal ERC20 with `balances` at storage slot 0 (the established mock the
/// V3 oracle harness ships) — the callbacks mint exactly what the pool owes.
contract MockERC20Capture {
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

/// Wave-2 capture-generation harness (ergo LCRP37): deploys the canonical
/// `UniswapV3Pool` from v3-core as real bytecode and drives REAL
/// initialize / mint / burn / swap frames so the manufactured captures are
/// execution products of the real pool contract, never hand-authored
/// answers. Same deployer + callback roles as `V3SwapOracleHarness`, plus
/// the mint callback (pays the pool by minting mock tokens — the IIA trick)
/// and the `PoolCreated` announcement.
contract V3CaptureHarness is V3CaptureEvents, IUniswapV3PoolDeployer, IUniswapV3SwapCallback, IUniswapV3MintCallback {
    address public pool;
    MockERC20Capture public token0;
    MockERC20Capture public token1;
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
        token0 = new MockERC20Capture();
        token1 = new MockERC20Capture();
        params = Parameters({
            factory: address(this),
            token0: address(token0),
            token1: address(token1),
            fee: _fee,
            tickSpacing: _tickSpacing
        });
        // The pool CREATE is deferred to `setupPool()` so it runs as a CALL
        // with the FULL transaction gas forwarded (63/64 of ~16M), not the
        // 63/64 fraction remaining inside the constructor (the pattern the
        // V3 swap-oracle harness established for the 22KB code deposit).
    }

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

    /// Announce the pool through the REAL `PoolCreated` event shape (the
    /// harness IS the factory role — it deployed the pool via the deployer
    /// interface), so the updater's creation fetch discovers it.
    function announcePool() external {
        require(pool != address(0), "not setup");
        emit PoolCreated(address(token0), address(token1), fee, tickSpacing, pool);
    }

    /// Drives the real `pool.initialize` (real slot0, real `Initialize` event).
    function initialize(uint160 sqrtPriceX96) external {
        IUniswapV3Pool(pool).initialize(sqrtPriceX96);
    }

    /// Drives the real `pool.mint` with the harness as recipient/owner (the
    /// callback pays; the harness holds the token references).
    function mint(int24 tickLower, int24 tickUpper, uint128 amount)
        external
        returns (uint256 amount0, uint256 amount1)
    {
        (amount0, amount1) = IUniswapV3Pool(pool).mint(address(this), tickLower, tickUpper, amount, "");
    }

    /// Drives the real `pool.burn` (owner = the harness = msg.sender).
    function burn(int24 tickLower, int24 tickUpper, uint128 amount)
        external
        returns (uint256 amount0, uint256 amount1)
    {
        (amount0, amount1) = IUniswapV3Pool(pool).burn(tickLower, tickUpper, amount);
    }

    /// Drives the real `pool.swap` (recipient = the harness).
    function swap(bool zeroForOne, int256 amountSpecified, uint160 sqrtPriceLimitX96)
        external
        returns (int256 amount0, int256 amount1)
    {
        (amount0, amount1) = IUniswapV3Pool(pool).swap(address(this), zeroForOne, amountSpecified, sqrtPriceLimitX96, "");
    }

    function uniswapV3MintCallback(
        uint256 amount0Owed,
        uint256 amount1Owed,
        bytes calldata
    ) external override {
        // The pool calls back on the caller (the harness). Minting to the
        // pool satisfies the `IIA` balance check regardless of seeded
        // balances — the same trick the swap callback uses.
        if (amount0Owed > 0) token0.mint(msg.sender, amount0Owed);
        if (amount1Owed > 0) token1.mint(msg.sender, amount1Owed);
    }

    function uniswapV3SwapCallback(
        int256 amount0Delta,
        int256 amount1Delta,
        bytes calldata
    ) external override {
        if (amount0Delta > 0) token0.mint(msg.sender, uint256(amount0Delta));
        if (amount1Delta > 0) token1.mint(msg.sender, uint256(amount1Delta));
    }
}
