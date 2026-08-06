// SPDX-License-Identifier: MIT
pragma solidity 0.7.6;

import {UniswapV3Pool} from "v3-core/contracts/UniswapV3Pool.sol";
import {IUniswapV3PoolDeployer} from "v3-core/contracts/interfaces/IUniswapV3PoolDeployer.sol";
import {IUniswapV3Pool} from "v3-core/contracts/interfaces/IUniswapV3Pool.sol";

/// Minimal ERC20 with `balances` at storage slot 0 so a revm `CacheDB` test
/// oracle can seed/deploy without a full token implementation. Arithmetic is
/// Solidity 0.7 wrapping (unchecked). Shared by BOTH V3 pools in the executor
/// topology so the route token (WETH) is the SAME contract across hop0/hop2.
contract MockERC20Executor {
    mapping(address => uint256) public balanceOf; // slot 0

    function transfer(address to, uint256 amount) external returns (bool) {
        balanceOf[msg.sender] -= amount; // 0.7 wrapping — never reverts
        balanceOf[to] += amount;
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
        return true;
    }

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }
}

/// Tier-3b executor-topology V3 harness (BHL2R2 / tier-3b family). Deploys TWO
/// real `UniswapV3Pool`s from v3-core as real bytecode, using EXTERNAL token
/// addresses supplied by the caller — so the Rust revm test can make hop0 and
/// hop2 route through a SINGLE shared WETH contract (the executor is paid in
/// hop0's WETH output and must pay hop2's WETH input, so both pools MUST point
/// at the same WETH). The Rust side seeds each pool's
/// `slot0`/`liquidity`/`ticks`/`tickBitmap` slots directly (see
/// `degenbot_pools::v3_storage_slots`) — NO `initialize`/`mint` needed.
///
/// Pool construction uses v3-core's CREATE2 surrogate deployer: `parameters()`
/// is read by the pool at construction, so we swap `params` before each
/// `new UniswapV3Pool()`. Because the pool CREATE is gas-hungry (~4.4M for the
/// 22KB deposit), pool creation is deferred to `setupPools()` so it runs as a
/// CALL with full gas, not the constructor's 63/64 fraction.
contract ExecutorV3Harness is IUniswapV3PoolDeployer {
    address public poolA; // MATIC / WETH (hop0)
    address public poolB; // UNI  / WETH (hop2)
    // Currencies are minted/transferable real ERC20s so the executor's V3
    // callbacks (it pays pools in the swap callback) resolve to real balances.
    MockERC20Executor public matic;
    MockERC20Executor public uni;
    MockERC20Executor public weth;

    uint24 public feeA_;
    int24 public tickSpacingA_;
    uint24 public feeB_;
    int24 public tickSpacingB_;

    struct Parameters {
        address factory;
        address token0;
        address token1;
        uint24 fee;
        int24 tickSpacing;
    }
    Parameters private params;

    constructor(
        uint24 feeA,
        int24 tickSpacingA,
        uint24 feeB,
        int24 tickSpacingB
    ) {
        feeA_ = feeA;
        tickSpacingA_ = tickSpacingA;
        feeB_ = feeB;
        tickSpacingB_ = tickSpacingB;
        // Deploy the shared route tokens. WETH is canonical, MATIC/UNI are the
        // V4-side currencies the pool key uses.
        matic = new MockERC20Executor();
        uni = new MockERC20Executor();
        weth = new MockERC20Executor();
    }

    /// Deploys both real UniswapV3Pools with the caller-supplied token order.
    /// `tokenA0/1` order both pools; poolA uses feeA, poolB feeB. Call after
    /// construction, with full transaction gas.
    function setupPools(
        address tokenA0,
        address tokenA1,
        address tokenB0,
        address tokenB1
    ) external {
        require(address(poolA) == address(0), "already setup");

        params = Parameters(address(this), tokenA0, tokenA1, feeA_, tickSpacingA_);
        poolA = address(new UniswapV3Pool());

        params = Parameters(address(this), tokenB0, tokenB1, feeB_, tickSpacingB_);
        poolB = address(new UniswapV3Pool());
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
}
