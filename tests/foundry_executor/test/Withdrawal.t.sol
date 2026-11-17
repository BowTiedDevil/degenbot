// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {PoolKey} from "../src/generated/Types.sol";

/// Port of `tests/executor/test_withdrawal.py`.
///
/// Withdrawal ("exit") flows — the commands that convert internal PM
/// accounting entries into physical tokens and move them out:
///   1. ERC6909 -> physical: V4_BURN_COMPACT + V4_TAKE_DELTA inside V4_UNLOCK
///      (one and two transaction forms), then ERC20_XFER_BALANCE out.
///   2. ERC6909 native ETH -> physical ETH: burn + take_delta + SEND_ETH_ALL.
///   3. Arbitrary ERC20 out of the executor: ERC20_XFER_BALANCE and a
///      specific-amount ERC20_TRANSFER.
///   4. Native ETH out of the executor: SEND_ETH / SEND_ETH_ALL.
///   5. Full round trips: swap -> mint -> burn -> take -> pay out.
///
/// Every execution keeps the source's config = 2, except the ERC20-only
/// withdrawal transaction, which uses the source's default config = 1.
contract WithdrawalTest is PoolSetup {
    /// The source AddressTable() order: pm, weth, usdc, executor, zero, owner.
    /// The bare table resolves every address to an appended index — the
    /// sentinels stay unused, exactly as the Python helper behaves here.
    /// One memory struct keeps the via-ir stack shallow at the call sites.
    struct WTable {
        Cmd.Table at;
        uint8 pmIdx;
        uint8 wethIdx;
        uint8 usdcIdx;
        uint8 executorIdx;
        uint8 zeroIdx;
        uint8 ownerIdx;
    }

    function _withdrawalTable() internal view returns (WTable memory t) {
        t.at = _emptyTable();
        t.pmIdx = Cmd.add(t.at, address(v4Pm));
        t.wethIdx = Cmd.add(t.at, address(weth));
        t.usdcIdx = Cmd.add(t.at, address(usdc));
        t.executorIdx = Cmd.add(t.at, address(executor));
        t.zeroIdx = Cmd.add(t.at, address(0));
        t.ownerIdx = Cmd.add(t.at, address(this));
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

    /// WETH/USDC (fee 3000, ts 60): sell WETH, buy USDC.
    function _setupWethUsdcPool() internal returns (PoolKey memory key, bool zfo) {
        key = _poolKey(address(weth), address(usdc), 3000, 60);
        zfo = key.currency0 == address(weth);
        _setupV4Swap(v4Pm, key, AMOUNT_WETH, AMOUNT_USDC, zfo, address(usdc));
    }

    /// NATIVE/WETH (fee 500, ts 10): sell WETH, buy native ETH (zfo is false
    /// — native is always currency0).
    function _setupNativeWethPool() internal returns (PoolKey memory key, bool zfo) {
        key = _poolKey(address(0), address(weth), 500, 10);
        zfo = key.currency0 == address(weth);
        _setupV4Swap(v4Pm, key, AMOUNT_WETH, AMOUNT_WETH, zfo, address(0));
    }

    // ── 1. Withdraw ERC6909 USDC from the PoolManager ────────────────────

    /// Two-transaction flow: (1) swap + mint USDC as ERC6909 (stays inside
    /// the PM); (2) burn + take + transfer out to the owner.
    function testWithdrawErc6909UsdcTwoTx() public {
        weth.mint(address(executor), AMOUNT_WETH);
        uint256 usdcId = uint256(uint160(address(usdc)));
        (PoolKey memory key, bool zfo) = _setupWethUsdcPool();
        WTable memory t = _withdrawalTable();
        Cmd.Table memory at = t.at;
        uint8 pmIdx = t.pmIdx;
        uint8 wethIdx = t.wethIdx;
        uint8 usdcIdx = t.usdcIdx;
        uint8 executorIdx = t.executorIdx;
        uint8 zeroIdx = t.zeroIdx;
        uint8 ownerIdx = t.ownerIdx;

        bytes memory inner = _v4Swap(at, key, zfo, zeroIdx, AMOUNT_WETH);
        inner = bytes.concat(inner, Cmd.v4MintCompact(usdcIdx, executorIdx, AMOUNT_USDC));
        inner = bytes.concat(inner, Cmd.v4Sync(wethIdx));
        inner = bytes.concat(inner, Cmd.erc20Transfer(wethIdx, pmIdx, AMOUNT_WETH));
        inner = bytes.concat(inner, Cmd.v4Settle());
        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)), 2);

        assertEq(v4Pm.balanceOf(address(executor), usdcId), AMOUNT_USDC, "ERC6909 USDC minted");
        assertEq(usdc.balanceOf(address(executor)), 0, "no physical USDC at the executor");

        bytes memory inner2 = Cmd.v4BurnCompact(usdcIdx, AMOUNT_USDC);
        inner2 = bytes.concat(inner2, Cmd.v4TakeDelta(usdcIdx, executorIdx));
        bytes memory commands = Cmd.sendEthAll(ownerIdx); // no-op (0 ETH during unlock)
        commands = bytes.concat(commands, Cmd.v4Unlock(inner2));
        // After the unlock the executor holds physical USDC: send it out.
        commands = bytes.concat(commands, Cmd.erc20XferBalance(usdcIdx, ownerIdx));
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);

        assertEq(v4Pm.balanceOf(address(executor), usdcId), 0, "ERC6909 USDC burned");
        assertEq(usdc.balanceOf(address(executor)), 0, "executor holds no USDC");
        assertGe(usdc.balanceOf(address(this)), AMOUNT_USDC, "owner received the USDC");
    }

    /// Single-transaction flow: mint + burn + take + transfer side by side
    /// with the original swap — burn/take works next to a live swap.
    function testWithdrawErc6909UsdcSingleTx() public {
        weth.mint(address(executor), AMOUNT_WETH);
        uint256 usdcId = uint256(uint160(address(usdc)));
        (PoolKey memory key, bool zfo) = _setupWethUsdcPool();
        WTable memory t = _withdrawalTable();
        Cmd.Table memory at = t.at;
        uint8 pmIdx = t.pmIdx;
        uint8 wethIdx = t.wethIdx;
        uint8 usdcIdx = t.usdcIdx;
        uint8 executorIdx = t.executorIdx;
        uint8 zeroIdx = t.zeroIdx;
        uint8 ownerIdx = t.ownerIdx;

        bytes memory inner = _v4Swap(at, key, zfo, zeroIdx, AMOUNT_WETH);
        inner = bytes.concat(inner, Cmd.v4MintCompact(usdcIdx, executorIdx, AMOUNT_USDC));
        inner = bytes.concat(inner, Cmd.v4Sync(wethIdx));
        inner = bytes.concat(inner, Cmd.erc20Transfer(wethIdx, pmIdx, AMOUNT_WETH));
        inner = bytes.concat(inner, Cmd.v4Settle());
        inner = bytes.concat(inner, Cmd.v4BurnCompact(usdcIdx, AMOUNT_USDC));
        inner = bytes.concat(inner, Cmd.v4TakeDelta(usdcIdx, executorIdx));
        bytes memory commands = bytes.concat(Cmd.v4Unlock(inner), Cmd.erc20XferBalance(usdcIdx, ownerIdx));
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);

        assertEq(v4Pm.balanceOf(address(executor), usdcId), 0, "minted then burned");
        assertGe(usdc.balanceOf(address(this)), AMOUNT_USDC, "physical USDC sent to owner");
        assertEq(usdc.balanceOf(address(executor)), 0, "executor holds no USDC");
    }

    // ── 2. Withdraw ERC6909 native ETH from the PoolManager ──────────────

    /// Two-phase: swap WETH -> native ETH, mint ETH as ERC6909 (no physical
    /// transfer), then burn + take + SEND_ETH_ALL to the owner.
    function testWithdrawErc6909NativeEth() public {
        weth.mint(address(executor), AMOUNT_WETH);
        (PoolKey memory key, bool zfo) = _setupNativeWethPool();
        WTable memory t = _withdrawalTable();
        Cmd.Table memory at = t.at;
        uint8 wethIdx = t.wethIdx;
        uint8 executorIdx = t.executorIdx;
        uint8 zeroIdx = t.zeroIdx;
        uint8 ownerIdx = t.ownerIdx;
        uint8 nativeIdx = Cmd.add(at, address(0)); // the native sentinel

        bytes memory inner = _v4Swap(at, key, zfo, zeroIdx, AMOUNT_WETH);
        inner = bytes.concat(inner, Cmd.v4MintCompact(nativeIdx, executorIdx, AMOUNT_WETH));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(wethIdx));
        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)), 2);
        assertEq(v4Pm.balanceOf(address(executor), 0), AMOUNT_WETH, "ERC6909 native ETH minted");

        bytes memory inner2 = Cmd.v4BurnCompact(nativeIdx, AMOUNT_WETH);
        inner2 = bytes.concat(inner2, Cmd.v4TakeDelta(nativeIdx, executorIdx));
        bytes memory commands = bytes.concat(Cmd.v4Unlock(inner2), Cmd.sendEthAll(ownerIdx));
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);

        assertEq(v4Pm.balanceOf(address(executor), 0), 0, "ERC6909 native ETH burned");
        assertLt(address(executor).balance, 1e15, "executor ETH depleted by SEND_ETH_ALL");
    }

    // ── 3. Withdraw arbitrary ERC20 from the executor ────────────────────

    /// After a V4 swap that takes physical USDC to the executor, drain the
    /// full balance to the owner with ERC20_XFER_BALANCE.
    function testWithdrawErc20XferBalance() public {
        weth.mint(address(executor), AMOUNT_WETH);
        (PoolKey memory key, bool zfo) = _setupWethUsdcPool();
        WTable memory t = _withdrawalTable();
        Cmd.Table memory at = t.at;
        uint8 wethIdx = t.wethIdx;
        uint8 usdcIdx = t.usdcIdx;
        uint8 executorIdx = t.executorIdx;
        uint8 zeroIdx = t.zeroIdx;
        uint8 ownerIdx = t.ownerIdx;

        bytes memory inner = _v4Swap(at, key, zfo, zeroIdx, AMOUNT_WETH);
        inner = bytes.concat(inner, Cmd.v4TakeCompact(usdcIdx, executorIdx, AMOUNT_USDC));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(wethIdx));
        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)), 2);
        assertEq(usdc.balanceOf(address(executor)), AMOUNT_USDC, "physical USDC taken");

        uint256 usdcBefore = usdc.balanceOf(address(this));
        // The source's withdrawal tx runs at the default config (= 1).
        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.erc20XferBalance(usdcIdx, ownerIdx)));
        assertEq(usdc.balanceOf(address(executor)), 0, "executor drained");
        assertEq(usdc.balanceOf(address(this)), usdcBefore + AMOUNT_USDC, "owner received exactly AMOUNT_USDC");
    }

    /// Transfer a specific amount (not the full balance) with ERC20_TRANSFER.
    function testWithdrawErc20TransferSpecificAmount() public {
        usdc.mint(address(executor), AMOUNT_USDC * 3);
        assertEq(usdc.balanceOf(address(executor)), AMOUNT_USDC * 3, "executor funded");

        Cmd.Table memory at = _emptyTable();
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 ownerIdx = Cmd.add(at, address(this));

        _execute(
            executor, bytes.concat(Cmd.preamble(at), Cmd.erc20Transfer(usdcIdx, ownerIdx, AMOUNT_USDC)), 2
        );
        assertEq(usdc.balanceOf(address(executor)), AMOUNT_USDC * 2, "2x left behind");
        assertGe(usdc.balanceOf(address(this)), AMOUNT_USDC, "owner received the transfer");
    }

    // ── 4. Withdraw native ETH from the executor ─────────────────────────

    function testSendEthSpecificAmount() public {
        uint256 ethAmount = 5 ether;
        vm.deal(address(executor), ethAmount);

        Cmd.Table memory at = _emptyTable();
        uint8 ownerIdx = Cmd.add(at, address(this));

        uint256 ownerBefore = address(this).balance;
        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.sendEth(ownerIdx, ethAmount)), 2);
        assertGt(address(this).balance, ownerBefore, "owner received the ETH");
        assertEq(address(executor).balance, 0, "SEND_ETH moved the exact amount");
    }

    function testSendEthAll() public {
        vm.deal(address(executor), 7 ether);

        Cmd.Table memory at = _emptyTable();
        uint8 ownerIdx = Cmd.add(at, address(this));

        uint256 ownerBefore = address(this).balance;
        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.sendEthAll(ownerIdx)), 2);
        assertLt(address(executor).balance, 1e15, "executor ETH depleted");
        assertGt(address(this).balance, ownerBefore, "owner received the ETH");
    }

    // ── 5. Full round trips ──────────────────────────────────────────────

    /// Swap WETH->USDC, mint as ERC6909, burn + take, xfer-balance to owner,
    /// then SEND_ETH_ALL the remainder.
    function testFullRoundTripUsdcViaErc6909() public {
        weth.mint(address(executor), AMOUNT_WETH);
        uint256 usdcId = uint256(uint160(address(usdc)));
        (PoolKey memory key, bool zfo) = _setupWethUsdcPool();
        WTable memory t = _withdrawalTable();
        Cmd.Table memory at = t.at;
        uint8 pmIdx = t.pmIdx;
        uint8 wethIdx = t.wethIdx;
        uint8 usdcIdx = t.usdcIdx;
        uint8 executorIdx = t.executorIdx;
        uint8 zeroIdx = t.zeroIdx;
        uint8 ownerIdx = t.ownerIdx;

        bytes memory inner = _v4Swap(at, key, zfo, zeroIdx, AMOUNT_WETH);
        inner = bytes.concat(inner, Cmd.v4MintCompact(usdcIdx, executorIdx, AMOUNT_USDC));
        inner = bytes.concat(inner, Cmd.v4Sync(wethIdx));
        inner = bytes.concat(inner, Cmd.erc20Transfer(wethIdx, pmIdx, AMOUNT_WETH));
        inner = bytes.concat(inner, Cmd.v4Settle());
        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)), 2);
        assertEq(v4Pm.balanceOf(address(executor), usdcId), AMOUNT_USDC, "ERC6909 USDC minted");

        bytes memory inner2 = Cmd.v4BurnCompact(usdcIdx, AMOUNT_USDC);
        inner2 = bytes.concat(inner2, Cmd.v4TakeDelta(usdcIdx, executorIdx));
        bytes memory commands = Cmd.v4Unlock(inner2);
        commands = bytes.concat(commands, Cmd.erc20XferBalance(usdcIdx, ownerIdx));
        commands = bytes.concat(commands, Cmd.sendEthAll(ownerIdx));
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);

        assertEq(v4Pm.balanceOf(address(executor), usdcId), 0, "ERC6909 USDC burned");
        assertEq(usdc.balanceOf(address(executor)), 0, "executor holds no USDC");
        assertGe(usdc.balanceOf(address(this)), AMOUNT_USDC, "owner received the USDC");
        assertLt(address(executor).balance, 1 ether, "ETH sent out");
    }

    /// Swap WETH->native ETH, mint ETH as ERC6909, burn + take, SEND_ETH_ALL.
    function testFullRoundTripNativeEthViaErc6909() public {
        weth.mint(address(executor), AMOUNT_WETH);
        (PoolKey memory key, bool zfo) = _setupNativeWethPool();
        WTable memory t = _withdrawalTable();
        Cmd.Table memory at = t.at;
        uint8 wethIdx = t.wethIdx;
        uint8 executorIdx = t.executorIdx;
        uint8 zeroIdx = t.zeroIdx;
        uint8 ownerIdx = t.ownerIdx;
        uint8 nativeIdx = Cmd.add(at, address(0));

        bytes memory inner = _v4Swap(at, key, zfo, zeroIdx, AMOUNT_WETH);
        inner = bytes.concat(inner, Cmd.v4MintCompact(nativeIdx, executorIdx, AMOUNT_WETH));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(wethIdx));
        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)), 2);
        assertEq(v4Pm.balanceOf(address(executor), 0), AMOUNT_WETH, "ERC6909 native ETH minted");

        bytes memory inner2 = Cmd.v4BurnCompact(nativeIdx, AMOUNT_WETH);
        inner2 = bytes.concat(inner2, Cmd.v4TakeDelta(nativeIdx, executorIdx));
        bytes memory commands = bytes.concat(Cmd.v4Unlock(inner2), Cmd.sendEthAll(ownerIdx));
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);

        assertEq(v4Pm.balanceOf(address(executor), 0), 0, "ERC6909 native ETH burned");
        assertLt(address(executor).balance, 1e15, "executor ETH sent out");
    }
}
