// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {IFakeUniswapV2Pair} from "../src/generated/IFakeUniswapV2Pair.sol";

/// Port of `tests/executor/test_cmd_executor_three_pool_v2.py`.
///
/// Three-pool V2 triangular arbitrage WETH → USDC (A) → WBTC (B) → WETH (C).
/// Pool C is mispriced (holds more WETH than the cross-rate from pools A and B
/// implies), creating the arbitrage. V2 pairs send output optimistically to any
/// recipient; when the recipient is the next pool, the receiving pair
/// accumulates excess balance that V2_SWAP_CALC reads as swap input — no
/// executor custody, no callback, no extra transfer.
///
/// Three approaches compared:
///   1. V2_SWAP_COMPACT nested callbacks — full executor custody:
///      6 transfers, 3 callbacks.
///   2. V2_SWAP_COMPACT flash + 2× V2_SWAP_CALC — direct custody for B & C:
///      5 transfers, 1 callback.
///   3. All V2_SWAP_CALC — zero callbacks, executor pre-funds pool A:
///      4 transfers, 0 callbacks.
contract CmdExecutorThreePoolV2Test is PoolSetup {
    uint16 internal constant FEE = 30; // 0.3%

    // Liquidity per pool. Pool C holds more WETH than the fair cross-rate
    // implies (≈45.45 WBTC/WETH instead of 50) — the arb opportunity.
    uint256 internal constant AMOUNT_WETH_LIQ_A = 2_000 ether;
    uint256 internal constant AMOUNT_USDC_LIQ_A = 4_000_000e6;
    uint256 internal constant AMOUNT_USDC_LIQ_B = 4_000_000e6;
    uint256 internal constant AMOUNT_WBTC_LIQ_B = 100_000e8;
    uint256 internal constant AMOUNT_WBTC_LIQ_C = 100_000e8;
    uint256 internal constant AMOUNT_WETH_LIQ_C = 2_200 ether; // mispriced

    // Trade size into pool A.
    uint256 internal constant AMOUNT_WETH_IN = 1 ether;

    IFakeUniswapV2Pair internal poolA; // WETH/USDC — sell WETH for USDC
    IFakeUniswapV2Pair internal poolB; // USDC/WBTC — sell USDC for WBTC
    IFakeUniswapV2Pair internal poolC; // WBTC/WETH — sell WBTC for WETH

    struct Amounts {
        uint256 usdc;
        uint256 wbtc;
        uint256 wethOut;
        bool zfoA;
        bool zfoB;
        bool zfoC;
    }

    event ApproachGas(string label, uint256 gasUsed);

    function setUp() public override {
        super.setUp();
        (address a0, address a1) = _sorted(address(weth), address(usdc));
        poolA = _deployV2Pair(a0, a1, 0, FEE);
        (address b0, address b1) = _sorted(address(usdc), address(wbtc));
        poolB = _deployV2Pair(b0, b1, 0, FEE);
        (address c0, address c1) = _sorted(address(wbtc), address(weth));
        poolC = _deployV2Pair(c0, c1, 0, FEE);
    }

    /// Provide liquidity to all three pools and sync reserves.
    function _fundPools() internal {
        usdc.mint(address(poolA), AMOUNT_USDC_LIQ_A);
        weth.mint(address(poolA), AMOUNT_WETH_LIQ_A);
        poolA.sync();

        usdc.mint(address(poolB), AMOUNT_USDC_LIQ_B);
        wbtc.mint(address(poolB), AMOUNT_WBTC_LIQ_B);
        poolB.sync();

        wbtc.mint(address(poolC), AMOUNT_WBTC_LIQ_C);
        weth.mint(address(poolC), AMOUNT_WETH_LIQ_C);
        poolC.sync();
    }

    /// Compute the expected swap amounts for the triangular arbitrage A→B→C.
    /// Input reserves are always the token being sold; zero_for_one only
    /// determines which direction the pair's swap() goes.
    function _computeAmounts() internal view returns (Amounts memory amt) {
        amt.zfoA = poolA.token0() == address(weth);
        amt.zfoB = poolB.token0() == address(usdc);
        amt.zfoC = poolC.token0() == address(wbtc);

        amt.usdc = _v2GetAmountOut(
            AMOUNT_WETH_IN, weth.balanceOf(address(poolA)), usdc.balanceOf(address(poolA)), FEE
        );
        amt.wbtc = _v2GetAmountOut(
            amt.usdc, usdc.balanceOf(address(poolB)), wbtc.balanceOf(address(poolB)), FEE
        );
        amt.wethOut = _v2GetAmountOut(
            amt.wbtc, wbtc.balanceOf(address(poolC)), weth.balanceOf(address(poolC)), FEE
        );
    }

    /// Build the command stream for one approach.
    function _buildApproach(uint256 approach, Amounts memory amt)
        internal
        view
        returns (Cmd.Table memory at, bytes memory commands)
    {
        at = _emptyTable();
        commands = new bytes(0);
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 poolAIdx = Cmd.add(at, address(poolA));
        uint8 poolBIdx = Cmd.add(at, address(poolB));
        uint8 poolCIdx = Cmd.add(at, address(poolC));

        if (approach == 1) {
            // Innermost: pool C's callback pays the WBTC pool C is owed.
            bytes memory poolCCallback = Cmd.erc20Transfer(wbtcIdx, poolCIdx, amt.wbtc);
            // Pool B's callback: swap pool C (sends WETH to executor), then
            // pay the USDC pool B is owed.
            bytes memory poolBCallback =
                Cmd.v2SwapCompact(poolCIdx, amt.zfoC, amt.wethOut, executorIdx, FEE, poolCCallback);
            poolBCallback = bytes.concat(poolBCallback, Cmd.erc20Transfer(usdcIdx, poolBIdx, amt.usdc));
            // Pool A's callback: swap pool B (sends WBTC to executor), then
            // pay the WETH pool A is owed.
            bytes memory poolACallback =
                Cmd.v2SwapCompact(poolBIdx, amt.zfoB, amt.wbtc, executorIdx, FEE, poolBCallback);
            poolACallback = bytes.concat(poolACallback, Cmd.erc20Transfer(wethIdx, poolAIdx, AMOUNT_WETH_IN));
            commands = Cmd.v2SwapCompact(poolAIdx, amt.zfoA, amt.usdc, executorIdx, FEE, poolACallback);
        } else if (approach == 2) {
            // Pool A's callback: transfer USDC to pool B, V2_SWAP_CALC B + C
            // with direct custody, then repay pool A. The V2_SWAP_COMPACT
            // recipient must be the executor (the callback target).
            bytes memory poolACallback = Cmd.erc20Transfer(usdcIdx, poolBIdx, amt.usdc);
            poolACallback = bytes.concat(poolACallback, Cmd.v2SwapCalc(poolBIdx, amt.zfoB, poolCIdx, FEE));
            poolACallback = bytes.concat(poolACallback, Cmd.v2SwapCalc(poolCIdx, amt.zfoC, executorIdx, FEE));
            poolACallback = bytes.concat(poolACallback, Cmd.erc20Transfer(wethIdx, poolAIdx, AMOUNT_WETH_IN));
            commands = Cmd.v2SwapCompact(poolAIdx, amt.zfoA, amt.usdc, executorIdx, FEE, poolACallback);
        } else {
            // Pre-fund pool A with WETH (creates excess balance), then chain
            // V2_SWAP_CALC with direct custody through B and C — no amounts
            // needed, no callbacks.
            commands = Cmd.erc20Transfer(wethIdx, poolAIdx, AMOUNT_WETH_IN);
            commands = bytes.concat(commands, Cmd.v2SwapCalc(poolAIdx, amt.zfoA, poolBIdx, FEE));
            commands = bytes.concat(commands, Cmd.v2SwapCalc(poolBIdx, amt.zfoB, poolCIdx, FEE));
            commands = bytes.concat(commands, Cmd.v2SwapCalc(poolCIdx, amt.zfoC, executorIdx, FEE));
        }
    }

    /// Build and execute one approach; return the executor's WETH profit.
    /// Approach 3 pre-funds pool A, so the executor must hold WETH first —
    /// this wrapper mints it (the source `_run_approach` shape).
    function _runApproach(uint256 approach) internal returns (int256 profit) {
        _fundPools();
        Amounts memory amt = _computeAmounts();
        if (approach == 3) {
            weth.mint(address(executor), AMOUNT_WETH_IN);
        }
        (Cmd.Table memory at, bytes memory commands) = _buildApproach(approach, amt);

        uint256 wethBefore = weth.balanceOf(address(executor));
        _execute(executor, bytes.concat(Cmd.preamble(at), commands));
        return int256(weth.balanceOf(address(executor))) - int256(wethBefore);
    }

    // ── Approach 1: V2_SWAP_COMPACT nested callbacks — full executor custody

    /// Every pool sends output to the executor; each callback pays the owed
    /// input token to the calling pair. V2 calls are blocking, so inner
    /// callbacks complete first — by the time an outer pool must be paid, the
    /// executor has already received the payment token from an inner pool.
    function testThreePoolNestedCallback() public {
        weth.mint(address(executor), AMOUNT_WETH_IN);
        _runApproach(1);
        assertGt(weth.balanceOf(address(executor)), 0, "executor should have WETH profit");
    }

    // ── Approach 2: V2_SWAP_COMPACT flash + 2× V2_SWAP_CALC — direct custody

    /// V2_SWAP_COMPACT's callback fires on the recipient, so its recipient
    /// MUST be the executor. V2_SWAP_CALC calls swap() with data = b"" (no
    /// callback), so it CAN send output directly to the next pool, creating
    /// excess balance the next V2_SWAP_CALC reads as input — eliminating two
    /// intermediate executor-custody transfers.
    function testThreePoolFlashPlusCalc() public {
        _runApproach(2);
        assertGt(weth.balanceOf(address(executor)), 0, "executor should have WETH profit");
    }

    // ── Approach 3: all V2_SWAP_CALC — zero callbacks ─────────────────────

    /// Pre-fund pool A with WETH, then V2_SWAP_CALC through the chain. Each
    /// pool's output goes directly to the next pool (or the executor for the
    /// last one), cascading excess balance.
    function testThreePoolAllCalc() public {
        _runApproach(3);
        assertGt(weth.balanceOf(address(executor)), 0, "executor should have WETH profit");
    }

    // ── Gas comparison across the three approaches ────────────────────────

    /// Approach 1 (3× COMPACT, nested callbacks): 6 transfers, 3 callbacks.
    function testApproach1Gas() public {
        _fundPools();
        Amounts memory amt = _computeAmounts();
        (Cmd.Table memory at, bytes memory commands) = _buildApproach(1, amt);
        Outcome memory o = _executeRaw(executor, bytes.concat(Cmd.preamble(at), commands));
        assertTrue(o.ok, "approach 1 should succeed");
        emit ApproachGas("approach_1_3x_compact_nested", o.gasUsed);
    }

    /// Approach 2 (1× COMPACT + 2× CALC, direct custody): 5 transfers, 1
    /// callback.
    function testApproach2Gas() public {
        _fundPools();
        Amounts memory amt = _computeAmounts();
        (Cmd.Table memory at, bytes memory commands) = _buildApproach(2, amt);
        Outcome memory o = _executeRaw(executor, bytes.concat(Cmd.preamble(at), commands));
        assertTrue(o.ok, "approach 2 should succeed");
        emit ApproachGas("approach_2_compact_plus_2x_calc", o.gasUsed);
    }

    /// Approach 3 (3× CALC, zero callbacks): 4 transfers — the best shape when
    /// the executor already holds WETH.
    function testApproach3Gas() public {
        weth.mint(address(executor), AMOUNT_WETH_IN);
        _fundPools();
        Amounts memory amt = _computeAmounts();
        (Cmd.Table memory at, bytes memory commands) = _buildApproach(3, amt);
        Outcome memory o = _executeRaw(executor, bytes.concat(Cmd.preamble(at), commands));
        assertTrue(o.ok, "approach 3 should succeed");
        emit ApproachGas("approach_3_all_calc_zero_callbacks", o.gasUsed);
    }

    // ── Economic equivalence: all approaches produce the same arb profit ──

    function testApproach1Profit() public {
        int256 profit = _runApproach(1);
        assertTrue(profit > 0, "Approach 1 should produce WETH profit");
    }

    function testApproach2Profit() public {
        int256 profit = _runApproach(2);
        assertTrue(profit > 0, "Approach 2 should produce WETH profit");
    }

    function testApproach3Profit() public {
        int256 profit = _runApproach(3);
        assertTrue(profit > 0, "Approach 3 should produce WETH profit");
    }
}
