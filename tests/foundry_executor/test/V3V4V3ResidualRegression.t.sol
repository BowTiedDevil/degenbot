// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {ExecutorErrors} from "../src/generated/Errors.sol";
import {IFakeErc20} from "../src/generated/IFakeErc20.sol";
import {IFakeUniswapV3Pool} from "../src/generated/IFakeUniswapV3Pool.sol";
import {PoolKey} from "../src/generated/Types.sol";

/// Port of `tests/executor/test_v3_v4_v3_residual_regression.py`.
///
/// V3-V4-V3 CurrencyNotSettled regression: the fake V4 pool OVER-PRODUCES
/// (`b_out_actual = b_out + WBTC_SURPLUS`), the case that leaves a positive
/// residual forward_b delta with the old static-amount encoder.
///
///   OLD: V4_SETTLE + V4_SWAP_COMPACT(a_out) + V4_TAKE_COMPACT(b_out)
///   NEW: V4_SETTLE + V4_SWAP_DYNAMIC + V4_TAKE_DELTA → V3c + V4_SETTLE_ALL
///
/// The nesting is unchanged between OLD and NEW — V3a's optimistic output
/// transfer delivers forward_a to the PM before V3a's callback runs the V4
/// unlock. The bug is the static amounts, not the nesting.
contract V3V4V3ResidualRegressionTest is PoolSetup {
    uint256 internal constant AMOUNT_WETH_PROFIT = 2 ether;

    /// Surplus the V4 swap produces beyond the solver's `b_out` — the residual
    /// forward_b delta that triggers CurrencyNotSettled under the old encoder.
    uint256 internal constant WBTC_SURPLUS = 5e8;

    IFakeUniswapV3Pool internal v3a; // WETH/USDC
    IFakeUniswapV3Pool internal v3c; // WBTC/WETH

    function setUp() public override {
        super.setUp();
        (address a0, address a1) = _sorted(address(weth), address(usdc));
        v3a = _deployV3Pool(a0, a1, 0, 3000);
        (address c0, address c1) = _sorted(address(wbtc), address(weth));
        v3c = _deployV3Pool(c0, c1, 0, 3000);
    }

    /// Program the fake V4 USDC→WBTC pool.
    function _setupV4UsdcWbtc(uint256 amountIn, uint256 amountOut)
        internal
        returns (PoolKey memory key, bool zfo)
    {
        key = _poolKey(address(usdc), address(wbtc), 500, 10);
        zfo = key.currency0 == address(usdc);
        _setupV4Swap(v4Pm, key, amountIn, amountOut, zfo, address(wbtc));
    }

    struct Residual {
        bool aZfo;
        uint256 aOut; // V3a's actual USDC output (the V4 pool's actual input)
        PoolKey bPk;
        bool bZfo;
        uint256 bOut; // the solver's WBTC prediction (V3c's exact input)
        bool cZfo;
    }

    function _setupPools() internal returns (Residual memory r) {
        (r.aZfo, r.aOut) = _setupV3(v3a, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        // V4 produces MORE than the solver's b_out (over-production).
        (r.bPk, r.bZfo) = _setupV4UsdcWbtc(r.aOut, AMOUNT_WBTC + WBTC_SURPLUS);
        r.bOut = AMOUNT_WBTC;
        (r.cZfo,) = _setupV3(v3c, wbtc, IFakeErc20(address(weth)), r.bOut, AMOUNT_WETH_PROFIT);
    }

    /// The source builds its table with sentinel roles — WETH, executor, PM,
    /// USDC (user0) and WBTC (user1) resolve to reserved indices and never
    /// take SET_ADDRESS entries; only V3c and V3a are appended.
    function _sentinelTable() internal view returns (Cmd.Table memory at) {
        at = _emptyTable();
        at.weth = address(weth);
        at.executor = address(executor);
        at.pm = address(v4Pm);
        at.user0 = address(usdc);
        at.user1 = address(wbtc);
    }

    /// OLD static-amount encoder under V4 over-production.
    ///
    /// The source executes with `config = 0` and asserts the transaction
    /// reverts — an intentional, exact-revert characterization (manifest
    /// disposition 10). Naming the revert identity precisely: `execute` checks
    /// the profit mode BEFORE any command runs (`assert profit_mode == 1 or 2`),
    /// so under the source's `config = 0` the boundary reject is exactly
    /// `InvalidProfitMode(0)` — the intentional current contract behavior under
    /// this configuration. The over-production residual this suite documents
    /// would surface as the PM's CurrencyNotSettled once the mode check passes.
    /// The OLD static over-production stream, shared by the config=0 source
    /// characterization and the valid-config residual proof below.
    function _buildOldStaticCommands(Residual memory r)
        internal
        view
        returns (Cmd.Table memory at, bytes memory stream)
    {
        at = _sentinelTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3cIdx = Cmd.add(at, address(v3c));
        uint8 v3aIdx = Cmd.add(at, address(v3a));
        uint8 pmIdx = Cmd.add(at, address(v4Pm));

        // OLD: SETTLE + V4_SWAP_COMPACT(a_out) + V4_TAKE_COMPACT(b_out).
        // b_out_actual > b_out → residual forward_b delta survives → revert.
        bytes memory v4Inner = Cmd.v4Settle();
        v4Inner = bytes.concat(
            v4Inner,
            Cmd.v4SwapCompact(
                Cmd.add(at, r.bPk.currency0),
                Cmd.add(at, r.bPk.currency1),
                500,
                10,
                Cmd.SENTINEL_NATIVE,
                r.bZfo,
                r.aOut
            )
        );
        v4Inner = bytes.concat(v4Inner, Cmd.v4TakeCompact(wbtcIdx, v3cIdx, r.bOut));

        bytes memory aFwd = Cmd.erc20Transfer(wethIdx, v3aIdx, AMOUNT_WETH);
        aFwd = bytes.concat(aFwd, Cmd.v4Unlock(v4Inner));
        bytes memory cFwd = Cmd.v3SwapCompact(v3aIdx, r.aZfo, AMOUNT_WETH, pmIdx, aFwd);
        bytes memory commands = Cmd.v4Sync(usdcIdx);
        commands =
            bytes.concat(commands, Cmd.v3SwapCompact(v3cIdx, r.cZfo, r.bOut, executorIdx, cFwd));

        return (at, bytes.concat(Cmd.preamble(at), commands));
    }

    function testOldStaticEncoderRevertsOnV4Overproduction() public {
        Residual memory r = _setupPools();
        (Cmd.Table memory at, bytes memory stream) = _buildOldStaticCommands(r);

        Outcome memory o = _executeRaw(executor, stream, 0);
        _assertRevertData(
            o,
            _executorRevert(ExecutorErrors.INVALID_PROFIT_MODE, 0),
            "old static encoder under config=0"
        );
    }

    /// NEW dynamic/delta encoder under the same over-production: succeeds and
    /// profits. The source's `run_executor(...)` default is profit mode 1.
    function testNewDynamicEncoderSucceedsOnV4Overproduction() public {
        Residual memory r = _setupPools();
        Cmd.Table memory at = _sentinelTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3cIdx = Cmd.add(at, address(v3c));
        uint8 v3aIdx = Cmd.add(at, address(v3a));
        uint8 pmIdx = Cmd.add(at, address(v4Pm));

        // NEW: SETTLE + V4_SWAP_DYNAMIC + V4_TAKE_DELTA(forward_b→v3c)
        //      + V4_SETTLE_ALL — consumes the actual settled forward_a, takes
        //      the full actual forward_b, sweeps dust → no residual.
        bytes memory v4Inner = Cmd.v4Settle();
        v4Inner = bytes.concat(
            v4Inner,
            Cmd.v4SwapDynamic(
                Cmd.add(at, r.bPk.currency0),
                Cmd.add(at, r.bPk.currency1),
                500,
                10,
                Cmd.SENTINEL_NATIVE,
                r.bZfo
            )
        );
        v4Inner = bytes.concat(v4Inner, Cmd.v4TakeDelta(wbtcIdx, v3cIdx));
        v4Inner = bytes.concat(v4Inner, Cmd.v4SettleAll());

        bytes memory aFwd = Cmd.erc20Transfer(wethIdx, v3aIdx, AMOUNT_WETH);
        aFwd = bytes.concat(aFwd, Cmd.v4Unlock(v4Inner));
        bytes memory cFwd = Cmd.v3SwapCompact(v3aIdx, r.aZfo, AMOUNT_WETH, pmIdx, aFwd);
        bytes memory commands = Cmd.v4Sync(usdcIdx);
        commands =
            bytes.concat(commands, Cmd.v3SwapCompact(v3cIdx, r.cZfo, r.bOut, executorIdx, cFwd));

        uint256 wethBefore = weth.balanceOf(address(executor));
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 1);
        uint256 profit = weth.balanceOf(address(executor)) - wethBefore;
        assertGt(
            profit, 0, "executor must profit (V3c WETH output minus V3a WETH input)"
        );
    }

    /// The identical old static over-production stream under the
    /// profit-checked `config = 1` — the configuration that actually executes
    /// the stream and proves the documented residual regression. The V4 swap produces
    /// `b_out + WBTC_SURPLUS` while `V4_TAKE_COMPACT` removes only `b_out`, so
    /// a positive WBTC delta survives at unlock-end and the PoolManager raises
    /// the exact `CurrencyNotSettled:<currency>: +<residual>` string (the fake
    /// PM's computed raise format, pinned address-and-amount exact via
    /// `vm.toString`).
    function testOldStaticEncoderLeavesResidualWithValidConfig() public {
        Residual memory r = _setupPools();
        (, bytes memory stream) = _buildOldStaticCommands(r);

        Outcome memory o = _executeRaw(executor, stream, 1);
        string memory expectedMessage = string.concat(
            "CurrencyNotSettled:",
            vm.toString(address(wbtc)),
            ":+",
            vm.toString(WBTC_SURPLUS)
        );
        _assertRevertData(
            o, abi.encodeWithSignature("Error(string)", expectedMessage), "old static residual"
        );
    }
}
