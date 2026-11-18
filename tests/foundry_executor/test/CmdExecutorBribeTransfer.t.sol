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

/// Port of `tests/executor/test_cmd_executor_bribe_transfer.py`.
///
/// Verifies that bribe configuration actually transfers ETH to the right
/// address, to the wei:
///   - Bribe to coinbase (recipient_idx=0) sends profit*bips/10000 ETH to
///     block.coinbase
///   - Bribe to address (recipient_idx>0) sends profit*bips/10000 ETH to the
///     specified address-table entry
///   - No bribe is sent when bips == 0
///   - Fractional precision (1% = 100 bips)
///
/// Two-transaction funding strategy:
///   TX1: Deposit 1 ETH of value into the V4 PoolManager via a WETH->native
///        swap, then V4_MINT_COMPACT to hold it as ERC6909. The ETH stays in
///        the PM. The source runs this at config = 2 (profit mode 2), so the
///        1 WETH the swap settles costs the executor nothing in its own
///        mode-2 profit domain.
///   TX2: V4_BURN_COMPACT the ERC6909 balance + V4_TAKE_DELTA to pull the
///        1 ETH back out. The returning ETH appears as "profit" (combined
///        after > combined before by exactly 1 ETH). The bribe sends
///        bips/10000 of that to the chosen recipient.
///
/// Coinbase model: forge tests run at gasprice 0 and basefee 0 and credit the
/// block beneficiary nothing, so the default `block.coinbase` is address(0)
/// and would only burn a bribe. The coinbase case pins `vm.coinbase` to a
/// dedicated funded address and asserts its exact delta — which fails loudly
/// if this lane ever starts crediting block rewards on top of the bribe.
contract CmdExecutorBribeTransferTest is PoolSetup {
    uint256 internal constant PROFIT_ETH = 1 ether;
    uint16 internal constant BIPS = 5000;

    address internal constant BRIBE_RECIPIENT = address(0xB122);
    address internal constant COINBASE = address(0xC01C);

    /// uint160(NATIVE_ADDRESS) — the ERC6909 id of the native-ETH claims.
    uint256 internal constant NATIVE_ID = 0;

    function _wethId() internal view returns (uint256) {
        return uint256(uint160(address(weth)));
    }

    function _setCoinbase() internal {
        CoinbasePin(address(uint160(uint256(keccak256("hevm cheat code"))))).coinbase(COINBASE);
    }

    // ── Two-transaction funding flow ──────────────────────────────────────

    /// TX1 (source `_setup_and_mint_native_eth`): fund the PM's native side,
    /// swap WETH->native in the (NATIVE, WETH) pool, and hold the native
    /// proceeds as ERC6909 claims via V4_MINT_COMPACT. Config = 2 as source.
    function _setupAndMintNativeEth() internal {
        PoolKey memory key = _poolKey(address(0), address(weth), 500, 10);
        bool zfo = key.currency0 == address(weth); // false: NATIVE < WETH
        _setupV4Swap(v4Pm, key, PROFIT_ETH, PROFIT_ETH, zfo, address(0));

        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 nativeIdx = Cmd.add(at, address(0));

        bytes memory inner = Cmd.v4SwapCompact(
            Cmd.add(at, key.currency0),
            Cmd.add(at, key.currency1),
            uint16(key.fee),
            int16(key.tick_spacing),
            Cmd.SENTINEL_NATIVE, // hooks: no hooks (source's zero_idx)
            zfo,
            PROFIT_ETH
        );
        inner = bytes.concat(inner, Cmd.v4MintCompact(nativeIdx, executorIdx, PROFIT_ETH));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(wethIdx));

        weth.mint(address(executor), PROFIT_ETH);
        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)), 2);
        assertEq(v4Pm.balanceOf(address(executor), NATIVE_ID), PROFIT_ETH, "ERC6909 native claims minted");
    }

    /// TX2 (source `_build_burn_and_take_native_eth`): burn the ERC6909
    /// native claims back into a PM delta and take the native ETH out to the
    /// executor. `recipient` joins the table when the run bribes it. The PM
    /// leads the appended entries as in the source tables: recipient index 0
    /// is shadowed by the coinbase recipient (config idx 0 = block.coinbase),
    /// so a table bribe recipient must land at index >= 1.
    function _burnAndTakeStream(address recipient) internal view returns (bytes memory stream, uint8 recipientIdx) {
        Cmd.Table memory at = _emptyTable();
        Cmd.add(at, address(v4Pm));
        recipientIdx = 0;
        if (recipient != address(0)) {
            recipientIdx = Cmd.add(at, recipient);
        }
        bytes memory inner = bytes.concat(
            Cmd.v4BurnCompact(Cmd.SENTINEL_NATIVE, PROFIT_ETH),
            Cmd.v4TakeDelta(Cmd.SENTINEL_NATIVE, Cmd.SENTINEL_SELF)
        );
        stream = bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner));
    }

    // ── TestBribeTransferBalances ─────────────────────────────────────────

    /// Bribe 50% to the table recipient: the recipient receives exactly
    /// 0.5 ETH and the executor retains 0.5 ETH of the exact 1 ETH profit.
    function testBribeAddressSendsToRecipient() public {
        _setupAndMintNativeEth();
        (bytes memory stream, uint8 recipientIdx) = _burnAndTakeStream(BRIBE_RECIPIENT);

        uint256 combinedBefore = weth.balanceOf(address(executor)) + address(executor).balance;
        uint256 recipientBefore = BRIBE_RECIPIENT.balance;

        Outcome memory o = _execute(executor, stream, Cmd.makeConfig(1, BIPS, recipientIdx, 0));

        uint256 combinedAfter = weth.balanceOf(address(executor)) + address(executor).balance;
        uint256 actualBribe = BRIBE_RECIPIENT.balance - recipientBefore;

        // The source's profit identity: profit = combined delta + bribe.
        assertEq(combinedAfter - combinedBefore + actualBribe, PROFIT_ETH, "profit is exactly 1 ETH");
        assertEq(o.grossProfit, PROFIT_ETH, "receipt gross profit");
        assertEq(o.bribePaid, PROFIT_ETH / 2, "receipt bribe_paid");
        assertEq(actualBribe, PROFIT_ETH / 2, "recipient received exactly half");
        assertEq(combinedAfter - combinedBefore, PROFIT_ETH - PROFIT_ETH / 2, "executor retains half");
    }

    /// Bribe 50% to coinbase via the config word (recipient_idx = 0).
    function testBribeCoinbaseSendsEth() public {
        _setupAndMintNativeEth();
        (bytes memory stream,) = _burnAndTakeStream(address(0));

        _setCoinbase();
        vm.deal(COINBASE, 1 ether);
        uint256 combinedBefore = weth.balanceOf(address(executor)) + address(executor).balance;

        Outcome memory o = _execute(executor, stream, Cmd.makeConfig(1, BIPS, 0, 0));

        uint256 combinedAfter = weth.balanceOf(address(executor)) + address(executor).balance;
        assertEq(o.grossProfit, PROFIT_ETH, "receipt gross profit");
        assertEq(o.bribePaid, PROFIT_ETH / 2, "receipt bribe_paid");
        assertEq(combinedAfter - combinedBefore, PROFIT_ETH - PROFIT_ETH / 2, "executor net gain");
        // Local block-reward semantics: this lane's forge EVM runs at
        // gasprice 0 / basefee 0 and credits the beneficiary nothing, so the
        // coinbase delta is the bribe and nothing else. The exact pin fails
        // loudly if reward crediting is ever introduced.
        assertEq(COINBASE.balance - 1 ether, PROFIT_ETH / 2, "coinbase received exactly the bribe");
    }

    /// No bribe (the Vyper default config = 1): the recipient gets nothing
    /// and the executor keeps the full profit.
    function testNoBribeSendsNothing() public {
        _setupAndMintNativeEth();
        (bytes memory stream,) = _burnAndTakeStream(address(0));

        uint256 combinedBefore = weth.balanceOf(address(executor)) + address(executor).balance;
        uint256 recipientBefore = BRIBE_RECIPIENT.balance;

        Outcome memory o = _execute(executor, stream);

        assertEq(BRIBE_RECIPIENT.balance, recipientBefore, "recipient untouched");
        assertEq(weth.balanceOf(address(executor)) + address(executor).balance - combinedBefore, PROFIT_ETH, "executor keeps full profit");
        assertEq(o.bribePaid, 0, "receipt bribe_paid zero");
    }

    /// Bribe 1% (100 bips) via the config word: exactly 0.01 ETH.
    function testBribeFractionalPrecision() public {
        _setupAndMintNativeEth();
        (bytes memory stream, uint8 recipientIdx) = _burnAndTakeStream(BRIBE_RECIPIENT);

        uint256 recipientBefore = BRIBE_RECIPIENT.balance;

        Outcome memory o = _execute(executor, stream, Cmd.makeConfig(1, 100, recipientIdx, 0));

        assertEq(o.grossProfit, PROFIT_ETH, "receipt gross profit");
        assertEq(o.bribePaid, PROFIT_ETH * 100 / 10_000, "receipt bribe_paid (0.01 ETH)");
        assertEq(BRIBE_RECIPIENT.balance - recipientBefore, PROFIT_ETH * 100 / 10_000, "recipient got exactly 0.01 ETH");
    }
}
