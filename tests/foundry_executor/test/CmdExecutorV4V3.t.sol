// SPDX-License-Identifier: MIT

pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {IFakeErc20} from "../src/generated/IFakeErc20.sol";
import {IFakeUniswapV3Pool} from "../src/generated/IFakeUniswapV3Pool.sol";
import {PoolKey} from "../src/generated/Types.sol";

/// Port of `tests/executor/test_cmd_executor_v4v3.py`.
///
/// Cross-protocol V4↔V3 legs: V4 swap → take → V3 swap (explicit forward_data
/// that pays USDC and pre-settles WETH), the V4→V3 auto-pay form, and the
/// V3-first shape whose auto-pay callback opens the V4 unlock.
contract CmdExecutorV4V3Test is PoolSetup {
    IFakeUniswapV3Pool internal v3Pool; // WETH/USDC, callback variant 0, fee 3000

    function setUp() public override {
        super.setUp();
        (address token0, address token1) = _sorted(address(usdc), address(weth));
        v3Pool = _deployV3Pool(token0, token1, 0, 3000);
    }

    // ── Shared helpers ───────────────────────────────────────────────────

    /// Pool A: WETH/USDC (fee 3000, ts 60) — sell WETH, buy USDC.
    function _setupPoolA() internal returns (PoolKey memory key, bool zfo) {
        key = _poolKey(address(weth), address(usdc), 3000, 60);
        zfo = key.currency0 == address(weth);
        _setupV4Swap(v4Pm, key, AMOUNT_WETH, AMOUNT_USDC, zfo, address(usdc));
    }

    /// The source `AddressTable()` — every address appended in source order
    /// (pm, weth, usdc, executor, v3, zero). Indices resolve idempotently via
    /// `Cmd.add` at each use site.
    function _table() internal view returns (Cmd.Table memory at) {
        at = _emptyTable();
        Cmd.add(at, address(v4Pm));
        Cmd.add(at, address(weth));
        Cmd.add(at, address(usdc));
        Cmd.add(at, address(executor));
        Cmd.add(at, address(v3Pool));
        Cmd.add(at, address(0));
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

    // ── TestV4ToV3 ───────────────────────────────────────────────────────

    /// V4→V3 with explicit forward_data for the V3 callback: pay USDC to the
    /// V3 pool, then sync + transfer + settle WETH to the PM inside the
    /// callback (the executor's pre-minted WETH covers the 0.3% V3 fee gap).
    function testV4V3UsdcToWeth() public {
        // The executor needs starting WETH to cover the V4 settlement
        // (the V3 output is slightly less than AMOUNT_WETH due to the 0.3% fee).
        weth.mint(address(executor), AMOUNT_WETH);
        (PoolKey memory keyA, bool zfoA) = _setupPoolA();

        (bool v3Zfo,) = _setupV3(v3Pool, usdc, IFakeErc20(address(weth)), AMOUNT_USDC, AMOUNT_WETH);

        Cmd.Table memory at = _table();

        // V3 callback forward_data: pay USDC to the V3 pool, then sync +
        // transfer + settle WETH to the PM.
        bytes memory v3Callback =
            Cmd.erc20Transfer(Cmd.add(at, address(usdc)), Cmd.add(at, address(v3Pool)), AMOUNT_USDC);
        bytes memory sync = Cmd.v4Sync(Cmd.add(at, address(weth)));
        v3Callback = bytes.concat(v3Callback, sync);
        bytes memory payWeth = Cmd.erc20Transfer(Cmd.add(at, address(weth)), Cmd.add(at, address(v4Pm)), AMOUNT_WETH);
        v3Callback = bytes.concat(v3Callback, payWeth);
        v3Callback = bytes.concat(v3Callback, Cmd.v4Settle());

        bytes memory inner = _v4Swap(at, keyA, zfoA, Cmd.add(at, address(0)), AMOUNT_WETH);
        inner = bytes.concat(inner, Cmd.v4Take(Cmd.add(at, address(usdc)), Cmd.add(at, address(executor)), AMOUNT_USDC));
        bytes memory v3Swap = Cmd.v3SwapCompact(
            Cmd.add(at, address(v3Pool)), v3Zfo, AMOUNT_USDC, Cmd.add(at, address(executor)), v3Callback
        );
        inner = bytes.concat(inner, v3Swap);

        bytes memory commands = Cmd.v4Unlock(inner);
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);
    }

    // ── TestV4ToV3AutoPay ────────────────────────────────────────────────

    /// V4→V3 with auto-pay — empty forward_data: the executor pays the V3
    /// pool from the USDC taken out of the PM one command earlier, and the
    /// WETH owed to the PM settles after the swap.
    function testV4V3AutoPayUsdcToWeth() public {
        weth.mint(address(executor), AMOUNT_WETH);
        (PoolKey memory keyA, bool zfoA) = _setupPoolA();

        (bool v3Zfo,) = _setupV3(v3Pool, usdc, IFakeErc20(address(weth)), AMOUNT_USDC, AMOUNT_WETH);

        Cmd.Table memory at = _table();

        bytes memory inner = _v4Swap(at, keyA, zfoA, Cmd.add(at, address(0)), AMOUNT_WETH);
        bytes memory take = Cmd.v4Take(Cmd.add(at, address(usdc)), Cmd.add(at, address(executor)), AMOUNT_USDC);
        inner = bytes.concat(inner, take);
        // Auto-pay: no forward_data.
        bytes memory v3Swap =
            Cmd.v3SwapCompact(Cmd.add(at, address(v3Pool)), v3Zfo, AMOUNT_USDC, Cmd.add(at, address(executor)), "");
        inner = bytes.concat(inner, v3Swap);
        bytes memory settle = Cmd.v4SettleDelta(Cmd.add(at, address(weth)));
        inner = bytes.concat(inner, settle);

        bytes memory commands = Cmd.v4Unlock(inner);
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);
    }

    // ── TestV3ToV4AutoPay ────────────────────────────────────────────────

    /// V3→V4 with auto-pay in the V3 callback: the outer V3 swap's callback
    /// opens the V4 unlock — sync + transfer + settle pre-funds the PM, then
    /// the V4 swap takes the WETH profit as a delta.
    function testV3V4AutoPayWethToUsdc() public {
        weth.mint(address(executor), AMOUNT_WETH);

        (bool v3Zfo, uint256 v3UsdcOut) = _setupV3(v3Pool, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);

        PoolKey memory keyB = _poolKey(address(weth), address(usdc), 3000, 60);
        bool zfoB = keyB.currency0 == address(usdc);
        _setupV4Swap(v4Pm, keyB, v3UsdcOut, 2 * AMOUNT_WETH, zfoB, address(weth));

        Cmd.Table memory at = _table();

        // Outer: V3 swap (auto-pay callback).
        bytes memory outer =
            Cmd.v3SwapCompact(Cmd.add(at, address(v3Pool)), v3Zfo, AMOUNT_WETH, Cmd.add(at, address(executor)), "");

        // Inner (inside unlock): sync + transfer + settle (pre-settle for the
        // V4 swap).
        bytes memory inner = Cmd.v4Sync(Cmd.add(at, address(usdc)));
        inner =
            bytes.concat(inner, Cmd.erc20Transfer(Cmd.add(at, address(usdc)), Cmd.add(at, address(v4Pm)), v3UsdcOut));
        inner = bytes.concat(inner, Cmd.v4Settle());
        bytes memory swap = _v4Swap(at, keyB, zfoB, Cmd.add(at, address(0)), v3UsdcOut);
        inner = bytes.concat(inner, swap);
        bytes memory takeDelta = Cmd.v4TakeDelta(Cmd.add(at, address(weth)), Cmd.add(at, address(executor)));
        inner = bytes.concat(inner, takeDelta);

        outer = bytes.concat(outer, Cmd.v4Unlock(inner));
        bytes memory payload = bytes.concat(Cmd.preamble(at), outer);
        _execute(executor, payload);
    }
}
