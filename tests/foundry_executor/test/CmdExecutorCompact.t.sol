// SPDX-License-Identifier: MIT

pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {PoolKey} from "../src/generated/Types.sol";

/// Port of `tests/executor/test_cmd_executor_compact.py`.
///
/// Compact and warm-balance commands under the source's `config = 2` (profit
/// mode 2) on every execution: V4_SWAP_COMPACT legs (uint128 amount + default
/// sqrt limit), the ERC20_XFER_BALANCE warm transfer, V4_TAKE_COMPACT's
/// uint128 amount, and the WETH_DEPOSIT_ALL / WETH_WITHDRAW_ALL pair.
contract CmdExecutorCompactTest is PoolSetup {
    // ── Shared legs ──────────────────────────────────────────────────────

    /// Pool A: WETH/USDC (fee 3000, ts 60) — sell WETH, buy USDC.
    function _setupPoolA() internal returns (PoolKey memory key, bool zfo) {
        key = _poolKey(address(weth), address(usdc), 3000, 60);
        zfo = key.currency0 == address(weth);
        _setupV4Swap(v4Pm, key, AMOUNT_WETH, AMOUNT_USDC, zfo, address(usdc));
    }

    /// Pool B: WETH/USDC (fee 3000, ts 120) — sell USDC, buy WETH.
    function _setupPoolB() internal returns (PoolKey memory key, bool zfo) {
        key = _poolKey(address(weth), address(usdc), 3000, 120);
        zfo = key.currency0 == address(usdc);
        _setupV4Swap(v4Pm, key, AMOUNT_USDC, 2 * AMOUNT_WETH, zfo, address(weth));
    }

    /// Pool B (native leg): ETH/USDC (fee 500, ts 10) — sell USDC, buy ETH.
    function _setupPoolBNative() internal returns (PoolKey memory key, bool zfo) {
        key = _poolKey(address(0), address(usdc), 500, 10);
        zfo = key.currency0 == address(usdc);
        _setupV4Swap(v4Pm, key, AMOUNT_USDC, 2 * AMOUNT_WETH, zfo, address(0));
    }

    /// The source `AddressTable()` carries no sentinel shortcuts: every
    /// address is appended in source order, and the zero address resolves to
    /// the native sentinel without consuming an index.
    function _table()
        internal
        view
        returns (Cmd.Table memory at, uint8 pmIdx, uint8 wethIdx, uint8 usdcIdx, uint8 executorIdx, uint8 zeroIdx)
    {
        at = _emptyTable();
        pmIdx = Cmd.add(at, address(v4Pm));
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

    // ── TestV4SwapCompact ────────────────────────────────────────────────

    /// V4→V4 using V4_SWAP_COMPACT (uint128 amount + default sqrt) on both
    /// legs; the WETH profit is taken with V4_TAKE_DELTA.
    function testV4SwapCompactWethUsdc() public {
        (PoolKey memory keyA, bool zfoA) = _setupPoolA();
        (PoolKey memory keyB, bool zfoB) = _setupPoolB();
        (Cmd.Table memory at,, uint8 wethIdx,, uint8 executorIdx, uint8 zeroIdx) = _table();

        bytes memory inner = _v4Swap(at, keyA, zfoA, zeroIdx, AMOUNT_WETH);
        inner = bytes.concat(inner, _v4Swap(at, keyB, zfoB, zeroIdx, AMOUNT_USDC));
        inner = bytes.concat(inner, Cmd.v4TakeDelta(wethIdx, executorIdx));

        bytes memory commands = Cmd.v4Unlock(inner);
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);
    }

    // ── TestErc20XferBalance ─────────────────────────────────────────────

    /// ERC20_XFER_BALANCE after a V4 take: the executor takes USDC, syncs the
    /// currency, forwards the PM's whole USDC balance with one command, and
    /// settles — then runs the second leg and takes the WETH profit.
    function testXferBalanceAfterV4Take() public {
        (PoolKey memory keyA, bool zfoA) = _setupPoolA();
        (PoolKey memory keyB, bool zfoB) = _setupPoolB();
        (Cmd.Table memory at, uint8 pmIdx, uint8 wethIdx, uint8 usdcIdx, uint8 executorIdx, uint8 zeroIdx) = _table();

        bytes memory inner = _v4Swap(at, keyA, zfoA, zeroIdx, AMOUNT_WETH);
        inner = bytes.concat(inner, Cmd.v4Take(usdcIdx, executorIdx, AMOUNT_USDC));
        inner = bytes.concat(inner, Cmd.v4Sync(usdcIdx));
        inner = bytes.concat(inner, Cmd.erc20XferBalance(usdcIdx, pmIdx));
        inner = bytes.concat(inner, Cmd.v4Settle());
        inner = bytes.concat(inner, _v4Swap(at, keyB, zfoB, zeroIdx, AMOUNT_USDC));
        inner = bytes.concat(inner, Cmd.v4TakeDelta(wethIdx, executorIdx));

        bytes memory commands = Cmd.v4Unlock(inner);
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);
    }

    // ── TestV4TakeCompact ────────────────────────────────────────────────

    /// V4_TAKE_COMPACT: uint128 amount (16 bytes instead of 32). The source
    /// runs with reverts raised and pins the exact USDC take — the fake pool
    /// parameters don't guarantee profit in ETH+WETH terms, hence the fixed
    /// take instead of a take-delta.
    function testV4TakeCompact() public {
        weth.mint(address(executor), AMOUNT_WETH);
        (PoolKey memory key, bool zfo) = _setupPoolA();
        (Cmd.Table memory at,, uint8 wethIdx, uint8 usdcIdx, uint8 executorIdx, uint8 zeroIdx) = _table();

        bytes memory inner = _v4Swap(at, key, zfo, zeroIdx, AMOUNT_WETH);
        inner = bytes.concat(inner, Cmd.v4TakeCompact(usdcIdx, executorIdx, AMOUNT_USDC));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(wethIdx));

        bytes memory commands = Cmd.v4Unlock(inner);
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);
        assertEq(usdc.balanceOf(address(executor)), AMOUNT_USDC, "USDC take compact amount");
    }

    // ── TestWethDepositAll ───────────────────────────────────────────────

    /// WETH_DEPOSIT_ALL wraps all ETH: the executor takes the native ETH
    /// profit out of the PM and wraps it in the same unlock.
    function testDepositAllEth() public {
        weth.mint(address(executor), AMOUNT_WETH);
        (PoolKey memory keyA, bool zfoA) = _setupPoolA();
        (PoolKey memory keyB, bool zfoB) = _setupPoolBNative();
        (Cmd.Table memory at,, uint8 wethIdx,, uint8 executorIdx, uint8 zeroIdx) = _table();
        uint8 nativeIdx = Cmd.add(at, address(0)); // the native sentinel

        bytes memory inner = _v4Swap(at, keyA, zfoA, zeroIdx, AMOUNT_WETH);
        inner = bytes.concat(inner, _v4Swap(at, keyB, zfoB, zeroIdx, AMOUNT_USDC));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(wethIdx));
        inner = bytes.concat(inner, Cmd.v4TakeDelta(nativeIdx, executorIdx));
        inner = bytes.concat(inner, Cmd.wethDepositAll());

        bytes memory commands = Cmd.v4Unlock(inner);
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);
    }

    // ── TestWethWithdrawAll ──────────────────────────────────────────────

    /// WETH_WITHDRAW_ALL unwraps all WETH: after the deposit-all leg the
    /// executor's whole WETH balance ends the execution back as ETH.
    function testWithdrawAllWeth() public {
        weth.mint(address(executor), AMOUNT_WETH);
        (PoolKey memory keyA, bool zfoA) = _setupPoolA();
        (PoolKey memory keyB, bool zfoB) = _setupPoolBNative();
        (Cmd.Table memory at,, uint8 wethIdx,, uint8 executorIdx, uint8 zeroIdx) = _table();
        uint8 nativeIdx = Cmd.add(at, address(0)); // the native sentinel

        bytes memory inner = _v4Swap(at, keyA, zfoA, zeroIdx, AMOUNT_WETH);
        inner = bytes.concat(inner, _v4Swap(at, keyB, zfoB, zeroIdx, AMOUNT_USDC));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(wethIdx));
        inner = bytes.concat(inner, Cmd.v4TakeDelta(nativeIdx, executorIdx));
        inner = bytes.concat(inner, Cmd.wethDepositAll());
        inner = bytes.concat(inner, Cmd.wethWithdrawAll());

        bytes memory commands = Cmd.v4Unlock(inner);
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);
        assertEq(weth.balanceOf(address(executor)), 0, "all WETH should be unwrapped");
    }
}
