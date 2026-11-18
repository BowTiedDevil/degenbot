// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {PoolKey} from "../src/generated/Types.sol";

/// The one cheatcode the shared `Vm` surface does not carry: pinning
/// `block.coinbase` for the explicit coinbase model below.
interface CoinbasePin {
    function coinbase(address) external;
}

/// Port of `tests/executor/test_cmd_executor_bribe_commands.py`.
///
/// Bribe configuration (bips + recipient index) rides in the execute() config
/// word, not in the command stream; the payment happens after the profit
/// check. The bribe flows are 2-hop WETH->USDC->WETH routes whose terminal
/// profit is WETH, so a self-funded slow-path transaction (profit mode != 0
/// asserts combined-after >= combined-before) stays satisfiable.
///
/// Coinbase model: forge tests run at gasprice 0 and basefee 0 and credit the
/// block beneficiary nothing, so the default `block.coinbase` is address(0)
/// and would only burn a bribe. Every coinbase case pins `vm.coinbase` to a
/// dedicated funded address and asserts its exact delta — which fails loudly
/// if this lane ever starts crediting block rewards on top of the bribe.
contract CmdExecutorBribeCommandsTest is PoolSetup {
    /// 2-hop bribe route amounts (source constants): 10 WETH in, 20 WETH out.
    uint256 internal constant IN_WETH = 10 ether;
    uint256 internal constant MID_USDC = 2000e6;
    uint256 internal constant OUT_WETH = 20 ether;
    uint256 internal constant ROUTE_PROFIT = OUT_WETH - IN_WETH;

    address internal constant COINBASE = address(0xC01B);
    address internal constant BRIBE_RECIPIENT = address(0xB121);

    function _setCoinbase() internal {
        CoinbasePin(address(uint160(uint256(keccak256("hevm cheat code"))))).coinbase(COINBASE);
    }

    // ── 2-hop route (source `_two_hop_weth_usdc_weth`) ────────────────────

    struct Route {
        PoolKey a;
        PoolKey b;
        bool za;
        bool zb;
    }

    /// Pool A: WETH/USDC (fee 0, ts 60) — sell WETH, buy USDC.
    /// Pool B: WETH/USDC (fee 0, ts 120) — sell USDC, buy WETH.
    function _route() internal returns (Route memory r) {
        r.a = _poolKey(address(weth), address(usdc), 0, 60);
        r.za = r.a.currency0 == address(weth);
        _setupV4Swap(v4Pm, r.a, IN_WETH, MID_USDC, r.za, address(usdc));
        r.b = _poolKey(address(weth), address(usdc), 0, 120);
        r.zb = r.b.currency0 == address(usdc);
        _setupV4Swap(v4Pm, r.b, MID_USDC, OUT_WETH, r.zb, address(weth));
    }

    /// The bare source AddressTable: pm, weth, usdc, executor, zero. The
    /// executor's sentinel resolution folds weth/executor/pm/native into
    /// reserved indices, so only genuinely dynamic addresses consume slots.
    function _table() internal view returns (Cmd.Table memory at, uint8 wethIdx, uint8 usdcIdx, uint8 executorIdx, uint8 zeroIdx) {
        at = _emptyTable();
        Cmd.add(at, address(v4Pm));
        wethIdx = Cmd.add(at, address(weth));
        usdcIdx = Cmd.add(at, address(usdc));
        executorIdx = Cmd.add(at, address(executor));
        zeroIdx = Cmd.add(at, address(0));
    }

    function _v4Swap(Cmd.Table memory at, PoolKey memory key, bool zfo, uint8 hooksIdx, uint256 amount)
        internal
        pure
        returns (bytes memory)
    {
        return Cmd.v4SwapCompact(
            Cmd.add(at, key.currency0),
            Cmd.add(at, key.currency1),
            uint16(key.fee),
            int16(key.tick_spacing),
            hooksIdx,
            zfo,
            amount
        );
    }

    /// The two swap legs plus the trailing V4_TAKE of the WETH profit: all PM
    /// deltas net to zero, so no settle command is needed.
    function _routeBody(Cmd.Table memory at, Route memory r, uint8 wethIdx, uint8 executorIdx, uint8 zeroIdx)
        internal
        pure
        returns (bytes memory inner)
    {
        inner = _v4Swap(at, r.a, r.za, zeroIdx, IN_WETH);
        inner = bytes.concat(inner, _v4Swap(at, r.b, r.zb, zeroIdx, MID_USDC));
        inner = bytes.concat(inner, Cmd.v4Take(wethIdx, executorIdx, ROUTE_PROFIT));
    }

    // ── TestBribeCoinbase ─────────────────────────────────────────────────

    /// 50% bribe to block.coinbase via the config word, paid from WETH profit.
    function testBribeCoinbaseFirstCommand() public {
        _setCoinbase();
        vm.deal(COINBASE, 1 ether);
        Route memory r = _route();
        (Cmd.Table memory at, uint8 wethIdx,, uint8 executorIdx, uint8 zeroIdx) = _table();
        bytes memory stream =
            bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(_routeBody(at, r, wethIdx, executorIdx, zeroIdx)));

        weth.mint(address(executor), IN_WETH);
        uint256 combinedBefore = weth.balanceOf(address(executor)) + address(executor).balance;
        uint256 coinbaseBefore = COINBASE.balance;

        Outcome memory o = _execute(executor, stream, Cmd.makeConfig(1, 5000, 0, 0));

        // The receipt reports the measured gross and the actual payout.
        assertEq(o.grossProfit, ROUTE_PROFIT, "gross profit");
        assertEq(o.bribePaid, ROUTE_PROFIT / 2, "bribe paid");
        uint256 combinedAfter = weth.balanceOf(address(executor)) + address(executor).balance;
        assertEq(combinedAfter - combinedBefore, ROUTE_PROFIT - ROUTE_PROFIT / 2, "retained profit");
        assertEq(COINBASE.balance - coinbaseBefore, ROUTE_PROFIT / 2, "coinbase delta == bribe");
    }

    /// 0 bips in the config sends no bribe: the one-hop WETH->USDC flow at the
    /// source's config = 2, with the USDC take compact-pinned.
    function testBribeCoinbaseZeroBips() public {
        _setCoinbase();
        uint256 coinbaseBefore = COINBASE.balance;

        PoolKey memory key = _poolKey(address(weth), address(usdc), 0, 60);
        bool zfo = key.currency0 == address(weth); // selling WETH, buying USDC
        _setupV4Swap(v4Pm, key, AMOUNT_WETH, AMOUNT_USDC, zfo, address(usdc));
        (Cmd.Table memory at, uint8 wethIdx, uint8 usdcIdx, uint8 executorIdx, uint8 zeroIdx) = _table();

        bytes memory inner = _v4Swap(at, key, zfo, zeroIdx, AMOUNT_WETH);
        inner = bytes.concat(inner, Cmd.v4TakeCompact(usdcIdx, executorIdx, AMOUNT_USDC));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(wethIdx));
        bytes memory stream = bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner));

        weth.mint(address(executor), AMOUNT_WETH);
        Outcome memory o = _execute(executor, stream, Cmd.makeConfig(2, 0, 0, 0));

        assertEq(usdc.balanceOf(address(executor)), AMOUNT_USDC, "USDC take (source: >= AMOUNT_USDC)");
        assertEq(o.bribePaid, 0, "receipt bribe_paid is zero");
        assertEq(COINBASE.balance, coinbaseBefore, "0 bips sends no bribe");
    }

    // ── TestBribeAddress ──────────────────────────────────────────────────

    /// 10% bribe to an address-table entry via the config word.
    function testBribeAddressFirstCommand() public {
        Route memory r = _route();
        (Cmd.Table memory at, uint8 wethIdx,, uint8 executorIdx, uint8 zeroIdx) = _table();
        uint8 recipientIdx = Cmd.add(at, BRIBE_RECIPIENT);
        bytes memory stream =
            bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(_routeBody(at, r, wethIdx, executorIdx, zeroIdx)));

        weth.mint(address(executor), IN_WETH);
        uint256 combinedBefore = weth.balanceOf(address(executor)) + address(executor).balance;
        uint256 recipientBefore = BRIBE_RECIPIENT.balance;

        Outcome memory o = _execute(executor, stream, Cmd.makeConfig(1, 1000, recipientIdx, 0));

        assertEq(o.grossProfit, ROUTE_PROFIT, "gross profit");
        assertEq(o.bribePaid, ROUTE_PROFIT / 10, "10% bribe paid");
        assertEq(BRIBE_RECIPIENT.balance - recipientBefore, ROUTE_PROFIT / 10, "recipient delta");
        uint256 combinedAfter = weth.balanceOf(address(executor)) + address(executor).balance;
        assertEq(combinedAfter - combinedBefore, ROUTE_PROFIT - ROUTE_PROFIT / 10, "retained profit");
    }

    event BribeGasOverhead(uint256 baseline, uint256 withBribe, uint256 overhead);

    /// Named gas evidence (source TestBribeAddress.test_bribe_address_gas_overhead):
    /// the same 2-hop flow run from byte-identical state with and without the
    /// bribe config — the slow path's balance reads, WETH withdraw, and ETH
    /// transfer must cost measurably more, but bounded. The bribe run pays
    /// the owner (the test contract), mirroring the source's owner recipient.
    function testBribeAddressGasOverhead() public {
        Route memory r = _route();
        (Cmd.Table memory at, uint8 wethIdx,, uint8 executorIdx, uint8 zeroIdx) = _table();
        uint8 recipientIdx = Cmd.add(at, address(this));
        bytes memory stream =
            bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(_routeBody(at, r, wethIdx, executorIdx, zeroIdx)));

        // Snapshot-isolated: both runs start from byte-identical state and
        // share the identical stream (the table carries the recipient entry
        // in both, as in the source).
        uint256 snapshot = vm.snapshotState();
        weth.mint(address(executor), IN_WETH);
        Outcome memory baseline = _executeRaw(executor, stream, 1);
        vm.revertToState(snapshot);

        weth.mint(address(executor), IN_WETH);
        Outcome memory bribed = _executeRaw(executor, stream, Cmd.makeConfig(1, 1000, recipientIdx, 0));

        uint256 overhead = bribed.gasUsed - baseline.gasUsed;
        emit BribeGasOverhead(baseline.gasUsed, bribed.gasUsed, overhead);
        assertGt(overhead, 0, "bribe path must cost more than the no-bribe fast path");
        assertLt(overhead, 60_000, "bribe overhead too high");
    }

    // ── TestBribeWETHAutoWithdraw ─────────────────────────────────────────

    /// When the profit is WETH and the executor holds no ETH, the bribe
    /// auto-withdraws WETH to cover the ETH shortfall — asserted to the wei.
    function testBribeAutoWithdrawsWeth() public {
        _setCoinbase();
        uint256 coinbaseBefore = COINBASE.balance;

        // Drain the executor's physical ETH (source withdraws the full balance).
        executor.withdraw(address(executor).balance, address(this));
        assertEq(address(executor).balance, 0, "executor ETH drained");

        PoolKey memory key = _poolKey(address(weth), address(usdc), 0, 60);
        bool zfo = key.currency0 == address(usdc); // selling USDC, buying WETH
        _setupV4Swap(v4Pm, key, AMOUNT_USDC, AMOUNT_WETH, zfo, address(weth));
        (Cmd.Table memory at, uint8 wethIdx, uint8 usdcIdx, uint8 executorIdx, uint8 zeroIdx) = _table();

        bytes memory inner = _v4Swap(at, key, zfo, zeroIdx, AMOUNT_USDC);
        inner = bytes.concat(inner, Cmd.v4TakeCompact(wethIdx, executorIdx, AMOUNT_WETH));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(usdcIdx));
        bytes memory stream = bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner));

        usdc.mint(address(executor), AMOUNT_USDC);
        uint256 wethBefore = weth.balanceOf(address(executor));

        Outcome memory o = _execute(executor, stream, Cmd.makeConfig(1, 5000, 0, 0));

        assertEq(o.grossProfit, AMOUNT_WETH, "gross profit is the WETH take");
        assertEq(o.bribePaid, AMOUNT_WETH / 2, "bribe paid from withdrawn WETH");
        assertEq(
            weth.balanceOf(address(executor)),
            wethBefore + AMOUNT_WETH - AMOUNT_WETH / 2,
            "WETH unwrapped for the bribe"
        );
        assertEq(address(executor).balance, 0, "no ETH retained (source: < AMOUNT_WETH / 10)");
        assertEq(COINBASE.balance - coinbaseBefore, AMOUNT_WETH / 2, "coinbase delta");
    }

    // ── Boundary the source module leaves implicit ────────────────────────

    /// Zero gross profit pays zero regardless of the configured bips: the
    /// demanded bribe bips * 0 / 10000 is 0, so no WETH is auto-withdrawn and
    /// no ETH leaves — the receipt reports (0, 0) exactly for every bips
    /// value, and the recipient is untouched.
    function testZeroGrossProfitPaysZeroRegardlessOfBips() public {
        (Cmd.Table memory at, uint8 wethIdx,, uint8 executorIdx,) = _table();
        uint8 recipientIdx = Cmd.add(at, BRIBE_RECIPIENT);
        // V4_TAKE_DELTA on a zero delta is a clean no-op: zero gross in the
        // mode-1 domain, no state movement at all.
        bytes memory stream = bytes.concat(
            Cmd.preamble(at), Cmd.v4Unlock(Cmd.v4TakeDelta(wethIdx, executorIdx))
        );

        uint256 recipientBefore = BRIBE_RECIPIENT.balance;
        uint16[3] memory bipsValues = [uint16(1), 5000, 10_000];
        for (uint256 i = 0; i < bipsValues.length; i++) {
            Outcome memory o = _execute(executor, stream, Cmd.makeConfig(1, bipsValues[i], recipientIdx, 0));
            assertEq(o.grossProfit, 0, "zero gross profit");
            assertEq(o.bribePaid, 0, "zero bribe regardless of bips");
        }
        assertEq(BRIBE_RECIPIENT.balance, recipientBefore, "recipient untouched");
        assertEq(weth.balanceOf(address(executor)), 0, "no WETH withdrawn");
        assertEq(address(executor).balance, 0, "no ETH moved");
    }
}
