// SPDX-License-Identifier: MIT

pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {IFakeUniswapV2Pair} from "../src/generated/IFakeUniswapV2Pair.sol";
import {PoolKey} from "../src/generated/Types.sol";

/// Port of `tests/executor/test_cmd_executor_v4v2.py`.
///
/// Cross-protocol V4↔V2 legs: V4 swap → take → V2 swap with an explicit
/// callback payment (V2_SWAP_COMPACT + forward_data), the V2-first shape
/// (outer V2 swap whose callback opens the V4 unlock), and the direct-custody
/// V2_SWAP_CALC form where V4_TAKE funds the pair's excess balance.
contract CmdExecutorV4V2Test is PoolSetup {
    uint256 internal constant V2_LIQUIDITY_WETH = 100 ether;
    uint256 internal constant V2_LIQUIDITY_USDC = 200_000e6;

    IFakeUniswapV2Pair internal v2Pair; // WETH/USDC, callback variant 0, fee 30

    function setUp() public override {
        super.setUp();
        (address token0, address token1) = _sorted(address(usdc), address(weth));
        v2Pair = _deployV2Pair(token0, token1, 0, V2_FEE);
    }

    // ── Shared helpers ───────────────────────────────────────────────────

    /// Pool A: WETH/USDC (fee 3000, ts 60) — sell WETH, buy USDC.
    function _setupPoolA() internal returns (PoolKey memory key, bool zfo) {
        key = _poolKey(address(weth), address(usdc), 3000, 60);
        zfo = key.currency0 == address(weth);
        _setupV4Swap(v4Pm, key, AMOUNT_WETH, AMOUNT_USDC, zfo, address(usdc));
    }

    /// The source `AddressTable()` — every address appended in source order
    /// (pm, weth, usdc, executor, v2, zero). Indices resolve idempotently via
    /// `Cmd.add` at each use site.
    function _table() internal view returns (Cmd.Table memory at) {
        at = _emptyTable();
        Cmd.add(at, address(v4Pm));
        Cmd.add(at, address(weth));
        Cmd.add(at, address(usdc));
        Cmd.add(at, address(executor));
        Cmd.add(at, address(v2Pair));
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

    // ── TestV4ToV2 ───────────────────────────────────────────────────────

    /// V4→V2: V4 swap, take USDC, V2 swap with explicit callback payment —
    /// the USDC the V2 pair needs is forwarded inside the callback's
    /// forward_data, so the executor's combined balance drops by the paid
    /// input.
    function testV4V2UsdcToWeth() public {
        weth.mint(address(executor), AMOUNT_WETH);
        (PoolKey memory keyA, bool zfoA) = _setupPoolA();

        // Set up V2 pair with ample liquidity for K-invariant.
        usdc.mint(address(v2Pair), V2_LIQUIDITY_USDC);
        weth.mint(address(v2Pair), V2_LIQUIDITY_WETH);
        v2Pair.sync();

        bool v2Zfo = v2Pair.token0() == address(usdc);
        uint256 v2Out =
            _v2GetAmountOut(AMOUNT_USDC, usdc.balanceOf(address(v2Pair)), weth.balanceOf(address(v2Pair)), V2_FEE);

        Cmd.Table memory at = _table();

        bytes memory inner = _v4Swap(at, keyA, zfoA, Cmd.add(at, address(0)), AMOUNT_WETH);
        inner = bytes.concat(inner, Cmd.v4Take(Cmd.add(at, address(usdc)), Cmd.add(at, address(executor)), AMOUNT_USDC));

        // V2_SWAP_COMPACT carries a uint128 amount_out.
        bytes memory v2Callback =
            Cmd.erc20Transfer(Cmd.add(at, address(usdc)), Cmd.add(at, address(v2Pair)), AMOUNT_USDC);
        bytes memory v2Swap = Cmd.v2SwapCompact(
            Cmd.add(at, address(v2Pair)), v2Zfo, v2Out, Cmd.add(at, address(executor)), V2_FEE, v2Callback
        );
        inner = bytes.concat(inner, v2Swap);
        inner = bytes.concat(inner, Cmd.v4SettleDelta(Cmd.add(at, address(weth))));

        bytes memory commands = Cmd.v4Unlock(inner);
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);
    }

    // ── TestV2ToV4 ───────────────────────────────────────────────────────

    /// V2→V4: V2 swap first (its callback opens the V4 unlock), then
    /// sync + transfer + settle pre-funds the PM before the V4 swap.
    function testV2V4WethToUsdc() public {
        weth.mint(address(executor), AMOUNT_WETH);
        weth.mint(address(v2Pair), V2_LIQUIDITY_WETH);
        usdc.mint(address(v2Pair), V2_LIQUIDITY_USDC);
        v2Pair.sync();

        bool v2Zfo = v2Pair.token0() == address(weth);
        uint256 v2Out =
            _v2GetAmountOut(AMOUNT_WETH, weth.balanceOf(address(v2Pair)), usdc.balanceOf(address(v2Pair)), V2_FEE);

        PoolKey memory keyB = _poolKey(address(weth), address(usdc), 3000, 60);
        bool zfoB = keyB.currency0 == address(usdc);
        _setupV4Swap(v4Pm, keyB, v2Out, 2 * AMOUNT_WETH, zfoB, address(weth));

        Cmd.Table memory at = _table();

        bytes memory v2Callback =
            Cmd.erc20Transfer(Cmd.add(at, address(weth)), Cmd.add(at, address(v2Pair)), AMOUNT_WETH);
        bytes memory outer = Cmd.v2SwapCompact(
            Cmd.add(at, address(v2Pair)), v2Zfo, v2Out, Cmd.add(at, address(executor)), V2_FEE, v2Callback
        );

        bytes memory inner = Cmd.v4Sync(Cmd.add(at, address(usdc)));
        inner = bytes.concat(inner, Cmd.erc20Transfer(Cmd.add(at, address(usdc)), Cmd.add(at, address(v4Pm)), v2Out));
        inner = bytes.concat(inner, Cmd.v4Settle());
        bytes memory swap = _v4Swap(at, keyB, zfoB, Cmd.add(at, address(0)), v2Out);
        inner = bytes.concat(inner, swap);
        inner = bytes.concat(
            inner, Cmd.v4Take(Cmd.add(at, address(weth)), Cmd.add(at, address(executor)), 2 * AMOUNT_WETH)
        );

        outer = bytes.concat(outer, Cmd.v4Unlock(inner));
        bytes memory payload = bytes.concat(Cmd.preamble(at), outer);
        _execute(executor, payload);
    }

    // ── TestV4ToV2SwapCalc ───────────────────────────────────────────────

    /// V4→V2 using V2_SWAP_CALC with excess balance: V4_TAKE sends USDC
    /// directly to the V2 pair (direct custody), and V2_SWAP_CALC reads the
    /// excess balance, computes the output on-chain, and swaps with no
    /// callback.
    function testV4V2SwapCalc() public {
        weth.mint(address(executor), AMOUNT_WETH);
        (PoolKey memory keyA, bool zfoA) = _setupPoolA();

        // Add liquidity to V2 pair and initialize reserves.
        usdc.mint(address(v2Pair), 10_000e6);
        weth.mint(address(v2Pair), 5 ether);
        v2Pair.sync();

        // V4→V2 path: selling USDC to V2, buying WETH from V2.
        bool v2Zfo = v2Pair.token0() == address(usdc);

        Cmd.Table memory at = _table();

        bytes memory inner = _v4Swap(at, keyA, zfoA, Cmd.add(at, address(0)), AMOUNT_WETH);
        // V4_TAKE sends USDC directly to the V2 pair (creates excess balance).
        inner = bytes.concat(inner, Cmd.v4Take(Cmd.add(at, address(usdc)), Cmd.add(at, address(v2Pair)), AMOUNT_USDC));
        // V2_SWAP_CALC reads the excess balance, computes the output, no callback.
        bytes memory calc = Cmd.v2SwapCalc(Cmd.add(at, address(v2Pair)), v2Zfo, Cmd.add(at, address(executor)), V2_FEE);
        inner = bytes.concat(inner, calc);
        inner = bytes.concat(inner, Cmd.v4SettleDelta(Cmd.add(at, address(weth))));

        bytes memory commands = Cmd.v4Unlock(inner);
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);
    }
}
