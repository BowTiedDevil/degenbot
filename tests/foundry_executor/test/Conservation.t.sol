// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {ConservationOps} from "../src/ConservationOps.sol";
import {Cmd} from "../src/Cmd.sol";
import {ExecutorErrors} from "../src/generated/Errors.sol";
import {IFakeUniswapV2Pair} from "../src/generated/IFakeUniswapV2Pair.sol";
import {PoolKey} from "../src/generated/Types.sol";

/// Port of `tests/executor/test_conservation.py`.
///
/// Negative tests proving the conservation check catches real balance bugs
/// rather than passing trivially. Each case deliberately introduces an error
/// into the command stream and pins the helper's exact failure reason:
///
/// 1. V4_TAKE to an address in the AddressTable but OUTSIDE the tracked
///    account set: WETH+ETH conservation fails (tracked accounts lost value
///    that no tracked account gained).
/// 2. A stray ERC20_TRANSFER inside the stream to an untracked address: the
///    transaction succeeds, then WETH+ETH conservation fails.
/// 3. V4_TAKE to a tracked pool instead of the executor: conservation
///    PASSES (value stayed inside the tracked set) but the executor-profit
///    assertion fails — conservation prevents leaks, the profit check
///    verifies distribution.
/// 4. A stray ERC20_TRANSFER appended after a V2-V2-V2 callback chain:
///    the swap succeeds, then WETH+ETH conservation fails.
///
/// Every failure is asserted through an external-call negative control on
/// the exact Error(string) reason, never a broad pass/fail check that a
/// setup error could satisfy.
contract ConservationTest is PoolSetup, ConservationOps {
    uint256 internal constant AMOUNT_WETH_PROFIT = 2 ether;
    uint256 internal constant NET_PROFIT = 1 ether;
    uint16 internal constant CONSERVATION_V2_FEE = 30;

    /// An untracked recipient address (never added to any tracked set).
    address internal constant UNTRACKED = address(0xD1E);

    // ── Negative-control surface ─────────────────────────────────────────

    function verifyConservationNC(
        BalanceSnapshot memory beforeSnap,
        BalanceSnapshot memory afterSnap,
        address executorAccount,
        bool checkExecutorProfit,
        int256 expectedWethEth,
        string memory label
    )
        external
        pure
    {
        _verifyConservation(
            beforeSnap, afterSnap, executorAccount, checkExecutorProfit, expectedWethEth, label
        );
    }

    /// Require the helper to reject the (before, after) pair with exactly the
    /// expected Error(string) reason, named by the label. A passing call or a
    /// rejection for any other reason fails here.
    function _expectConservationFailure(
        BalanceSnapshot memory beforeSnap,
        BalanceSnapshot memory afterSnap,
        address executorAccount,
        bool checkExecutorProfit,
        int256 expectedWethEth,
        string memory label,
        string memory phrase
    )
        internal
    {
        (bool ok, bytes memory ret) = address(this).call(
            abi.encodeCall(
                this.verifyConservationNC,
                (beforeSnap, afterSnap, executorAccount, checkExecutorProfit, expectedWethEth, label)
            )
        );
        assertFalse(ok, string.concat(label, ": expected the helper to reject"));
        assertEq(bytes4(ret), ExecutorErrors.ERROR_STRING, "rejection is Error(string)");
        assertTrue(_contains(ret, label), "rejection carries the assertion label");
        assertTrue(_contains(ret, phrase), "rejection names the expected failure reason");
    }

    // ── Shared V4 three-hop fixture ──────────────────────────────────────

    struct V4Hops {
        PoolKey a; // WETH -> USDC
        PoolKey b; // USDC -> WBTC
        PoolKey c; // WBTC -> WETH
        bool zfoA;
        bool zfoB;
        bool zfoC;
    }

    /// Pool A: WETH/USDC (fee 3000, ts 60), B: USDC/WBTC (fee 500, ts 10),
    /// C: WBTC/WETH (fee 10000, ts 200). C overpays WETH: the arb is worth
    /// exactly NET_PROFIT.
    function _setupV4ThreeHops() internal returns (V4Hops memory h) {
        h.a = _poolKey(address(weth), address(usdc), 3000, 60);
        h.zfoA = h.a.currency0 == address(weth);
        _setupV4Swap(v4Pm, h.a, AMOUNT_WETH, AMOUNT_USDC, h.zfoA, address(usdc));

        h.b = _poolKey(address(usdc), address(wbtc), 500, 10);
        h.zfoB = h.b.currency0 == address(usdc);
        _setupV4Swap(v4Pm, h.b, AMOUNT_USDC, AMOUNT_WBTC, h.zfoB, address(wbtc));

        h.c = _poolKey(address(wbtc), address(weth), 10_000, 200);
        h.zfoC = h.c.currency0 == address(wbtc);
        _setupV4Swap(v4Pm, h.c, AMOUNT_WBTC, AMOUNT_WETH_PROFIT, h.zfoC, address(weth));
    }

    /// The source `AddressTable()` — every address appended in source order;
    /// the owner (this contract) is IN the table (the commands need it) but
    /// must stay OUT of the tracked account set to be a leak.
    function _v4Table()
        internal
        view
        returns (
            Cmd.Table memory at,
            uint8 wethIdx,
            uint8 usdcIdx,
            uint8 wbtcIdx,
            uint8 executorIdx,
            uint8 zeroIdx,
            uint8 ownerIdx
        )
    {
        at = _emptyTable();
        wethIdx = Cmd.add(at, address(weth));
        usdcIdx = Cmd.add(at, address(usdc));
        wbtcIdx = Cmd.add(at, address(wbtc));
        executorIdx = Cmd.add(at, address(executor));
        zeroIdx = Cmd.add(at, address(0));
        ownerIdx = Cmd.add(at, address(this));
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

    /// tokens[0] is WETH (the profit domain); NATIVE rides along for the
    /// combined WETH+ETH check, exactly as the source snapshot's automatic
    /// ETH tracking does.
    function _tokens() internal view returns (address[] memory t) {
        t = new address[](4);
        t[0] = address(weth);
        t[1] = address(usdc);
        t[2] = address(wbtc);
        t[3] = NATIVE;
    }

    function _accountsExecutorPm() internal view returns (address[] memory a) {
        a = new address[](2);
        a[0] = address(executor);
        a[1] = address(v4Pm);
    }

    // ── 1. V4_TAKE to untracked address: WETH+ETH conservation fails ─────

    function testV4TakeToUntrackedAddress() public {
        V4Hops memory h = _setupV4ThreeHops();
        (Cmd.Table memory at, uint8 wethIdx, , , , uint8 zeroIdx, uint8 ownerIdx) = _v4Table();

        bytes memory inner = _v4Swap(at, h.a, h.zfoA, zeroIdx, AMOUNT_WETH);
        inner = bytes.concat(inner, _v4Swap(at, h.b, h.zfoB, zeroIdx, AMOUNT_USDC));
        inner = bytes.concat(inner, _v4Swap(at, h.c, h.zfoC, zeroIdx, AMOUNT_WBTC));
        // BUG (deliberate): profit to the owner, who is in the address table
        // but NOT in the tracked account set.
        inner = bytes.concat(inner, Cmd.v4Take(wethIdx, ownerIdx, NET_PROFIT));

        address[] memory tokens = _tokens();
        address[] memory accounts = _accountsExecutorPm();
        BalanceSnapshot memory before = _snapshotBalances(tokens, accounts);

        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)), 1);

        _expectConservationFailure(
            before,
            _snapshotBalances(tokens, accounts),
            address(executor),
            false,
            0,
            "V4TakeToUntracked",
            "WETH+ETH conservation violated"
        );
    }

    // ── 2. Extra ERC20_TRANSFER to untracked address ─────────────────────

    function testExtraErc20TransferToUntrackedAddress() public {
        V4Hops memory h = _setupV4ThreeHops();
        (Cmd.Table memory at, uint8 wethIdx, , , uint8 executorIdx, uint8 zeroIdx, uint8 ownerIdx) =
            _v4Table();

        bytes memory inner = _v4Swap(at, h.a, h.zfoA, zeroIdx, AMOUNT_WETH);
        inner = bytes.concat(inner, _v4Swap(at, h.b, h.zfoB, zeroIdx, AMOUNT_USDC));
        inner = bytes.concat(inner, _v4Swap(at, h.c, h.zfoC, zeroIdx, AMOUNT_WBTC));
        inner = bytes.concat(inner, Cmd.v4Take(wethIdx, executorIdx, NET_PROFIT));
        // BUG (deliberate): half the profit leaks out of the tracked set.
        inner = bytes.concat(inner, Cmd.erc20Transfer(wethIdx, ownerIdx, NET_PROFIT / 2));

        address[] memory tokens = _tokens();
        address[] memory accounts = _accountsExecutorPm();
        BalanceSnapshot memory before = _snapshotBalances(tokens, accounts);

        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)), 1);

        _expectConservationFailure(
            before,
            _snapshotBalances(tokens, accounts),
            address(executor),
            false,
            0,
            "ExtraTransferLeak",
            "WETH+ETH conservation violated"
        );
    }

    // ── 3. V4_TAKE to wrong tracked address: conservation passes, profit fails ──

    function testV4TakeToTrackedPoolProfitCheckFails() public {
        V4Hops memory h = _setupV4ThreeHops();
        (address t0, address t1) = _sorted(address(weth), address(usdc));
        IFakeUniswapV2Pair v2a = _deployV2Pair(t0, t1, 0, CONSERVATION_V2_FEE);
        (Cmd.Table memory at, uint8 wethIdx, , , , uint8 zeroIdx,) = _v4Table();
        uint8 v2aIdx = Cmd.add(at, address(v2a));

        bytes memory inner = _v4Swap(at, h.a, h.zfoA, zeroIdx, AMOUNT_WETH);
        inner = bytes.concat(inner, _v4Swap(at, h.b, h.zfoB, zeroIdx, AMOUNT_USDC));
        inner = bytes.concat(inner, _v4Swap(at, h.c, h.zfoC, zeroIdx, AMOUNT_WBTC));
        // BUG (deliberate): profit to a TRACKED pool instead of the executor.
        inner = bytes.concat(inner, Cmd.v4Take(wethIdx, v2aIdx, NET_PROFIT));

        address[] memory tokens = _tokens();
        address[] memory accounts = new address[](3);
        accounts[0] = address(executor);
        accounts[1] = address(v4Pm);
        accounts[2] = address(v2a);
        BalanceSnapshot memory before = _snapshotBalances(tokens, accounts);

        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)), 1);
        BalanceSnapshot memory after_ = _snapshotBalances(tokens, accounts);

        // Conservation PASSES: the WETH moved between tracked accounts.
        _verifyConservation(before, after_, address(executor), false, 0, "V4TakeToTracked");

        // But the executor-profit assertion fails: the executor got nothing.
        _expectConservationFailure(
            before,
            after_,
            address(executor),
            true,
            int256(NET_PROFIT),
            "V4TakeToTracked",
            "executor WETH+ETH profit mismatch"
        );
    }

    // ── 4. Stray ERC20_TRANSFER inside the V2-V2-V2 callback chain ───────

    function testStrayWethTransferAfterV2Swap() public {
        (address a0, address a1) = _sorted(address(weth), address(usdc));
        IFakeUniswapV2Pair v2a = _deployV2Pair(a0, a1, 0, CONSERVATION_V2_FEE);
        (address b0, address b1) = _sorted(address(usdc), address(wbtc));
        IFakeUniswapV2Pair v2b = _deployV2Pair(b0, b1, 0, CONSERVATION_V2_FEE);
        (address c0, address c1) = _sorted(address(wbtc), address(weth));
        IFakeUniswapV2Pair v2c = _deployV2Pair(c0, c1, 0, CONSERVATION_V2_FEE);

        // The source's `_setup_v2_for_calc`: ample two-sided liquidity at the
        // input*100 / output*100 ratio, synced reserves.
        weth.mint(address(v2a), AMOUNT_WETH * 100);
        usdc.mint(address(v2a), AMOUNT_USDC * 100);
        v2a.sync();
        usdc.mint(address(v2b), AMOUNT_USDC * 100);
        wbtc.mint(address(v2b), AMOUNT_WBTC * 100);
        v2b.sync();
        wbtc.mint(address(v2c), AMOUNT_WBTC * 100);
        weth.mint(address(v2c), AMOUNT_WETH_PROFIT * 100);
        v2c.sync();

        bool zfoA = v2a.token0() == address(weth);
        bool zfoB = v2b.token0() == address(usdc);
        bool zfoC = v2c.token0() == address(wbtc);

        uint256 aOut = _v2GetAmountOut(
            AMOUNT_WETH, weth.balanceOf(address(v2a)), usdc.balanceOf(address(v2a)), CONSERVATION_V2_FEE
        );
        uint256 bOut = _v2GetAmountOut(
            aOut, usdc.balanceOf(address(v2b)), wbtc.balanceOf(address(v2b)), CONSERVATION_V2_FEE
        );
        uint256 cOut = _v2GetAmountOut(
            bOut, wbtc.balanceOf(address(v2c)), weth.balanceOf(address(v2c)), CONSERVATION_V2_FEE
        );

        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 wbtcIdx = Cmd.add(at, address(wbtc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v2aIdx = Cmd.add(at, address(v2a));
        uint8 v2bIdx = Cmd.add(at, address(v2b));
        uint8 v2cIdx = Cmd.add(at, address(v2c));
        uint8 ownerIdx = Cmd.add(at, address(this)); // untracked in the snapshot

        uint256 strayAmount = cOut / 4; // leak 25% of pool C's output

        // Pool C's flash swap sends the WETH output to the executor BEFORE
        // the callback, so the callback chain can fund pool A and leak.
        bytes memory cFwd = Cmd.erc20Transfer(wethIdx, v2aIdx, AMOUNT_WETH);
        cFwd = bytes.concat(cFwd, Cmd.v2SwapCalc(v2aIdx, zfoA, v2bIdx, CONSERVATION_V2_FEE));
        cFwd = bytes.concat(cFwd, Cmd.v2SwapCalc(v2bIdx, zfoB, v2cIdx, CONSERVATION_V2_FEE));
        // BUG (deliberate): stray WETH transfer after the swap chain completes.
        cFwd = bytes.concat(cFwd, Cmd.erc20Transfer(wethIdx, ownerIdx, strayAmount));

        bytes memory commands = Cmd.v2SwapCompact(v2cIdx, zfoC, cOut, executorIdx, CONSERVATION_V2_FEE, cFwd);

        address[] memory tokens = _tokens();
        address[] memory accounts = new address[](4);
        accounts[0] = address(executor);
        accounts[1] = address(v2a);
        accounts[2] = address(v2b);
        accounts[3] = address(v2c);
        BalanceSnapshot memory before = _snapshotBalances(tokens, accounts);

        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 1);

        _expectConservationFailure(
            before,
            _snapshotBalances(tokens, accounts),
            address(executor),
            false,
            0,
            "StrayWethAfterSwap",
            "WETH+ETH conservation violated"
        );
    }

    // ── Direct-helper meta-tests (documented additions, not source cases) ──
    //
    // The source module validates the helper THROUGH live executor runs.
    // These three meta-tests drive the same negative controls directly
    // against balance movement, pinning each failure mode the helper must
    // separate — without an executor in the loop.

    /// Untracked leak: value leaves the tracked set with no tracked account
    /// gaining it — the WETH+ETH violation the live V4-TAKE leak produces.
    function testHelperUntrackedLeakFailsConservation() public {
        vm.deal(address(executor), 1 ether);
        address[] memory tokens = _tokens();
        address[] memory accounts = _accountsExecutorPm();
        BalanceSnapshot memory before = _snapshotBalances(tokens, accounts);

        executor.withdraw(0.5 ether, UNTRACKED);

        _expectConservationFailure(
            before,
            _snapshotBalances(tokens, accounts),
            address(executor),
            false,
            0,
            "HelperUntrackedLeak",
            "WETH+ETH conservation violated"
        );
    }

    /// Wrong tracked recipient: value moves between tracked accounts, so
    /// conservation passes; only the executor-profit assertion catches the
    /// wrong distribution. Both verdicts are pinned on one movement.
    function testHelperWrongTrackedRecipientConservesButFailsProfit() public {
        (address t0, address t1) = _sorted(address(weth), address(usdc));
        IFakeUniswapV2Pair pair = _deployV2Pair(t0, t1, 0, CONSERVATION_V2_FEE);
        weth.mint(address(v4Pm), 1 ether);

        address[] memory tokens = new address[](2);
        tokens[0] = address(weth);
        tokens[1] = NATIVE;
        address[] memory accounts = new address[](3);
        accounts[0] = address(executor);
        accounts[1] = address(v4Pm);
        accounts[2] = address(pair);
        BalanceSnapshot memory before = _snapshotBalances(tokens, accounts);

        vm.prank(address(v4Pm));
        weth.transfer(address(pair), 1 ether);
        BalanceSnapshot memory after_ = _snapshotBalances(tokens, accounts);

        _verifyConservation(before, after_, address(executor), false, 0, "HelperTrackedRecipient");
        _expectConservationFailure(
            before,
            after_,
            address(executor),
            true,
            1 ether,
            "HelperTrackedRecipient",
            "executor WETH+ETH profit mismatch"
        );
    }

    /// Stray-transfer shape: the executor's WETH is siphoned to an untracked
    /// address while every tracked V2 pool is untouched — detected as the
    /// same WETH+ETH violation the live stray-transfer case produces.
    function testHelperStrayTransferShapeDetected() public {
        (address a0, address a1) = _sorted(address(weth), address(usdc));
        IFakeUniswapV2Pair v2a = _deployV2Pair(a0, a1, 0, CONSERVATION_V2_FEE);
        (address b0, address b1) = _sorted(address(usdc), address(wbtc));
        IFakeUniswapV2Pair v2b = _deployV2Pair(b0, b1, 0, CONSERVATION_V2_FEE);
        (address c0, address c1) = _sorted(address(wbtc), address(weth));
        IFakeUniswapV2Pair v2c = _deployV2Pair(c0, c1, 0, CONSERVATION_V2_FEE);

        weth.mint(address(executor), 4 ether);
        address[] memory tokens = _tokens();
        address[] memory accounts = new address[](4);
        accounts[0] = address(executor);
        accounts[1] = address(v2a);
        accounts[2] = address(v2b);
        accounts[3] = address(v2c);
        BalanceSnapshot memory before = _snapshotBalances(tokens, accounts);

        vm.prank(address(executor));
        weth.transfer(UNTRACKED, 1 ether);

        _expectConservationFailure(
            before,
            _snapshotBalances(tokens, accounts),
            address(executor),
            false,
            0,
            "HelperStrayTransfer",
            "WETH+ETH conservation violated"
        );
    }

    function _contains(bytes memory haystack, string memory needle) private pure returns (bool) {
        bytes memory n = bytes(needle);
        if (n.length > haystack.length) return false;
        for (uint256 i = 0; i + n.length <= haystack.length; i++) {
            uint256 j = 0;
            while (j < n.length && haystack[i + j] == n[j]) {
                j++;
            }
            if (j == n.length) return true;
        }
        return false;
    }
}
