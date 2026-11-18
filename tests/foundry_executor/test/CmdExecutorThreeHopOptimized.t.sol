// SPDX-License-Identifier: MIT

pragma solidity ^0.8.28;

import {Vm} from "../src/Vm.sol";
import {PoolSetup} from "../src/Setup.sol";
import {ConservationOps} from "../src/ConservationOps.sol";
import {Cmd} from "../src/Cmd.sol";
import {IFakeErc20} from "../src/generated/IFakeErc20.sol";
import {IFakeUniswapV2Pair} from "../src/generated/IFakeUniswapV2Pair.sol";
import {IFakeUniswapV3Pool} from "../src/generated/IFakeUniswapV3Pool.sol";
import {PoolKey} from "../src/generated/Types.sol";

/// Port of `tests/executor/test_cmd_executor_three_hop_optimized.py`.
///
/// Optimized (minimum-transfer) routing for all 27 V2/V3/V4 three-hop
/// permutations, applying the direct-custody rules from docs/pool-mechanics.md:
///
/// 1. V2→V2: executor pre-funds V2a, then V2_SWAP_DIRECT chains through excess
///    balance (4 transfers)
/// 2. V2→V3: V2 called inside V3b's callback with to=V3b, output arrives during
///    the callback (IIA satisfied) — saves 1
/// 3. V2→V4: executor sends to the PM via sync+send+settle inside the V2
///    callback — saves 1
/// 4. V3→V2: V3 sends output directly to the V2 pair (excess → V2_SWAP_DIRECT)
///    — saves 1
/// 5. V3→V3: reverse-order direct custody — the inner V3 fires first, the outer
///    sends during the callback — saves 2
/// 6. V3→V4: V3→PM (sync before swap, settle inside unlock) + V4_TAKE→V3a (IIA
///    satisfied) — saves 1-2
/// 7. V4→V2: V4_TAKE sends tokens directly to the V2 pair (excess →
///    V2_SWAP_DIRECT) — saves 1
/// 8. V4→V3: reverse-order from V3 — V4_TAKE→V3b during V3b's callback
///    satisfies IIA — saves 1
/// 9. V4→V4: delta netting — zero internal transfers in the same unlock — saves
///    2/pair
///
/// Every row executes through `_runAndVerify`, which snapshots the tracked
/// accounts, runs the stream with the source's profit-mode selection, asserts
/// the exact on-chain transfer count, and verifies tracked-account token
/// conservation through the shared `ConservationOps` composed helper (WETH+ETH
/// combined, every other tracked token summed to zero — the source
/// `_run_and_verify`/`_verify_conservation` pair).
///
/// The source's `record_gas`/`.gas-results` surface is replaced by the
/// committed Forge gas snapshot (`just test-executor-snapshot`): each test
/// name is the stable per-row label, and `just test-executor-snapshot-check`
/// fails on snapshot drift.
contract CmdExecutorThreeHopOptimizedTest is PoolSetup, ConservationOps {
    uint256 internal constant AMOUNT_WETH_PROFIT = 2 ether;

    IFakeUniswapV2Pair internal v2A; // WETH/USDC
    IFakeUniswapV2Pair internal v2B; // USDC/WBTC
    IFakeUniswapV2Pair internal v2C; // WBTC/WETH
    IFakeUniswapV3Pool internal v3A; // WETH/USDC
    IFakeUniswapV3Pool internal v3B; // USDC/WBTC
    IFakeUniswapV3Pool internal v3C; // WBTC/WETH

    function setUp() public override {
        super.setUp();
        (address a0, address a1) = _sorted(address(weth), address(usdc));
        v2A = _deployV2Pair(a0, a1, 0, V2_FEE);
        (address b0, address b1) = _sorted(address(usdc), address(wbtc));
        v2B = _deployV2Pair(b0, b1, 0, V2_FEE);
        (address c0, address c1) = _sorted(address(wbtc), address(weth));
        v2C = _deployV2Pair(c0, c1, 0, V2_FEE);
        v3A = _deployV3Pool(a0, a1, 0, 3000);
        v3B = _deployV3Pool(b0, b1, 0, 3000);
        v3C = _deployV3Pool(c0, c1, 0, 3000);
    }

    // ── Shared setup helpers ──────────────────────────────────────────────

    /// The source AddressTable shape: WETH, executor, PM (when the row has one),
    /// USDC (user0) and WBTC (user1) resolve to reserved sentinels and never
    /// take SET_ADDRESS entries; only pool addresses are appended.
    function _sentinelTable(address pm) internal view returns (Cmd.Table memory at) {
        at = _emptyTable();
        at.weth = address(weth);
        at.executor = address(executor);
        at.pm = pm;
        at.user0 = address(usdc);
        at.user1 = address(wbtc);
    }

    /// V2 pair funded at the correct price for V2_SWAP_DIRECT (the source
    /// `_setup_v2_for_calc`).
    function _setupV2ForCalc(
        IFakeUniswapV2Pair pool,
        IFakeErc20 inputToken,
        IFakeErc20 outputToken,
        uint256 amountIn,
        uint256 amountOut
    ) internal returns (bool zfo) {
        inputToken.mint(address(pool), amountIn * 100);
        outputToken.mint(address(pool), amountOut * 100);
        pool.sync();
        zfo = pool.token0() == address(inputToken);
    }

    /// Program the fake PoolManager's next swap and fund the output side.
    function _setupV4Pool(
        address c0,
        address c1,
        uint256 amountIn,
        uint256 amountOut,
        uint24 fee,
        int24 tickSpacing,
        address outputToken
    ) internal returns (PoolKey memory key, bool zfo) {
        key = _poolKey(c0, c1, fee, tickSpacing);
        zfo = key.currency0 == c0;
        _setupV4Swap(v4Pm, key, amountIn, amountOut, zfo, outputToken);
    }

    // ── Stream-building helpers (the source module-level encoders) ─────────

    function _v2SwapDirect(
        Cmd.Table memory at,
        IFakeUniswapV2Pair pool,
        bool zfo,
        uint256 amountOut,
        uint8 recipientIdx
    ) internal pure returns (bytes memory) {
        return Cmd.v2SwapDirect(Cmd.add(at, address(pool)), zfo, amountOut, recipientIdx);
    }

    function _v2Swap(
        Cmd.Table memory at,
        IFakeUniswapV2Pair pool,
        bool zfo,
        uint256 amountOut,
        uint8 recipientIdx,
        bytes memory forwardData
    ) internal pure returns (bytes memory) {
        return Cmd.v2SwapCompact(Cmd.add(at, address(pool)), zfo, amountOut, recipientIdx, V2_FEE, forwardData);
    }

    function _v3Swap(
        Cmd.Table memory at,
        IFakeUniswapV3Pool pool,
        bool zfo,
        uint256 amount,
        uint8 recipientIdx,
        bytes memory forwardData
    ) internal pure returns (bytes memory) {
        return Cmd.v3SwapCompact(Cmd.add(at, address(pool)), zfo, amount, recipientIdx, forwardData);
    }

    function _v4Swap(Cmd.Table memory at, PoolKey memory key, bool zfo, uint256 amount)
        internal
        pure
        returns (bytes memory)
    {
        return Cmd.v4SwapCompact(
            Cmd.add(at, key.currency0),
            Cmd.add(at, key.currency1),
            uint16(key.fee),
            int16(key.tick_spacing),
            Cmd.SENTINEL_NATIVE, // the source's hooks byte 0xFF
            zfo,
            amount
        );
    }

    function _erc20Xfer(Cmd.Table memory at, address token, address recipient, uint256 amount)
        internal
        pure
        returns (bytes memory)
    {
        return Cmd.erc20Transfer(Cmd.add(at, token), Cmd.add(at, recipient), amount);
    }

    // ── Execute + verify (the source `_run_and_verify`) ───────────────────

    function _tokens() internal view returns (address[] memory t) {
        t = new address[](4);
        t[0] = address(weth);
        t[1] = address(usdc);
        t[2] = address(wbtc);
        t[3] = NATIVE;
    }

    function _accounts1(address b) internal view returns (address[] memory a) {
        a = new address[](2);
        a[0] = address(executor);
        a[1] = b;
    }

    function _accounts2(address b, address c) internal view returns (address[] memory a) {
        a = new address[](3);
        a[0] = address(executor);
        a[1] = b;
        a[2] = c;
    }

    function _accounts3(address b, address c, address d) internal view returns (address[] memory a) {
        a = new address[](4);
        a[0] = address(executor);
        a[1] = b;
        a[2] = c;
        a[3] = d;
    }

    /// Snapshot, execute with the row's profit mode, assert the exact transfer
    /// count from on-chain events, then verify tracked-account conservation.
    ///
    /// The source leaves `expected_weth_delta` unset on every row, so the
    /// executor-profit pin is off and only the conservation invariants run —
    /// the same shape the source's `_verify_conservation` takes.
    function _runAndVerify(
        Cmd.Table memory at,
        bytes memory commands,
        address[] memory accounts,
        uint256 expectedTransfers,
        string memory label,
        uint256 mode
    ) internal {
        address[] memory tokens = _tokens();
        BalanceSnapshot memory before = _snapshotBalances(tokens, accounts);
        vm.recordLogs();
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), mode);
        Vm.Log[] memory logs = vm.getRecordedLogs();

        assertEq(_countTransfers(logs), expectedTransfers, string.concat(label, ": transfer count mismatch"));

        BalanceSnapshot memory afterSnap = _snapshotBalances(tokens, accounts);
        _verifyConservation(before, afterSnap, address(executor), false, 0, label);
    }

    function _profit() internal pure returns (uint256) {
        return AMOUNT_WETH_PROFIT - AMOUNT_WETH;
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 1. V2-V2-V2 — 4 transfers: reverse-order flash borrow (zero balance).
    //
    // V2c fires first and sends the WETH profit to the executor; inside V2c's
    // callback: ERC20_TRANSFER WETH→V2a (excess), V2a→V2b, V2b→V2c (pays the
    // flash borrow). Each V2 pair has its own reentrancy guard, and
    // V2_SWAP_DIRECT calls swap(data="") — no callback on the recipient.
    // ═══════════════════════════════════════════════════════════════════════

    function testV2V2V2() public {
        bool aZfo = _setupV2ForCalc(v2A, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        bool bZfo = _setupV2ForCalc(v2B, usdc, IFakeErc20(address(wbtc)), AMOUNT_USDC, AMOUNT_WBTC);
        bool cZfo =
            _setupV2ForCalc(v2C, IFakeErc20(address(wbtc)), IFakeErc20(address(weth)), AMOUNT_WBTC, AMOUNT_WETH_PROFIT);
        uint256 aOut = _v2GetAmountOut(AMOUNT_WETH, weth.balanceOf(address(v2A)), usdc.balanceOf(address(v2A)), V2_FEE);
        uint256 bOut = _v2GetAmountOut(aOut, usdc.balanceOf(address(v2B)), wbtc.balanceOf(address(v2B)), V2_FEE);
        uint256 cOut = _v2GetAmountOut(bOut, wbtc.balanceOf(address(v2C)), weth.balanceOf(address(v2C)), V2_FEE);

        Cmd.Table memory at = _sentinelTable(address(0));
        uint8 executorIdx = Cmd.add(at, address(executor));

        bytes memory cFwd = _erc20Xfer(at, address(weth), address(v2A), AMOUNT_WETH);
        cFwd = bytes.concat(cFwd, _v2SwapDirect(at, v2A, aZfo, aOut, Cmd.add(at, address(v2B))));
        cFwd = bytes.concat(cFwd, _v2SwapDirect(at, v2B, bZfo, bOut, Cmd.add(at, address(v2C))));

        bytes memory commands = _v2Swap(at, v2C, cZfo, cOut, executorIdx, cFwd);
        _runAndVerify(at, commands, _accounts3(address(v2A), address(v2B), address(v2C)), 4, "TestV2V2V2", 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 2. V2-V2-V3 — 4 transfers: reverse-order flash borrow.
    //
    // V3c fires first (sends WETH to the executor); inside V3c's callback:
    // WETH→V2a, V2a→V2b, V2b→V3c. V3c's IIA check sees the WBTC V2b deposited
    // during the callback.
    // ═══════════════════════════════════════════════════════════════════════

    function testV2V2V3() public {
        bool aZfo = _setupV2ForCalc(v2A, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        bool bZfo = _setupV2ForCalc(v2B, usdc, IFakeErc20(address(wbtc)), AMOUNT_USDC, AMOUNT_WBTC);
        uint256 aOut = _v2GetAmountOut(AMOUNT_WETH, weth.balanceOf(address(v2A)), usdc.balanceOf(address(v2A)), V2_FEE);
        uint256 bOut = _v2GetAmountOut(aOut, usdc.balanceOf(address(v2B)), wbtc.balanceOf(address(v2B)), V2_FEE);
        (bool cZfo,) = _setupV3(v3C, IFakeErc20(address(wbtc)), IFakeErc20(address(weth)), bOut, AMOUNT_WETH_PROFIT);

        Cmd.Table memory at = _sentinelTable(address(0));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3cIdx = Cmd.add(at, address(v3C));

        bytes memory cFwd = _erc20Xfer(at, address(weth), address(v2A), AMOUNT_WETH);
        cFwd = bytes.concat(cFwd, _v2SwapDirect(at, v2A, aZfo, aOut, Cmd.add(at, address(v2B))));
        cFwd = bytes.concat(cFwd, _v2SwapDirect(at, v2B, bZfo, bOut, v3cIdx));

        bytes memory commands = _v3Swap(at, v3C, cZfo, bOut, executorIdx, cFwd);
        _runAndVerify(at, commands, _accounts3(address(v2A), address(v2B), address(v3C)), 4, "TestV2V2V3", 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 3. V2-V2-V4 — 4 transfers: V4_TAKE→V2a direct, V2b→PM delta netting.
    //
    // V4c swap → V4_TAKE WETH→V2a (excess) → V2a V2_SWAP_DIRECT→V2b →
    // V4_SYNC + V2b V2_SWAP_DIRECT→PM → V4_SETTLE. The WBTC delta from the V2b
    // deposit nets V4c's debit internally; the WETH take is the profit.
    // ═══════════════════════════════════════════════════════════════════════

    function testV2V2V4() public {
        bool aZfo = _setupV2ForCalc(v2A, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        bool bZfo = _setupV2ForCalc(v2B, usdc, IFakeErc20(address(wbtc)), AMOUNT_USDC, AMOUNT_WBTC);
        uint256 aOut = _v2GetAmountOut(AMOUNT_WETH, weth.balanceOf(address(v2A)), usdc.balanceOf(address(v2A)), V2_FEE);
        uint256 bOut = _v2GetAmountOut(aOut, usdc.balanceOf(address(v2B)), wbtc.balanceOf(address(v2B)), V2_FEE);
        (PoolKey memory cPk, bool cZfo) =
            _setupV4Pool(address(wbtc), address(weth), bOut, AMOUNT_WETH_PROFIT, 10_000, 200, address(weth));

        Cmd.Table memory at = _sentinelTable(address(v4Pm));
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 pmIdx = Cmd.add(at, address(v4Pm));
        uint8 executorIdx = Cmd.add(at, address(executor));

        bytes memory inner = _v4Swap(at, cPk, cZfo, bOut);
        inner = bytes.concat(inner, Cmd.v4TakeCompact(wethIdx, Cmd.add(at, address(v2A)), AMOUNT_WETH));
        inner = bytes.concat(inner, _v2SwapDirect(at, v2A, aZfo, aOut, Cmd.add(at, address(v2B))));
        inner = bytes.concat(inner, Cmd.v4Sync(wbtcIdx));
        inner = bytes.concat(inner, _v2SwapDirect(at, v2B, bZfo, bOut, pmIdx));
        inner = bytes.concat(inner, Cmd.v4Settle());
        inner = bytes.concat(inner, Cmd.v4TakeCompact(wethIdx, executorIdx, _profit()));

        _runAndVerify(
            at, Cmd.v4Unlock(inner), _accounts3(address(v4Pm), address(v2A), address(v2B)), 4, "TestV2V2V4", 1
        );
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 4. V2-V3-V2 — 4 transfers: reverse-order from V2c.
    //
    // V2c fires first; inside V2c's callback V3b swaps WBTC→V2c (K-invariant),
    // and V3b's callback pre-funds V2a with WETH then V2a→V3b sends the USDC
    // during the callback (IIA satisfied).
    // ═══════════════════════════════════════════════════════════════════════

    function testV2V3V2() public {
        weth.mint(address(executor), AMOUNT_WETH);
        bool aZfo = _setupV2ForCalc(v2A, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        uint256 aOut = _v2GetAmountOut(AMOUNT_WETH, weth.balanceOf(address(v2A)), usdc.balanceOf(address(v2A)), V2_FEE);
        (bool bZfo, uint256 bOut) = _setupV3(v3B, usdc, IFakeErc20(address(wbtc)), aOut, AMOUNT_WBTC);
        bool cZfo = _setupV2ForCalc(v2C, IFakeErc20(address(wbtc)), IFakeErc20(address(weth)), bOut, AMOUNT_WETH_PROFIT);
        uint256 cOut = _v2GetAmountOut(bOut, wbtc.balanceOf(address(v2C)), weth.balanceOf(address(v2C)), V2_FEE);

        Cmd.Table memory at = _sentinelTable(address(0));
        uint8 executorIdx = Cmd.add(at, address(executor));

        bytes memory bFwd = _erc20Xfer(at, address(weth), address(v2A), AMOUNT_WETH);
        bFwd = bytes.concat(bFwd, _v2SwapDirect(at, v2A, aZfo, aOut, Cmd.add(at, address(v3B))));

        bytes memory cFwd = _v3Swap(at, v3B, bZfo, aOut, Cmd.add(at, address(v2C)), bFwd);
        bytes memory commands = _v2Swap(at, v2C, cZfo, cOut, executorIdx, cFwd);
        _runAndVerify(at, commands, _accounts3(address(v2A), address(v3B), address(v2C)), 4, "TestV2V3V2", 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 5. V2-V3-V3 — 4 transfers: V3c outermost, V2a inside V3b's callback.
    //
    // V2a is pre-funded with excess WETH and swaps to=V3b, so its optimistic
    // USDC transfer lands during V3b's callback (IIA satisfied).
    // ═══════════════════════════════════════════════════════════════════════

    function testV2V3V3() public {
        (bool aZfo, uint256 aOut) = _setupV2Pair(v2A, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, V2_FEE);
        (bool bZfo, uint256 bOut) = _setupV3(v3B, usdc, IFakeErc20(address(wbtc)), aOut, AMOUNT_WBTC);
        (bool cZfo,) = _setupV3(v3C, IFakeErc20(address(wbtc)), IFakeErc20(address(weth)), bOut, AMOUNT_WETH_PROFIT);

        Cmd.Table memory at = _sentinelTable(address(0));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3bIdx = Cmd.add(at, address(v3B));
        uint8 v3cIdx = Cmd.add(at, address(v3C));

        bytes memory v3bFwd = _erc20Xfer(at, address(weth), address(v2A), AMOUNT_WETH);
        v3bFwd = bytes.concat(v3bFwd, _v2SwapDirect(at, v2A, aZfo, aOut, v3bIdx));

        bytes memory v3cFwd = _v3Swap(at, v3B, bZfo, aOut, v3cIdx, v3bFwd);
        bytes memory commands = _v3Swap(at, v3C, cZfo, bOut, executorIdx, v3cFwd);
        _runAndVerify(at, commands, _accounts3(address(v2A), address(v3B), address(v3C)), 4, "TestV2V3V3", 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 6. V2-V3-V4 — 4 transfers: V3b outermost (sends WBTC to the PM).
    //
    // Inside V3b's callback the V4 unlock takes WETH→V2a (excess) and V2a→V3b
    // sends the USDC during the callback (IIA satisfied).
    // ═══════════════════════════════════════════════════════════════════════

    function testV2V3V4() public {
        (bool aZfo, uint256 aOut) = _setupV2Pair(v2A, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, V2_FEE);
        (bool bZfo, uint256 bOut) = _setupV3(v3B, usdc, IFakeErc20(address(wbtc)), aOut, AMOUNT_WBTC);
        (PoolKey memory cPk, bool cZfo) =
            _setupV4Pool(address(wbtc), address(weth), bOut, AMOUNT_WETH_PROFIT, 10_000, 200, address(weth));

        Cmd.Table memory at = _sentinelTable(address(v4Pm));
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 pmIdx = Cmd.add(at, address(v4Pm));

        bytes memory v4Inner = Cmd.v4Settle();
        v4Inner = bytes.concat(v4Inner, _v4Swap(at, cPk, cZfo, bOut));
        v4Inner = bytes.concat(v4Inner, Cmd.v4TakeCompact(wethIdx, Cmd.add(at, address(v2A)), AMOUNT_WETH));
        v4Inner = bytes.concat(v4Inner, Cmd.v4TakeCompact(wethIdx, Cmd.add(at, address(executor)), _profit()));

        bytes memory bFwd = Cmd.v4Unlock(v4Inner);
        bFwd = bytes.concat(bFwd, _v2SwapDirect(at, v2A, aZfo, aOut, Cmd.add(at, address(v3B))));

        bytes memory commands = bytes.concat(Cmd.v4Sync(wbtcIdx), _v3Swap(at, v3B, bZfo, aOut, pmIdx, bFwd));
        _runAndVerify(at, commands, _accounts3(address(v4Pm), address(v2A), address(v3B)), 4, "TestV2V3V4", 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 7. V2-V4-V2 — 4 transfers: reverse-order from V2c.
    //
    // V2c fires first; inside V2c's callback: WETH→V2a (excess), then the V4
    // unlock with V2a→PM (USDC delta netting), the V4b swap, and V4_TAKE
    // WBTC→V2c (K-invariant satisfied directly).
    // ═══════════════════════════════════════════════════════════════════════

    function testV2V4V2() public {
        weth.mint(address(executor), AMOUNT_WETH);
        bool aZfo = _setupV2ForCalc(v2A, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        uint256 aOut = _v2GetAmountOut(AMOUNT_WETH, weth.balanceOf(address(v2A)), usdc.balanceOf(address(v2A)), V2_FEE);
        (PoolKey memory bPk, bool bZfo) =
            _setupV4Pool(address(usdc), address(wbtc), aOut, AMOUNT_WBTC, 500, 10, address(wbtc));
        bool cZfo =
            _setupV2ForCalc(v2C, IFakeErc20(address(wbtc)), IFakeErc20(address(weth)), AMOUNT_WBTC, AMOUNT_WETH_PROFIT);
        uint256 cOut = _v2GetAmountOut(AMOUNT_WBTC, wbtc.balanceOf(address(v2C)), weth.balanceOf(address(v2C)), V2_FEE);

        Cmd.Table memory at = _sentinelTable(address(v4Pm));
        Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 pmIdx = Cmd.add(at, address(v4Pm));
        uint8 executorIdx = Cmd.add(at, address(executor));

        bytes memory v4Inner = Cmd.v4Sync(usdcIdx);
        v4Inner = bytes.concat(v4Inner, _v2SwapDirect(at, v2A, aZfo, aOut, pmIdx));
        v4Inner = bytes.concat(v4Inner, Cmd.v4Settle());
        v4Inner = bytes.concat(v4Inner, _v4Swap(at, bPk, bZfo, aOut));
        v4Inner = bytes.concat(v4Inner, Cmd.v4TakeCompact(wbtcIdx, Cmd.add(at, address(v2C)), AMOUNT_WBTC));

        bytes memory cFwd = _erc20Xfer(at, address(weth), address(v2A), AMOUNT_WETH);
        cFwd = bytes.concat(cFwd, Cmd.v4Unlock(v4Inner));

        bytes memory commands = _v2Swap(at, v2C, cZfo, cOut, executorIdx, cFwd);
        _runAndVerify(at, commands, _accounts3(address(v4Pm), address(v2A), address(v2C)), 4, "TestV2V4V2", 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 8. V2-V4-V3 — 4 transfers: V3c fires first.
    //
    // Inside V3c's callback: WETH→V2a (excess), then the V4 unlock syncs USDC
    // BEFORE the V2a deposit, V2a→PM, settles to credit the delta, swaps V4b,
    // and takes WBTC→V3c (IIA satisfied during the callback).
    // ═══════════════════════════════════════════════════════════════════════

    function testV2V4V3() public {
        bool aZfo = _setupV2ForCalc(v2A, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        uint256 aOut = _v2GetAmountOut(AMOUNT_WETH, weth.balanceOf(address(v2A)), usdc.balanceOf(address(v2A)), V2_FEE);
        (PoolKey memory bPk, bool bZfo) =
            _setupV4Pool(address(usdc), address(wbtc), aOut, AMOUNT_WBTC, 500, 10, address(wbtc));
        uint256 bOut = AMOUNT_WBTC;
        (bool cZfo,) = _setupV3(v3C, IFakeErc20(address(wbtc)), IFakeErc20(address(weth)), bOut, AMOUNT_WETH_PROFIT);

        Cmd.Table memory at = _sentinelTable(address(v4Pm));
        Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 pmIdx = Cmd.add(at, address(v4Pm));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3cIdx = Cmd.add(at, address(v3C));

        bytes memory v4Inner = Cmd.v4Sync(usdcIdx);
        v4Inner = bytes.concat(v4Inner, _v2SwapDirect(at, v2A, aZfo, aOut, pmIdx));
        v4Inner = bytes.concat(v4Inner, Cmd.v4Settle());
        v4Inner = bytes.concat(v4Inner, _v4Swap(at, bPk, bZfo, aOut));
        v4Inner = bytes.concat(v4Inner, Cmd.v4TakeCompact(wbtcIdx, v3cIdx, AMOUNT_WBTC));

        bytes memory cFwd = _erc20Xfer(at, address(weth), address(v2A), AMOUNT_WETH);
        cFwd = bytes.concat(cFwd, Cmd.v4Unlock(v4Inner));

        bytes memory commands = _v3Swap(at, v3C, cZfo, bOut, executorIdx, cFwd);
        _runAndVerify(at, commands, _accounts3(address(v4Pm), address(v2A), address(v3C)), 4, "TestV2V4V3", 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 9. V2-V4-V4 — 3 transfers: V4_SYNC(USDC), V4_TAKE WETH→V2a (excess),
    // V2a→PM, V4_SETTLE, V4b + V4c delta netting, V4_TAKE profit.
    // ═══════════════════════════════════════════════════════════════════════

    function testV2V4V4() public {
        bool aZfo = _setupV2ForCalc(v2A, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        uint256 aOut = _v2GetAmountOut(AMOUNT_WETH, weth.balanceOf(address(v2A)), usdc.balanceOf(address(v2A)), V2_FEE);
        (PoolKey memory bPk, bool bZfo) =
            _setupV4Pool(address(usdc), address(wbtc), aOut, AMOUNT_WBTC, 500, 10, address(wbtc));
        (PoolKey memory cPk, bool cZfo) =
            _setupV4Pool(address(wbtc), address(weth), AMOUNT_WBTC, AMOUNT_WETH_PROFIT, 10_000, 200, address(weth));
        uint256 bOut = AMOUNT_WBTC;

        Cmd.Table memory at = _sentinelTable(address(v4Pm));
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 pmIdx = Cmd.add(at, address(v4Pm));
        uint8 executorIdx = Cmd.add(at, address(executor));

        bytes memory v4Inner = Cmd.v4Sync(usdcIdx);
        v4Inner = bytes.concat(v4Inner, Cmd.v4TakeCompact(wethIdx, Cmd.add(at, address(v2A)), AMOUNT_WETH));
        v4Inner = bytes.concat(v4Inner, _v2SwapDirect(at, v2A, aZfo, aOut, pmIdx));
        v4Inner = bytes.concat(v4Inner, Cmd.v4Settle());
        v4Inner = bytes.concat(v4Inner, _v4Swap(at, bPk, bZfo, aOut));
        v4Inner = bytes.concat(v4Inner, _v4Swap(at, cPk, cZfo, bOut));
        v4Inner = bytes.concat(v4Inner, Cmd.v4TakeCompact(wethIdx, executorIdx, _profit()));

        _runAndVerify(at, Cmd.v4Unlock(v4Inner), _accounts2(address(v4Pm), address(v2A)), 3, "TestV2V4V4", 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 10. V3-V2-V2 — 4 transfers: V3a→V2b direct, V2b→V2c direct, then the
    // explicit WETH payment to V3a inside its callback.
    // ═══════════════════════════════════════════════════════════════════════

    function testV3V2V2() public {
        weth.mint(address(executor), AMOUNT_WETH);
        (bool aZfo, uint256 aOut) = _setupV3(v3A, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        bool bZfo = _setupV2ForCalc(v2B, usdc, IFakeErc20(address(wbtc)), aOut, AMOUNT_WBTC);
        uint256 bOut = _v2GetAmountOut(aOut, usdc.balanceOf(address(v2B)), wbtc.balanceOf(address(v2B)), V2_FEE);
        bool cZfo = _setupV2ForCalc(v2C, IFakeErc20(address(wbtc)), IFakeErc20(address(weth)), bOut, AMOUNT_WETH_PROFIT);
        uint256 cOut = _v2GetAmountOut(bOut, wbtc.balanceOf(address(v2C)), weth.balanceOf(address(v2C)), V2_FEE);

        Cmd.Table memory at = _sentinelTable(address(0));
        uint8 executorIdx = Cmd.add(at, address(executor));

        bytes memory bCmd = _v2SwapDirect(at, v2B, bZfo, bOut, Cmd.add(at, address(v2C)));
        bytes memory cCmd = _v2SwapDirect(at, v2C, cZfo, cOut, executorIdx);
        bytes memory aFwd = bytes.concat(bCmd, cCmd, _erc20Xfer(at, address(weth), address(v3A), AMOUNT_WETH));

        bytes memory commands = _v3Swap(at, v3A, aZfo, AMOUNT_WETH, Cmd.add(at, address(v2B)), aFwd);
        _runAndVerify(at, commands, _accounts3(address(v3A), address(v2B), address(v2C)), 4, "TestV3V2V2", 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 11. V3-V2-V3 — 4 transfers: reverse-order from V3c.
    //
    // Inside V3c's callback V3a swaps USDC→V2b directly (excess); inside V3a's
    // callback V2b sends WBTC→V3c (IIA satisfied) and the executor pays
    // WETH→V3a (IIA satisfied).
    // ═══════════════════════════════════════════════════════════════════════

    function testV3V2V3() public {
        (bool aZfo, uint256 aOut) = _setupV3(v3A, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        bool bZfo = _setupV2ForCalc(v2B, usdc, IFakeErc20(address(wbtc)), aOut, AMOUNT_WBTC);
        uint256 bOut = _v2GetAmountOut(aOut, usdc.balanceOf(address(v2B)), wbtc.balanceOf(address(v2B)), V2_FEE);
        (bool cZfo,) = _setupV3(v3C, IFakeErc20(address(wbtc)), IFakeErc20(address(weth)), bOut, AMOUNT_WETH_PROFIT);

        Cmd.Table memory at = _sentinelTable(address(0));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3cIdx = Cmd.add(at, address(v3C));

        bytes memory v3aFwd = _v2SwapDirect(at, v2B, bZfo, bOut, v3cIdx);
        v3aFwd = bytes.concat(v3aFwd, _erc20Xfer(at, address(weth), address(v3A), AMOUNT_WETH));

        bytes memory v3cFwd = _v3Swap(at, v3A, aZfo, AMOUNT_WETH, Cmd.add(at, address(v2B)), v3aFwd);
        bytes memory commands = _v3Swap(at, v3C, cZfo, bOut, executorIdx, v3cFwd);
        _runAndVerify(at, commands, _accounts3(address(v3A), address(v2B), address(v3C)), 4, "TestV3V2V3", 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 12. V3-V2-V4 — 4 transfers: V3a→V2b direct, V2b→PM (WBTC delta
    // netting), V4_TAKE WETH→V3a directly (IIA satisfied).
    // ═══════════════════════════════════════════════════════════════════════

    function testV3V2V4() public {
        (bool aZfo, uint256 aOut) = _setupV3(v3A, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        bool bZfo = _setupV2ForCalc(v2B, usdc, IFakeErc20(address(wbtc)), aOut, AMOUNT_WBTC);
        uint256 bOut = _v2GetAmountOut(aOut, usdc.balanceOf(address(v2B)), wbtc.balanceOf(address(v2B)), V2_FEE);
        (PoolKey memory cPk, bool cZfo) =
            _setupV4Pool(address(wbtc), address(weth), bOut, AMOUNT_WETH_PROFIT, 10_000, 200, address(weth));

        Cmd.Table memory at = _sentinelTable(address(v4Pm));
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 pmIdx = Cmd.add(at, address(v4Pm));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3aIdx = Cmd.add(at, address(v3A));

        bytes memory v4Inner = Cmd.v4Sync(wbtcIdx);
        v4Inner = bytes.concat(v4Inner, _v2SwapDirect(at, v2B, bZfo, bOut, pmIdx));
        v4Inner = bytes.concat(v4Inner, Cmd.v4Settle());
        v4Inner = bytes.concat(v4Inner, _v4Swap(at, cPk, cZfo, bOut));
        v4Inner = bytes.concat(v4Inner, Cmd.v4TakeCompact(wethIdx, v3aIdx, AMOUNT_WETH));
        v4Inner = bytes.concat(v4Inner, Cmd.v4TakeCompact(wethIdx, executorIdx, _profit()));

        bytes memory commands = _v3Swap(at, v3A, aZfo, AMOUNT_WETH, Cmd.add(at, address(v2B)), Cmd.v4Unlock(v4Inner));
        _runAndVerify(at, commands, _accounts3(address(v4Pm), address(v3A), address(v2B)), 4, "TestV3V2V4", 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 13. V3-V3-V2 — 4 transfers: reverse-order.
    //
    // V3b fires first (sends WBTC to V2c); inside V3b's callback V3a swaps
    // USDC→V3b directly (IIA satisfied); inside V3a's callback V2c
    // V2_SWAP_DIRECT→executor and the executor pays WETH→V3a.
    // ═══════════════════════════════════════════════════════════════════════

    function testV3V3V2() public {
        (bool aZfo, uint256 aOut) = _setupV3(v3A, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        (bool bZfo, uint256 bOut) = _setupV3(v3B, usdc, IFakeErc20(address(wbtc)), aOut, AMOUNT_WBTC);
        bool cZfo = _setupV2ForCalc(v2C, IFakeErc20(address(wbtc)), IFakeErc20(address(weth)), bOut, AMOUNT_WETH_PROFIT);
        uint256 cOut = _v2GetAmountOut(bOut, wbtc.balanceOf(address(v2C)), weth.balanceOf(address(v2C)), V2_FEE);

        Cmd.Table memory at = _sentinelTable(address(0));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3bIdx = Cmd.add(at, address(v3B));

        bytes memory v3aFwd = _v2SwapDirect(at, v2C, cZfo, cOut, executorIdx);
        v3aFwd = bytes.concat(v3aFwd, _erc20Xfer(at, address(weth), address(v3A), AMOUNT_WETH));

        bytes memory v3bFwd = _v3Swap(at, v3A, aZfo, AMOUNT_WETH, v3bIdx, v3aFwd);
        bytes memory commands = _v3Swap(at, v3B, bZfo, aOut, Cmd.add(at, address(v2C)), v3bFwd);
        _runAndVerify(at, commands, _accounts3(address(v3A), address(v3B), address(v2C)), 4, "TestV3V3V2", 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 14. V3-V3-V3 — 4 transfers: reverse-order direct custody.
    //
    // V3c fires first (WETH→executor); V3b swaps WBTC→V3c during V3c's
    // callback; V3a swaps USDC→V3b during V3b's callback; V3a auto-pays WETH
    // from the executor's just-received profit.
    // ═══════════════════════════════════════════════════════════════════════

    function testV3V3V3() public {
        (bool aZfo, uint256 aOut) = _setupV3(v3A, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        (bool bZfo, uint256 bOut) = _setupV3(v3B, usdc, IFakeErc20(address(wbtc)), aOut, AMOUNT_WBTC);
        (bool cZfo,) = _setupV3(v3C, IFakeErc20(address(wbtc)), IFakeErc20(address(weth)), bOut, AMOUNT_WETH_PROFIT);

        Cmd.Table memory at = _sentinelTable(address(0));
        uint8 executorIdx = Cmd.add(at, address(executor));
        Cmd.add(at, address(v3A));
        uint8 v3bIdx = Cmd.add(at, address(v3B));
        uint8 v3cIdx = Cmd.add(at, address(v3C));

        bytes memory v3bCallback = _v3Swap(at, v3A, aZfo, AMOUNT_WETH, v3bIdx, "");
        bytes memory v3cCallback = _v3Swap(at, v3B, bZfo, aOut, v3cIdx, v3bCallback);
        bytes memory commands = _v3Swap(at, v3C, cZfo, bOut, executorIdx, v3cCallback);
        _runAndVerify(at, commands, _accounts3(address(v3A), address(v3B), address(v3C)), 4, "TestV3V3V3", 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 15. V3-V3-V4 — 4 transfers: V3b outermost (sends WBTC to the PM).
    //
    // V3a runs inside V3b's callback and the V4 unlock takes WETH→V3a directly,
    // satisfying V3a's IIA check.
    // ═══════════════════════════════════════════════════════════════════════

    function testV3V3V4() public {
        (bool aZfo, uint256 aOut) = _setupV3(v3A, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        (bool bZfo, uint256 bOut) = _setupV3(v3B, usdc, IFakeErc20(address(wbtc)), aOut, AMOUNT_WBTC);
        (PoolKey memory cPk, bool cZfo) =
            _setupV4Pool(address(wbtc), address(weth), bOut, AMOUNT_WETH_PROFIT, 10_000, 200, address(weth));

        Cmd.Table memory at = _sentinelTable(address(v4Pm));
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 pmIdx = Cmd.add(at, address(v4Pm));
        uint8 v3aIdx = Cmd.add(at, address(v3A));
        uint8 v3bIdx = Cmd.add(at, address(v3B));

        bytes memory v4Inner = Cmd.v4Settle();
        v4Inner = bytes.concat(v4Inner, _v4Swap(at, cPk, cZfo, bOut));
        v4Inner = bytes.concat(v4Inner, Cmd.v4TakeCompact(wethIdx, v3aIdx, AMOUNT_WETH));
        v4Inner = bytes.concat(v4Inner, Cmd.v4TakeCompact(wethIdx, Cmd.add(at, address(executor)), _profit()));

        bytes memory aFwd = Cmd.v4Unlock(v4Inner);
        bytes memory bFwd = _v3Swap(at, v3A, aZfo, AMOUNT_WETH, v3bIdx, aFwd);

        bytes memory commands = bytes.concat(Cmd.v4Sync(wbtcIdx), _v3Swap(at, v3B, bZfo, aOut, pmIdx, bFwd));
        _runAndVerify(at, commands, _accounts3(address(v4Pm), address(v3A), address(v3B)), 4, "TestV3V3V4", 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 16. V3-V4-V2 — 4 transfers: V3a→PM (USDC delta netting), V4_TAKE
    // WBTC→V2c direct (K-invariant), V2c V2_SWAP_DIRECT→executor.
    // ═══════════════════════════════════════════════════════════════════════

    function testV3V4V2() public {
        weth.mint(address(executor), AMOUNT_WETH);
        (bool aZfo, uint256 aOut) = _setupV3(v3A, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        (PoolKey memory bPk, bool bZfo) =
            _setupV4Pool(address(usdc), address(wbtc), aOut, AMOUNT_WBTC, 500, 10, address(wbtc));
        uint256 bOut = AMOUNT_WBTC;
        bool cZfo = _setupV2ForCalc(v2C, IFakeErc20(address(wbtc)), IFakeErc20(address(weth)), bOut, AMOUNT_WETH_PROFIT);
        uint256 cOut = _v2GetAmountOut(bOut, wbtc.balanceOf(address(v2C)), weth.balanceOf(address(v2C)), V2_FEE);

        Cmd.Table memory at = _sentinelTable(address(v4Pm));
        Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 pmIdx = Cmd.add(at, address(v4Pm));
        uint8 executorIdx = Cmd.add(at, address(executor));

        bytes memory v4Inner = Cmd.v4Settle();
        v4Inner = bytes.concat(v4Inner, _v4Swap(at, bPk, bZfo, aOut));
        v4Inner = bytes.concat(v4Inner, Cmd.v4TakeCompact(wbtcIdx, Cmd.add(at, address(v2C)), AMOUNT_WBTC));

        bytes memory cCmd = _v2SwapDirect(at, v2C, cZfo, cOut, executorIdx);
        bytes memory aFwd =
            bytes.concat(Cmd.v4Unlock(v4Inner), cCmd, _erc20Xfer(at, address(weth), address(v3A), AMOUNT_WETH));

        bytes memory commands = bytes.concat(Cmd.v4Sync(usdcIdx), _v3Swap(at, v3A, aZfo, AMOUNT_WETH, pmIdx, aFwd));
        _runAndVerify(at, commands, _accounts3(address(v4Pm), address(v3A), address(v2C)), 4, "TestV3V4V2", 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 17. V3-V4-V3 — 4 transfers: V3c fires first.
    //
    // Inside V3c's callback V3a swaps USDC→PM with forward data that pays
    // WETH→V3a (IIA) and unlocks V4 (V4b swap + V4_TAKE WBTC→V3c, IIA
    // satisfied during the V3c callback). V4_SYNC(USDC) runs before V3a
    // deposits to the PM.
    // ═══════════════════════════════════════════════════════════════════════

    function testV3V4V3() public {
        (bool aZfo, uint256 aOut) = _setupV3(v3A, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        (PoolKey memory bPk, bool bZfo) =
            _setupV4Pool(address(usdc), address(wbtc), aOut, AMOUNT_WBTC, 500, 10, address(wbtc));
        uint256 bOut = AMOUNT_WBTC;
        (bool cZfo,) = _setupV3(v3C, IFakeErc20(address(wbtc)), IFakeErc20(address(weth)), bOut, AMOUNT_WETH_PROFIT);

        Cmd.Table memory at = _sentinelTable(address(v4Pm));
        Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 pmIdx = Cmd.add(at, address(v4Pm));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3cIdx = Cmd.add(at, address(v3C));

        bytes memory v4Inner = Cmd.v4Settle();
        v4Inner = bytes.concat(v4Inner, _v4Swap(at, bPk, bZfo, aOut));
        v4Inner = bytes.concat(v4Inner, Cmd.v4TakeCompact(wbtcIdx, v3cIdx, bOut));

        bytes memory aFwd = _erc20Xfer(at, address(weth), address(v3A), AMOUNT_WETH);
        aFwd = bytes.concat(aFwd, Cmd.v4Unlock(v4Inner));

        bytes memory cFwd = _v3Swap(at, v3A, aZfo, AMOUNT_WETH, pmIdx, aFwd);
        bytes memory commands = bytes.concat(Cmd.v4Sync(usdcIdx), _v3Swap(at, v3C, cZfo, bOut, executorIdx, cFwd));
        _runAndVerify(at, commands, _accounts3(address(v4Pm), address(v3A), address(v3C)), 4, "TestV3V4V3", 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 18. V3-V4-V4 — 3 transfers: V3a→PM, V4b + V4c delta netting, and
    // V4_TAKE WETH→V3a directly (IIA satisfied).
    // ═══════════════════════════════════════════════════════════════════════

    function testV3V4V4() public {
        (bool aZfo, uint256 aOut) = _setupV3(v3A, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        (PoolKey memory bPk, bool bZfo) =
            _setupV4Pool(address(usdc), address(wbtc), aOut, AMOUNT_WBTC, 500, 10, address(wbtc));
        uint256 bOut = AMOUNT_WBTC;
        (PoolKey memory cPk, bool cZfo) =
            _setupV4Pool(address(wbtc), address(weth), bOut, AMOUNT_WETH_PROFIT, 10_000, 200, address(weth));

        Cmd.Table memory at = _sentinelTable(address(v4Pm));
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 pmIdx = Cmd.add(at, address(v4Pm));
        uint8 v3aIdx = Cmd.add(at, address(v3A));

        bytes memory v4Inner = Cmd.v4Settle();
        v4Inner = bytes.concat(v4Inner, _v4Swap(at, bPk, bZfo, aOut));
        v4Inner = bytes.concat(v4Inner, _v4Swap(at, cPk, cZfo, bOut));
        v4Inner = bytes.concat(v4Inner, Cmd.v4TakeCompact(wethIdx, v3aIdx, AMOUNT_WETH));
        v4Inner = bytes.concat(v4Inner, Cmd.v4TakeCompact(wethIdx, Cmd.add(at, address(executor)), _profit()));

        bytes memory commands =
            bytes.concat(Cmd.v4Sync(usdcIdx), _v3Swap(at, v3A, aZfo, AMOUNT_WETH, pmIdx, Cmd.v4Unlock(v4Inner)));
        _runAndVerify(at, commands, _accounts2(address(v4Pm), address(v3A)), 3, "TestV3V4V4", 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 19. V4-V2-V2 — 4 transfers: V4a swap, V4_TAKE USDC→V2b (excess),
    // V2b→V2c direct, V2c→executor, V4_SETTLE_DELTA WETH.
    // ═══════════════════════════════════════════════════════════════════════

    function testV4V2V2() public {
        weth.mint(address(executor), AMOUNT_WETH);
        (PoolKey memory aPk, bool aZfo) =
            _setupV4Pool(address(weth), address(usdc), AMOUNT_WETH, AMOUNT_USDC, 3000, 60, address(usdc));
        bool bZfo = _setupV2ForCalc(v2B, usdc, IFakeErc20(address(wbtc)), AMOUNT_USDC, AMOUNT_WBTC);
        uint256 bOut = _v2GetAmountOut(AMOUNT_USDC, usdc.balanceOf(address(v2B)), wbtc.balanceOf(address(v2B)), V2_FEE);
        bool cZfo =
            _setupV2ForCalc(v2C, IFakeErc20(address(wbtc)), IFakeErc20(address(weth)), AMOUNT_WBTC, AMOUNT_WETH_PROFIT);
        uint256 cOut = _v2GetAmountOut(bOut, wbtc.balanceOf(address(v2C)), weth.balanceOf(address(v2C)), V2_FEE);

        Cmd.Table memory at = _sentinelTable(address(v4Pm));
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 executorIdx = Cmd.add(at, address(executor));

        bytes memory bCmd = _v2SwapDirect(at, v2B, bZfo, bOut, Cmd.add(at, address(v2C)));
        bytes memory cCmd = _v2SwapDirect(at, v2C, cZfo, cOut, executorIdx);

        bytes memory inner = _v4Swap(at, aPk, aZfo, AMOUNT_WETH);
        inner = bytes.concat(inner, Cmd.v4TakeCompact(usdcIdx, Cmd.add(at, address(v2B)), AMOUNT_USDC));
        inner = bytes.concat(inner, bCmd, cCmd);
        inner = bytes.concat(inner, Cmd.v4SettleDelta(wethIdx));

        _runAndVerify(
            at, Cmd.v4Unlock(inner), _accounts3(address(v4Pm), address(v2B), address(v2C)), 4, "TestV4V2V2", 1
        );
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 20. V4-V2-V3 — 4 transfers: V3c fires first.
    //
    // Inside V3c's callback the V4 unlock swaps, V4_TAKEs USDC→V2b (direct
    // custody), and V2b V2_SWAP_DIRECT→V3c delivers the WBTC during the
    // callback (IIA satisfied), then V4_SETTLE_DELTA WETH covers V4a's debit.
    // ═══════════════════════════════════════════════════════════════════════

    function testV4V2V3() public {
        (PoolKey memory aPk, bool aZfo) =
            _setupV4Pool(address(weth), address(usdc), AMOUNT_WETH, AMOUNT_USDC, 3000, 60, address(usdc));
        bool bZfo = _setupV2ForCalc(v2B, usdc, IFakeErc20(address(wbtc)), AMOUNT_USDC, AMOUNT_WBTC);
        uint256 bOut = _v2GetAmountOut(AMOUNT_USDC, usdc.balanceOf(address(v2B)), wbtc.balanceOf(address(v2B)), V2_FEE);
        (bool cZfo,) = _setupV3(v3C, IFakeErc20(address(wbtc)), IFakeErc20(address(weth)), bOut, AMOUNT_WETH_PROFIT);

        Cmd.Table memory at = _sentinelTable(address(v4Pm));
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3cIdx = Cmd.add(at, address(v3C));

        bytes memory v4Inner = _v4Swap(at, aPk, aZfo, AMOUNT_WETH);
        v4Inner = bytes.concat(v4Inner, Cmd.v4TakeCompact(usdcIdx, Cmd.add(at, address(v2B)), AMOUNT_USDC));
        v4Inner = bytes.concat(v4Inner, _v2SwapDirect(at, v2B, bZfo, bOut, v3cIdx));
        v4Inner = bytes.concat(v4Inner, Cmd.v4SettleDelta(wethIdx));

        bytes memory commands = _v3Swap(at, v3C, cZfo, bOut, executorIdx, Cmd.v4Unlock(v4Inner));
        _runAndVerify(at, commands, _accounts3(address(v4Pm), address(v2B), address(v3C)), 4, "TestV4V2V3", 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 21. V4-V2-V4 — 3 transfers: single unlock.
    //
    // V4a swap, V4_TAKE USDC→V2b (excess), sync WBTC, V2b→PM (delta netting),
    // settle, V4c swap, take the net profit WETH.
    // ═══════════════════════════════════════════════════════════════════════

    function testV4V2V4() public {
        (PoolKey memory aPk, bool aZfo) =
            _setupV4Pool(address(weth), address(usdc), AMOUNT_WETH, AMOUNT_USDC, 3000, 60, address(usdc));
        bool bZfo = _setupV2ForCalc(v2B, usdc, IFakeErc20(address(wbtc)), AMOUNT_USDC, AMOUNT_WBTC);
        uint256 bOut = _v2GetAmountOut(AMOUNT_USDC, usdc.balanceOf(address(v2B)), wbtc.balanceOf(address(v2B)), V2_FEE);
        (PoolKey memory cPk, bool cZfo) =
            _setupV4Pool(address(wbtc), address(weth), bOut, AMOUNT_WETH_PROFIT, 10_000, 200, address(weth));

        Cmd.Table memory at = _sentinelTable(address(v4Pm));
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 pmIdx = Cmd.add(at, address(v4Pm));
        uint8 executorIdx = Cmd.add(at, address(executor));

        bytes memory inner = _v4Swap(at, aPk, aZfo, AMOUNT_WETH);
        inner = bytes.concat(inner, Cmd.v4TakeCompact(usdcIdx, Cmd.add(at, address(v2B)), AMOUNT_USDC));
        inner = bytes.concat(inner, Cmd.v4Sync(wbtcIdx));
        inner = bytes.concat(inner, _v2SwapDirect(at, v2B, bZfo, bOut, pmIdx));
        inner = bytes.concat(inner, Cmd.v4Settle());
        inner = bytes.concat(inner, _v4Swap(at, cPk, cZfo, bOut));
        inner = bytes.concat(inner, Cmd.v4TakeCompact(wethIdx, executorIdx, _profit()));

        _runAndVerify(at, Cmd.v4Unlock(inner), _accounts2(address(v4Pm), address(v2B)), 3, "TestV4V2V4", 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 22. V4-V3-V2 — 4 transfers: V4a swap, then inside V3b's forward data
    // V4_TAKE USDC→V3b (IIA satisfied during the callback); V3b→V2c direct
    // custody; V2c V2_SWAP_DIRECT→executor.
    // ═══════════════════════════════════════════════════════════════════════

    function testV4V3V2() public {
        weth.mint(address(executor), AMOUNT_WETH);
        (PoolKey memory aPk, bool aZfo) =
            _setupV4Pool(address(weth), address(usdc), AMOUNT_WETH, AMOUNT_USDC, 3000, 60, address(usdc));
        uint256 aOut = AMOUNT_USDC;
        (bool bZfo, uint256 bOut) = _setupV3(v3B, usdc, IFakeErc20(address(wbtc)), aOut, AMOUNT_WBTC);
        bool cZfo = _setupV2ForCalc(v2C, IFakeErc20(address(wbtc)), IFakeErc20(address(weth)), bOut, AMOUNT_WETH_PROFIT);
        uint256 cOut = _v2GetAmountOut(bOut, wbtc.balanceOf(address(v2C)), weth.balanceOf(address(v2C)), V2_FEE);

        Cmd.Table memory at = _sentinelTable(address(v4Pm));
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3bIdx = Cmd.add(at, address(v3B));

        bytes memory bFwd = Cmd.v4TakeCompact(usdcIdx, v3bIdx, aOut);
        bFwd = bytes.concat(bFwd, _v2SwapDirect(at, v2C, cZfo, cOut, executorIdx));

        bytes memory inner = _v4Swap(at, aPk, aZfo, AMOUNT_WETH);
        inner = bytes.concat(inner, _v3Swap(at, v3B, bZfo, aOut, Cmd.add(at, address(v2C)), bFwd));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(wethIdx));

        _runAndVerify(
            at, Cmd.v4Unlock(inner), _accounts3(address(v4Pm), address(v3B), address(v2C)), 4, "TestV4V3V2", 1
        );
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 23. V4-V3-V3 — 4 transfers: V3c→V3b reverse-order, with V4_TAKE
    // USDC→V3b during V3b's callback (IIA satisfied) and a merged WETH settle
    // through V4_SETTLE_DELTA (sync + transfer + settle in one command).
    // ═══════════════════════════════════════════════════════════════════════

    function testV4V3V3() public {
        (PoolKey memory aPk, bool aZfo) =
            _setupV4Pool(address(weth), address(usdc), AMOUNT_WETH, AMOUNT_USDC, 3000, 60, address(usdc));
        uint256 aOut = AMOUNT_USDC;
        (bool bZfo, uint256 bOut) = _setupV3(v3B, usdc, IFakeErc20(address(wbtc)), aOut, AMOUNT_WBTC);
        (bool cZfo,) = _setupV3(v3C, IFakeErc20(address(wbtc)), IFakeErc20(address(weth)), bOut, AMOUNT_WETH_PROFIT);

        Cmd.Table memory at = _sentinelTable(address(v4Pm));
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3bIdx = Cmd.add(at, address(v3B));
        uint8 v3cIdx = Cmd.add(at, address(v3C));

        bytes memory bFwd = Cmd.v4TakeCompact(usdcIdx, v3bIdx, aOut);
        bytes memory inner = _v4Swap(at, aPk, aZfo, AMOUNT_WETH);
        inner =
            bytes.concat(inner, _v3Swap(at, v3C, cZfo, bOut, executorIdx, _v3Swap(at, v3B, bZfo, aOut, v3cIdx, bFwd)));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(wethIdx));

        _runAndVerify(
            at, Cmd.v4Unlock(inner), _accounts3(address(v4Pm), address(v3B), address(v3C)), 4, "TestV4V3V3", 1
        );
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 24. V4-V3-V4 — 3 transfers: V4a and V4c inside one unlock.
    //
    // V3b fires inside the unlock with V4_TAKE USDC→V3b in its forward data
    // (IIA satisfied), then V3b→PM for WBTC delta netting (sync + settle).
    // ═══════════════════════════════════════════════════════════════════════

    function testV4V3V4() public {
        (PoolKey memory aPk, bool aZfo) =
            _setupV4Pool(address(weth), address(usdc), AMOUNT_WETH, AMOUNT_USDC, 3000, 60, address(usdc));
        uint256 aOut = AMOUNT_USDC;
        (bool bZfo, uint256 bOut) = _setupV3(v3B, usdc, IFakeErc20(address(wbtc)), aOut, AMOUNT_WBTC);
        (PoolKey memory cPk, bool cZfo) =
            _setupV4Pool(address(wbtc), address(weth), bOut, AMOUNT_WETH_PROFIT, 10_000, 200, address(weth));

        Cmd.Table memory at = _sentinelTable(address(v4Pm));
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 pmIdx = Cmd.add(at, address(v4Pm));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3bIdx = Cmd.add(at, address(v3B));

        bytes memory bFwd = Cmd.v4TakeCompact(usdcIdx, v3bIdx, aOut);
        bFwd = bytes.concat(bFwd, _v4Swap(at, cPk, cZfo, bOut));
        bFwd = bytes.concat(bFwd, Cmd.v4TakeCompact(wethIdx, executorIdx, _profit()));

        bytes memory inner = _v4Swap(at, aPk, aZfo, AMOUNT_WETH);
        inner = bytes.concat(inner, Cmd.v4Sync(wbtcIdx));
        inner = bytes.concat(inner, _v3Swap(at, v3B, bZfo, aOut, pmIdx, bFwd));
        inner = bytes.concat(inner, Cmd.v4Settle());

        _runAndVerify(at, Cmd.v4Unlock(inner), _accounts2(address(v4Pm), address(v3B)), 3, "TestV4V3V4", 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 25. V4-V4-V2 — 3 transfers: V4a + V4b delta netting, V4_TAKE WBTC→V2c
    // (excess), V2c V2_SWAP_DIRECT→executor, V4_SETTLE_DELTA WETH.
    // ═══════════════════════════════════════════════════════════════════════

    function testV4V4V2() public {
        weth.mint(address(executor), AMOUNT_WETH);
        (PoolKey memory aPk, bool aZfo) =
            _setupV4Pool(address(weth), address(usdc), AMOUNT_WETH, AMOUNT_USDC, 3000, 60, address(usdc));
        uint256 aOut = AMOUNT_USDC;
        (PoolKey memory bPk, bool bZfo) =
            _setupV4Pool(address(usdc), address(wbtc), AMOUNT_USDC, AMOUNT_WBTC, 500, 10, address(wbtc));
        bool cZfo =
            _setupV2ForCalc(v2C, IFakeErc20(address(wbtc)), IFakeErc20(address(weth)), AMOUNT_WBTC, AMOUNT_WETH_PROFIT);
        uint256 cOut = _v2GetAmountOut(AMOUNT_WBTC, wbtc.balanceOf(address(v2C)), weth.balanceOf(address(v2C)), V2_FEE);

        Cmd.Table memory at = _sentinelTable(address(v4Pm));
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 executorIdx = Cmd.add(at, address(executor));

        bytes memory cCmd = _v2SwapDirect(at, v2C, cZfo, cOut, executorIdx);

        bytes memory inner = _v4Swap(at, aPk, aZfo, AMOUNT_WETH);
        inner = bytes.concat(inner, _v4Swap(at, bPk, bZfo, aOut));
        inner = bytes.concat(inner, Cmd.v4TakeCompact(wbtcIdx, Cmd.add(at, address(v2C)), AMOUNT_WBTC));
        inner = bytes.concat(inner, cCmd);
        inner = bytes.concat(inner, Cmd.v4SettleDelta(wethIdx));

        _runAndVerify(at, Cmd.v4Unlock(inner), _accounts2(address(v4Pm), address(v2C)), 3, "TestV4V4V2", 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 26. V4-V4-V3 — 3 transfers: V4a + V4b delta netting, V4_TAKE WBTC→V3c in
    // V3c's forward data (tokens arrive during the callback — IIA satisfied),
    // V4_SETTLE_DELTA WETH.
    // ═══════════════════════════════════════════════════════════════════════

    function testV4V4V3() public {
        (PoolKey memory aPk, bool aZfo) =
            _setupV4Pool(address(weth), address(usdc), AMOUNT_WETH, AMOUNT_USDC, 3000, 60, address(usdc));
        uint256 aOut = AMOUNT_USDC;
        (PoolKey memory bPk, bool bZfo) =
            _setupV4Pool(address(usdc), address(wbtc), AMOUNT_USDC, AMOUNT_WBTC, 500, 10, address(wbtc));
        uint256 bOut = AMOUNT_WBTC;
        (bool cZfo,) = _setupV3(v3C, IFakeErc20(address(wbtc)), IFakeErc20(address(weth)), bOut, AMOUNT_WETH_PROFIT);

        Cmd.Table memory at = _sentinelTable(address(v4Pm));
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3cIdx = Cmd.add(at, address(v3C));

        bytes memory cTake = Cmd.v4TakeCompact(wbtcIdx, v3cIdx, bOut);

        bytes memory inner = _v4Swap(at, aPk, aZfo, AMOUNT_WETH);
        inner = bytes.concat(inner, _v4Swap(at, bPk, bZfo, aOut));
        inner = bytes.concat(inner, _v3Swap(at, v3C, cZfo, bOut, executorIdx, cTake));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(wethIdx));

        _runAndVerify(at, Cmd.v4Unlock(inner), _accounts2(address(v4Pm), address(v3C)), 3, "TestV4V4V3", 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // 27. V4-V4-V4 — 1 transfer: pure delta netting inside one unlock.
    //
    // Deltas net: V4a -1WETH +2000USDC, V4b -2000USDC +100WBTC, V4c -100WBTC
    // +2WETH → net +1WETH, captured by V4_MINT_COMPACT as an ERC6909 claim. This
    // is the source's `profit_mode = 2` row — the only MINT-capture path. No
    // settle is needed: the WETH delta is zero after the mint.
    // ═══════════════════════════════════════════════════════════════════════

    function testV4V4V4() public {
        (PoolKey memory aPk, bool aZfo) =
            _setupV4Pool(address(weth), address(usdc), AMOUNT_WETH, AMOUNT_USDC, 3000, 60, address(usdc));
        uint256 aOut = AMOUNT_USDC;
        (PoolKey memory bPk, bool bZfo) =
            _setupV4Pool(address(usdc), address(wbtc), AMOUNT_USDC, AMOUNT_WBTC, 500, 10, address(wbtc));
        uint256 bOut = AMOUNT_WBTC;
        (PoolKey memory cPk, bool cZfo) =
            _setupV4Pool(address(wbtc), address(weth), AMOUNT_WBTC, AMOUNT_WETH_PROFIT, 10_000, 200, address(weth));

        Cmd.Table memory at = _sentinelTable(address(v4Pm));
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 executorIdx = Cmd.add(at, address(executor));

        bytes memory inner = _v4Swap(at, aPk, aZfo, AMOUNT_WETH);
        inner = bytes.concat(inner, _v4Swap(at, bPk, bZfo, aOut));
        inner = bytes.concat(inner, _v4Swap(at, cPk, cZfo, bOut));
        inner = bytes.concat(inner, Cmd.v4MintCompact(wethIdx, executorIdx, _profit()));

        _runAndVerify(at, Cmd.v4Unlock(inner), _accounts1(address(v4Pm)), 1, "TestV4V4V4", 2);
    }
}
