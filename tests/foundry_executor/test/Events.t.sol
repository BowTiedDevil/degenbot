// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {Vm} from "../src/Vm.sol";
import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {IFakeUniswapV2Pair} from "../src/generated/IFakeUniswapV2Pair.sol";
import {IFakeErc20} from "../src/generated/IFakeErc20.sol";

/// Port of `tests/executor/test_events.py`.
contract EventsTest is PoolSetup {
    uint16 internal constant EVENTS_V2_FEE = 30;

    function _record() internal {
        vm.recordLogs();
    }

    function testErc20TransferEvent() public {
        _record();
        usdc.transfer(address(0xB1), 1000);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(_countTransfers(logs), 1, "one Transfer");
    }

    function testErc20MintEvent() public {
        _record();
        usdc.mint(address(0xB1), 1000);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(_countTransfers(logs), 1, "mint emits one Transfer from the zero address");
    }

    function testErc20BurnEvent() public {
        _record();
        usdc.burn(1000);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(_countTransfers(logs), 1, "burn emits one Transfer to the zero address");
    }

    function testWethDepositEvent() public {
        _record();
        weth.deposit{value: 1 ether}();
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(_countTransfers(logs), 1, "deposit emits one Transfer");
    }

    function testWethWithdrawEvent() public {
        weth.deposit{value: 1 ether}();
        _record();
        weth.withdraw(1 ether);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(_countTransfers(logs), 1, "withdraw emits one Transfer");
    }

    function testV2SwapEvents() public {
        IFakeUniswapV2Pair v2 =
            _deployV2Pair(address(weth), address(usdc), 0, uint256(EVENTS_V2_FEE));
        usdc.mint(address(v2), 2000e6);
        weth.mint(address(v2), 10 ether);
        v2.sync();

        uint256 amountIn = 1 ether;
        uint256 amountOut = _v2GetAmountOut(
            amountIn, weth.balanceOf(address(v2)), usdc.balanceOf(address(v2)), V2_FEE
        );

        weth.transfer(address(v2), amountIn);
        _record();
        v2.swap(0, amountOut, address(this), "");
        Vm.Log[] memory logs = vm.getRecordedLogs();

        assertEq(_countTransfers(logs), 1, "one Transfer (USDC pair -> owner)");
        assertEq(_countTopic(logs, V2_SYNC_TOPIC), 1, "one Sync");
        assertEq(_countTopic(logs, FAKE_V2_SWAP_TOPIC), 1, "one Swap");
    }
}
