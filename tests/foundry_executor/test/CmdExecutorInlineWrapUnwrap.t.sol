// SPDX-License-Identifier: MIT

pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {IFakeErc20} from "../src/generated/IFakeErc20.sol";
import {IFakeUniswapV2Pair} from "../src/generated/IFakeUniswapV2Pair.sol";
import {IFakeUniswapV3Pool} from "../src/generated/IFakeUniswapV3Pool.sol";
import {PoolKey} from "../src/generated/Types.sol";

/// Port of `tests/executor/test_cmd_executor_inline_wrap_unwrap.py`.
///
/// Inline WETH wrapping/unwrapping in cross-protocol arbitrage: one leg uses
/// native ETH (a V4 pool over NATIVE_ADDRESS) and the other uses WETH (V2 or
/// V3), so a WETH_DEPOSIT / WETH_WITHDRAW command bridges the token
/// representation gap mid-path (exact-amount and _ALL variants).
///
/// Both fake V2 and V3 pools transfer output tokens BEFORE invoking the
/// callback, so the executor holds the output tokens when the callback's
/// forward_data executes.
///
/// Scenarios:
///   A. V4 outputs ETH → WETH_DEPOSIT → V2 consumes WETH (wrap between legs)
///   B. V4 outputs ETH → WETH_DEPOSIT → V3 consumes WETH (wrap between legs)
///   C. V2 outputs WETH → WETH_WITHDRAW → V4 consumes ETH (unwrap between legs)
///   D. V3 outputs WETH → WETH_WITHDRAW → V4 consumes ETH (unwrap between legs)
contract CmdExecutorInlineWrapUnwrapTest is PoolSetup {
    uint256 internal constant V2_LIQUIDITY_WETH = 100 ether;
    uint256 internal constant V2_LIQUIDITY_USDC = 200_000e6;
    uint256 internal constant AMOUNT_ETH = 1 ether;

    IFakeUniswapV2Pair internal v2Pair; // WETH/USDC, callback variant 0, fee 30
    IFakeUniswapV3Pool internal v3Pool; // WETH/USDC, callback variant 0, fee 3000

    function setUp() public override {
        super.setUp();
        (address token0, address token1) = _sorted(address(usdc), address(weth));
        v2Pair = _deployV2Pair(token0, token1, 0, V2_FEE);
        v3Pool = _deployV3Pool(token0, token1, 0, 3000);
    }

    // ── Shared helpers ───────────────────────────────────────────────────

    /// The source table: weth, usdc, executor, v2/v3, zero, native. Indices
    /// resolve idempotently via `Cmd.add` at each use site.
    function _table(address pool) internal view returns (Cmd.Table memory at) {
        at = _emptyTable();
        Cmd.add(at, address(weth));
        Cmd.add(at, address(usdc));
        Cmd.add(at, address(executor));
        Cmd.add(at, pool);
        Cmd.add(at, address(0));
        Cmd.add(at, address(0)); // the native sentinel
    }

    /// Fund the pair and return (zfo, amountOut) for selling `amountIn`.
    function _setupV2Sell(IFakeErc20 inputToken, IFakeErc20 outputToken, uint256 amountIn)
        internal
        returns (bool zfo, uint256 amountOut)
    {
        weth.mint(address(v2Pair), V2_LIQUIDITY_WETH);
        usdc.mint(address(v2Pair), V2_LIQUIDITY_USDC);
        v2Pair.sync();

        zfo = v2Pair.token0() == address(inputToken);
        amountOut = _v2GetAmountOut(
            amountIn, inputToken.balanceOf(address(v2Pair)), outputToken.balanceOf(address(v2Pair)), V2_FEE
        );
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

    // ── A. TestV4ToV2InlineWrap: V4 (ETH out) → WETH_DEPOSIT → V2 ────────

    /// V4 (sell USDC, buy ETH) → WETH_DEPOSIT → V2 (sell WETH, buy USDC).
    /// V4's USDC demand is sized to V2's net output, so the arb profit is in
    /// ETH/WETH, not USDC.
    function testV4EthToV2WethWithInlineWrap() public {
        // ── V2 pair: WETH/USDC — sell WETH, buy USDC.
        (bool v2Zfo, uint256 v2Out) = _setupV2Sell(IFakeErc20(address(weth)), usdc, AMOUNT_WETH);

        // ── V4 pool: ETH/USDC — sell USDC, buy ETH (size USDC demand to V2's
        // net output; the PM sends native ETH).
        PoolKey memory keyV4 = _poolKey(address(0), address(usdc), 3000, 60);
        bool v4Zfo = keyV4.currency0 == address(usdc); // false: sell USDC
        _setupV4Swap(v4Pm, keyV4, v2Out, AMOUNT_ETH, v4Zfo, address(0));

        Cmd.Table memory at = _table(address(v2Pair));

        // V2 callback: pay WETH to the V2 pair.
        bytes memory v2Callback =
            Cmd.erc20Transfer(Cmd.add(at, address(weth)), Cmd.add(at, address(v2Pair)), AMOUNT_WETH);

        bytes memory inner = _v4Swap(at, keyV4, v4Zfo, Cmd.add(at, address(0)), v2Out);
        // V4 take native ETH → the executor holds raw ETH.
        bytes memory takeEth = Cmd.v4Take(Cmd.add(at, address(0)), Cmd.add(at, address(executor)), AMOUNT_ETH);
        inner = bytes.concat(inner, takeEth);
        // ── Inline wrap: ETH → WETH.
        bytes memory deposit = Cmd.wethDeposit(AMOUNT_WETH);
        inner = bytes.concat(inner, deposit);
        // V2 swap: sell WETH for USDC (callback pays the just-wrapped WETH).
        bytes memory v2Swap = Cmd.v2SwapCompact(
            Cmd.add(at, address(v2Pair)), v2Zfo, v2Out, Cmd.add(at, address(executor)), V2_FEE, v2Callback
        );
        inner = bytes.concat(inner, v2Swap);
        // V4 settle USDC delta (negative: owed to the PM, paid from V2 output).
        bytes memory settle = Cmd.v4SettleDelta(Cmd.add(at, address(usdc)));
        inner = bytes.concat(inner, settle);

        bytes memory commands = Cmd.v4Unlock(inner);
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);
    }

    /// Same path using WETH_DEPOSIT_ALL (wraps the entire ETH balance).
    function testV4EthToV2WethWithInlineWrapAll() public {
        (bool v2Zfo, uint256 v2Out) = _setupV2Sell(IFakeErc20(address(weth)), usdc, AMOUNT_WETH);

        PoolKey memory keyV4 = _poolKey(address(0), address(usdc), 3000, 60);
        bool v4Zfo = keyV4.currency0 == address(usdc);
        _setupV4Swap(v4Pm, keyV4, v2Out, AMOUNT_ETH, v4Zfo, address(0));

        Cmd.Table memory at = _table(address(v2Pair));

        bytes memory v2Callback =
            Cmd.erc20Transfer(Cmd.add(at, address(weth)), Cmd.add(at, address(v2Pair)), AMOUNT_WETH);

        bytes memory inner = _v4Swap(at, keyV4, v4Zfo, Cmd.add(at, address(0)), v2Out);
        bytes memory takeEth = Cmd.v4Take(Cmd.add(at, address(0)), Cmd.add(at, address(executor)), AMOUNT_ETH);
        inner = bytes.concat(inner, takeEth);
        // ── Inline wrap ALL: ETH → WETH.
        bytes memory deposit = Cmd.wethDepositAll();
        inner = bytes.concat(inner, deposit);
        bytes memory v2Swap = Cmd.v2SwapCompact(
            Cmd.add(at, address(v2Pair)), v2Zfo, v2Out, Cmd.add(at, address(executor)), V2_FEE, v2Callback
        );
        inner = bytes.concat(inner, v2Swap);
        bytes memory settle = Cmd.v4SettleDelta(Cmd.add(at, address(usdc)));
        inner = bytes.concat(inner, settle);

        bytes memory commands = Cmd.v4Unlock(inner);
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);
    }

    // ── B. TestV4ToV3InlineWrap: V4 (ETH out) → WETH_DEPOSIT → V3 ────────

    /// V4 (sell USDC, buy ETH) → WETH_DEPOSIT → V3 (sell WETH, buy USDC,
    /// auto-pay). The V3 output is canned and sized to produce more USDC than
    /// V4 demands, leaving USDC profit on the executor.
    function testV4EthToV3WethWithInlineWrap() public {
        // ── V4 pool: ETH/USDC — sell USDC, buy ETH.
        PoolKey memory keyV4 = _poolKey(address(0), address(usdc), 3000, 60);
        bool v4Zfo = keyV4.currency0 == address(usdc);
        _setupV4Swap(v4Pm, keyV4, AMOUNT_USDC, AMOUNT_ETH, v4Zfo, address(0));

        // ── V3 pool: WETH/USDC — sell WETH, buy USDC (more USDC out than V4
        // demands → profit).
        (bool v3Zfo,) = _setupV3(v3Pool, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC * 2);

        Cmd.Table memory at = _table(address(v3Pool));

        bytes memory inner = _v4Swap(at, keyV4, v4Zfo, Cmd.add(at, address(0)), AMOUNT_USDC);
        // V4 take native ETH.
        bytes memory takeEth = Cmd.v4Take(Cmd.add(at, address(0)), Cmd.add(at, address(executor)), AMOUNT_ETH);
        inner = bytes.concat(inner, takeEth);
        // ── Inline wrap: ETH → WETH.
        bytes memory deposit = Cmd.wethDeposit(AMOUNT_WETH);
        inner = bytes.concat(inner, deposit);
        // V3 swap: sell WETH for USDC (auto-pay uses the just-wrapped WETH).
        bytes memory v3Swap =
            Cmd.v3SwapCompact(Cmd.add(at, address(v3Pool)), v3Zfo, AMOUNT_WETH, Cmd.add(at, address(executor)), "");
        inner = bytes.concat(inner, v3Swap);
        // V4 settle USDC (from the V3 output).
        bytes memory settle = Cmd.v4SettleDelta(Cmd.add(at, address(usdc)));
        inner = bytes.concat(inner, settle);

        bytes memory commands = Cmd.v4Unlock(inner);
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);

        // The executor keeps the USDC profit (V3 gave ~AMOUNT_USDC*2 after the
        // 0.3% fee; V4 took AMOUNT_USDC).
        assertGt(usdc.balanceOf(address(executor)), 0, "USDC profit");
    }

    /// Same path using WETH_DEPOSIT_ALL.
    function testV4EthToV3WethWithInlineWrapAll() public {
        PoolKey memory keyV4 = _poolKey(address(0), address(usdc), 3000, 60);
        bool v4Zfo = keyV4.currency0 == address(usdc);
        _setupV4Swap(v4Pm, keyV4, AMOUNT_USDC, AMOUNT_ETH, v4Zfo, address(0));

        (bool v3Zfo,) = _setupV3(v3Pool, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC * 2);

        Cmd.Table memory at = _table(address(v3Pool));

        bytes memory inner = _v4Swap(at, keyV4, v4Zfo, Cmd.add(at, address(0)), AMOUNT_USDC);
        bytes memory takeEth = Cmd.v4Take(Cmd.add(at, address(0)), Cmd.add(at, address(executor)), AMOUNT_ETH);
        inner = bytes.concat(inner, takeEth);
        bytes memory deposit = Cmd.wethDepositAll();
        inner = bytes.concat(inner, deposit);
        bytes memory v3Swap =
            Cmd.v3SwapCompact(Cmd.add(at, address(v3Pool)), v3Zfo, AMOUNT_WETH, Cmd.add(at, address(executor)), "");
        inner = bytes.concat(inner, v3Swap);
        bytes memory settle = Cmd.v4SettleDelta(Cmd.add(at, address(usdc)));
        inner = bytes.concat(inner, settle);

        bytes memory commands = Cmd.v4Unlock(inner);
        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);

        assertGt(usdc.balanceOf(address(executor)), 0, "USDC profit");
    }

    // ── C. TestV2ToV4InlineUnwrap: V2 (WETH out) → WETH_WITHDRAW → V4 ────

    /// V2 (sell USDC, buy WETH) → WETH_WITHDRAW → V4 (sell ETH, buy USDC).
    /// V2 flash-sends WETH; the callback unwraps it, runs the whole V4 leg,
    /// and pays the USDC back to the V2 pair.
    function testV2WethToV4EthWithInlineUnwrap() public {
        weth.mint(address(executor), AMOUNT_WETH);
        // ── V2 pair: WETH/USDC — sell USDC, buy WETH.
        (bool v2Zfo, uint256 v2Out) = _setupV2Sell(usdc, IFakeErc20(address(weth)), AMOUNT_USDC);

        // ── V4 pool: ETH/USDC — sell ETH, buy USDC (more USDC out than V2
        // demands; the arb profit is the WETH/ETH difference).
        PoolKey memory keyV4 = _poolKey(address(0), address(usdc), 3000, 60);
        bool v4Zfo = keyV4.currency0 == address(0); // true: sell ETH
        _setupV4Swap(v4Pm, keyV4, AMOUNT_ETH, AMOUNT_USDC * 2, v4Zfo, address(usdc));

        Cmd.Table memory at = _table(address(v2Pair));

        // V2 callback forward_data:
        //   1. WETH_WITHDRAW — unwrap V2's just-sent WETH → ETH.
        //   2. V4_UNLOCK — V4 swap (sell ETH, buy USDC), settle ETH, take USDC.
        //   3. ERC20_TRANSFER — pay USDC to the V2 pair.
        bytes memory v2Fwd = Cmd.wethWithdraw(AMOUNT_WETH);

        bytes memory v4Inner = _v4Swap(at, keyV4, v4Zfo, Cmd.add(at, address(0)), AMOUNT_ETH);
        // V4 settle ETH: pay the unwrapped ETH to the PM.
        bytes memory settleEth = Cmd.v4SettleDelta(Cmd.add(at, address(0)));
        v4Inner = bytes.concat(v4Inner, settleEth);
        // V4 take USDC.
        bytes memory takeUsdc = Cmd.v4Take(Cmd.add(at, address(usdc)), Cmd.add(at, address(executor)), AMOUNT_USDC * 2);
        v4Inner = bytes.concat(v4Inner, takeUsdc);

        bytes memory unlocked = Cmd.v4Unlock(v4Inner);
        v2Fwd = bytes.concat(v2Fwd, unlocked);
        bytes memory payV2 = Cmd.erc20Transfer(Cmd.add(at, address(usdc)), Cmd.add(at, address(v2Pair)), AMOUNT_USDC);
        v2Fwd = bytes.concat(v2Fwd, payV2);

        bytes memory commands = Cmd.v2SwapCompact(
            Cmd.add(at, address(v2Pair)), v2Zfo, v2Out, Cmd.add(at, address(executor)), V2_FEE, v2Fwd
        );

        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);

        // The executor profits: V4 gave AMOUNT_USDC*2, paid AMOUNT_USDC to V2.
        assertGt(usdc.balanceOf(address(executor)), 0, "USDC profit");
    }

    /// Same path using WETH_WITHDRAW_ALL (unwraps the deployment wrap + the
    /// V2 output).
    function testV2WethToV4EthWithInlineUnwrapAll() public {
        weth.mint(address(executor), AMOUNT_WETH);
        (bool v2Zfo, uint256 v2Out) = _setupV2Sell(usdc, IFakeErc20(address(weth)), AMOUNT_USDC);

        PoolKey memory keyV4 = _poolKey(address(0), address(usdc), 3000, 60);
        bool v4Zfo = keyV4.currency0 == address(0);
        _setupV4Swap(v4Pm, keyV4, AMOUNT_ETH, AMOUNT_USDC * 2, v4Zfo, address(usdc));

        Cmd.Table memory at = _table(address(v2Pair));

        bytes memory v2Fwd = Cmd.wethWithdrawAll();

        bytes memory v4Inner = _v4Swap(at, keyV4, v4Zfo, Cmd.add(at, address(0)), AMOUNT_ETH);
        bytes memory settleEth = Cmd.v4SettleDelta(Cmd.add(at, address(0)));
        v4Inner = bytes.concat(v4Inner, settleEth);
        bytes memory takeUsdc = Cmd.v4Take(Cmd.add(at, address(usdc)), Cmd.add(at, address(executor)), AMOUNT_USDC * 2);
        v4Inner = bytes.concat(v4Inner, takeUsdc);

        bytes memory unlocked = Cmd.v4Unlock(v4Inner);
        v2Fwd = bytes.concat(v2Fwd, unlocked);
        bytes memory payV2 = Cmd.erc20Transfer(Cmd.add(at, address(usdc)), Cmd.add(at, address(v2Pair)), AMOUNT_USDC);
        v2Fwd = bytes.concat(v2Fwd, payV2);

        bytes memory commands = Cmd.v2SwapCompact(
            Cmd.add(at, address(v2Pair)), v2Zfo, v2Out, Cmd.add(at, address(executor)), V2_FEE, v2Fwd
        );

        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);

        assertGt(usdc.balanceOf(address(executor)), 0, "USDC profit");
    }

    // ── D. TestV3ToV4InlineUnwrap: V3 (WETH out) → WETH_WITHDRAW → V4 ────

    /// V3 (sell USDC, buy WETH) → WETH_WITHDRAW → V4 (sell ETH, buy USDC).
    /// Explicit forward_data (NOT auto-pay) so the USDC payment to the V3
    /// pool happens AFTER V4 provides the USDC.
    function testV3WethToV4EthWithInlineUnwrap() public {
        // ── V3 pool: WETH/USDC — sell USDC, buy WETH.
        (bool v3Zfo, uint256 v3WethOut) = _setupV3(v3Pool, usdc, IFakeErc20(address(weth)), AMOUNT_USDC, AMOUNT_WETH);

        // ── V4 pool: ETH/USDC — sell ETH, buy USDC (ETH amount matches V3's
        // actual output; more USDC out than V3 demands → arb profit).
        PoolKey memory keyV4 = _poolKey(address(0), address(usdc), 3000, 60);
        bool v4Zfo = keyV4.currency0 == address(0);
        _setupV4Swap(v4Pm, keyV4, v3WethOut, AMOUNT_USDC * 2, v4Zfo, address(usdc));

        Cmd.Table memory at = _table(address(v3Pool));

        // V3 callback forward_data (NOT auto-pay):
        //   1. WETH_WITHDRAW — unwrap V3's just-sent WETH → ETH.
        //   2. V4_UNLOCK — V4 swap (sell ETH, buy USDC), settle ETH, take USDC.
        //   3. ERC20_TRANSFER — pay USDC to the V3 pool.
        bytes memory v3Fwd = Cmd.wethWithdraw(v3WethOut);

        bytes memory v4Inner = _v4Swap(at, keyV4, v4Zfo, Cmd.add(at, address(0)), v3WethOut);
        bytes memory settleEth = Cmd.v4SettleDelta(Cmd.add(at, address(0)));
        v4Inner = bytes.concat(v4Inner, settleEth);
        bytes memory takeUsdc = Cmd.v4Take(Cmd.add(at, address(usdc)), Cmd.add(at, address(executor)), AMOUNT_USDC * 2);
        v4Inner = bytes.concat(v4Inner, takeUsdc);

        bytes memory unlocked = Cmd.v4Unlock(v4Inner);
        v3Fwd = bytes.concat(v3Fwd, unlocked);
        bytes memory payV3 = Cmd.erc20Transfer(Cmd.add(at, address(usdc)), Cmd.add(at, address(v3Pool)), AMOUNT_USDC);
        v3Fwd = bytes.concat(v3Fwd, payV3);

        bytes memory commands =
            Cmd.v3SwapCompact(Cmd.add(at, address(v3Pool)), v3Zfo, AMOUNT_USDC, Cmd.add(at, address(executor)), v3Fwd);

        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);

        // The executor profits: V4 gave AMOUNT_USDC*2, paid AMOUNT_USDC to V3.
        assertGt(usdc.balanceOf(address(executor)), 0, "USDC profit");
    }

    /// Same path using WETH_WITHDRAW_ALL (unwraps the deployment wrap + the
    /// V3 output).
    function testV3WethToV4EthWithInlineUnwrapAll() public {
        (bool v3Zfo, uint256 v3WethOut) = _setupV3(v3Pool, usdc, IFakeErc20(address(weth)), AMOUNT_USDC, AMOUNT_WETH);

        PoolKey memory keyV4 = _poolKey(address(0), address(usdc), 3000, 60);
        bool v4Zfo = keyV4.currency0 == address(0);
        _setupV4Swap(v4Pm, keyV4, v3WethOut, AMOUNT_USDC * 2, v4Zfo, address(usdc));

        Cmd.Table memory at = _table(address(v3Pool));

        bytes memory v3Fwd = Cmd.wethWithdrawAll();

        bytes memory v4Inner = _v4Swap(at, keyV4, v4Zfo, Cmd.add(at, address(0)), v3WethOut);
        bytes memory settleEth = Cmd.v4SettleDelta(Cmd.add(at, address(0)));
        v4Inner = bytes.concat(v4Inner, settleEth);
        bytes memory takeUsdc = Cmd.v4Take(Cmd.add(at, address(usdc)), Cmd.add(at, address(executor)), AMOUNT_USDC * 2);
        v4Inner = bytes.concat(v4Inner, takeUsdc);

        bytes memory unlocked = Cmd.v4Unlock(v4Inner);
        v3Fwd = bytes.concat(v3Fwd, unlocked);
        bytes memory payV3 = Cmd.erc20Transfer(Cmd.add(at, address(usdc)), Cmd.add(at, address(v3Pool)), AMOUNT_USDC);
        v3Fwd = bytes.concat(v3Fwd, payV3);

        bytes memory commands =
            Cmd.v3SwapCompact(Cmd.add(at, address(v3Pool)), v3Zfo, AMOUNT_USDC, Cmd.add(at, address(executor)), v3Fwd);

        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);

        assertGt(usdc.balanceOf(address(executor)), 0, "USDC profit");
    }
}
