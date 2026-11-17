// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {IFakeUniswapV2Pair} from "../src/generated/IFakeUniswapV2Pair.sol";
import {IFakeErc20} from "../src/generated/IFakeErc20.sol";
import {PoolKey} from "../src/generated/Types.sol";

/// Port of `tests/executor/test_cmd_executor_edge_cases.py`.
contract EdgeCasesTest is PoolSetup {
    uint256 internal constant AMOUNT_ETH = 1 ether;

    function testV4NativeEthInput() public {
        vm.deal(address(executor), address(executor).balance + AMOUNT_ETH);

        PoolKey memory key = _poolKey(address(0), address(usdc), 500, 10);
        bool zfo = true; // native is always currency0, so zfo = sell ETH for USDC
        _setupV4Swap(v4Pm, key, AMOUNT_ETH, AMOUNT_USDC, zfo, address(usdc));

        Cmd.Table memory at = _emptyTable();
        uint8 pmIdx = Cmd.add(at, address(v4Pm));
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 zeroIdx = Cmd.add(at, address(0));
        uint8 nativeIdx = Cmd.add(at, address(0));

        bytes memory inner = Cmd.v4SwapCompact(
            nativeIdx, usdcIdx, uint16(key.fee), int16(key.tick_spacing), zeroIdx, zfo, AMOUNT_ETH
        );
        inner = bytes.concat(inner, Cmd.v4Take(usdcIdx, executorIdx, AMOUNT_USDC));
        inner = bytes.concat(inner, Cmd.v4SettleDelta(nativeIdx));

        _execute(executor, bytes.concat(Cmd.preamble(at), Cmd.v4Unlock(inner)), 2);
        assertEq(usdc.balanceOf(address(executor)), AMOUNT_USDC, "USDC taken to executor");
    }

    function testV2DirectSwapNoCallback() public {
        (address t0, address t1) = _sorted(address(usdc), address(weth));
        IFakeUniswapV2Pair pair = _deployV2Pair(t0, t1, 0, 30);

        weth.mint(address(executor), AMOUNT_WETH);
        (bool zfo, uint256 amountOut) =
            _setupV2Pair(pair, IFakeErc20(address(weth)), IFakeErc20(address(usdc)), AMOUNT_WETH, 30);

        Cmd.Table memory at = _emptyTable();
        uint8 wethIdx = Cmd.add(at, address(weth));
        uint8 usdcIdx = Cmd.add(at, address(usdc));
        uint8 executorIdx = Cmd.add(at, address(executor));
        uint8 pairIdx = Cmd.add(at, address(pair));

        bytes memory commands = Cmd.erc20Transfer(wethIdx, pairIdx, AMOUNT_WETH);
        commands = bytes.concat(
            commands, Cmd.v2SwapCompact(pairIdx, zfo, amountOut, executorIdx, 30, "")
        );

        _execute(executor, bytes.concat(Cmd.preamble(at), commands), 2);
        assertEq(usdc.balanceOf(address(executor)), amountOut, "direct V2 swap delivered output");
    }
}
