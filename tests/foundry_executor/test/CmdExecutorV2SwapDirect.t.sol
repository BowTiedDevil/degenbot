// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Vm} from "../src/Vm.sol";
import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {IFakeUniswapV2Pair} from "../src/generated/IFakeUniswapV2Pair.sol";
import {IFakeErc20} from "../src/generated/IFakeErc20.sol";
import {PoolKey} from "../src/generated/Types.sol";

/// Port of `tests/executor/test_cmd_executor_v2_swap_direct.py`.
///
/// V2_SWAP_DIRECT (0x22) — explicit-amount, no-callback V2 swap. The pair must
/// already hold input tokens (excess balance from a pre-fund), and the executor
/// calls swap(data = b"") — no callback invoked. Compared to V2_SWAP_CALC it
/// skips the on-chain amount computation (getReserves, token0, token1,
/// balanceOf, getAmountOut) at the cost of carrying amount_out in the stream;
/// the pair's runtime K-invariant check verifies correctness either way.
///
/// Address-table indices are resolved inline with `Cmd.add` (idempotent per
/// table) instead of cached locals, matching the Ape encoders' behavior.
contract CmdExecutorV2SwapDirectTest is PoolSetup {
    // AMOUNT_WETH / AMOUNT_USDC / AMOUNT_WBTC are the fixture constants.
    uint256 internal constant AMOUNT_WETH_PROFIT = 2 ether;

    IFakeUniswapV2Pair internal v2A; // WETH/USDC
    IFakeUniswapV2Pair internal v2B; // USDC/WBTC
    IFakeUniswapV2Pair internal v2C; // WBTC/WETH

    function setUp() public override {
        super.setUp();
        (address t01, address t02) = _sorted(address(weth), address(usdc));
        v2A = _deployV2Pair(t01, t02, 0, V2_FEE);
        (address t11, address t12) = _sorted(address(usdc), address(wbtc));
        v2B = _deployV2Pair(t11, t12, 0, V2_FEE);
        (address t21, address t22) = _sorted(address(wbtc), address(weth));
        v2C = _deployV2Pair(t21, t22, 0, V2_FEE);
    }

    // ── Helpers ───────────────────────────────────────────────────────────

    /// Set up a V2 pair with ample liquidity at the correct price for
    /// V2_SWAP_CALC / V2_SWAP_DIRECT (the source `_setup_v2_for_calc`).
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

    function _v4Swap(Cmd.Table memory at, PoolKey memory key, bool zfo, uint256 amount, uint8 zeroIdx)
        internal
        pure
        returns (bytes memory)
    {
        return Cmd.v4SwapCompact(
            Cmd.add(at, key.currency0),
            Cmd.add(at, key.currency1),
            uint16(key.fee),
            int16(key.tick_spacing),
            zeroIdx,
            zfo,
            amount
        );
    }

    function _v2SwapDirect(
        Cmd.Table memory at,
        IFakeUniswapV2Pair pool,
        bool zfo,
        uint256 amountOut,
        uint8 recipientIdx
    ) internal pure returns (bytes memory) {
        return Cmd.v2SwapDirect(Cmd.add(at, address(pool)), zfo, amountOut, recipientIdx);
    }

    function _v2SwapCalc(
        Cmd.Table memory at,
        IFakeUniswapV2Pair pool,
        bool zfo,
        uint8 recipientIdx
    ) internal pure returns (bytes memory) {
        return Cmd.v2SwapCalc(Cmd.add(at, address(pool)), zfo, recipientIdx, V2_FEE);
    }

    function _v2Swap(
        Cmd.Table memory at,
        IFakeUniswapV2Pair pool,
        bool zfo,
        uint256 amountOut,
        uint8 recipientIdx,
        bytes memory forwardData
    ) internal pure returns (bytes memory) {
        return Cmd.v2SwapCompact(
            Cmd.add(at, address(pool)), zfo, amountOut, recipientIdx, V2_FEE, forwardData
        );
    }

    function _erc20Xfer(Cmd.Table memory at, address token, address recipient, uint256 amount)
        internal
        pure
        returns (bytes memory)
    {
        return Cmd.erc20Transfer(Cmd.add(at, token), Cmd.add(at, recipient), amount);
    }

    /// Snapshot balances, execute with profit mode 2, then verify both the
    /// physical transfer count and the tracked-token conservation invariants
    /// (the source `_run_and_verify` + `_verify_conservation`).
    function _runAndVerify(
        Cmd.Table memory at,
        bytes memory commands,
        address[] memory accounts,
        uint256 expectedTransfers,
        string memory label
    ) internal {
        address[] memory tokens = new address[](4);
        tokens[0] = address(weth);
        tokens[1] = address(usdc);
        tokens[2] = address(wbtc);
        tokens[3] = NATIVE;

        BalanceSnapshot memory before = _snapshotBalances(tokens, accounts);
        vm.recordLogs();
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);
        Vm.Log[] memory logs = vm.getRecordedLogs();

        assertEq(
            _countTransfers(logs),
            expectedTransfers,
            string.concat(label, ": physical transfer count mismatch")
        );

        BalanceSnapshot memory afterSnap = _snapshotBalances(tokens, accounts);
        _assertTokenConserved(before, afterSnap, 1, label); // USDC
        _assertTokenConserved(before, afterSnap, 2, label); // WBTC
        _assertWethEthConserved(before, afterSnap, 0, label); // WETH + ETH
    }

    // ── Basic V2_SWAP_DIRECT functionality ────────────────────────────────

    /// V2_SWAP_DIRECT with the executor pre-funding the V2 pair via
    /// ERC20_TRANSFER (creates excess balance), then an explicit-amount swap
    /// with no callback; the pair's K-invariant passes.
    function testV2SwapDirectSimple() public {
        weth.mint(address(executor), AMOUNT_WETH);
        bool aZfo = _setupV2ForCalc(v2A, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        uint256 aOut =
            _v2GetAmountOut(AMOUNT_WETH, weth.balanceOf(address(v2A)), usdc.balanceOf(address(v2A)), V2_FEE);

        Cmd.Table memory at = _emptyTable();
        bytes memory commands = _erc20Xfer(at, address(weth), address(v2A), AMOUNT_WETH);
        commands = bytes.concat(
            commands,
            Cmd.v2SwapDirect(
                Cmd.add(at, address(v2A)), aZfo, aOut, Cmd.add(at, address(executor))
            )
        );

        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);
    }

    /// V2_SWAP_DIRECT with V4_TAKE pre-funding the V2 pair — the motivating
    /// use case: V4_TAKE sends tokens directly to a V2 pair, creating excess
    /// balance; V2_SWAP_DIRECT then swaps without on-chain computation.
    ///
    /// Flow (V4→V2 single hop): V4 swap WETH→USDC; V4_TAKE USDC→V2a;
    /// V2_SWAP_DIRECT V2a (sends WETH back to executor); V4_SETTLE_DELTA WETH.
    function testV2SwapDirectV4TakePrefund() public {
        weth.mint(address(executor), AMOUNT_WETH);
        bool aZfo = _setupV2ForCalc(v2A, usdc, IFakeErc20(address(weth)), AMOUNT_USDC, AMOUNT_WETH);
        uint256 aOut =
            _v2GetAmountOut(AMOUNT_USDC, usdc.balanceOf(address(v2A)), weth.balanceOf(address(v2A)), V2_FEE);

        (PoolKey memory key, bool v4Zfo) = _setupV4Pool(
            address(weth), address(usdc), AMOUNT_WETH, AMOUNT_USDC, 3000, 60, address(usdc)
        );

        Cmd.Table memory at = _emptyTable();
        bytes memory inner = _v4Swap(at, key, v4Zfo, AMOUNT_WETH, Cmd.add(at, address(0)));
        inner = bytes.concat(
            inner, Cmd.v4Take(Cmd.add(at, address(usdc)), Cmd.add(at, address(v2A)), AMOUNT_USDC)
        );
        inner = bytes.concat(
            inner, _v2SwapDirect(at, v2A, aZfo, aOut, Cmd.add(at, address(executor)))
        );
        inner = bytes.concat(inner, Cmd.v4SettleDelta(Cmd.add(at, address(weth))));

        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)), 2);
    }

    // ── V2_SWAP_DIRECT vs V2_SWAP_CALC — same results, different gas ──────

    /// V2-V2-V2 with V2_SWAP_DIRECT for all V2 swaps: explicit amounts, no
    /// on-chain computation. Same routing as the optimized V2-V2-V2 but the
    /// amounts are pre-computed off-chain from the same V2 math.
    ///
    /// Transfers: V2c→executor (WETH), executor→V2a (WETH), V2a→V2b (USDC),
    /// V2b→V2c (WBTC) = 4.
    function testV2V2V2Direct() public {
        bool aZfo = _setupV2ForCalc(v2A, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        bool bZfo = _setupV2ForCalc(v2B, usdc, IFakeErc20(address(wbtc)), AMOUNT_USDC, AMOUNT_WBTC);
        bool cZfo =
            _setupV2ForCalc(
                v2C, IFakeErc20(address(wbtc)), IFakeErc20(address(weth)), AMOUNT_WBTC, AMOUNT_WETH_PROFIT
            );

        // Compute the chain amounts from V2 math (what V2_SWAP_CALC would
        // compute on-chain).
        uint256 aOut =
            _v2GetAmountOut(AMOUNT_WETH, weth.balanceOf(address(v2A)), usdc.balanceOf(address(v2A)), V2_FEE);
        uint256 bOut =
            _v2GetAmountOut(aOut, usdc.balanceOf(address(v2B)), wbtc.balanceOf(address(v2B)), V2_FEE);
        uint256 cOut =
            _v2GetAmountOut(bOut, wbtc.balanceOf(address(v2C)), weth.balanceOf(address(v2C)), V2_FEE);

        Cmd.Table memory at = _emptyTable();
        uint8 executorIdx = Cmd.add(at, address(executor));

        // Inside V2c's callback: transfer WETH to V2a, then V2a→V2b, then
        // V2b→V2c — all with V2_SWAP_DIRECT.
        bytes memory cFwd = _erc20Xfer(at, address(weth), address(v2A), AMOUNT_WETH);
        cFwd = bytes.concat(
            cFwd, Cmd.v2SwapDirect(Cmd.add(at, address(v2A)), aZfo, aOut, Cmd.add(at, address(v2B)))
        );
        cFwd = bytes.concat(
            cFwd, Cmd.v2SwapDirect(Cmd.add(at, address(v2B)), bZfo, bOut, Cmd.add(at, address(v2C)))
        );

        // Flash-borrow WETH from V2c — callback fires on the executor.
        bytes memory commands = _v2Swap(at, v2C, cZfo, cOut, executorIdx, cFwd);

        address[] memory accounts = new address[](4);
        accounts[0] = address(executor);
        accounts[1] = address(v2A);
        accounts[2] = address(v2B);
        accounts[3] = address(v2C);
        _runAndVerify(at, commands, accounts, 4, "TestV2V2V2.Direct");
    }

    // ── Three-hop paths using V2_SWAP_DIRECT ──────────────────────────────

    /// V2-V4-V4 with V2_SWAP_DIRECT (the path that motivated the command).
    ///
    /// Inside V4 unlock: V4_SYNC(USDC); V4_TAKE(WETH, V2a) [1 xfer];
    /// V2_SWAP_DIRECT V2a→PM [1 xfer]; V4_SETTLE(); V4b + V4c swaps (delta
    /// netting); V4_TAKE(WETH, executor, profit) [1 xfer].
    function testV2V4V4Direct() public {
        bool aZfo = _setupV2ForCalc(v2A, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        uint256 aOut =
            _v2GetAmountOut(AMOUNT_WETH, weth.balanceOf(address(v2A)), usdc.balanceOf(address(v2A)), V2_FEE);

        (PoolKey memory bPk, bool bZfo) =
            _setupV4Pool(address(usdc), address(wbtc), aOut, AMOUNT_WBTC, 500, 10, address(wbtc));
        (PoolKey memory cPk, bool cZfo) = _setupV4Pool(
            address(wbtc), address(weth), AMOUNT_WBTC, AMOUNT_WETH_PROFIT, 10_000, 200, address(weth)
        );

        Cmd.Table memory at = _emptyTable();
        uint8 zeroIdx = Cmd.add(at, address(0));
        uint256 profit = AMOUNT_WETH_PROFIT - AMOUNT_WETH;

        bytes memory v4Inner = Cmd.v4Sync(Cmd.add(at, address(usdc)));
        v4Inner = bytes.concat(
            v4Inner, Cmd.v4Take(Cmd.add(at, address(weth)), Cmd.add(at, address(v2A)), AMOUNT_WETH)
        );
        v4Inner = bytes.concat(
            v4Inner,
            Cmd.v2SwapDirect(Cmd.add(at, address(v2A)), aZfo, aOut, Cmd.add(at, address(v4Pm)))
        );
        v4Inner = bytes.concat(v4Inner, Cmd.v4Settle());
        v4Inner = bytes.concat(v4Inner, _v4Swap(at, bPk, bZfo, aOut, zeroIdx));
        v4Inner = bytes.concat(v4Inner, _v4Swap(at, cPk, cZfo, AMOUNT_WBTC, zeroIdx));
        v4Inner = bytes.concat(
            v4Inner,
            Cmd.v4Take(Cmd.add(at, address(weth)), Cmd.add(at, address(executor)), profit)
        );

        address[] memory accounts = new address[](3);
        accounts[0] = address(executor);
        accounts[1] = address(v4Pm);
        accounts[2] = address(v2A);
        _runAndVerify(at, Cmd.v4Unlock(v4Inner), accounts, 3, "TestV2V4V4.Direct");
    }

    /// V4-V2-V2 with V2_SWAP_DIRECT for both V2 pools: V4_TAKE sends USDC to
    /// V2b (excess), V2b sends WBTC to V2c (excess), V2c sends WETH to the
    /// executor — no callbacks on the V2 pairs. 4 physical transfers.
    function testV4V2V2Direct() public {
        (PoolKey memory aPk, bool aZfo) = _setupV4Pool(
            address(weth), address(usdc), AMOUNT_WETH, AMOUNT_USDC, 3000, 60, address(usdc)
        );
        bool bZfo = _setupV2ForCalc(v2B, usdc, IFakeErc20(address(wbtc)), AMOUNT_USDC, AMOUNT_WBTC);
        uint256 bOut =
            _v2GetAmountOut(AMOUNT_USDC, usdc.balanceOf(address(v2B)), wbtc.balanceOf(address(v2B)), V2_FEE);

        bool cZfo =
            _setupV2ForCalc(
                v2C, IFakeErc20(address(wbtc)), IFakeErc20(address(weth)), AMOUNT_WBTC, AMOUNT_WETH_PROFIT
            );
        uint256 cOut =
            _v2GetAmountOut(bOut, wbtc.balanceOf(address(v2C)), weth.balanceOf(address(v2C)), V2_FEE);

        Cmd.Table memory at = _emptyTable();
        uint8 zeroIdx = Cmd.add(at, address(0));

        bytes memory inner = _v4Swap(at, aPk, aZfo, AMOUNT_WETH, zeroIdx);
        inner = bytes.concat(
            inner, Cmd.v4Take(Cmd.add(at, address(usdc)), Cmd.add(at, address(v2B)), AMOUNT_USDC)
        );
        inner = bytes.concat(
            inner, _v2SwapDirect(at, v2B, bZfo, bOut, Cmd.add(at, address(v2C)))
        );
        inner = bytes.concat(
            inner, _v2SwapDirect(at, v2C, cZfo, cOut, Cmd.add(at, address(executor)))
        );
        inner = bytes.concat(inner, Cmd.v4SettleDelta(Cmd.add(at, address(weth))));

        address[] memory accounts = new address[](4);
        accounts[0] = address(executor);
        accounts[1] = address(v4Pm);
        accounts[2] = address(v2B);
        accounts[3] = address(v2C);
        _runAndVerify(at, Cmd.v4Unlock(inner), accounts, 4, "TestV4V2V2.Direct");
    }

    /// V4-V2-V4 with V2_SWAP_DIRECT: V4_TAKE sends USDC directly to V2b
    /// (excess balance); V2_SWAP_DIRECT sends WBTC to the executor (no
    /// callback, no on-chain computation); the V4c swap consumes the WBTC
    /// delta. 4 physical transfers.
    function testV4V2V4Direct() public {
        (PoolKey memory aPk, bool aZfo) = _setupV4Pool(
            address(weth), address(usdc), AMOUNT_WETH, AMOUNT_USDC, 3000, 60, address(usdc)
        );
        bool bZfo = _setupV2ForCalc(v2B, usdc, IFakeErc20(address(wbtc)), AMOUNT_USDC, AMOUNT_WBTC);
        uint256 bOut =
            _v2GetAmountOut(AMOUNT_USDC, usdc.balanceOf(address(v2B)), wbtc.balanceOf(address(v2B)), V2_FEE);

        (PoolKey memory cPk, bool cZfo) = _setupV4Pool(
            address(wbtc), address(weth), bOut, AMOUNT_WETH_PROFIT, 10_000, 200, address(weth)
        );

        Cmd.Table memory at = _emptyTable();
        uint8 zeroIdx = Cmd.add(at, address(0));

        bytes memory inner = _v4Swap(at, aPk, aZfo, AMOUNT_WETH, zeroIdx);
        inner = bytes.concat(
            inner, Cmd.v4Take(Cmd.add(at, address(usdc)), Cmd.add(at, address(v2B)), AMOUNT_USDC)
        );
        inner = bytes.concat(
            inner, _v2SwapDirect(at, v2B, bZfo, bOut, Cmd.add(at, address(executor)))
        );
        inner = bytes.concat(inner, _v4Swap(at, cPk, cZfo, bOut, zeroIdx));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(Cmd.add(at, address(wbtc))));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(Cmd.add(at, address(weth))));

        address[] memory accounts = new address[](3);
        accounts[0] = address(executor);
        accounts[1] = address(v4Pm);
        accounts[2] = address(v2B);
        _runAndVerify(at, Cmd.v4Unlock(inner), accounts, 4, "TestV4V2V4.Direct");
    }

    /// V2-V4-V2 with V2_SWAP_DIRECT for V2a→PM (delta netting): same routing
    /// as the optimized V2-V4-V2 but with an explicit amount and no on-chain
    /// computation for the V2 leg. 4 physical transfers.
    function testV2V4V2Direct() public {
        weth.mint(address(executor), AMOUNT_WETH);
        bool aZfo = _setupV2ForCalc(v2A, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        uint256 aOut =
            _v2GetAmountOut(AMOUNT_WETH, weth.balanceOf(address(v2A)), usdc.balanceOf(address(v2A)), V2_FEE);

        (PoolKey memory bPk, bool bZfo) =
            _setupV4Pool(address(usdc), address(wbtc), aOut, AMOUNT_WBTC, 500, 10, address(wbtc));
        (bool cZfo, uint256 cOut) =
            _setupV2Pair(v2C, IFakeErc20(address(wbtc)), IFakeErc20(address(weth)), AMOUNT_WBTC, V2_FEE);

        Cmd.Table memory at = _emptyTable();
        uint8 zeroIdx = Cmd.add(at, address(0));

        // V4 unlock: sync/settle USDC, V2a→PM (DIRECT), V4b swap, take
        // WBTC→V2c.
        bytes memory v4Inner = Cmd.v4Sync(Cmd.add(at, address(usdc)));
        v4Inner = bytes.concat(
            v4Inner,
            Cmd.v2SwapDirect(Cmd.add(at, address(v2A)), aZfo, aOut, Cmd.add(at, address(v4Pm)))
        );
        v4Inner = bytes.concat(v4Inner, Cmd.v4Settle());
        v4Inner = bytes.concat(v4Inner, _v4Swap(at, bPk, bZfo, aOut, zeroIdx));
        v4Inner = bytes.concat(
            v4Inner,
            Cmd.v4Take(Cmd.add(at, address(wbtc)), Cmd.add(at, address(v2C)), AMOUNT_WBTC)
        );

        // V2c fires first. Callback: WETH→V2a (excess), then the V4 unlock.
        bytes memory cFwd = _erc20Xfer(at, address(weth), address(v2A), AMOUNT_WETH);
        cFwd = bytes.concat(cFwd, Cmd.v4Unlock(v4Inner));

        bytes memory commands = _v2Swap(
            at, v2C, cZfo, cOut, Cmd.add(at, address(executor)), cFwd
        );

        address[] memory accounts = new address[](4);
        accounts[0] = address(executor);
        accounts[1] = address(v4Pm);
        accounts[2] = address(v2A);
        accounts[3] = address(v2C);
        _runAndVerify(at, commands, accounts, 4, "TestV2V4V2.Direct");
    }

    // ── Gas comparison: V2_SWAP_DIRECT vs V2_SWAP_CALC ────────────────────

    event DirectVsCalcGas(string label, uint256 calcGas, uint256 directGas, int256 saved);

    /// One leg of the gas comparison: build the V4-unlock stream for the
    /// V2-V4-V4 path with either V2_SWAP_CALC or V2_SWAP_DIRECT for the V2a→PM
    /// leg, execute with the default profit check, and report the gas.
    function _runV2V4V4Leg(bool useDirect) internal returns (uint256 gasUsed, uint256 aOut) {
        bool aZfo = v2A.token0() == address(weth);
        aOut = _v2GetAmountOut(
            AMOUNT_WETH, weth.balanceOf(address(v2A)), usdc.balanceOf(address(v2A)), V2_FEE
        );
        uint256 profit = AMOUNT_WETH_PROFIT - AMOUNT_WETH;

        (PoolKey memory bPk, bool bZfo) =
            _setupV4Pool(address(usdc), address(wbtc), aOut, AMOUNT_WBTC, 500, 10, address(wbtc));
        (PoolKey memory cPk, bool cZfo) = _setupV4Pool(
            address(wbtc), address(weth), AMOUNT_WBTC, AMOUNT_WETH_PROFIT, 10_000, 200, address(weth)
        );

        Cmd.Table memory at = _emptyTable();
        uint8 zeroIdx = Cmd.add(at, address(0));

        bytes memory v4Inner = Cmd.v4Sync(Cmd.add(at, address(usdc)));
        v4Inner = bytes.concat(
            v4Inner, Cmd.v4Take(Cmd.add(at, address(weth)), Cmd.add(at, address(v2A)), AMOUNT_WETH)
        );
        if (useDirect) {
            v4Inner = bytes.concat(
                v4Inner,
                Cmd.v2SwapDirect(Cmd.add(at, address(v2A)), aZfo, aOut, Cmd.add(at, address(v4Pm)))
            );
        } else {
            v4Inner = bytes.concat(
                v4Inner,
                Cmd.v2SwapCalc(Cmd.add(at, address(v2A)), aZfo, Cmd.add(at, address(v4Pm)), V2_FEE)
            );
        }
        v4Inner = bytes.concat(v4Inner, Cmd.v4Settle());
        v4Inner = bytes.concat(v4Inner, _v4Swap(at, bPk, bZfo, aOut, zeroIdx));
        v4Inner = bytes.concat(v4Inner, _v4Swap(at, cPk, cZfo, AMOUNT_WBTC, zeroIdx));
        v4Inner = bytes.concat(
            v4Inner,
            Cmd.v4Take(Cmd.add(at, address(weth)), Cmd.add(at, address(executor)), profit)
        );

        Outcome memory o =
            _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(v4Inner)));
        return (o.gasUsed, aOut);
    }

    /// Named gas-evidence case (the port of the source's `pytest.mark.gas`
    /// surface): V2_SWAP_DIRECT vs V2_SWAP_CALC on the V2-V4-V4 path. Both
    /// commands produce identical results; DIRECT saves ~4 staticcalls on cold
    /// V2 pair slots by skipping the on-chain amount computation, and the
    /// assertion pins DIRECT < CALC.
    function testGasComparisonV2V4V4() public {
        _setupV2ForCalc(v2A, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        (uint256 gasCalc, uint256 aOut) = _runV2V4V4Leg(false);

        // Reset and re-setup the V2 pair (was drained by the CALC run).
        v2A.reset();
        _setupV2ForCalc(v2A, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        (uint256 gasDirect, uint256 aOut2) = _runV2V4V4Leg(true);
        assertEq(aOut2, aOut, "V2 output amount should be identical after reset");

        emit DirectVsCalcGas("V2-V4-V4", gasCalc, gasDirect, int256(gasCalc) - int256(gasDirect));
        assertLt(
            gasDirect,
            gasCalc,
            "V2_SWAP_DIRECT should be cheaper than V2_SWAP_CALC (skips ~4 staticcalls)"
        );
    }
}
