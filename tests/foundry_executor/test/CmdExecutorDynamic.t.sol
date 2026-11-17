// SPDX-License-Identifier: MIT

pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {PoolKey} from "../src/generated/Types.sol";

/// Port of `tests/executor/test_cmd_executor_dynamic.py`.
///
/// Dynamic value discovery inside a single V4 unlock: V4_TAKE_DELTA,
/// V4_SETTLE_DELTA, V4_SETTLE_ALL, V4_SWAP_DYNAMIC (the amount read from the
/// PM's exttload delta), and the mixed explicit/dynamic sequences. Every
/// source execution uses the executor's default config (profit mode 1).
contract CmdExecutorDynamicTest is PoolSetup {
    uint256 internal constant POOL_A_IN = 1 ether;
    uint256 internal constant POOL_A_OUT = 2000e6;
    uint256 internal constant POOL_B_IN = 2000e6;
    uint256 internal constant POOL_B_OUT = 2 ether;

    // ── Shared legs ──────────────────────────────────────────────────────

    /// Pool A: WETH/USDC (fee 3000, ts 60) — sell WETH, buy USDC.
    function _setupPoolA() internal returns (PoolKey memory key, bool zfo) {
        key = _poolKey(address(weth), address(usdc), 3000, 60);
        zfo = key.currency0 == address(weth);
        _setupV4Swap(v4Pm, key, POOL_A_IN, POOL_A_OUT, zfo, address(usdc));
    }

    /// Pool B: WETH/USDC (fee 3000, ts 120) — sell USDC, buy WETH.
    function _setupPoolB() internal returns (PoolKey memory key, bool zfo) {
        key = _poolKey(address(weth), address(usdc), 3000, 120);
        zfo = key.currency0 == address(usdc);
        _setupV4Swap(v4Pm, key, POOL_B_IN, POOL_B_OUT, zfo, address(weth));
    }

    /// Pool B (native leg): ETH/USDC (fee 500, ts 10) — sell USDC, buy ETH.
    function _setupPoolBNative() internal returns (PoolKey memory key, bool zfo) {
        key = _poolKey(address(0), address(usdc), 500, 10);
        zfo = key.currency0 == address(usdc);
        _setupV4Swap(v4Pm, key, POOL_B_IN, POOL_B_OUT, zfo, address(0));
    }

    /// The source `AddressTable()` — every address appended in source order;
    /// the zero address resolves to the native sentinel without an index.
    function _table()
        internal
        view
        returns (Cmd.Table memory at, uint8 wethIdx, uint8 usdcIdx, uint8 executorIdx, uint8 zeroIdx, uint8 nativeIdx)
    {
        at = _emptyTable();
        Cmd.add(at, address(v4Pm));
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

    // ── TestV4TakeDelta ──────────────────────────────────────────────────

    /// V4→V4 same-currency using V4_TAKE_DELTA instead of V4_TAKE: the whole
    /// positive WETH delta (pool B's output minus pool A's input) lands on
    /// the executor.
    function testV4TakeDeltaWethProfit() public {
        (PoolKey memory keyA, bool zfoA) = _setupPoolA();
        (PoolKey memory keyB, bool zfoB) = _setupPoolB();
        (Cmd.Table memory at, uint8 wethIdx,,, uint8 zeroIdx,) = _table();
        uint8 executorIdx = Cmd.add(at, address(executor));

        bytes memory inner = _v4Swap(at, keyA, zfoA, zeroIdx, POOL_A_IN);
        inner = bytes.concat(inner, _v4Swap(at, keyB, zfoB, zeroIdx, POOL_B_IN));
        inner = bytes.concat(inner, Cmd.v4TakeDelta(wethIdx, executorIdx));

        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)));
    }

    // ── TestV4SettleDelta ────────────────────────────────────────────────

    /// V4→V4 cross-currency using V4_SETTLE_DELTA: pool A's WETH debit is
    /// covered by pool B's WETH output inside the PM's transient deltas.
    function testV4SettleDeltaWeth() public {
        (PoolKey memory keyA, bool zfoA) = _setupPoolA();
        (PoolKey memory keyB, bool zfoB) = _setupPoolB();
        (Cmd.Table memory at, uint8 wethIdx,,, uint8 zeroIdx,) = _table();
        uint8 executorIdx = Cmd.add(at, address(executor));

        bytes memory inner = _v4Swap(at, keyA, zfoA, zeroIdx, POOL_A_IN);
        inner = bytes.concat(inner, _v4Swap(at, keyB, zfoB, zeroIdx, POOL_B_IN));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(wethIdx));

        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)));
    }

    // ── TestV4SettleAll ──────────────────────────────────────────────────

    /// V4→V4 cross-currency (WETH in, native ETH out) using V4_SETTLE_ALL:
    /// one command settles every remaining PM delta.
    function testV4SettleAllV4v4CrossCurrency() public {
        (PoolKey memory keyA, bool zfoA) = _setupPoolA();
        (PoolKey memory keyB, bool zfoB) = _setupPoolBNative();
        (Cmd.Table memory at,,,, uint8 zeroIdx,) = _table();

        bytes memory inner = _v4Swap(at, keyA, zfoA, zeroIdx, POOL_A_IN);
        inner = bytes.concat(inner, _v4Swap(at, keyB, zfoB, zeroIdx, POOL_B_IN));
        inner = bytes.concat(inner, Cmd.v4SettleAll());

        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)));
    }

    // ── TestV4SwapDynamic ────────────────────────────────────────────────

    /// V4→V4 using V4_SWAP_DYNAMIC for the second swap: its amount comes from
    /// the PM's exttload delta of the first swap instead of the stream.
    function testV4SwapDynamicV4v4() public {
        (PoolKey memory keyA, bool zfoA) = _setupPoolA();
        (PoolKey memory keyB, bool zfoB) = _setupPoolB();
        (Cmd.Table memory at, uint8 wethIdx,,, uint8 zeroIdx,) = _table();
        uint8 executorIdx = Cmd.add(at, address(executor));

        // First swap: explicit amount.
        bytes memory inner = _v4Swap(at, keyA, zfoA, zeroIdx, POOL_A_IN);
        // Second swap: dynamic amount from exttload.
        inner = bytes.concat(
            inner,
            Cmd.v4SwapDynamic(
                Cmd.add(at, keyB.currency0),
                Cmd.add(at, keyB.currency1),
                uint16(keyB.fee),
                int16(keyB.tick_spacing),
                zeroIdx,
                zfoB
            )
        );
        inner = bytes.concat(inner, Cmd.v4TakeDelta(wethIdx, executorIdx));

        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)));
    }

    // ── TestMixedExplicitDynamic ─────────────────────────────────────────

    /// Mixing explicit and dynamic commands: both legs explicit, the profit
    /// taken dynamically with V4_TAKE_DELTA.
    function testMixedV4TakeAndTakeDelta() public {
        (PoolKey memory keyA, bool zfoA) = _setupPoolA();
        (PoolKey memory keyB, bool zfoB) = _setupPoolB();
        (Cmd.Table memory at, uint8 wethIdx,,, uint8 zeroIdx,) = _table();
        uint8 executorIdx = Cmd.add(at, address(executor));

        bytes memory inner = _v4Swap(at, keyA, zfoA, zeroIdx, POOL_A_IN);
        inner = bytes.concat(inner, _v4Swap(at, keyB, zfoB, zeroIdx, POOL_B_IN));
        inner = bytes.concat(inner, Cmd.v4TakeDelta(wethIdx, executorIdx));

        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)));
    }

    /// V4_SETTLE_DELTA for WETH, then V4_SETTLE_ALL for the rest: the WETH
    /// leg is pre-funded by the executor's own balance, the native leg nets
    /// out with settle-all.
    function testSettleThenSettleAll() public {
        weth.mint(address(executor), 1 ether);
        (PoolKey memory keyA, bool zfoA) = _setupPoolA();
        (PoolKey memory keyB, bool zfoB) = _setupPoolBNative();
        (Cmd.Table memory at, uint8 wethIdx,,, uint8 zeroIdx, uint8 nativeIdx) = _table();
        Cmd.add(at, address(executor));

        bytes memory inner = _v4Swap(at, keyA, zfoA, zeroIdx, POOL_A_IN);
        inner = bytes.concat(inner, _v4Swap(at, keyB, zfoB, zeroIdx, POOL_B_IN));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(wethIdx));
        inner = bytes.concat(inner, Cmd.v4SettleAll());

        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)));
    }
}
