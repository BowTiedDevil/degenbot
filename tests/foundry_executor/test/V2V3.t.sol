// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {IFakeUniswapV2Pair} from "../src/generated/IFakeUniswapV2Pair.sol";
import {IFakeUniswapV3Pool} from "../src/generated/IFakeUniswapV3Pool.sol";
import {IFakeErc20} from "../src/generated/IFakeErc20.sol";

/// Port of `tests/executor/test_cmd_executor_v2v3.py`.
contract V2V3Test is PoolSetup {
    uint256 internal constant V2_LIQ_WETH = 100 ether;
    uint256 internal constant V2_LIQ_USDC = 200_000e6;

    IFakeUniswapV2Pair internal v2Pair;
    IFakeUniswapV3Pool internal v3Pool;

    function setUp() public override {
        super.setUp();
        (address t0, address t1) = _sorted(address(usdc), address(weth));
        v2Pair = _deployV2Pair(t0, t1, 0, 30);
        v3Pool = _deployV3Pool(t0, t1, 0, 3000);
    }

    function testV2V3WethUsdcToWeth() public {
        weth.mint(address(executor), AMOUNT_WETH);

        weth.mint(address(v2Pair), V2_LIQ_WETH);
        usdc.mint(address(v2Pair), V2_LIQ_USDC);
        v2Pair.sync();

        bool v2Zfo = v2Pair.token0() == address(weth);
        uint256 v2AmountOut = _v2GetAmountOut(
            AMOUNT_WETH, weth.balanceOf(address(v2Pair)), usdc.balanceOf(address(v2Pair)), 30
        );

        (bool v3Zfo,) = _setupV3(
            v3Pool,
            IFakeErc20(address(usdc)),
            IFakeErc20(address(weth)),
            v2AmountOut,
            AMOUNT_WETH
        );

        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3Idx = Cmd.add(at, address(v3Pool));
        uint8 v2Idx = Cmd.add(at, address(v2Pair));

        // V3 callback: the USDC must arrive during the callback (the IIA check).
        bytes memory v3Fwd = Cmd.erc20Transfer(usdcIdx, v3Idx, v2AmountOut);

        // V2 callback: run the V3 swap, then repay the V2 flash borrow.
        bytes memory v2Fwd = Cmd.v3SwapCompact(v3Idx, v3Zfo, v2AmountOut, executorIdx, v3Fwd);
        v2Fwd = bytes.concat(v2Fwd, Cmd.erc20Transfer(wethIdx, v2Idx, AMOUNT_WETH));

        bytes memory commands = Cmd.v2SwapCompact(
            v2Idx, v2Zfo, v2AmountOut, executorIdx, 30, v2Fwd
        );

        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);
    }

    function testV3V2WethUsdcToWeth() public {
        weth.mint(address(executor), AMOUNT_WETH);
        (bool v3Zfo, uint256 v3AmountOut) = _setupV3(
            v3Pool,
            IFakeErc20(address(weth)),
            IFakeErc20(address(usdc)),
            AMOUNT_WETH,
            AMOUNT_USDC
        );

        usdc.mint(address(v2Pair), V2_LIQ_USDC);
        weth.mint(address(v2Pair), V2_LIQ_WETH);
        v2Pair.sync();

        bool v2Zfo = v2Pair.token0() == address(usdc);
        uint256 v2AmountOut = _v2GetAmountOut(
            v3AmountOut, usdc.balanceOf(address(v2Pair)), weth.balanceOf(address(v2Pair)), 30
        );

        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 v3Idx = Cmd.add(at, address(v3Pool));
        uint8 v2Idx = Cmd.add(at, address(v2Pair));

        bytes memory v3Callback = Cmd.erc20Transfer(wethIdx, v3Idx, AMOUNT_WETH);
        v3Callback = bytes.concat(v3Callback, Cmd.erc20Transfer(usdcIdx, v2Idx, v3AmountOut));
        v3Callback = bytes.concat(v3Callback, Cmd.v2SwapCompact(v2Idx, v2Zfo, v2AmountOut, executorIdx, 30, ""));

        bytes memory commands =
            Cmd.v3SwapCompact(v3Idx, v3Zfo, AMOUNT_WETH, executorIdx, v3Callback);

        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);
    }
}
