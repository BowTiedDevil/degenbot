// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {IFakeUniswapV2Pair} from "../src/generated/IFakeUniswapV2Pair.sol";

/// Port of `tests/executor/test_v2_swap_compact_fee.py`.
///
/// V2_SWAP_COMPACT (0x20) with the inline fee field. The fee is written to the
/// executor's per-pair fee table before swap(), enabling correct auto-pay
/// computation in the V2 callback. UniswapV2/SushiSwapV2 use 30, PancakeSwapV2
/// uses 25.
contract V2SwapCompactFeeTest is PoolSetup {
    uint256 internal constant V2_LIQUIDITY_WETH = 100 ether;
    uint256 internal constant V2_LIQUIDITY_USDC = 200_000e6;

    function _deployWethUsdcPair(uint16 fee) internal returns (IFakeUniswapV2Pair pair) {
        (address t0, address t1) = _sorted(address(usdc), address(weth));
        pair = _deployV2Pair(t0, t1, 0, fee);
    }

    /// V2_SWAP_COMPACT with fee = 30 and the auto-pay sentinel pays the correct
    /// owed amount.
    function testV2SwapCompactUniswapFee() public {
        IFakeUniswapV2Pair v2 = _deployWethUsdcPair(30);

        weth.mint(address(v2), V2_LIQUIDITY_WETH);
        usdc.mint(address(v2), V2_LIQUIDITY_USDC);
        v2.sync();

        bool v2Zfo = v2.token0() == address(weth);
        uint256 amountOut =
            _v2GetAmountOut(AMOUNT_WETH, weth.balanceOf(address(v2)), usdc.balanceOf(address(v2)), 30);

        // Fund the executor with WETH so it can pay the pair during auto-pay.
        weth.mint(address(executor), AMOUNT_WETH);

        Cmd.Table memory at = _emptyTable();
        uint8 v2Idx = Cmd.add(at, address(v2));
        uint8 executorIdx = Cmd.add(at, address(executor));

        // Pass the sentinel as forward_data to trigger the auto-pay callback.
        bytes memory commands = bytes.concat(
            Cmd.preamble(at),
            Cmd.v2SwapCompact(v2Idx, v2Zfo, amountOut, executorIdx, 30, hex"fe")
        );

        _execute(executor, commands, 2);
        assertGe(usdc.balanceOf(address(executor)), amountOut, "executor received USDC output");
    }

    /// V2_SWAP_COMPACT with fee = 25 and the auto-pay sentinel pays the correct
    /// owed amount.
    function testV2SwapCompactPancakeFee() public {
        IFakeUniswapV2Pair v2 = _deployWethUsdcPair(25);

        weth.mint(address(v2), V2_LIQUIDITY_WETH);
        usdc.mint(address(v2), V2_LIQUIDITY_USDC);
        v2.sync();

        bool v2Zfo = v2.token0() == address(weth);
        uint256 amountOut =
            _v2GetAmountOut(AMOUNT_WETH, weth.balanceOf(address(v2)), usdc.balanceOf(address(v2)), 25);

        weth.mint(address(executor), AMOUNT_WETH);

        Cmd.Table memory at = _emptyTable();
        uint8 v2Idx = Cmd.add(at, address(v2));
        uint8 executorIdx = Cmd.add(at, address(executor));

        bytes memory commands = bytes.concat(
            Cmd.preamble(at),
            Cmd.v2SwapCompact(v2Idx, v2Zfo, amountOut, executorIdx, 25, hex"fe")
        );

        _execute(executor, commands, 2);
        assertGe(usdc.balanceOf(address(executor)), amountOut, "executor received USDC output");
    }

    /// For the same input amount, a lower fee produces more output — so the
    /// auto-pay path owes less input for a given output.
    function testLowerFeePaysLessInput() public {
        IFakeUniswapV2Pair v2LowFee = _deployWethUsdcPair(10);
        IFakeUniswapV2Pair v2HighFee = _deployWethUsdcPair(99);

        // Same liquidity in both pairs.
        weth.mint(address(v2LowFee), V2_LIQUIDITY_WETH);
        usdc.mint(address(v2LowFee), V2_LIQUIDITY_USDC);
        v2LowFee.sync();
        weth.mint(address(v2HighFee), V2_LIQUIDITY_WETH);
        usdc.mint(address(v2HighFee), V2_LIQUIDITY_USDC);
        v2HighFee.sync();

        uint256 amountOutLow = _v2GetAmountOut(
            AMOUNT_WETH, weth.balanceOf(address(v2LowFee)), usdc.balanceOf(address(v2LowFee)), 10
        );
        uint256 amountOutHigh = _v2GetAmountOut(
            AMOUNT_WETH, weth.balanceOf(address(v2HighFee)), usdc.balanceOf(address(v2HighFee)), 99
        );

        assertGt(amountOutLow, amountOutHigh, "Lower fee should produce more output");
    }

    /// V2→V2 path with different fees — PancakeSwap V2 (25 bps) → Uniswap V2
    /// (30 bps) via V2_SWAP_COMPACT with correct per-pair fees.
    function testMixedFeeV2V2Path() public {
        (address t0, address t1) = _sorted(address(usdc), address(weth));

        // PancakeSwap pair: fee = 25. Uniswap pair: fee = 30.
        IFakeUniswapV2Pair v2Pancake = _deployV2Pair(t0, t1, 0, 25);
        IFakeUniswapV2Pair v2Uni = _deployV2Pair(t0, t1, 0, 30);

        // PancakeSwap pair: WETH → USDC.
        weth.mint(address(v2Pancake), V2_LIQUIDITY_WETH);
        usdc.mint(address(v2Pancake), V2_LIQUIDITY_USDC);
        v2Pancake.sync();

        bool pZfo = v2Pancake.token0() == address(weth);
        uint256 pAmountOut = _v2GetAmountOut(
            AMOUNT_WETH, weth.balanceOf(address(v2Pancake)), usdc.balanceOf(address(v2Pancake)), 25
        );

        // Uniswap pair: USDC → WETH.
        usdc.mint(address(v2Uni), V2_LIQUIDITY_USDC);
        weth.mint(address(v2Uni), V2_LIQUIDITY_WETH);
        v2Uni.sync();

        bool uZfo = v2Uni.token0() == address(usdc);
        uint256 uAmountOut = _v2GetAmountOut(
            pAmountOut, usdc.balanceOf(address(v2Uni)), weth.balanceOf(address(v2Uni)), 30
        );

        // Fund the executor with WETH to pay the first pair.
        weth.mint(address(executor), AMOUNT_WETH);

        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 pIdx = Cmd.add(at, address(v2Pancake));
        uint8 uIdx = Cmd.add(at, address(v2Uni));

        // First swap: PancakeSwap WETH → USDC, callback with:
        // 1. Pay USDC to the Uniswap pair
        // 2. Uniswap swap USDC → WETH (no callback)
        // 3. Pay WETH to the PancakeSwap pair
        bytes memory pCallback = Cmd.erc20Transfer(usdcIdx, uIdx, pAmountOut);
        pCallback = bytes.concat(
            pCallback, Cmd.v2SwapCompact(uIdx, uZfo, uAmountOut, executorIdx, 30, "")
        );
        pCallback = bytes.concat(pCallback, Cmd.erc20Transfer(wethIdx, pIdx, AMOUNT_WETH));

        bytes memory commands = bytes.concat(
            Cmd.preamble(at),
            Cmd.v2SwapCompact(pIdx, pZfo, pAmountOut, executorIdx, 25, pCallback)
        );

        _execute(executor, commands, 2);
        assertGt(weth.balanceOf(address(executor)), 0, "executor should have WETH profit");
    }
}
