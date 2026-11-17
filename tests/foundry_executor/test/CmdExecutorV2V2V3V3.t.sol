// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {IFakeErc20} from "../src/generated/IFakeErc20.sol";
import {IFakeUniswapV2Pair} from "../src/generated/IFakeUniswapV2Pair.sol";
import {IFakeUniswapV3Pool} from "../src/generated/IFakeUniswapV3Pool.sol";

/// Port of `tests/executor/test_cmd_executor_v2v2_v3v3.py`.
///
/// V3-V3 and V2-V2 nested-callback swaps over the WETH/USDC pair, both
/// executed under profit mode 2 (PoolManager ERC6909 WETH claims): the profit
/// check requires no decrease in the claims domain, not a positive profit.
contract CmdExecutorV2V2V3V3Test is PoolSetup {
    uint256 internal constant V2_LIQ_WETH = 100 ether;
    uint256 internal constant V2_LIQ_USDC = 200_000e6;

    IFakeUniswapV3Pool internal v3a;
    IFakeUniswapV3Pool internal v3b;
    IFakeUniswapV2Pair internal v2a;
    IFakeUniswapV2Pair internal v2b;

    function setUp() public override {
        super.setUp();
        (address t0, address t1) = _sorted(address(usdc), address(weth));
        v3a = _deployV3Pool(t0, t1, 0, 3000);
        v3b = _deployV3Pool(t0, t1, 0, 3000);
        v2a = _deployV2Pair(t0, t1, 0, 30);
        v2b = _deployV2Pair(t0, t1, 0, 30);
    }

    /// V3a swap (WETH→USDC) whose forward_data pays WETH to V3a and runs the
    /// V3b swap (USDC→WETH) with empty forward_data — the callback auto-detects
    /// the owed USDC and pays it from the executor's running balance (double
    /// auto-pay shape: the inner swap carries no forward commands at all).
    function testV3V3NestedCallbackDoubleAutopay() public {
        weth.mint(address(executor), AMOUNT_WETH);

        // V3a: WETH→USDC (input = WETH, zfo = input == token0).
        (bool v3aZfo, uint256 v3aUsdcOut) =
            _setupV3(v3a, IFakeErc20(address(weth)), usdc, AMOUNT_WETH, AMOUNT_USDC);

        // V3b: USDC→WETH, funded with V3a's actual USDC output as input.
        (bool v3bZfo,) =
            _setupV3(v3b, usdc, IFakeErc20(address(weth)), v3aUsdcOut, AMOUNT_WETH * 2);

        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3aIdx = Cmd.add(at, address(v3a));
        uint8 v3bIdx = Cmd.add(at, address(v3b));

        // V3a callback: pay WETH to V3a, then the V3b swap (auto-pay).
        bytes memory v3aCallback = Cmd.erc20Transfer(wethIdx, v3aIdx, AMOUNT_WETH);
        v3aCallback =
            bytes.concat(v3aCallback, Cmd.v3SwapCompact(v3bIdx, v3bZfo, v3aUsdcOut, executorIdx, ""));

        bytes memory commands =
            Cmd.v3SwapCompact(v3aIdx, v3aZfo, AMOUNT_WETH, executorIdx, v3aCallback);

        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);
    }

    /// V2a: WETH→USDC (flash), V2b: USDC→WETH (no callback).
    ///
    /// V2a sends USDC to the executor, its callback pays USDC to V2b, the V2b
    /// swap sends WETH to the executor, and the callback repays WETH to V2a
    /// (the flash repayment) before V2a's K-invariant check runs.
    function testV2V2NestedCallback() public {
        weth.mint(address(executor), AMOUNT_WETH);

        // V2a: WETH→USDC (input = WETH, zfo = input == token0).
        weth.mint(address(v2a), V2_LIQ_WETH);
        usdc.mint(address(v2a), V2_LIQ_USDC);
        v2a.sync();

        bool v2aZfo = v2a.token0() == address(weth);
        uint256 v2aAmountOut = _v2GetAmountOut(
            AMOUNT_WETH, weth.balanceOf(address(v2a)), usdc.balanceOf(address(v2a)), 30
        );

        // V2b: USDC→WETH (input = USDC, zfo = input == token0).
        usdc.mint(address(v2b), V2_LIQ_USDC);
        weth.mint(address(v2b), V2_LIQ_WETH);
        v2b.sync();

        bool v2bZfo = v2b.token0() == address(usdc);
        uint256 v2bAmountOut = _v2GetAmountOut(
            v2aAmountOut, usdc.balanceOf(address(v2b)), weth.balanceOf(address(v2b)), 30
        );

        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v2aIdx = Cmd.add(at, address(v2a));
        uint8 v2bIdx = Cmd.add(at, address(v2b));

        // V2a callback: pay USDC to V2b, run the V2b swap (its empty callback
        // data is a no-op — the input already arrived via the transfer), then
        // repay the WETH flash borrow to V2a.
        bytes memory v2aCallback = Cmd.erc20Transfer(usdcIdx, v2bIdx, v2aAmountOut);
        v2aCallback =
            bytes.concat(v2aCallback, Cmd.v2SwapCompact(v2bIdx, v2bZfo, v2bAmountOut, executorIdx, 30, ""));
        v2aCallback = bytes.concat(v2aCallback, Cmd.erc20Transfer(wethIdx, v2aIdx, AMOUNT_WETH));

        bytes memory commands =
            Cmd.v2SwapCompact(v2aIdx, v2aZfo, v2aAmountOut, executorIdx, 30, v2aCallback);

        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);
    }
}
