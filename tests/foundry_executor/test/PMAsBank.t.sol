// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {ExecutorErrors} from "../src/generated/Errors.sol";
import {IFakeErc20} from "../src/generated/IFakeErc20.sol";
import {IFakeUniswapV2Pair} from "../src/generated/IFakeUniswapV2Pair.sol";
import {IFakeUniswapV3Pool} from "../src/generated/IFakeUniswapV3Pool.sol";

/// Port of `tests/executor/test_pm_as_bank.py`.
///
/// The V4 PoolManager as a zero-fee flash-loan source for non-V4 three-hop
/// paths. Each path runs twice:
///   A. V4_MINT profit capture (ERC6909) — the settled positive delta becomes
///      an ERC6909 balance inside the PM.
///   B. V4_TAKE_DELTA — the settled positive delta leaves as physical WETH.
///
/// Path facts preserved from the source:
///   - V2-ending paths (V2-V2-V2, V3-V2-V2, V2-V3-V2, V3-V3-V2): delta
///     accounting works cleanly — V2c sends WETH to the PM directly and
///     settle() captures it; profit = chainOut - borrow.
///   - V3-ending V2-V2-V3: V3c's optimistic transfer sends WETH BEFORE the
///     callback, so the executor repays the PM manually (5 transfers), the
///     profit is physical, and a trailing V4_MINT is impossible — pinned to
///     the executor's exact `InsufficientMintDelta(0, 1)`: the unpaid debit
///     remains -1 WETH while the positive delta available to mint is 0, not a
///     bare failure.
///
/// The source's `_gas_results` module-global summary (disposition 6) is
/// replaced by ONE self-contained named comparison:
/// `testMintVsTakeGasComparison` runs every path's MINT and TAKE variants
/// from the same pristine pool state (state-snapshot isolated) and emits both
/// gas figures with the difference — no execution-order dependence, no
/// cross-test in-memory state.
contract PMAsBankTest is PoolSetup {
    uint256 internal constant AMOUNT_WETH_PROFIT = 2 ether;

    event PmBankGasComparison(
        string path, uint256 mintGas, uint256 takeGas
    );

    /// The source's bare AddressTable order for every builder: weth,
    /// executor, pm, then the path's pools.
    struct BankTable {
        Cmd.Table at;
        uint8 wethIdx;
        uint8 executorIdx;
        uint8 pmIdx;
    }

    struct V2Pools {
        IFakeUniswapV2Pair a; // WETH/USDC
        IFakeUniswapV2Pair b; // USDC/WBTC
        IFakeUniswapV2Pair c; // WBTC/WETH
    }

    function _bankTable() internal view returns (BankTable memory t) {
        t.at = _emptyTable();
        t.wethIdx = Cmd.add(t.at, address(weth));
        t.executorIdx = Cmd.add(t.at, address(executor));
        t.pmIdx = Cmd.add(t.at, address(v4Pm));
    }

    function _deployV2Chain() internal returns (V2Pools memory p) {
        (address a0, address a1) = _sorted(address(weth), address(usdc));
        p.a = _deployV2Pair(a0, a1, 0, V2_FEE);
        (address b0, address b1) = _sorted(address(usdc), address(wbtc));
        p.b = _deployV2Pair(b0, b1, 0, V2_FEE);
        (address c0, address c1) = _sorted(address(wbtc), address(weth));
        p.c = _deployV2Pair(c0, c1, 0, V2_FEE);
    }

    function _deployV3WethUsdc() internal returns (IFakeUniswapV3Pool) {
        (address t0, address t1) = _sorted(address(weth), address(usdc));
        return _deployV3Pool(t0, t1, 0, 3000);
    }

    function _deployV3UsdcWbtc() internal returns (IFakeUniswapV3Pool) {
        (address t0, address t1) = _sorted(address(usdc), address(wbtc));
        return _deployV3Pool(t0, t1, 0, 3000);
    }

    function _deployV3WbtcWeth() internal returns (IFakeUniswapV3Pool) {
        (address t0, address t1) = _sorted(address(wbtc), address(weth));
        return _deployV3Pool(t0, t1, 0, 3000);
    }

    /// The source `_setup_v2c`: mint a canned price ratio (ain*100 / aout*100)
    /// and sync — the arb's price skew. `inp` is the sold token, `out` the
    /// bought one.
    function _setupV2C(
        IFakeUniswapV2Pair pair,
        IFakeErc20 inp,
        IFakeErc20 out,
        uint256 ain,
        uint256 aout
    ) internal returns (bool zfo) {
        inp.mint(address(pair), ain * 100);
        out.mint(address(pair), aout * 100);
        pair.sync();
        zfo = pair.token0() == address(inp);
    }

    /// The bank's lending inventory: the PM must hold physical WETH to pay
    /// the executor's borrow out.
    function _fundPm() internal {
        weth.mint(address(v4Pm), AMOUNT_WETH * 10);
    }

    // ── V2-V2-V2 ──────────────────────────────────────────────────────────

    function _setupV2V2V2(V2Pools memory p)
        internal
        returns (uint256 aOut, uint256 bOut, uint256 cOut, uint256 profit)
    {
        _setupV2C(p.a, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        _setupV2C(p.b, usdc, wbtc, AMOUNT_USDC, AMOUNT_WBTC);
        _setupV2C(p.c, wbtc, IFakeErc20(address(weth)), AMOUNT_WBTC, AMOUNT_WETH_PROFIT);
        aOut = _v2GetAmountOut(
            AMOUNT_WETH, weth.balanceOf(address(p.a)), usdc.balanceOf(address(p.a)), V2_FEE
        );
        bOut = _v2GetAmountOut(
            aOut, usdc.balanceOf(address(p.b)), wbtc.balanceOf(address(p.b)), V2_FEE
        );
        cOut = _v2GetAmountOut(
            bOut, wbtc.balanceOf(address(p.c)), weth.balanceOf(address(p.c)), V2_FEE
        );
        profit = cOut - AMOUNT_WETH;
    }

    function _execV2V2V2(V2Pools memory p, bool mint)
        internal
        returns (Outcome memory o, uint256 profit)
    {
        (, , , profit) = _setupV2V2V2(p);
        BankTable memory t = _bankTable();
        uint8 aIdx = Cmd.add(t.at, address(p.a));
        uint8 bIdx = Cmd.add(t.at, address(p.b));
        uint8 cIdx = Cmd.add(t.at, address(p.c));
        bool az = p.a.token0() == address(weth);
        bool bz = p.b.token0() == address(usdc);
        bool cz = p.c.token0() == address(wbtc);

        // take→V2a (the borrow), V2a→V2b→V2c(calc), V2c→PM(calc), settle, profit
        bytes memory inner = Cmd.v4Take(t.wethIdx, aIdx, AMOUNT_WETH);
        inner = bytes.concat(inner, Cmd.v4Sync(t.wethIdx));
        inner = bytes.concat(inner, Cmd.v2SwapCalc(aIdx, az, bIdx, V2_FEE));
        inner = bytes.concat(inner, Cmd.v2SwapCalc(bIdx, bz, cIdx, V2_FEE));
        inner = bytes.concat(inner, Cmd.v2SwapCalc(cIdx, cz, t.pmIdx, V2_FEE));
        inner = bytes.concat(inner, Cmd.v4Settle());
        inner = bytes.concat(
            inner,
            mint
                ? Cmd.v4MintCompact(t.wethIdx, t.executorIdx, profit)
                : Cmd.v4TakeDelta(t.wethIdx, t.executorIdx)
        );
        o = _executeRaw(executor, bytes.concat(Cmd.preamble(t.at), Cmd.v4Unlock(inner)));
    }

    function testV2V2V2Mint() public {
        _fundPm();
        (Outcome memory o, uint256 profit) = _execV2V2V2(_deployV2Chain(), true);
        assertTrue(o.ok, "V2-V2-V2 MINT should succeed");
        assertEq(
            v4Pm.balanceOf(address(executor), uint256(uint160(address(weth)))) - 1,
            profit,
            "profit captured as ERC6909 WETH"
        );
    }

    function testV2V2V2Take() public {
        _fundPm();
        (Outcome memory o, uint256 profit) = _execV2V2V2(_deployV2Chain(), false);
        assertTrue(o.ok, "V2-V2-V2 TAKE should succeed");
        assertEq(
            weth.balanceOf(address(executor)), profit, "profit taken as physical WETH"
        );
    }

    // ── V3-V2-V2 ──────────────────────────────────────────────────────────

    function _execV3V2V2(IFakeUniswapV3Pool v3a, V2Pools memory bc, bool mint)
        internal
        returns (Outcome memory o, uint256 profit)
    {
        (bool aZfo, uint256 aUsdcOut) =
            _setupV3(v3a, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        _setupV2C(bc.b, usdc, wbtc, aUsdcOut, AMOUNT_WBTC);
        _setupV2C(bc.c, wbtc, IFakeErc20(address(weth)), AMOUNT_WBTC, AMOUNT_WETH_PROFIT);
        uint256 bOut = _v2GetAmountOut(
            aUsdcOut, usdc.balanceOf(address(bc.b)), wbtc.balanceOf(address(bc.b)), V2_FEE
        );
        uint256 cOut = _v2GetAmountOut(
            bOut, wbtc.balanceOf(address(bc.c)), weth.balanceOf(address(bc.c)), V2_FEE
        );
        profit = cOut - AMOUNT_WETH;

        BankTable memory t = _bankTable();
        uint8 a3Idx = Cmd.add(t.at, address(v3a));
        uint8 bIdx = Cmd.add(t.at, address(bc.b));
        uint8 cIdx = Cmd.add(t.at, address(bc.c));
        bool bz = bc.b.token0() == address(usdc);
        bool cz = bc.c.token0() == address(wbtc);

        // V3a callback: take(WETH→V3a, IIA), sync, V2b→V2c→PM, settle, profit
        bytes memory fwd = Cmd.v4Take(t.wethIdx, a3Idx, AMOUNT_WETH);
        fwd = bytes.concat(fwd, Cmd.v4Sync(t.wethIdx));
        fwd = bytes.concat(fwd, Cmd.v2SwapCalc(bIdx, bz, cIdx, V2_FEE));
        fwd = bytes.concat(fwd, Cmd.v2SwapCalc(cIdx, cz, t.pmIdx, V2_FEE));
        fwd = bytes.concat(fwd, Cmd.v4Settle());
        fwd = bytes.concat(
            fwd,
            mint
                ? Cmd.v4MintCompact(t.wethIdx, t.executorIdx, profit)
                : Cmd.v4TakeDelta(t.wethIdx, t.executorIdx)
        );

        bytes memory inner =
            Cmd.v3SwapCompact(a3Idx, aZfo, AMOUNT_WETH, bIdx, fwd);
        o = _executeRaw(executor, bytes.concat(Cmd.preamble(t.at), Cmd.v4Unlock(inner)));
    }

    function testV3V2V2Mint() public {
        _fundPm();
        (Outcome memory o, uint256 profit) =
            _execV3V2V2(_deployV3WethUsdc(), _deployV2Chain(), true);
        assertTrue(o.ok, "V3-V2-V2 MINT should succeed");
        assertEq(
            v4Pm.balanceOf(address(executor), uint256(uint160(address(weth)))) - 1,
            profit,
            "profit captured as ERC6909 WETH"
        );
    }

    function testV3V2V2Take() public {
        _fundPm();
        (Outcome memory o, uint256 profit) =
            _execV3V2V2(_deployV3WethUsdc(), _deployV2Chain(), false);
        assertTrue(o.ok, "V3-V2-V2 TAKE should succeed");
        assertEq(
            weth.balanceOf(address(executor)), profit, "profit taken as physical WETH"
        );
    }

    // ── V2-V3-V2 ──────────────────────────────────────────────────────────

    function _execV2V3V2(
        V2Pools memory ac,
        IFakeUniswapV3Pool v3b,
        bool mint
    ) internal returns (Outcome memory o, uint256 profit) {
        _setupV2C(ac.a, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        uint256 aOut = _v2GetAmountOut(
            AMOUNT_WETH, weth.balanceOf(address(ac.a)), usdc.balanceOf(address(ac.a)), V2_FEE
        );
        (bool bZfo, uint256 bOut) =
            _setupV3(v3b, usdc, wbtc, aOut, AMOUNT_WBTC);
        _setupV2C(ac.c, wbtc, IFakeErc20(address(weth)), bOut, AMOUNT_WETH_PROFIT);
        uint256 cOut = _v2GetAmountOut(
            bOut, wbtc.balanceOf(address(ac.c)), weth.balanceOf(address(ac.c)), V2_FEE
        );
        profit = cOut - AMOUNT_WETH;

        BankTable memory t = _bankTable();
        uint8 a2Idx = Cmd.add(t.at, address(ac.a));
        uint8 b3Idx = Cmd.add(t.at, address(v3b));
        uint8 cIdx = Cmd.add(t.at, address(ac.c));
        bool az = ac.a.token0() == address(weth);
        bool cz = ac.c.token0() == address(wbtc);

        // V3b callback: take(WETH→V2a, IIA), sync, V2a→V3b (calc)
        bytes memory bFwd = Cmd.v4Take(t.wethIdx, a2Idx, AMOUNT_WETH);
        bFwd = bytes.concat(bFwd, Cmd.v4Sync(t.wethIdx));
        bFwd = bytes.concat(bFwd, Cmd.v2SwapCalc(a2Idx, az, b3Idx, V2_FEE));

        // After V3b: V2c→PM(calc), settle, profit
        bytes memory inner =
            Cmd.v3SwapCompact(b3Idx, bZfo, aOut, cIdx, bFwd);
        inner = bytes.concat(inner, Cmd.v2SwapCalc(cIdx, cz, t.pmIdx, V2_FEE));
        inner = bytes.concat(inner, Cmd.v4Settle());
        inner = bytes.concat(
            inner,
            mint
                ? Cmd.v4MintCompact(t.wethIdx, t.executorIdx, profit)
                : Cmd.v4TakeDelta(t.wethIdx, t.executorIdx)
        );
        o = _executeRaw(executor, bytes.concat(Cmd.preamble(t.at), Cmd.v4Unlock(inner)));
    }

    function testV2V3V2Mint() public {
        _fundPm();
        (Outcome memory o, uint256 profit) =
            _execV2V3V2(_deployV2Chain(), _deployV3UsdcWbtc(), true);
        assertTrue(o.ok, "V2-V3-V2 MINT should succeed");
        assertEq(
            v4Pm.balanceOf(address(executor), uint256(uint160(address(weth)))) - 1,
            profit,
            "profit captured as ERC6909 WETH"
        );
    }

    function testV2V3V2Take() public {
        _fundPm();
        (Outcome memory o, uint256 profit) =
            _execV2V3V2(_deployV2Chain(), _deployV3UsdcWbtc(), false);
        assertTrue(o.ok, "V2-V3-V2 TAKE should succeed");
        assertEq(
            weth.balanceOf(address(executor)), profit, "profit taken as physical WETH"
        );
    }

    // ── V2-V2-V3 (V3-ending: physical profit, MINT impossible) ────────────

    struct V3EndingShape {
        uint256 bOut; // V2b output feeding V3c
        uint256 cOut; // V3c actual WETH output to the executor
        bool cZfo;
    }

    function _setupV2V2V3(V2Pools memory ab, IFakeUniswapV3Pool v3c)
        internal
        returns (V3EndingShape memory s)
    {
        _setupV2C(ab.a, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        _setupV2C(ab.b, usdc, wbtc, AMOUNT_USDC, AMOUNT_WBTC);
        uint256 aOut = _v2GetAmountOut(
            AMOUNT_WETH, weth.balanceOf(address(ab.a)), usdc.balanceOf(address(ab.a)), V2_FEE
        );
        s.bOut = _v2GetAmountOut(
            aOut, usdc.balanceOf(address(ab.b)), wbtc.balanceOf(address(ab.b)), V2_FEE
        );
        (s.cZfo, s.cOut) = _setupV3(v3c, wbtc, IFakeErc20(address(weth)), s.bOut, AMOUNT_WETH_PROFIT);
    }

    /// V3-ending paths can't use V4_MINT for profit — the profit is physical
    /// WETH at the executor. After the manual PM repayment the unpaid debit
    /// REMAINS -1 WETH (the settle credits zero: sync runs AFTER the
    /// transfer), so the positive delta available to mint is 0. Pinned to the
    /// executor's exact InsufficientMintDelta(0, 1).
    function testV2V2V3MintImpossible() public {
        _fundPm();
        V2Pools memory ab = _deployV2Chain();
        IFakeUniswapV3Pool v3c = _deployV3WbtcWeth();
        V3EndingShape memory s = _setupV2V2V3(ab, v3c);

        BankTable memory t = _bankTable();
        uint8 aIdx = Cmd.add(t.at, address(ab.a));
        uint8 bIdx = Cmd.add(t.at, address(ab.b));
        uint8 c3Idx = Cmd.add(t.at, address(v3c));
        bool az = ab.a.token0() == address(weth);
        bool bz = ab.b.token0() == address(usdc);

        // V3c callback: take(WETH→V2a), V2a→V2b→V3c (no sync — the repayment
        // is handled at the top level)
        bytes memory cFwd = Cmd.v4Take(t.wethIdx, aIdx, AMOUNT_WETH);
        cFwd = bytes.concat(cFwd, Cmd.v2SwapCalc(aIdx, az, bIdx, V2_FEE));
        cFwd = bytes.concat(cFwd, Cmd.v2SwapCalc(bIdx, bz, c3Idx, V2_FEE));

        // V3c sends output to the executor; executor repays PM, settles, then
        // attempts the impossible 1-wei mint.
        bytes memory inner =
            Cmd.v3SwapCompact(c3Idx, s.cZfo, s.bOut, t.executorIdx, cFwd);
        inner = bytes.concat(inner, Cmd.erc20Transfer(t.wethIdx, t.pmIdx, AMOUNT_WETH));
        inner = bytes.concat(inner, Cmd.v4Sync(t.wethIdx));
        inner = bytes.concat(inner, Cmd.v4Settle());
        inner = bytes.concat(inner, Cmd.v4MintCompact(t.wethIdx, t.executorIdx, 1));

        Outcome memory o =
            _executeRaw(executor, bytes.concat(Cmd.preamble(t.at), Cmd.v4Unlock(inner)));
        _assertRevertData(
            o,
            _executorRevert(ExecutorErrors.INSUFFICIENT_MINT_DELTA, 0, 1),
            "V4_MINT on V3-ending path (debit remains -1 WETH, positive mint delta 0)"
        );
    }

    /// V3-ending path: the profit is physical WETH at the executor, 5
    /// transfers (borrow, V2a, V2b, V3c→executor, repayment).
    function testV2V2V3TakePhysical() public {
        _fundPm();
        V2Pools memory ab = _deployV2Chain();
        IFakeUniswapV3Pool v3c = _deployV3WbtcWeth();
        V3EndingShape memory s = _setupV2V2V3(ab, v3c);

        BankTable memory t = _bankTable();
        uint8 aIdx = Cmd.add(t.at, address(ab.a));
        uint8 bIdx = Cmd.add(t.at, address(ab.b));
        uint8 c3Idx = Cmd.add(t.at, address(v3c));
        bool az = ab.a.token0() == address(weth);
        bool bz = ab.b.token0() == address(usdc);

        bytes memory cFwd = Cmd.v4Take(t.wethIdx, aIdx, AMOUNT_WETH);
        cFwd = bytes.concat(cFwd, Cmd.v2SwapCalc(aIdx, az, bIdx, V2_FEE));
        cFwd = bytes.concat(cFwd, Cmd.v2SwapCalc(bIdx, bz, c3Idx, V2_FEE));

        bytes memory inner =
            Cmd.v3SwapCompact(c3Idx, s.cZfo, s.bOut, t.executorIdx, cFwd);
        // sync BEFORE the repayment (the take drained the PM), so the settle
        // credits the 1 WETH repayment.
        inner = bytes.concat(inner, Cmd.v4Sync(t.wethIdx));
        inner = bytes.concat(inner, Cmd.erc20Transfer(t.wethIdx, t.pmIdx, AMOUNT_WETH));
        inner = bytes.concat(inner, Cmd.v4Settle());

        vm.recordLogs();
        Outcome memory o =
            _executeRaw(executor, bytes.concat(Cmd.preamble(t.at), Cmd.v4Unlock(inner)));
        assertTrue(o.ok, "V2-V2-V3 TAKE-physical should succeed");
        assertEq(_countTransfers(vm.getRecordedLogs()), 5, "exactly 5 transfers");
        assertEq(
            weth.balanceOf(address(executor)),
            s.cOut - AMOUNT_WETH,
            "physical profit = V3c output minus the borrow"
        );
    }

    // ── V3-V3-V2 ──────────────────────────────────────────────────────────

    function _execV3V3V2(
        IFakeUniswapV3Pool v3a,
        IFakeUniswapV3Pool v3b,
        IFakeUniswapV2Pair v2c,
        bool mint
    ) internal returns (Outcome memory o, uint256 profit) {
        (bool aZfo, uint256 aUsdcOut) =
            _setupV3(v3a, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        (bool bZfo, uint256 bWbtcOut) =
            _setupV3(v3b, usdc, wbtc, aUsdcOut, AMOUNT_WBTC);
        _setupV2C(v2c, wbtc, IFakeErc20(address(weth)), bWbtcOut, AMOUNT_WETH_PROFIT);
        uint256 cOut = _v2GetAmountOut(
            bWbtcOut, wbtc.balanceOf(address(v2c)), weth.balanceOf(address(v2c)), V2_FEE
        );
        profit = cOut - AMOUNT_WETH;

        BankTable memory t = _bankTable();
        uint8 a3Idx = Cmd.add(t.at, address(v3a));
        uint8 b3Idx = Cmd.add(t.at, address(v3b));
        uint8 cIdx = Cmd.add(t.at, address(v2c));
        bool cz = v2c.token0() == address(wbtc);

        // V3a callback: take(WETH→V3a, IIA), sync, V2c→PM(calc), settle —
        // sync AFTER the take, so the settle captures the V2c deposit.
        bytes memory aFwd = Cmd.v4Take(t.wethIdx, a3Idx, AMOUNT_WETH);
        aFwd = bytes.concat(aFwd, Cmd.v4Sync(t.wethIdx));
        aFwd = bytes.concat(aFwd, Cmd.v2SwapCalc(cIdx, cz, t.pmIdx, V2_FEE));
        aFwd = bytes.concat(aFwd, Cmd.v4Settle());

        // V3b callback: V3a(to=V3b) with V3a's callback
        bytes memory bFwd =
            Cmd.v3SwapCompact(a3Idx, aZfo, AMOUNT_WETH, b3Idx, aFwd);

        bytes memory inner =
            Cmd.v3SwapCompact(b3Idx, bZfo, aUsdcOut, cIdx, bFwd);
        inner = bytes.concat(
            inner,
            mint
                ? Cmd.v4MintCompact(t.wethIdx, t.executorIdx, profit)
                : Cmd.v4TakeDelta(t.wethIdx, t.executorIdx)
        );
        o = _executeRaw(executor, bytes.concat(Cmd.preamble(t.at), Cmd.v4Unlock(inner)));
    }

    function testV3V3V2Mint() public {
        _fundPm();
        (Outcome memory o, uint256 profit) = _execV3V3V2(
            _deployV3WethUsdc(), _deployV3UsdcWbtc(), _deployV2Chain().c, true
        );
        assertTrue(o.ok, "V3-V3-V2 MINT should succeed");
        assertEq(
            v4Pm.balanceOf(address(executor), uint256(uint160(address(weth)))) - 1,
            profit,
            "profit captured as ERC6909 WETH"
        );
    }

    function testV3V3V2Take() public {
        _fundPm();
        (Outcome memory o, uint256 profit) = _execV3V3V2(
            _deployV3WethUsdc(), _deployV3UsdcWbtc(), _deployV2Chain().c, false
        );
        assertTrue(o.ok, "V3-V3-V2 TAKE should succeed");
        assertEq(
            weth.balanceOf(address(executor)), profit, "profit taken as physical WETH"
        );
    }

    // ── Self-contained MINT-vs-TAKE gas comparison (disposition 6) ────────

    /// Replaces the source's ordering-dependent `_gas_results` table. For
    /// each of the four V2-ending paths the MINT variant runs from the
    /// freshly-funded pool state, the state is rolled back, and the TAKE
    /// variant runs from the SAME pristine state — both figures are emitted
    /// with the path label, and no result depends on any other test's
    /// execution.
    function testMintVsTakeGasComparison() public {
        _fundPm();

        // V2-V2-V2
        V2Pools memory p222 = _deployV2Chain();
        uint256 snapshot = vm.snapshotState();
        (Outcome memory m, ) = _execV2V2V2(p222, true);
        assertTrue(m.ok, "V2-V2-V2 MINT gas run");
        uint256 mintGas = m.gasUsed;
        vm.revertToState(snapshot);
        (Outcome memory tk, ) = _execV2V2V2(p222, false);
        assertTrue(tk.ok, "V2-V2-V2 TAKE gas run");
        emit PmBankGasComparison("V2-V2-V2", mintGas, tk.gasUsed);

        // V3-V2-V2
        IFakeUniswapV3Pool v3a = _deployV3WethUsdc();
        V2Pools memory p322 = _deployV2Chain();
        snapshot = vm.snapshotState();
        (m, ) = _execV3V2V2(v3a, p322, true);
        assertTrue(m.ok, "V3-V2-V2 MINT gas run");
        mintGas = m.gasUsed;
        vm.revertToState(snapshot);
        (tk, ) = _execV3V2V2(v3a, p322, false);
        assertTrue(tk.ok, "V3-V2-V2 TAKE gas run");
        emit PmBankGasComparison("V3-V2-V2", mintGas, tk.gasUsed);

        // V2-V3-V2
        V2Pools memory p232 = _deployV2Chain();
        IFakeUniswapV3Pool v3b = _deployV3UsdcWbtc();
        snapshot = vm.snapshotState();
        (m, ) = _execV2V3V2(p232, v3b, true);
        assertTrue(m.ok, "V2-V3-V2 MINT gas run");
        mintGas = m.gasUsed;
        vm.revertToState(snapshot);
        (tk, ) = _execV2V3V2(p232, v3b, false);
        assertTrue(tk.ok, "V2-V3-V2 TAKE gas run");
        emit PmBankGasComparison("V2-V3-V2", mintGas, tk.gasUsed);

        // V3-V3-V2
        IFakeUniswapV3Pool g3a = _deployV3WethUsdc();
        IFakeUniswapV3Pool g3b = _deployV3UsdcWbtc();
        IFakeUniswapV2Pair v2c = _deployV2Chain().c;
        snapshot = vm.snapshotState();
        (m, ) = _execV3V3V2(g3a, g3b, v2c, true);
        assertTrue(m.ok, "V3-V3-V2 MINT gas run");
        mintGas = m.gasUsed;
        vm.revertToState(snapshot);
        (tk, ) = _execV3V3V2(g3a, g3b, v2c, false);
        assertTrue(tk.ok, "V3-V3-V2 TAKE gas run");
        emit PmBankGasComparison("V3-V3-V2", mintGas, tk.gasUsed);
    }

    // ── Per-variant gas labels (one test per path and mode) ──────────────
    //
    // The comparison above measures every path's MINT and TAKE arms in one
    // test, which a snapshot can only see as one figure. These tests run the
    // SAME arms through the same helpers, one variant per test, so the
    // committed `.gas-snapshot-evidence` carries a stable label per path and
    // mode (`testGasEvidencePmBank<Path><Mode>`). Setup is symmetric across a
    // path's two modes, so the difference between the committed figures is
    // the mode's cost, not a setup difference.

    function testGasEvidencePmBankV2V2V2Mint() public {
        _fundPm();
        (Outcome memory o, ) = _execV2V2V2(_deployV2Chain(), true);
        assertTrue(o.ok, "V2-V2-V2 MINT");
        emit GasEvidence("pm-bank/v2-v2-v2/mint", o.gasUsed);
    }

    function testGasEvidencePmBankV2V2V2Take() public {
        _fundPm();
        (Outcome memory o, ) = _execV2V2V2(_deployV2Chain(), false);
        assertTrue(o.ok, "V2-V2-V2 TAKE");
        emit GasEvidence("pm-bank/v2-v2-v2/take", o.gasUsed);
    }

    function testGasEvidencePmBankV3V2V2Mint() public {
        _fundPm();
        (Outcome memory o, ) = _execV3V2V2(_deployV3WethUsdc(), _deployV2Chain(), true);
        assertTrue(o.ok, "V3-V2-V2 MINT");
        emit GasEvidence("pm-bank/v3-v2-v2/mint", o.gasUsed);
    }

    function testGasEvidencePmBankV3V2V2Take() public {
        _fundPm();
        (Outcome memory o, ) = _execV3V2V2(_deployV3WethUsdc(), _deployV2Chain(), false);
        assertTrue(o.ok, "V3-V2-V2 TAKE");
        emit GasEvidence("pm-bank/v3-v2-v2/take", o.gasUsed);
    }

    function testGasEvidencePmBankV2V3V2Mint() public {
        _fundPm();
        (Outcome memory o, ) = _execV2V3V2(_deployV2Chain(), _deployV3UsdcWbtc(), true);
        assertTrue(o.ok, "V2-V3-V2 MINT");
        emit GasEvidence("pm-bank/v2-v3-v2/mint", o.gasUsed);
    }

    function testGasEvidencePmBankV2V3V2Take() public {
        _fundPm();
        (Outcome memory o, ) = _execV2V3V2(_deployV2Chain(), _deployV3UsdcWbtc(), false);
        assertTrue(o.ok, "V2-V3-V2 TAKE");
        emit GasEvidence("pm-bank/v2-v3-v2/take", o.gasUsed);
    }

    function testGasEvidencePmBankV3V3V2Mint() public {
        _fundPm();
        (Outcome memory o, ) = _execV3V3V2(
            _deployV3WethUsdc(), _deployV3UsdcWbtc(), _deployV2Chain().c, true
        );
        assertTrue(o.ok, "V3-V3-V2 MINT");
        emit GasEvidence("pm-bank/v3-v3-v2/mint", o.gasUsed);
    }

    function testGasEvidencePmBankV3V3V2Take() public {
        _fundPm();
        (Outcome memory o, ) = _execV3V3V2(
            _deployV3WethUsdc(), _deployV3UsdcWbtc(), _deployV2Chain().c, false
        );
        assertTrue(o.ok, "V3-V3-V2 TAKE");
        emit GasEvidence("pm-bank/v3-v3-v2/take", o.gasUsed);
    }
}
