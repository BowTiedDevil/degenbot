// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {ExecutorErrors} from "../src/generated/Errors.sol";
import {PoolKey} from "../src/generated/Types.sol";
import {IFakeUniswapV2Pair} from "../src/generated/IFakeUniswapV2Pair.sol";
import {IFakeErc20} from "../src/generated/IFakeErc20.sol";

/// Port of `tests/executor/test_execution_receipt.py`.
contract ExecutionReceiptTest is PoolSetup {
    uint256 internal constant AMOUNT_IN = 1000e6;
    uint256 internal constant PREEXISTING_WETH = 7 ether;
    address internal constant BRIBE_RECIPIENT = address(0xB1);
    address internal constant OTHER = address(0xB2);

    function _pairedV2() internal returns (IFakeUniswapV2Pair pair, uint256 expectedGross) {
        pair = _deployV2Pair(address(weth), address(usdc), 0, 30);
        usdc.mint(address(pair), AMOUNT_IN * 100);
        weth.mint(address(pair), 100 ether);
        pair.sync();
        expectedGross = _v2GetAmountOut(
            AMOUNT_IN, usdc.balanceOf(address(pair)), weth.balanceOf(address(pair)), 30
        );
    }

    function _zeroReceipt(uint256 profitMode) internal {
        bytes memory stream = bytes.concat(Cmd.preamble(_emptyTable()), Cmd.wethDepositAll());
        Outcome memory o = _executeSimulated(executor, stream, profitMode);
        assertTrue(o.ok, "simulated execute must succeed");
        assertEq(o.grossProfit, 0, "measured zero is a receipt field");
        assertEq(o.bribePaid, 0, "measured zero bribe");
    }

    function testExecuteReturnsZeroReceiptMode1() public {
        _zeroReceipt(1);
    }

    function testExecuteReturnsZeroReceiptMode2() public {
        _zeroReceipt(2);
    }

    function _bribeAccounting(uint16 bribeBips) internal {
        (IFakeUniswapV2Pair pair, uint256 expectedGross) = _pairedV2();
        usdc.mint(address(executor), AMOUNT_IN);
        weth.mint(address(executor), PREEXISTING_WETH);
        uint256 before = weth.balanceOf(address(executor)) + address(executor).balance;
        uint256 recipientBefore = BRIBE_RECIPIENT.balance;

        Cmd.Table memory at = _emptyTable();
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 pairIdx = Cmd.add(at, address(pair));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 bribeIdx = Cmd.add(at, BRIBE_RECIPIENT);

        bool zfo = pair.token0() == address(usdc);
        bytes memory commands = Cmd.erc20Transfer(usdcIdx, pairIdx, AMOUNT_IN);
        commands = bytes.concat(commands, Cmd.v2SwapDirect(pairIdx, zfo, expectedGross, executorIdx));
        bytes memory stream = bytes.concat(Cmd.preamble(at), commands);

        uint256 expectedBribe = expectedGross * bribeBips / 10_000;
        uint256 baseConfig =
            (uint256(bribeBips) << 8) | (uint256(bribeIdx) << 24) | 1;

        Outcome memory simulated = _executeSimulated(executor, stream, baseConfig);
        assertTrue(simulated.ok, "receipt simulation must succeed");
        assertEq(simulated.grossProfit, expectedGross, "gross profit");
        assertEq(simulated.bribePaid, expectedBribe, "bribe accounting");

        // A floor one wei above the measured gross must be refused with the
        // shortfall error naming (measured gross, demanded floor).
        Outcome memory tooHigh =
            _executeSimulated(executor, stream, ((expectedGross + 1) << 32) | baseConfig);
        _assertRevertData(
            tooHigh,
            _executorRevert(
                ExecutorErrors.INSUFFICIENT_PROFIT, expectedGross, expectedGross + 1
            ),
            "min_profit above the measured gross"
        );

        // The exact floor executes.
        _execute(executor, stream, (expectedGross << 32) | baseConfig);
        uint256 afterBal = weth.balanceOf(address(executor)) + address(executor).balance;
        assertEq(afterBal - before, expectedGross - expectedBribe, "retained profit");
        assertEq(BRIBE_RECIPIENT.balance - recipientBefore, expectedBribe, "bribe transferred");
    }

    function testPhysicalReceiptAndBribeAccountingNoBribe() public {
        _bribeAccounting(0);
    }

    function testPhysicalReceiptAndBribeAccountingHalfBribe() public {
        _bribeAccounting(5000);
    }

    function _invalidMode(uint256 mode) internal {
        bytes memory stream = bytes.concat(Cmd.preamble(_emptyTable()), Cmd.wethDepositAll());
        Outcome memory o = _executeSimulated(executor, stream, mode);
        // The mode check sits at the top of execute(), so the revert names
        // the rejected mode exactly.
        _assertRevertData(
            o, _executorRevert(ExecutorErrors.INVALID_PROFIT_MODE, mode), "invalid profit mode"
        );
    }

    function testInvalidProfitMode0() public {
        _invalidMode(0);
    }

    function testInvalidProfitMode3() public {
        _invalidMode(3);
    }

    function testInvalidProfitMode4() public {
        _invalidMode(4);
    }

    function testInvalidProfitMode255() public {
        _invalidMode(255);
    }

    function testDefaultConfigRejectsPhysicalBalanceDecrease() public {
        uint256 amount = 1 ether;
        weth.mint(address(executor), amount);
        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 recipientIdx = Cmd.add(at, OTHER);
        bytes memory stream = bytes.concat(
            Cmd.preamble(at), Cmd.erc20Transfer(wethIdx, recipientIdx, amount)
        );
        uint256 beforeBal = weth.balanceOf(address(executor)) + address(executor).balance;
        Outcome memory o = _executeSimulated(executor, stream, 1);
        // The decrease check raises InsufficientProfit(actual, expected) with
        // the measured combined balances.
        _assertRevertData(
            o,
            _executorRevert(ExecutorErrors.INSUFFICIENT_PROFIT, beforeBal - amount, beforeBal),
            "physical decrease"
        );
    }

    function testSweepIsSeparateFromExecution() public {
        uint256 wethAmount = 3 ether;
        uint256 nativeAmount = 2 ether;
        weth.mint(address(executor), wethAmount);
        _execute(
            executor,
            bytes.concat(Cmd.preamble(_emptyTable()), Cmd.wethWithdraw(nativeAmount)),
            1
        );
        uint256 destinationBefore = OTHER.balance;
        uint256 combinedBefore = weth.balanceOf(address(executor)) + address(executor).balance;

        executor.sweep(OTHER);

        assertEq(weth.balanceOf(address(executor)), 0, "sweep drained WETH");
        assertEq(address(executor).balance, 0, "sweep drained native ETH");
        assertEq(OTHER.balance - destinationBefore, combinedBefore, "destination received the sweep");
    }

    function testSweepIsOwnerOnly() public {
        vm.prank(OTHER);
        (bool ok, bytes memory ret) = address(executor).call(
            abi.encodeWithSignature("sweep(address)", BRIBE_RECIPIENT)
        );
        _assertRevertData(
            Outcome(ok, ret, 0, 0, 0),
            _executorRevert(ExecutorErrors.UNAUTHORIZED, OTHER),
            "non-owner sweep"
        );
    }

    function testMinProfitRejectsShortfall() public {
        // The stream must be non-degenerate: a bare separator reverts in the
        // dispatch loop before any profit handling. WETH_DEPOSIT_ALL with no
        // native ETH is a clean no-op that measures a zero gross, one wei
        // short of the floor.
        bytes memory stream = bytes.concat(Cmd.preamble(_emptyTable()), Cmd.wethDepositAll());
        Outcome memory o = _executeSimulated(executor, stream, (uint256(1) << 32) | 1);
        _assertRevertData(
            o,
            _executorRevert(ExecutorErrors.INSUFFICIENT_PROFIT, 0, 1),
            "min_profit shortfall"
        );
    }

    // ── Physical-balance-capped bribe: the receipt reports the ACTUAL paid ─

    /// A bribe demanded above the executor's physical WETH+ETH never reverts:
    /// the executor pays its whole physical balance (auto-withdrawing WETH
    /// first, of which there is none here) and `bribe_paid` reports the
    /// ACTUAL paid amount, not the bips formula. The profit lives entirely
    /// in the ERC6909 claims domain (mode 2, V4_MINT_COMPACT capture), so
    /// the physical side stays at the seeded balance until the payout.
    function testPhysicalBalanceCappedBribeReportsActualPaid() public {
        uint256 physicalSeed = 0.3 ether;
        uint256 gross = 1 ether;

        // 2-hop WETH->USDC->WETH pools; the trailing V4_MINT_COMPACT captures
        // the whole WETH profit as ERC6909 claims with no physical movement.
        PoolKey memory keyA = _poolKey(address(weth), address(usdc), 0, 60);
        bool za = keyA.currency0 == address(weth);
        _setupV4Swap(v4Pm, keyA, gross, 2000e6, za, address(usdc));
        PoolKey memory keyB = _poolKey(address(weth), address(usdc), 0, 120);
        bool zb = keyB.currency0 == address(usdc);
        _setupV4Swap(v4Pm, keyB, 2000e6, 2 * gross, zb, address(weth));

        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 recipientIdx = Cmd.add(at, BRIBE_RECIPIENT);

        bytes memory inner = Cmd.v4SwapCompact(
            Cmd.add(at, keyA.currency0), Cmd.add(at, keyA.currency1), 0, 60, Cmd.SENTINEL_NATIVE, za, gross
        );
        inner = bytes.concat(inner, Cmd.v4SwapCompact(
            Cmd.add(at, keyB.currency0), Cmd.add(at, keyB.currency1), 0, 120, Cmd.SENTINEL_NATIVE, zb, 2000e6
        ));
        inner = bytes.concat(inner, Cmd.v4MintCompact(wethIdx, executorIdx, gross));
        bytes memory stream = bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner));

        // Seed exactly the physical balance the cap will pay out: 0.3 ETH,
        // no WETH. The demanded bribe is 5000 bips * 1 ETH = 0.5 ETH.
        vm.deal(address(executor), physicalSeed);
        uint256 recipientBefore = BRIBE_RECIPIENT.balance;

        Outcome memory o = _execute(executor, stream, Cmd.makeConfig(2, 5000, recipientIdx, 0));

        // Exact receipt identity: measured gross in the claims domain, and
        // bribe_paid = min(demanded, physical) = the seeded balance itself.
        assertEq(o.grossProfit, gross, "gross profit measured in the claims domain");
        assertEq(o.bribePaid, physicalSeed, "bribe_paid is the actual paid amount");
        assertEq(BRIBE_RECIPIENT.balance - recipientBefore, physicalSeed, "recipient got the capped amount");
        // The physical side is drained; the retained profit stays as claims.
        assertEq(address(executor).balance, 0, "physical ETH drained by the capped bribe");
        assertEq(weth.balanceOf(address(executor)), 0, "no physical WETH");
        assertEq(
            v4Pm.balanceOf(address(executor), uint256(uint160(address(weth)))),
            gross + 1,
            "retained profit as ERC6909 claims (+1 warmup wei)"
        );
    }
}
