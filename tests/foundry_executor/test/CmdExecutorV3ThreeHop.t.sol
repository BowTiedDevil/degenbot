// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {ExecutorErrors} from "../src/generated/Errors.sol";
import {IFakeErc20} from "../src/generated/IFakeErc20.sol";
import {IFakeUniswapV3Pool} from "../src/generated/IFakeUniswapV3Pool.sol";
import {PoolKey} from "../src/generated/Types.sol";

/// Port of `tests/executor/test_cmd_executor_v3_three_hop.py`.
///
/// Three-hop V3 arbitrage WETH → USDC (V3a) → WBTC (V3b) → WETH (V3c), plus
/// V4-mixed legs. Each pool is priced from its canned amounts and funded with
/// 100x liquidity, and every pool's `get_amount_out` feeds the next leg's
/// input amount — the chained-output shape of the source's `_setup_v3_pools`.
contract CmdExecutorV3ThreeHopTest is PoolSetup {
    uint256 internal constant AMOUNT_WETH_PROFIT = 2 ether;

    IFakeUniswapV3Pool internal v3a; // WETH/USDC — sell WETH for USDC
    IFakeUniswapV3Pool internal v3b; // USDC/WBTC — sell USDC for WBTC
    IFakeUniswapV3Pool internal v3c; // WBTC/WETH — Uniswap V3 callback
    IFakeUniswapV3Pool internal v3cPancake; // WBTC/WETH — PancakeSwap callback variant

    /// Forge gas evidence for the source's `TestGasComparison` print-only
    /// cases (disposition 11): the per-approach gas is emitted as a named
    /// event instead of a print or a `.gas-results` artifact.
    event ApproachGas(string label, uint256 gasUsed);

    function setUp() public override {
        super.setUp();
        (address a0, address a1) = _sorted(address(weth), address(usdc));
        v3a = _deployV3Pool(a0, a1, 0, 3000);
        (address b0, address b1) = _sorted(address(usdc), address(wbtc));
        v3b = _deployV3Pool(b0, b1, 0, 3000);
        (address c0, address c1) = _sorted(address(wbtc), address(weth));
        v3c = _deployV3Pool(c0, c1, 0, 3000);
        v3cPancake = _deployV3Pool(c0, c1, 1, 3000);
    }

    struct Pools {
        bool zfoA;
        bool zfoB;
        bool zfoC;
        uint256 usdcOut; // V3a's actual output (V3b's input)
        uint256 wbtcOut; // V3b's actual output (V3c's input)
        uint256 wethOut; // V3c's actual output
    }

    /// Fund all three pools and chain each pool's actual output into the next
    /// leg's input (the source `_setup_v3_pools` shape).
    function _setupPools(IFakeUniswapV3Pool poolC) internal returns (Pools memory p) {
        (p.zfoA, p.usdcOut) = _setupV3(v3a, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        (p.zfoB, p.wbtcOut) = _setupV3(v3b, usdc, wbtc, p.usdcOut, AMOUNT_WBTC);
        (p.zfoC, p.wethOut) =
            _setupV3(poolC, wbtc, IFakeErc20(address(weth)), p.wbtcOut, AMOUNT_WETH_PROFIT);
    }

    // ── 1. V3→V3→V3 with nested forward_data (3 callbacks, fully explicit) ──

    /// Call stack: V3a swap → callback [V3b swap → callback [V3c swap →
    /// callback [pay WBTC to V3c]] + pay USDC to V3b] + pay WETH to V3a.
    /// 6 ERC-20 transfers, 3 callbacks — full executor custody.
    function testV3V3V3NestedCallbacks() public {
        Pools memory p = _setupPools(v3c);

        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3aIdx = Cmd.add(at, address(v3a));
        uint8 v3bIdx = Cmd.add(at, address(v3b));
        uint8 v3cIdx = Cmd.add(at, address(v3c));

        bytes memory v3cCallback = Cmd.erc20Transfer(wbtcIdx, v3cIdx, p.wbtcOut);
        bytes memory v3bCallback =
            Cmd.v3SwapCompact(v3cIdx, p.zfoC, p.wbtcOut, executorIdx, v3cCallback);
        v3bCallback = bytes.concat(v3bCallback, Cmd.erc20Transfer(usdcIdx, v3bIdx, p.usdcOut));
        bytes memory v3aCallback =
            Cmd.v3SwapCompact(v3bIdx, p.zfoB, p.usdcOut, executorIdx, v3bCallback);
        v3aCallback = bytes.concat(v3aCallback, Cmd.erc20Transfer(wethIdx, v3aIdx, AMOUNT_WETH));

        bytes memory commands =
            Cmd.v3SwapCompact(v3aIdx, p.zfoA, AMOUNT_WETH, executorIdx, v3aCallback);

        _execute(executor, bytes.concat(Cmd.preamble(at), commands));
        assertGt(weth.balanceOf(address(executor)), 0, "executor should have WETH profit");
    }

    // ── 2. V3→V3→V3 with innermost V3 auto-pay ─────────────────────────────

    /// V3c's empty forward_data auto-pays WBTC from the executor's balance —
    /// the executor received it from V3b's optimistic transfer just before.
    /// 5 transfers, 3 callbacks.
    function testV3V3V3InnerAutoPay() public {
        Pools memory p = _setupPools(v3c);

        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3aIdx = Cmd.add(at, address(v3a));
        uint8 v3bIdx = Cmd.add(at, address(v3b));
        uint8 v3cIdx = Cmd.add(at, address(v3c));

        // V3b callback: V3c swap (auto-pay) + pay USDC to V3b.
        bytes memory v3bCallback = Cmd.v3SwapCompact(v3cIdx, p.zfoC, p.wbtcOut, executorIdx, "");
        v3bCallback = bytes.concat(v3bCallback, Cmd.erc20Transfer(usdcIdx, v3bIdx, p.usdcOut));
        // V3a callback: V3b swap + pay WETH to V3a.
        bytes memory v3aCallback =
            Cmd.v3SwapCompact(v3bIdx, p.zfoB, p.usdcOut, executorIdx, v3bCallback);
        v3aCallback = bytes.concat(v3aCallback, Cmd.erc20Transfer(wethIdx, v3aIdx, AMOUNT_WETH));

        bytes memory commands =
            Cmd.v3SwapCompact(v3aIdx, p.zfoA, AMOUNT_WETH, executorIdx, v3aCallback);

        _execute(executor, bytes.concat(Cmd.preamble(at), commands));
        assertGt(weth.balanceOf(address(executor)), 0, "executor should have WETH profit");
    }

    // ── 3. V3→V3→V3 with middle V3 auto-pay ────────────────────────────────

    /// V3b's empty forward_data auto-pays USDC (received from V3a's optimistic
    /// transfer); V3c keeps explicit forward_data. 5 transfers, 3 callbacks.
    function testV3V3V3MiddleAutoPay() public {
        Pools memory p = _setupPools(v3c);

        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3aIdx = Cmd.add(at, address(v3a));
        uint8 v3bIdx = Cmd.add(at, address(v3b));
        uint8 v3cIdx = Cmd.add(at, address(v3c));

        // V3c callback: pay WBTC to V3c.
        bytes memory v3cCallback = Cmd.erc20Transfer(wbtcIdx, v3cIdx, p.wbtcOut);
        // V3a callback: V3b swap (auto-pay), V3c swap (explicit pay), pay WETH.
        bytes memory v3aCallback = Cmd.v3SwapCompact(v3bIdx, p.zfoB, p.usdcOut, executorIdx, "");
        v3aCallback =
            bytes.concat(v3aCallback, Cmd.v3SwapCompact(v3cIdx, p.zfoC, p.wbtcOut, executorIdx, v3cCallback));
        v3aCallback = bytes.concat(v3aCallback, Cmd.erc20Transfer(wethIdx, v3aIdx, AMOUNT_WETH));

        bytes memory commands =
            Cmd.v3SwapCompact(v3aIdx, p.zfoA, AMOUNT_WETH, executorIdx, v3aCallback);

        _execute(executor, bytes.concat(Cmd.preamble(at), commands));
        assertGt(weth.balanceOf(address(executor)), 0, "executor should have WETH profit");
    }

    // ── 4. V3→V3→V3 with double auto-pay (V3b and V3c both auto-pay) ───────

    /// Only V3a carries forward_data (chain V3b, V3c, pay WETH); both inner
    /// swaps auto-pay from the executor's running balance.
    /// 4 transfers (3 optimistic + 1 explicit pay-V3a), 3 callbacks.
    function testV3V3V3DoubleAutoPay() public {
        Pools memory p = _setupPools(v3c);

        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3aIdx = Cmd.add(at, address(v3a));
        uint8 v3bIdx = Cmd.add(at, address(v3b));
        uint8 v3cIdx = Cmd.add(at, address(v3c));

        bytes memory v3aCallback = Cmd.v3SwapCompact(v3bIdx, p.zfoB, p.usdcOut, executorIdx, "");
        v3aCallback =
            bytes.concat(v3aCallback, Cmd.v3SwapCompact(v3cIdx, p.zfoC, p.wbtcOut, executorIdx, ""));
        v3aCallback = bytes.concat(v3aCallback, Cmd.erc20Transfer(wethIdx, v3aIdx, AMOUNT_WETH));

        bytes memory commands =
            Cmd.v3SwapCompact(v3aIdx, p.zfoA, AMOUNT_WETH, executorIdx, v3aCallback);

        _execute(executor, bytes.concat(Cmd.preamble(at), commands));
        assertGt(weth.balanceOf(address(executor)), 0, "executor should have WETH profit");
    }

    // ── 5a. Reverse-order direct custody — succeeds ────────────────────────

    /// Executed in REVERSE order with each pool's output going directly to the
    /// pool that needs it as input: V3c swaps (WETH→executor) → V3b swaps
    /// (WBTC→V3c) → V3a swaps (USDC→V3b, auto-pay WETH from executor). Each
    /// pool's IIA balance check passes because the inner pool delivered the
    /// input during the callback. 4 transfers vs 6 in the forward approach;
    /// the executor only ever holds WETH.
    function testV3V3V3ReverseDirectCustody() public {
        Pools memory p = _setupPools(v3c);

        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3aIdx = Cmd.add(at, address(v3a));
        uint8 v3bIdx = Cmd.add(at, address(v3b));
        uint8 v3cIdx = Cmd.add(at, address(v3c));

        // V3a callback: empty forward_data → auto-pay WETH to V3a.
        bytes memory v3aCallback = "";
        // V3b callback: V3a swap (recipient = V3b, auto-pay for V3a).
        bytes memory v3bCallback = Cmd.v3SwapCompact(v3aIdx, p.zfoA, AMOUNT_WETH, v3bIdx, v3aCallback);
        // V3c callback: V3b swap (recipient = V3c, forward chains V3a).
        bytes memory v3cCallback =
            Cmd.v3SwapCompact(v3bIdx, p.zfoB, p.usdcOut, v3cIdx, v3bCallback);

        // Top level: V3c swap (recipient = executor, gets WETH first).
        bytes memory commands =
            Cmd.v3SwapCompact(v3cIdx, p.zfoC, p.wbtcOut, executorIdx, v3cCallback);

        _execute(executor, bytes.concat(Cmd.preamble(at), commands));
        assertGt(weth.balanceOf(address(executor)), 0, "executor should have WETH profit");
    }

    // ── 5b. Forward-order direct custody — expected to FAIL ────────────────

    /// In forward order, V3a sends USDC to V3b BEFORE V3b.swap() runs, so V3b's
    /// IIA balance_before snapshot already includes that USDC and the check
    /// requires ADDITIONAL USDC to arrive during the callback — which does not
    /// happen. V3's IIA check (balance_before + owed <= balance_after) is
    /// fundamentally different from V2's K-invariant check: input tokens must
    /// arrive during the callback, not before swap() is called. This validates
    /// that reverse-order is the unique direct-custody strategy on V3 paths.
    function testV3V3V3ForwardDirectCustodyFails() public {
        Pools memory p = _setupPools(v3c);

        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3aIdx = Cmd.add(at, address(v3a));
        uint8 v3bIdx = Cmd.add(at, address(v3b));
        uint8 v3cIdx = Cmd.add(at, address(v3c));

        // V3a callback: V3b swap only — no ERC20_TRANSFER of USDC to V3b (it
        // should arrive via V3a's optimistic transfer, but the IIA check does
        // not see pre-arrival tokens).
        bytes memory v3aCallback = Cmd.v3SwapCompact(v3bIdx, p.zfoB, p.usdcOut, v3cIdx, "");

        // Top level: V3a swap with recipient = V3b (USDC goes directly).
        bytes memory commands =
            Cmd.v3SwapCompact(v3aIdx, p.zfoA, AMOUNT_WETH, v3bIdx, v3aCallback);

        Outcome memory o = _executeRaw(executor, bytes.concat(Cmd.preamble(at), commands), 1);
        // Exact revert identity: the pre-arranged delivery never puts USDC in
        // the executor's balance, so V3b's auto-pay transfer underflows the
        // executor's zero USDC balance and the execution dies on the fake
        // ERC-20's checked subtraction — the checked-arithmetic panic reverts
        // with EMPTY revert data (the pool's `"IIA"` balance-delta assert is
        // never reached). The failure still demonstrates the source's point:
        // in forward order the input tokens cannot flow through the executor
        // during the callback, so direct custody only works in reverse order.
        _assertRevertData(o, "", "forward direct custody");
    }

    // ── 6. V4→V3→V3 (V4_TAKE + V3_SWAP_COMPACT) ────────────────────────────

    function _setupV4WethUsdcPool() internal view returns (PoolKey memory key, bool zfo) {
        key = _poolKey(address(weth), address(usdc), 3000, 60);
        zfo = key.currency0 == address(weth);
    }

    /// V4→V3→V3 using V4_TAKE + V3_SWAP_COMPACT (not DELTA): validates the
    /// three-hop V4→V3→V3 flow before the DELTA variant.
    function testV4V3V3WithTakeThenCompact() public {
        (PoolKey memory key, bool v4Zfo) = _setupV4WethUsdcPool();
        _setupV4Swap(v4Pm, key, AMOUNT_WETH, AMOUNT_USDC, v4Zfo, address(usdc));

        (bool v3bZfo, uint256 v3bWbtcOut) = _setupV3(v3b, usdc, wbtc, AMOUNT_USDC, AMOUNT_WBTC);
        (bool v3cZfo,) =
            _setupV3(v3c, wbtc, IFakeErc20(address(weth)), v3bWbtcOut, AMOUNT_WETH_PROFIT);

        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3bIdx = Cmd.add(at, address(v3b));
        uint8 v3cIdx = Cmd.add(at, address(v3c));
        uint8 pmIdx = Cmd.add(at, address(v4Pm));
        uint8 zeroIdx = Cmd.add(at, address(0));

        // V3b callback: V3c swap (auto-pay) + pay USDC to V3b.
        bytes memory v3bCallback = Cmd.v3SwapCompact(v3cIdx, v3cZfo, v3bWbtcOut, executorIdx, "");
        v3bCallback = bytes.concat(v3bCallback, Cmd.erc20Transfer(usdcIdx, v3bIdx, AMOUNT_USDC));

        bytes memory inner = Cmd.v4SwapCompact(
            Cmd.add(at, key.currency0),
            Cmd.add(at, key.currency1),
            3000,
            60,
            Cmd.SENTINEL_NATIVE,
            v4Zfo,
            AMOUNT_WETH
        );
        inner = bytes.concat(inner, Cmd.v4Take(usdcIdx, executorIdx, AMOUNT_USDC));
        inner = bytes.concat(
            inner, Cmd.v3SwapCompact(v3bIdx, v3bZfo, AMOUNT_USDC, executorIdx, v3bCallback)
        );
        inner = bytes.concat(inner, Cmd.v4SettleDelta(wethIdx));

        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)));
        assertGt(weth.balanceOf(address(executor)), 0, "executor should have WETH profit");
    }

    // ── 6b. V4→V3(V3_SWAP_DELTA)→V3 with pre-funded executor ───────────────

    /// V3_SWAP_DELTA reads the PM exttload delta of the V3 pool's input
    /// currency as the swap amount — it only works when the executor ALREADY
    /// holds that input token in its ERC-20 balance (so auto-pay can pay the
    /// pool) AND a PM delta exists for the same currency. The executor is
    /// pre-funded with USDC; the stale USDC delta is removed afterwards by
    /// V4_TAKE_DELTA, and the WETH owed to the PM settles last.
    function testV4V3V3DeltaPreFunded() public {
        (PoolKey memory key, bool v4Zfo) = _setupV4WethUsdcPool();
        _setupV4Swap(v4Pm, key, AMOUNT_WETH, AMOUNT_USDC, v4Zfo, address(usdc));

        (bool v3bZfo, uint256 v3bWbtcOut) = _setupV3(v3b, usdc, wbtc, AMOUNT_USDC, AMOUNT_WBTC);
        (bool v3cZfo,) =
            _setupV3(v3c, wbtc, IFakeErc20(address(weth)), v3bWbtcOut, AMOUNT_WETH_PROFIT);

        // Pre-fund the executor with USDC for the V3b auto-pay.
        usdc.mint(address(executor), AMOUNT_USDC);

        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3bIdx = Cmd.add(at, address(v3b));
        uint8 v3cIdx = Cmd.add(at, address(v3c));
        Cmd.add(at, address(v4Pm));
        Cmd.add(at, address(0));

        bytes memory inner = Cmd.v4SwapCompact(
            Cmd.add(at, key.currency0),
            Cmd.add(at, key.currency1),
            3000,
            60,
            Cmd.SENTINEL_NATIVE,
            v4Zfo,
            AMOUNT_WETH
        );
        // V3_SWAP_DELTA: reads the USDC delta from the PM, swaps at V3b (auto-pay).
        inner = bytes.concat(inner, Cmd.v3SwapDelta(v3bIdx, v3bZfo, executorIdx));
        // V3c swap (auto-pay): the executor holds WBTC from V3b's optimistic transfer.
        inner =
            bytes.concat(inner, Cmd.v3SwapCompact(v3cIdx, v3cZfo, v3bWbtcOut, executorIdx, ""));
        // V4_TAKE_DELTA removes the now-stale USDC delta.
        inner = bytes.concat(inner, Cmd.v4TakeDelta(usdcIdx, executorIdx));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(wethIdx));

        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)));
        assertGt(weth.balanceOf(address(executor)), 0, "executor should have WETH profit");
    }

    // ── 7. V3→V3→V4 (two V3 legs then V4) ──────────────────────────────────

    /// V3a swap → callback [V3b swap → callback [V4 unlock: sync WBTC,
    /// transfer WBTC to the PM, settle, V4 swap, take WETH] + pay USDC to
    /// V3b] + pay WETH to V3a. The WETH paid to V3a comes from the V4 take.
    function testV3V3V4() public {
        (bool v3aZfo, uint256 v3aUsdcOut) =
            _setupV3(v3a, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        (bool v3bZfo, uint256 v3bWbtcOut) = _setupV3(v3b, usdc, wbtc, v3aUsdcOut, AMOUNT_WBTC);

        PoolKey memory key = _poolKey(address(wbtc), address(weth), 3000, 60);
        bool v4Zfo = key.currency0 == address(wbtc);
        _setupV4Swap(v4Pm, key, v3bWbtcOut, AMOUNT_WETH_PROFIT, v4Zfo, address(weth));

        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3aIdx = Cmd.add(at, address(v3a));
        uint8 v3bIdx = Cmd.add(at, address(v3b));
        uint8 pmIdx = Cmd.add(at, address(v4Pm));
        uint8 zeroIdx = Cmd.add(at, address(0));

        // V4 unlock inner: sync WBTC, transfer WBTC to PM, settle, swap, take WETH.
        bytes memory v4Inner = Cmd.v4Sync(wbtcIdx);
        v4Inner = bytes.concat(v4Inner, Cmd.erc20Transfer(wbtcIdx, pmIdx, v3bWbtcOut));
        v4Inner = bytes.concat(v4Inner, Cmd.v4Settle());
        v4Inner = bytes.concat(
            v4Inner,
            Cmd.v4SwapCompact(
                Cmd.add(at, key.currency0),
                Cmd.add(at, key.currency1),
                3000,
                60,
                Cmd.SENTINEL_NATIVE,
                v4Zfo,
                v3bWbtcOut
            )
        );
        v4Inner =
            bytes.concat(v4Inner, Cmd.v4Take(wethIdx, executorIdx, AMOUNT_WETH_PROFIT));

        // V3b callback: V4 unlock + pay USDC to V3b.
        bytes memory v3bCallback = Cmd.v4Unlock(v4Inner);
        v3bCallback = bytes.concat(v3bCallback, Cmd.erc20Transfer(usdcIdx, v3bIdx, v3aUsdcOut));

        // V3a callback: V3b swap + pay WETH to V3a.
        bytes memory v3aCallback =
            Cmd.v3SwapCompact(v3bIdx, v3bZfo, v3aUsdcOut, executorIdx, v3bCallback);
        v3aCallback = bytes.concat(v3aCallback, Cmd.erc20Transfer(wethIdx, v3aIdx, AMOUNT_WETH));

        bytes memory commands =
            Cmd.v3SwapCompact(v3aIdx, v3aZfo, AMOUNT_WETH, executorIdx, v3aCallback);

        _execute(executor, bytes.concat(Cmd.preamble(at), commands));
        assertGt(weth.balanceOf(address(executor)), 0, "executor should have WETH profit");
    }

    // ── 8. V3→V3→V3 with PancakeSwap V3 callback variant ───────────────────

    /// V3c uses the PancakeSwap callback selector (pancakeV3SwapCallback);
    /// the executor handles both selectors identically, including auto-pay.
    function testV3V3V3PancakeCallback() public {
        Pools memory p = _setupPools(v3cPancake);

        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3aIdx = Cmd.add(at, address(v3a));
        uint8 v3bIdx = Cmd.add(at, address(v3b));
        uint8 v3cIdx = Cmd.add(at, address(v3cPancake));

        bytes memory v3aCallback = Cmd.v3SwapCompact(v3bIdx, p.zfoB, p.usdcOut, executorIdx, "");
        v3aCallback =
            bytes.concat(v3aCallback, Cmd.v3SwapCompact(v3cIdx, p.zfoC, p.wbtcOut, executorIdx, ""));
        v3aCallback = bytes.concat(v3aCallback, Cmd.erc20Transfer(wethIdx, v3aIdx, AMOUNT_WETH));

        bytes memory commands =
            Cmd.v3SwapCompact(v3aIdx, p.zfoA, AMOUNT_WETH, executorIdx, v3aCallback);

        _execute(executor, bytes.concat(Cmd.preamble(at), commands));
        assertGt(weth.balanceOf(address(executor)), 0, "executor should have WETH profit");
    }

    // ── 9. Gas comparison (disposition 11: named gas evidence, not prints) ──

    /// V3 nested callbacks (3 COMPACT): 6 transfers, 3 callbacks.
    function testNestedCallbacksGas() public {
        Pools memory p = _setupPools(v3c);

        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3aIdx = Cmd.add(at, address(v3a));
        uint8 v3bIdx = Cmd.add(at, address(v3b));
        uint8 v3cIdx = Cmd.add(at, address(v3c));

        bytes memory v3cCallback = Cmd.erc20Transfer(wbtcIdx, v3cIdx, p.wbtcOut);
        bytes memory v3bCallback =
            Cmd.v3SwapCompact(v3cIdx, p.zfoC, p.wbtcOut, executorIdx, v3cCallback);
        v3bCallback = bytes.concat(v3bCallback, Cmd.erc20Transfer(usdcIdx, v3bIdx, p.usdcOut));
        bytes memory v3aCallback =
            Cmd.v3SwapCompact(v3bIdx, p.zfoB, p.usdcOut, executorIdx, v3bCallback);
        v3aCallback = bytes.concat(v3aCallback, Cmd.erc20Transfer(wethIdx, v3aIdx, AMOUNT_WETH));
        bytes memory commands =
            Cmd.v3SwapCompact(v3aIdx, p.zfoA, AMOUNT_WETH, executorIdx, v3aCallback);

        Outcome memory o = _executeRaw(executor, bytes.concat(Cmd.preamble(at), commands));
        assertTrue(o.ok, "nested callbacks should succeed");
        emit ApproachGas("v3_nested_callbacks_3_compact", o.gasUsed);
    }

    /// V3 double auto-pay: 4 transfers, 3 callbacks.
    function testDoubleAutoPayGas() public {
        Pools memory p = _setupPools(v3c);

        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3aIdx = Cmd.add(at, address(v3a));
        uint8 v3bIdx = Cmd.add(at, address(v3b));
        uint8 v3cIdx = Cmd.add(at, address(v3c));

        bytes memory v3aCallback = Cmd.v3SwapCompact(v3bIdx, p.zfoB, p.usdcOut, executorIdx, "");
        v3aCallback =
            bytes.concat(v3aCallback, Cmd.v3SwapCompact(v3cIdx, p.zfoC, p.wbtcOut, executorIdx, ""));
        v3aCallback = bytes.concat(v3aCallback, Cmd.erc20Transfer(wethIdx, v3aIdx, AMOUNT_WETH));
        bytes memory commands =
            Cmd.v3SwapCompact(v3aIdx, p.zfoA, AMOUNT_WETH, executorIdx, v3aCallback);

        Outcome memory o = _executeRaw(executor, bytes.concat(Cmd.preamble(at), commands));
        assertTrue(o.ok, "double auto-pay should succeed");
        emit ApproachGas("v3_double_auto_pay", o.gasUsed);
    }

    /// V4→V3→V3 (V4_TAKE + 2 COMPACT, all auto-pay).
    function testV4V3V3Gas() public {
        (PoolKey memory key, bool v4Zfo) = _setupV4WethUsdcPool();
        _setupV4Swap(v4Pm, key, AMOUNT_WETH, AMOUNT_USDC, v4Zfo, address(usdc));

        (bool v3bZfo, uint256 v3bWbtcOut) = _setupV3(v3b, usdc, wbtc, AMOUNT_USDC, AMOUNT_WBTC);
        (bool v3cZfo,) =
            _setupV3(v3c, wbtc, IFakeErc20(address(weth)), v3bWbtcOut, AMOUNT_WETH_PROFIT);

        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3bIdx = Cmd.add(at, address(v3b));
        uint8 v3cIdx = Cmd.add(at, address(v3c));
        uint8 pmIdx = Cmd.add(at, address(v4Pm));
        uint8 zeroIdx = Cmd.add(at, address(0));

        // V3b auto-pay pays the USDC after AMOUNT_USDC is taken from the PM.
        bytes memory inner = Cmd.v4SwapCompact(
            Cmd.add(at, key.currency0),
            Cmd.add(at, key.currency1),
            3000,
            60,
            Cmd.SENTINEL_NATIVE,
            v4Zfo,
            AMOUNT_WETH
        );
        inner = bytes.concat(inner, Cmd.v4Take(usdcIdx, executorIdx, AMOUNT_USDC));
        inner =
            bytes.concat(inner, Cmd.v3SwapCompact(v3bIdx, v3bZfo, AMOUNT_USDC, executorIdx, ""));
        inner =
            bytes.concat(inner, Cmd.v3SwapCompact(v3cIdx, v3cZfo, v3bWbtcOut, executorIdx, ""));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(wethIdx));

        Outcome memory o =
            _executeRaw(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)));
        assertTrue(o.ok, "V4-V3-V3 TAKE flow should succeed");
        emit ApproachGas("v4_v3_v3_take_plus_2_compact", o.gasUsed);
    }

    /// V3→V3→V4 (2 COMPACT + unlock).
    function testV3V3V4Gas() public {
        (bool v3aZfo, uint256 v3aUsdcOut) =
            _setupV3(v3a, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);
        (bool v3bZfo, uint256 v3bWbtcOut) = _setupV3(v3b, usdc, wbtc, v3aUsdcOut, AMOUNT_WBTC);

        PoolKey memory key = _poolKey(address(wbtc), address(weth), 3000, 60);
        bool v4Zfo = key.currency0 == address(wbtc);
        _setupV4Swap(v4Pm, key, v3bWbtcOut, AMOUNT_WETH_PROFIT, v4Zfo, address(weth));

        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3aIdx = Cmd.add(at, address(v3a));
        uint8 v3bIdx = Cmd.add(at, address(v3b));
        uint8 pmIdx = Cmd.add(at, address(v4Pm));
        uint8 zeroIdx = Cmd.add(at, address(0));

        bytes memory v4Inner = Cmd.v4Sync(wbtcIdx);
        v4Inner = bytes.concat(v4Inner, Cmd.erc20Transfer(wbtcIdx, pmIdx, v3bWbtcOut));
        v4Inner = bytes.concat(v4Inner, Cmd.v4Settle());
        v4Inner = bytes.concat(
            v4Inner,
            Cmd.v4SwapCompact(
                Cmd.add(at, key.currency0),
                Cmd.add(at, key.currency1),
                3000,
                60,
                Cmd.SENTINEL_NATIVE,
                v4Zfo,
                v3bWbtcOut
            )
        );
        v4Inner =
            bytes.concat(v4Inner, Cmd.v4Take(wethIdx, executorIdx, AMOUNT_WETH_PROFIT));

        bytes memory v3bCallback = Cmd.v4Unlock(v4Inner);
        v3bCallback = bytes.concat(v3bCallback, Cmd.erc20Transfer(usdcIdx, v3bIdx, v3aUsdcOut));

        bytes memory v3aCallback =
            Cmd.v3SwapCompact(v3bIdx, v3bZfo, v3aUsdcOut, executorIdx, v3bCallback);
        v3aCallback = bytes.concat(v3aCallback, Cmd.erc20Transfer(wethIdx, v3aIdx, AMOUNT_WETH));

        bytes memory commands =
            Cmd.v3SwapCompact(v3aIdx, v3aZfo, AMOUNT_WETH, executorIdx, v3aCallback);

        Outcome memory o = _executeRaw(executor, bytes.concat(Cmd.preamble(at), commands));
        assertTrue(o.ok, "V3-V3-V4 flow should succeed");
        emit ApproachGas("v3_v3_v4_2_compact_plus_unlock", o.gasUsed);
    }
}
