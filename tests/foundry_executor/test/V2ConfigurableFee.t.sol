// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {IFakeUniswapV2Pair} from "../src/generated/IFakeUniswapV2Pair.sol";
import {ICmdExecutor} from "../src/generated/ICmdExecutor.sol";

/// Port of `tests/executor/test_v2_configurable_fee.py`.
///
/// V2 swaps with configurable swap fees (fraction of 10000): UniswapV2 /
/// SushiSwapV2 use 30, PancakeSwapV2 uses 25, and the sub-1% range spans
/// 10-99. V2_SWAP_CALC computes amounts from on-chain excess balance
/// (balanceOf(pair) - reserves); no callback is needed — the pair already
/// holds the input. With runtime K-invariant enforcement (no set_next_swap),
/// the pair verifies the constant-product invariant after each swap, so the
/// computed amounts are guaranteed to satisfy K.
///
/// The source deploys a per-test executor with the V2 pair as the
/// POOL_MANAGER_ADDR placeholder (never initialized) because V2_SWAP_CALC
/// never touches the PoolManager; the port mirrors that shape.
contract V2ConfigurableFeeTest is PoolSetup {
    // AMOUNT_WETH (1 ether) / AMOUNT_USDC (2000e6) are the fixture constants —
    // the liquidity provision; SWAP_INPUT is the deposited excess.
    uint256 internal constant SWAP_INPUT = 1 ether;

    /// Deploy a pair with `fee` and a bespoke executor whose PM placeholder is
    /// that pair (the source `_deploy_executor` shape, never initialized).
    function _deployPairAndExecutor(uint16 fee, uint256 callbackVariant)
        internal
        returns (IFakeUniswapV2Pair pair, ICmdExecutor ex)
    {
        (address t0, address t1) = _sorted(address(usdc), address(weth));
        pair = _deployV2Pair(t0, t1, callbackVariant, fee);
        ex = ICmdExecutor(
            _deploy(
                "CmdExecutor",
                abi.encode(address(weth), address(pair), address(weth), address(weth))
            )
        );
    }

    /// Fund a pair with liquidity, sync reserves, compute the expected output
    /// from the constant-product formula, then deposit the swap input to
    /// create excess balance. The pair enforces K at runtime.
    function _setupPairAndSwap(IFakeUniswapV2Pair pair, uint16 fee)
        internal
        returns (uint256 expectedOut)
    {
        bool zfo = pair.token0() == address(weth);
        usdc.mint(address(pair), AMOUNT_USDC);
        weth.mint(address(pair), AMOUNT_WETH);
        pair.sync();

        uint256 reserveIn = zfo ? weth.balanceOf(address(pair)) : usdc.balanceOf(address(pair));
        uint256 reserveOut = zfo ? usdc.balanceOf(address(pair)) : weth.balanceOf(address(pair));
        expectedOut = _v2GetAmountOut(SWAP_INPUT, reserveIn, reserveOut, fee);

        // Deposit input tokens to create excess balance (after sync, so the
        // reserves do not include it).
        if (zfo) {
            weth.mint(address(pair), SWAP_INPUT);
        } else {
            usdc.mint(address(pair), SWAP_INPUT);
        }
    }

    /// Run V2_SWAP_CALC (selling WETH for USDC) and return the executor's USDC
    /// balance afterwards.
    function _executeCalc(
        IFakeUniswapV2Pair pair,
        ICmdExecutor ex,
        uint16 fee
    ) internal returns (bool ok) {
        bool zfo = pair.token0() == address(weth);
        Cmd.Table memory at = _emptyTable();
        uint8 pairIdx = Cmd.add(at, address(pair));
        uint8 exIdx = Cmd.add(at, address(ex));
        bytes memory commands =
            bytes.concat(Cmd.preamble(at), Cmd.v2SwapCalc(pairIdx, zfo, exIdx, fee));
        return _executeRaw(ex, commands).ok;
    }

    /// Standard 30/10000 (0.3%) fee — UniswapV2 & SushiSwapV2.
    function testV2SwapCalcUniswapFee() public {
        (IFakeUniswapV2Pair pair, ICmdExecutor ex) = _deployPairAndExecutor(30, 0);
        uint256 expectedOut = _setupPairAndSwap(pair, 30);

        assertTrue(_executeCalc(pair, ex, 30), "execute should succeed");
        assertGe(usdc.balanceOf(address(ex)), expectedOut, "USDC output covers expected amount");
    }

    /// 25/10000 (0.25%) fee — PancakeSwapV2.
    function testV2SwapCalcPancakeFee() public {
        (IFakeUniswapV2Pair pair, ICmdExecutor ex) = _deployPairAndExecutor(25, 0);
        uint256 expectedOut = _setupPairAndSwap(pair, 25);

        assertTrue(_executeCalc(pair, ex, 25), "execute should succeed");
        assertGe(usdc.balanceOf(address(ex)), expectedOut, "USDC output covers expected amount");
    }

    /// 0.1% fee — very low fee DEX.
    function testV2SwapCalcFee10() public {
        (IFakeUniswapV2Pair pair, ICmdExecutor ex) = _deployPairAndExecutor(10, 0);
        uint256 expectedOut = _setupPairAndSwap(pair, 10);

        assertTrue(_executeCalc(pair, ex, 10), "execute should succeed");
        assertGe(usdc.balanceOf(address(ex)), expectedOut, "USDC output covers expected amount");
    }

    /// 0.5% fee — mid-range.
    function testV2SwapCalcFee50() public {
        (IFakeUniswapV2Pair pair, ICmdExecutor ex) = _deployPairAndExecutor(50, 0);
        uint256 expectedOut = _setupPairAndSwap(pair, 50);

        assertTrue(_executeCalc(pair, ex, 50), "execute should succeed");
        assertGe(usdc.balanceOf(address(ex)), expectedOut, "USDC output covers expected amount");
    }

    /// 0.99% fee — near 1% ceiling of the sub-1% range.
    function testV2SwapCalcFee99() public {
        (IFakeUniswapV2Pair pair, ICmdExecutor ex) = _deployPairAndExecutor(99, 0);
        uint256 expectedOut = _setupPairAndSwap(pair, 99);

        assertTrue(_executeCalc(pair, ex, 99), "execute should succeed");
        assertGe(usdc.balanceOf(address(ex)), expectedOut, "USDC output covers expected amount");
    }

    /// Different fees produce different output amounts: with identical reserves
    /// and input, lower fee means more output.
    function testLowerFeeProducesMoreOutput() public {
        (IFakeUniswapV2Pair pairLow, ICmdExecutor exLow) = _deployPairAndExecutor(10, 0);
        (IFakeUniswapV2Pair pairHigh, ICmdExecutor exHigh) = _deployPairAndExecutor(99, 0);

        uint256 expectedLow = _setupPairAndSwap(pairLow, 10);
        uint256 expectedHigh = _setupPairAndSwap(pairHigh, 99);

        // Lower fee computes more output (same input + reserves, less fee taken).
        assertGt(expectedLow, expectedHigh, "lower fee computes more output");

        assertTrue(_executeCalc(pairLow, exLow, 10), "low-fee execute should succeed");
        assertTrue(_executeCalc(pairHigh, exHigh, 99), "high-fee execute should succeed");

        assertGt(
            usdc.balanceOf(address(exLow)),
            usdc.balanceOf(address(exHigh)),
            "lower fee delivers more USDC output"
        );
    }

    /// PancakeSwap callback variant (variant=2) with 0.25% fee. V2_SWAP_CALC
    /// with excess balance calls swap() with data = b"" (no callback fires),
    /// so the callback_variant setting is irrelevant; the test verifies
    /// compatibility with PancakeSwap V2 pairs regardless.
    function testPancakeCallbackWithFee25() public {
        (IFakeUniswapV2Pair pair, ICmdExecutor ex) = _deployPairAndExecutor(25, 2);
        uint256 expectedOut = _setupPairAndSwap(pair, 25);

        assertTrue(_executeCalc(pair, ex, 25), "execute should succeed");
        assertGe(usdc.balanceOf(address(ex)), expectedOut, "USDC output covers expected amount");
    }
}
