// SPDX-License-Identifier: MIT

pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {PoolKey} from "../src/generated/Types.sol";

/// Port of `tests/executor/test_cmd_executor_v4v4_wrap_unwrap.py`.
///
/// Inline WETH wrapping/unwrapping within V4-V4 paths — everything stays
/// inside one V4 unlock: the executor takes tokens out of V4 (V4_TAKE),
/// converts between ETH and WETH (WETH_DEPOSIT / WETH_WITHDRAW, exact and
/// _ALL variants), sells the converted token at the second V4 pool, and
/// settles the remaining deltas.
///
/// Scenarios:
///   A. V4 take WETH → WETH_WITHDRAW → V4 sell ETH (unwrap between V4 legs)
///   B. V4 take ETH  → WETH_DEPOSIT  → V4 sell WETH (wrap between V4 legs)
contract CmdExecutorV4V4WrapUnwrapTest is PoolSetup {
    uint256 internal constant AMOUNT_ETH = 1 ether;

    // ── Shared helpers ───────────────────────────────────────────────────

    /// The source table: weth, usdc, executor, zero, native — no PM entry.
    function _table()
        internal
        view
        returns (Cmd.Table memory at, uint8 wethIdx, uint8 usdcIdx, uint8 executorIdx, uint8 zeroIdx, uint8 nativeIdx)
    {
        at = _emptyTable();
        wethIdx = Cmd.add(at, address(weth));
        usdcIdx = Cmd.add(at, address(usdc));
        executorIdx = Cmd.add(at, address(executor));
        zeroIdx = Cmd.add(at, address(0));
        nativeIdx = Cmd.add(at, address(0)); // the native sentinel
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

    // ── TestV4WethToV4Eth ────────────────────────────────────────────────

    /// V4 (sell USDC, buy WETH) → WETH_WITHDRAW → V4 (sell ETH, buy USDC).
    /// The unwrapped ETH pays the second pool's input; the USDC profit is
    /// taken as a delta.
    function testV4WethToV4EthWithInlineUnwrap() public {
        // Pool A: WETH/USDC (fee 3000, ts 60) — sell USDC, buy WETH.
        PoolKey memory keyA = _poolKey(address(weth), address(usdc), 3000, 60);
        bool zfoA = keyA.currency0 == address(usdc);
        _setupV4Swap(v4Pm, keyA, AMOUNT_USDC, AMOUNT_WETH, zfoA, address(weth));

        // Pool B: ETH/USDC (fee 500, ts 10) — sell ETH, buy USDC.
        PoolKey memory keyB = _poolKey(address(0), address(usdc), 500, 10);
        bool zfoB = keyB.currency0 == address(0);
        _setupV4Swap(v4Pm, keyB, AMOUNT_ETH, AMOUNT_USDC * 2, zfoB, address(usdc));

        (Cmd.Table memory at, uint8 wethIdx, uint8 usdcIdx, uint8 executorIdx, uint8 zeroIdx, uint8 nativeIdx) =
            _table();

        bytes memory inner = _v4Swap(at, keyA, zfoA, zeroIdx, AMOUNT_USDC);
        inner = bytes.concat(inner, Cmd.v4Take(wethIdx, executorIdx, AMOUNT_WETH));
        inner = bytes.concat(inner, Cmd.wethWithdraw(AMOUNT_WETH));
        inner = bytes.concat(inner, _v4Swap(at, keyB, zfoB, zeroIdx, AMOUNT_ETH));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(nativeIdx));
        inner = bytes.concat(inner, Cmd.v4TakeDelta(usdcIdx, executorIdx));

        bytes memory commands = Cmd.v4Unlock(inner);
        _execute(executor, bytes.concat(Cmd.preamble(at), commands));
        assertGe(usdc.balanceOf(address(executor)), AMOUNT_USDC, "USDC profit");
    }

    /// Same path using WETH_WITHDRAW_ALL.
    function testV4WethToV4EthWithInlineUnwrapAll() public {
        PoolKey memory keyA = _poolKey(address(weth), address(usdc), 3000, 60);
        bool zfoA = keyA.currency0 == address(usdc);
        _setupV4Swap(v4Pm, keyA, AMOUNT_USDC, AMOUNT_WETH, zfoA, address(weth));

        PoolKey memory keyB = _poolKey(address(0), address(usdc), 500, 10);
        bool zfoB = keyB.currency0 == address(0);
        _setupV4Swap(v4Pm, keyB, AMOUNT_ETH, AMOUNT_USDC * 2, zfoB, address(usdc));

        (Cmd.Table memory at, uint8 wethIdx, uint8 usdcIdx, uint8 executorIdx, uint8 zeroIdx, uint8 nativeIdx) =
            _table();

        bytes memory inner = _v4Swap(at, keyA, zfoA, zeroIdx, AMOUNT_USDC);
        inner = bytes.concat(inner, Cmd.v4Take(wethIdx, executorIdx, AMOUNT_WETH));
        inner = bytes.concat(inner, Cmd.wethWithdrawAll());
        inner = bytes.concat(inner, _v4Swap(at, keyB, zfoB, zeroIdx, AMOUNT_ETH));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(nativeIdx));
        inner = bytes.concat(inner, Cmd.v4TakeDelta(usdcIdx, executorIdx));

        bytes memory commands = Cmd.v4Unlock(inner);
        _execute(executor, bytes.concat(Cmd.preamble(at), commands));
    }

    // ── TestV4EthToV4Weth ────────────────────────────────────────────────

    /// V4 (sell USDC, buy ETH) → WETH_DEPOSIT → V4 (sell WETH, buy USDC).
    /// The wrapped WETH pays the second pool's input; the USDC profit is
    /// taken as a delta.
    function testV4EthToV4WethWithInlineWrap() public {
        // Pool A: ETH/USDC (fee 3000, ts 60) — sell USDC, buy ETH.
        PoolKey memory keyA = _poolKey(address(0), address(usdc), 3000, 60);
        bool zfoA = keyA.currency0 == address(usdc);
        _setupV4Swap(v4Pm, keyA, AMOUNT_USDC, AMOUNT_ETH, zfoA, address(0));

        // Pool B: WETH/USDC (fee 500, ts 10) — sell WETH, buy USDC.
        PoolKey memory keyB = _poolKey(address(weth), address(usdc), 500, 10);
        bool zfoB = keyB.currency0 == address(weth);
        _setupV4Swap(v4Pm, keyB, AMOUNT_WETH, AMOUNT_USDC * 2, zfoB, address(usdc));

        (Cmd.Table memory at, uint8 wethIdx, uint8 usdcIdx, uint8 executorIdx, uint8 zeroIdx, uint8 nativeIdx) =
            _table();

        bytes memory inner = _v4Swap(at, keyA, zfoA, zeroIdx, AMOUNT_USDC);
        inner = bytes.concat(inner, Cmd.v4Take(nativeIdx, executorIdx, AMOUNT_ETH));
        inner = bytes.concat(inner, Cmd.wethDeposit(AMOUNT_WETH));
        inner = bytes.concat(inner, _v4Swap(at, keyB, zfoB, zeroIdx, AMOUNT_WETH));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(wethIdx));
        inner = bytes.concat(inner, Cmd.v4TakeDelta(usdcIdx, executorIdx));

        bytes memory commands = Cmd.v4Unlock(inner);
        _execute(executor, bytes.concat(Cmd.preamble(at), commands));
        assertGe(usdc.balanceOf(address(executor)), AMOUNT_USDC, "USDC profit");
    }

    /// Same path using WETH_DEPOSIT_ALL.
    function testV4EthToV4WethWithInlineWrapAll() public {
        PoolKey memory keyA = _poolKey(address(0), address(usdc), 3000, 60);
        bool zfoA = keyA.currency0 == address(usdc);
        _setupV4Swap(v4Pm, keyA, AMOUNT_USDC, AMOUNT_ETH, zfoA, address(0));

        PoolKey memory keyB = _poolKey(address(weth), address(usdc), 500, 10);
        bool zfoB = keyB.currency0 == address(weth);
        _setupV4Swap(v4Pm, keyB, AMOUNT_WETH, AMOUNT_USDC * 2, zfoB, address(usdc));

        (Cmd.Table memory at, uint8 wethIdx, uint8 usdcIdx, uint8 executorIdx, uint8 zeroIdx, uint8 nativeIdx) =
            _table();

        bytes memory inner = _v4Swap(at, keyA, zfoA, zeroIdx, AMOUNT_USDC);
        inner = bytes.concat(inner, Cmd.v4Take(nativeIdx, executorIdx, AMOUNT_ETH));
        inner = bytes.concat(inner, Cmd.wethDepositAll());
        inner = bytes.concat(inner, _v4Swap(at, keyB, zfoB, zeroIdx, AMOUNT_WETH));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(wethIdx));
        inner = bytes.concat(inner, Cmd.v4TakeDelta(usdcIdx, executorIdx));

        bytes memory commands = Cmd.v4Unlock(inner);
        _execute(executor, bytes.concat(Cmd.preamble(at), commands));
    }
}
