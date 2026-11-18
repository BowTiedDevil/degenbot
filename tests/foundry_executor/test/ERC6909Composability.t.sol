// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {IFakeErc20} from "../src/generated/IFakeErc20.sol";
import {IFakeUniswapV2Pair} from "../src/generated/IFakeUniswapV2Pair.sol";
import {IFakeUniswapV3Pool} from "../src/generated/IFakeUniswapV3Pool.sol";
import {PoolKey} from "../src/generated/Types.sol";

/// Port of `tests/executor/test_erc6909_composability.py`, plus the direct
/// residual-debit case the source module only describes (its
/// `test_burn_settles_weth_debit` demonstrates the compound/withdraw pattern
/// instead of an actual burn-funded debit).
///
/// V4_MINT and V4_BURN reduce ERC20 transfers by keeping tokens inside the
/// PoolManager as accounting entries:
///   V4_MINT replaces V4_TAKE when profit goes to the executor (0 xfers vs 1)
///   V4_BURN replaces sync+transfer+settle when the executor holds ERC6909
///
/// WARMUP_WEI: `executor.initialize{value: 1}` mints exactly 1 wei of ERC6909
/// WETH to warm the PM's balance slot, so every exact ERC6909 WETH expectation
/// carries the +1.
contract ERC6909ComposabilityTest is PoolSetup {
    uint256 internal constant AMOUNT_WETH_PROFIT = 2 ether;
    uint256 internal constant NET_PROFIT = AMOUNT_WETH_PROFIT - AMOUNT_WETH;
    /// 1 wei ERC6909 WETH minted by initialize() to warm the slot.
    uint256 internal constant WARMUP_WEI = 1;

    event MintProfitGas(string label, uint256 gasUsed);

    /// The three-hop V4 arb pools: WETH/USDC (3000, 60), USDC/WBTC (500, 10),
    /// WBTC/WETH (10000, 200) — the source's pool_key_a/b/c.
    struct ArbPools {
        PoolKey a;
        PoolKey b;
        PoolKey c;
        bool za;
        bool zb;
        bool zc;
    }

    /// The source's bare AddressTable: weth, usdc, wbtc, executor, zero
    /// (native sentinel), optional bribe recipient, then the PM.
    struct ArbTable {
        Cmd.Table at;
        uint8 wethIdx;
        uint8 usdcIdx;
        uint8 wbtcIdx;
        uint8 executorIdx;
        uint8 zeroIdx;
        uint8 recipientIdx;
        uint8 pmIdx;
    }

    function _wethId() internal view returns (uint256) {
        return uint256(uint160(address(weth)));
    }

    function _setupArbPools() internal returns (ArbPools memory p) {
        p.a = _poolKey(address(weth), address(usdc), 3000, 60);
        p.za = p.a.currency0 == address(weth);
        _setupV4Swap(v4Pm, p.a, AMOUNT_WETH, AMOUNT_USDC, p.za, address(usdc));
        p.b = _poolKey(address(usdc), address(wbtc), 500, 10);
        p.zb = p.b.currency0 == address(usdc);
        _setupV4Swap(v4Pm, p.b, AMOUNT_USDC, AMOUNT_WBTC, p.zb, address(wbtc));
        p.c = _poolKey(address(wbtc), address(weth), 10000, 200);
        p.zc = p.c.currency0 == address(wbtc);
        _setupV4Swap(v4Pm, p.c, AMOUNT_WBTC, AMOUNT_WETH_PROFIT, p.zc, address(weth));
    }

    function _arbTable(address recipient) internal view returns (ArbTable memory t) {
        t.at = _emptyTable();
        t.wethIdx = Cmd.add(t.at, address(weth));
        t.usdcIdx = Cmd.add(t.at, address(usdc));
        t.wbtcIdx = Cmd.add(t.at, address(wbtc));
        t.executorIdx = Cmd.add(t.at, address(executor));
        t.zeroIdx = Cmd.add(t.at, address(0));
        if (recipient != address(0)) {
            t.recipientIdx = Cmd.add(t.at, recipient);
        }
        t.pmIdx = Cmd.add(t.at, address(v4Pm));
    }

    function _v4Swap(ArbTable memory t, PoolKey memory key, bool zfo, uint256 amount)
        internal
        pure
        returns (bytes memory)
    {
        return Cmd.v4SwapCompact(
            Cmd.add(t.at, key.currency0),
            Cmd.add(t.at, key.currency1),
            uint16(key.fee),
            int16(key.tick_spacing),
            t.zeroIdx,
            zfo,
            amount
        );
    }

    /// The three V4 swaps plus V4_MINT of the net profit — TX1 of the
    /// multi-transaction flows, and the whole body of the single-tx
    /// mint-profit cases. Profit is stored as ERC6909: zero ERC20 transfers.
    function _mintProfitBody(ArbTable memory t, ArbPools memory p)
        internal
        pure
        returns (bytes memory inner)
    {
        inner = _v4Swap(t, p.a, p.za, AMOUNT_WETH);
        inner = bytes.concat(inner, _v4Swap(t, p.b, p.zb, AMOUNT_USDC));
        inner = bytes.concat(inner, _v4Swap(t, p.c, p.zc, AMOUNT_WBTC));
        inner = bytes.concat(
            inner, Cmd.v4MintCompact(t.wethIdx, t.executorIdx, NET_PROFIT)
        );
    }

    /// Execute the mint-profit unlock at the source's default config (= 1).
    function _runMintProfitTx(ArbTable memory t, ArbPools memory p) internal {
        _execute(
            executor,
            bytes.concat(Cmd.preamble(t.at), Cmd.v4Unlock(_mintProfitBody(t, p)))
        );
    }

    // ═══════════════════════════════════════════════════════════════════════
    //  A. Single-tx V4_MINT profit: 0-transfer V4-V4-V4
    // ═══════════════════════════════════════════════════════════════════════

    /// V4-V4-V4 with V4_MINT instead of V4_TAKE: 1→0 ERC20 transfers. The
    /// profit is deferred — ERC6909 WETH inside the PM, withdrawable later
    /// via V4_BURN + V4_TAKE. Config = 2 (profit mode 2, ERC6909 claims),
    /// with the source's bribe_bips parametrization {0, 5000}. The bribe
    /// cannot exceed the executor's physical WETH+ETH (1 wei is consumed by
    /// the warmup), so the payout is 0 in both runs — asserted exactly.
    function _mintProfitZeroTransfer(uint16 bribeBips) internal {
        ArbPools memory p = _setupArbPools();
        // The source's accounts[1] — a plain second EOA.
        address recipient = address(uint160(0xA11CE));
        ArbTable memory t = _arbTable(recipient);
        uint256 config = Cmd.makeConfig(2, bribeBips, t.recipientIdx, 0);
        bytes memory stream =
            bytes.concat(Cmd.preamble(t.at), Cmd.v4Unlock(_mintProfitBody(t, p)));

        // Read-only receipt: (gross_profit, bribe_paid) exactly.
        Outcome memory simulated = _executeSimulated(executor, stream, config);
        assertTrue(simulated.ok, "simulated receipt should succeed");
        assertEq(simulated.grossProfit, NET_PROFIT, "gross_profit (mode 2)");
        // expected_bribe = min(bips * profit / 10000, physical WETH+ETH) = 0.
        uint256 physicalBefore = weth.balanceOf(address(executor)) + address(executor).balance;
        uint256 expectedBribe = (NET_PROFIT * bribeBips) / 10_000;
        if (expectedBribe > physicalBefore) {
            expectedBribe = physicalBefore;
        }
        assertEq(simulated.bribePaid, expectedBribe, "bribe_paid");

        // Real execution: profit stored as ERC6909, no physical WETH out.
        uint256 recipientBefore = recipient.balance;
        _execute(executor, stream, config);
        assertEq(
            v4Pm.balanceOf(address(executor), _wethId()),
            NET_PROFIT + WARMUP_WEI,
            "ERC6909 WETH minted"
        );
        assertEq(recipient.balance - recipientBefore, expectedBribe, "bribe delivered");
    }

    function testMintProfitZeroTransferNoBribe() public {
        _mintProfitZeroTransfer(0);
    }

    function testMintProfitZeroTransferHalfBribe() public {
        _mintProfitZeroTransfer(5000);
    }

    // ═══════════════════════════════════════════════════════════════════════
    //  B. Multi-tx composability: MINT in tx1, compound in tx2, withdraw in tx3
    // ═══════════════════════════════════════════════════════════════════════

    /// TX1 mints the profit, TX2 compounds it (another mint — over N V4
    /// operations an executor using MINT only needs 0 transfers per op), TX3
    /// withdraws everything as physical WETH via V4_BURN + V4_TAKE.
    function testMintThenBurnAcrossTransactions() public {
        ArbPools memory p = _setupArbPools();
        ArbTable memory t = _arbTable(address(0));

        // TX1: mint the profit.
        _runMintProfitTx(t, p);
        assertEq(
            v4Pm.balanceOf(address(executor), _wethId()),
            NET_PROFIT + WARMUP_WEI,
            "TX1 minted"
        );

        // TX2: same arb again, mint again — the balance compounds.
        _setupArbPools();
        _runMintProfitTx(t, p);
        assertEq(
            v4Pm.balanceOf(address(executor), _wethId()),
            2 * NET_PROFIT + WARMUP_WEI,
            "TX2 compounded"
        );

        // TX3: burn everything (including the warmup wei) and take it out.
        uint256 total = 2 * NET_PROFIT + WARMUP_WEI;
        bytes memory inner3 = Cmd.v4BurnCompact(t.wethIdx, total);
        inner3 = bytes.concat(inner3, Cmd.v4Take(t.wethIdx, t.executorIdx, total));
        _execute(executor, bytes.concat(Cmd.preamble(t.at), Cmd.v4Unlock(inner3)));

        assertEq(v4Pm.balanceOf(address(executor), _wethId()), 0, "TX3 drained");
        assertEq(weth.balanceOf(address(executor)), total, "physical WETH exact");
    }

    /// The source's second compound/withdraw demonstration: identical shape
    /// (TX1 mint, TX2 compound, TX3 burn+take), ported as its own case.
    function testBurnSettlesWethDebit() public {
        ArbPools memory p = _setupArbPools();
        ArbTable memory t = _arbTable(address(0));

        _runMintProfitTx(t, p);
        assertEq(
            v4Pm.balanceOf(address(executor), _wethId()),
            NET_PROFIT + WARMUP_WEI,
            "TX1 minted"
        );

        _setupArbPools();
        _runMintProfitTx(t, p);
        assertEq(
            v4Pm.balanceOf(address(executor), _wethId()),
            2 * NET_PROFIT + WARMUP_WEI,
            "TX2 compounded"
        );

        uint256 total = 2 * NET_PROFIT + WARMUP_WEI;
        bytes memory inner3 = Cmd.v4BurnCompact(t.wethIdx, total);
        inner3 = bytes.concat(inner3, Cmd.v4Take(t.wethIdx, t.executorIdx, total));
        _execute(executor, bytes.concat(Cmd.preamble(t.at), Cmd.v4Unlock(inner3)));

        assertEq(v4Pm.balanceOf(address(executor), _wethId()), 0, "TX3 drained");
        assertEq(weth.balanceOf(address(executor)), total, "physical WETH exact");
    }

    // ═══════════════════════════════════════════════════════════════════════
    //  B'. The missing direct case: V4_BURN funds a residual WETH debit
    // ═══════════════════════════════════════════════════════════════════════

    /// TX2 of the burn-debit case: V4→V2→V2 where the executor borrows 1 WETH
    /// from the PM (V4_TAKE into pool A) and the chain ends in V2, so the
    /// debit is NOT covered by V4 delta netting — the V2c output goes to the
    /// executor, leaving a -1 WETH PM delta that only the ERC6909 balance can
    /// pay. `withBurn` selects the paying stream vs the unpaid negative
    /// control; each call deploys and funds its own fresh V2 chain.
    function _burnDebitTx2(bool withBurn)
        internal
        returns (bytes memory stream, uint256 chainOut)
    {
        // A fresh table per call: the executor's stream bound is 288 bytes
        // (13 SET_ADDRESS entries), so a shared table that accumulates each
        // attempt's pools would overflow it.
        ArbTable memory t = _arbTable(address(0));
        (address a0, address a1) = _sorted(address(weth), address(usdc));
        IFakeUniswapV2Pair v2a = _deployV2Pair(a0, a1, 0, V2_FEE);
        (address b0, address b1) = _sorted(address(usdc), address(wbtc));
        IFakeUniswapV2Pair v2b = _deployV2Pair(b0, b1, 0, V2_FEE);
        (address c0, address c1) = _sorted(address(wbtc), address(weth));
        IFakeUniswapV2Pair v2c = _deployV2Pair(c0, c1, 0, V2_FEE);

        // Canned arb prices: 1 WETH → 2000 USDC → 100 WBTC → ~2 WETH.
        usdc.mint(address(v2a), AMOUNT_USDC * 100);
        weth.mint(address(v2a), AMOUNT_WETH * 100);
        v2a.sync();
        usdc.mint(address(v2b), AMOUNT_USDC * 100);
        wbtc.mint(address(v2b), AMOUNT_WBTC * 100);
        v2b.sync();
        wbtc.mint(address(v2c), AMOUNT_WBTC * 100);
        weth.mint(address(v2c), AMOUNT_WETH_PROFIT * 100);
        v2c.sync();

        bool az = v2a.token0() == address(weth);
        bool bz = v2b.token0() == address(usdc);
        bool cz = v2c.token0() == address(wbtc);
        uint256 aOut = _v2GetAmountOut(
            AMOUNT_WETH, weth.balanceOf(address(v2a)), usdc.balanceOf(address(v2a)), V2_FEE
        );
        uint256 bOut = _v2GetAmountOut(
            aOut, usdc.balanceOf(address(v2b)), wbtc.balanceOf(address(v2b)), V2_FEE
        );
        chainOut = _v2GetAmountOut(
            bOut, wbtc.balanceOf(address(v2c)), weth.balanceOf(address(v2c)), V2_FEE
        );

        uint8 v2aIdx = Cmd.add(t.at, address(v2a));
        uint8 v2bIdx = Cmd.add(t.at, address(v2b));
        uint8 v2cIdx = Cmd.add(t.at, address(v2c));

        // The borrow: -1 WETH PM delta the V4 deltas never offset.
        bytes memory inner = Cmd.v4Take(t.wethIdx, v2aIdx, AMOUNT_WETH);
        inner = bytes.concat(inner, Cmd.v2SwapCalc(v2aIdx, az, v2bIdx, V2_FEE));
        inner = bytes.concat(inner, Cmd.v2SwapCalc(v2bIdx, bz, v2cIdx, V2_FEE));
        // The chain ends in V2: the WETH output goes to the executor, not the
        // PM's delta accounting.
        inner = bytes.concat(inner, Cmd.v2SwapCalc(v2cIdx, cz, t.executorIdx, V2_FEE));
        if (withBurn) {
            // Pay the residual debit out of the ERC6909 balance: 0 ERC20
            // transfers, no sync, no settle.
            inner = bytes.concat(inner, Cmd.v4BurnCompact(t.wethIdx, AMOUNT_WETH));
        }
        stream = bytes.concat(Cmd.preamble(t.at), Cmd.v4Unlock(inner));
    }

    /// The direct V4_BURN-funded debit: TX1 mints the profit as ERC6909, TX2
    /// runs the V4→V2→V2 borrow above. The negative control runs the same
    /// stream WITHOUT the burn and pins the PM's exact
    /// `CurrencyNotSettled:<weth>:-1e18` — proving the debit is real and that
    /// the burn, not the withdrawal shape, is what pays it. The paying run
    /// then settles the debit with zero ERC20 transfers and exact balances:
    /// ERC6909 down by exactly 1 WETH, executor physical WETH up by the full
    /// V2c output (economic profit = chainOut - 1 WETH).
    function testBurnFundsResidualWethDebit() public {
        // TX1: V4-V4-V4, mint the profit.
        ArbPools memory p = _setupArbPools();
        ArbTable memory t = _arbTable(address(0));
        _runMintProfitTx(t, p);
        assertEq(
            v4Pm.balanceOf(address(executor), _wethId()),
            NET_PROFIT + WARMUP_WEI,
            "TX1 minted"
        );
        // The PM funds the borrow: physical WETH from the setup's output side.
        assertGe(weth.balanceOf(address(v4Pm)), AMOUNT_WETH, "PM holds the bank inventory");

        // Negative control: the identical borrow stream without the burn.
        (bytes memory unpaid, ) = _burnDebitTx2(false);
        Outcome memory negative = _executeRaw(executor, unpaid);
        _assertRevertData(
            negative,
            abi.encodeWithSignature(
                "Error(string)",
                string.concat(
                    "CurrencyNotSettled:",
                    vm.toString(address(weth)),
                    ":-",
                    vm.toString(AMOUNT_WETH)
                )
            ),
            "unpaid debit"
        );

        // Paying run: same shape + V4_BURN(1 WETH).
        (bytes memory paid, uint256 chainOut) = _burnDebitTx2(true);
        uint256 wethBefore = weth.balanceOf(address(executor));
        Outcome memory positive = _executeRaw(executor, paid);
        assertTrue(positive.ok, "burn should settle the debit");

        // The debit was paid from the ERC6909 balance — exactly 1 WETH.
        assertEq(
            v4Pm.balanceOf(address(executor), _wethId()),
            NET_PROFIT + WARMUP_WEI - AMOUNT_WETH,
            "ERC6909 down by exactly the debit"
        );
        // The full V2c output sits at the executor (no PM deposit, no settle).
        assertEq(
            weth.balanceOf(address(executor)) - wethBefore, chainOut, "full chain out"
        );
    }

    // ═══════════════════════════════════════════════════════════════════════
    //  C. V4_MINT on V2-V4-V4 and V3-V4-V4
    // ═══════════════════════════════════════════════════════════════════════

    /// V2-V4-V4 with V4_MINT profit: 3 ERC20 transfers → 2. The V4_TAKE
    /// profit extraction becomes V4_MINT; the USDC intermediate settles via
    /// V4_SETTLE_DELTA from the executor's physical V2 output.
    function testV2V4V4MintProfit() public {
        weth.mint(address(executor), AMOUNT_WETH);
        (address a0, address a1) = _sorted(address(weth), address(usdc));
        IFakeUniswapV2Pair v2a = _deployV2Pair(a0, a1, 0, V2_FEE);
        _setupV2Pair(v2a, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, V2_FEE);
        bool v2aZfo = v2a.token0() == address(weth);
        uint256 aOut = AMOUNT_USDC; // canned, per the source

        ArbPools memory p = _setupArbPools();
        ArbTable memory t = _arbTable(address(0));

        bytes memory v4Inner = _v4Swap(t, p.b, p.zb, aOut);
        v4Inner = bytes.concat(v4Inner, _v4Swap(t, p.c, p.zc, AMOUNT_WBTC));
        v4Inner = bytes.concat(
            v4Inner, Cmd.v4MintCompact(t.wethIdx, t.executorIdx, AMOUNT_WETH_PROFIT)
        );
        v4Inner = bytes.concat(v4Inner, Cmd.v4SettleDelta(t.usdcIdx));

        bytes memory aFwd = Cmd.v4Unlock(v4Inner);
        aFwd = bytes.concat(
            aFwd, Cmd.erc20Transfer(t.wethIdx, Cmd.add(t.at, address(v2a)), AMOUNT_WETH)
        );

        bytes memory commands = Cmd.v2SwapCompact(
            Cmd.add(t.at, address(v2a)), v2aZfo, aOut, t.executorIdx, V2_FEE, aFwd
        );
        _execute(executor, bytes.concat(Cmd.preamble(t.at), commands), 2);

        assertEq(
            v4Pm.balanceOf(address(executor), _wethId()),
            AMOUNT_WETH_PROFIT + WARMUP_WEI,
            "profit as ERC6909"
        );
        // The executor's own WETH went to pool A; no WETH came back out.
        assertEq(weth.balanceOf(address(executor)), 0, "input WETH paid to V2a");
    }

    /// V3-V4-V4 with V4_MINT profit: 3 ERC20 transfers → 2. The V3a input is
    /// paid from the PM's WETH delta (V4_TAKE WETH→V3a, IIA), and the net
    /// profit is minted instead of taken.
    function testV3V4V4MintProfit() public {
        (address a0, address a1) = _sorted(address(weth), address(usdc));
        IFakeUniswapV3Pool v3a = _deployV3Pool(a0, a1, 0, 3000);
        (bool aZfo, uint256 aUsdcOut) =
            _setupV3(v3a, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);

        ArbPools memory p = _setupArbPools();
        // Pool B's programmed input is the V3a ACTUAL output, not the canned
        // amount (the source passes `a_usdc_out` from `_setup_v3`).
        _setupV4Swap(v4Pm, p.b, aUsdcOut, AMOUNT_WBTC, p.zb, address(wbtc));
        ArbTable memory t = _arbTable(address(0));
        uint8 v3aIdx = Cmd.add(t.at, address(v3a));

        bytes memory v4Inner = Cmd.v4Settle();
        v4Inner = bytes.concat(v4Inner, _v4Swap(t, p.b, p.zb, aUsdcOut));
        v4Inner = bytes.concat(v4Inner, _v4Swap(t, p.c, p.zc, AMOUNT_WBTC));
        // routing → V3a (intermediate-asset payment inside the callback)
        v4Inner = bytes.concat(
            v4Inner, Cmd.v4Take(t.wethIdx, v3aIdx, AMOUNT_WETH)
        );
        v4Inner = bytes.concat(
            v4Inner, Cmd.v4MintCompact(t.wethIdx, t.executorIdx, NET_PROFIT)
        );

        bytes memory aFwd = Cmd.v4Unlock(v4Inner);
        bytes memory commands = Cmd.v4Sync(t.usdcIdx);
        commands = bytes.concat(
            commands,
            Cmd.v3SwapCompact(v3aIdx, aZfo, AMOUNT_WETH, t.pmIdx, aFwd)
        );
        _execute(executor, bytes.concat(Cmd.preamble(t.at), commands), 2);

        assertEq(
            v4Pm.balanceOf(address(executor), _wethId()),
            NET_PROFIT + WARMUP_WEI,
            "profit as ERC6909"
        );
    }

    // ═══════════════════════════════════════════════════════════════════════
    //  D. ETH-funded WETH settlement
    // ═══════════════════════════════════════════════════════════════════════

    /// V4-V4-V4 with WETH_DEPOSIT funding + V4_MINT profit. WETH_DEPOSIT
    /// wraps the executor's native ETH at WETH9, then sync+transfer+settle
    /// credits the deposit. The executor invests 1 ETH; the arb yields 2 WETH
    /// of ERC6909 (1 principal + 1 profit).
    function testV4V4V4WithWethDeposit() public {
        vm.deal(address(executor), address(executor).balance + AMOUNT_WETH);
        ArbPools memory p = _setupArbPools();
        ArbTable memory t = _arbTable(address(0));

        bytes memory inner = Cmd.wethDeposit(AMOUNT_WETH);
        inner = bytes.concat(inner, Cmd.v4Sync(t.wethIdx));
        inner = bytes.concat(inner, Cmd.erc20Transfer(t.wethIdx, t.pmIdx, AMOUNT_WETH));
        inner = bytes.concat(inner, Cmd.v4Settle());
        inner = bytes.concat(inner, _v4Swap(t, p.a, p.za, AMOUNT_WETH));
        inner = bytes.concat(inner, _v4Swap(t, p.b, p.zb, AMOUNT_USDC));
        inner = bytes.concat(inner, _v4Swap(t, p.c, p.zc, AMOUNT_WBTC));
        inner = bytes.concat(
            inner, Cmd.v4MintCompact(t.wethIdx, t.executorIdx, AMOUNT_WETH_PROFIT)
        );

        _execute(executor, bytes.concat(Cmd.preamble(t.at), Cmd.v4Unlock(inner)), 2);

        assertEq(
            v4Pm.balanceOf(address(executor), _wethId()),
            AMOUNT_WETH_PROFIT + WARMUP_WEI,
            "principal + profit as ERC6909"
        );
        // The deposited ETH is fully consumed by the wrap.
        assertEq(address(executor).balance, 0, "ETH invested");
    }
}
