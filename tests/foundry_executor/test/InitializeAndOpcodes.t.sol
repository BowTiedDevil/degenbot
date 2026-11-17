// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";
import {ICmdExecutor} from "../src/generated/ICmdExecutor.sol";

/// Port of `tests/executor/test_initialize_and_opcodes.py`.
contract InitializeAndOpcodesTest is PoolSetup {
    address internal constant ATTACKER = address(0xBAD);

    // ── L1: SEND_ETH / SEND_ETH_ALL opcodes ──────────────────────────────

    function testSendEthOpcode() public pure {
        bytes memory encoded = Cmd.sendEth(1, 1);
        assertEq(encoded.length, 14, "sendEth length");
        assertEq(uint8(encoded[0]), Cmd.SEND_ETH, "sendEth opcode");
        assertEq(uint8(encoded[0]), 0x16, "sendEth opcode literal");
    }

    function testSendEthAllOpcode() public pure {
        bytes memory encoded = Cmd.sendEthAll(1);
        assertEq(encoded.length, 2, "sendEthAll length");
        assertEq(uint8(encoded[0]), Cmd.SEND_ETH_ALL, "sendEthAll opcode");
        assertEq(uint8(encoded[0]), 0x17, "sendEthAll opcode literal");
    }

    // ── L2: initialize() ─────────────────────────────────────────────────

    function _freshExecutor() internal returns (ICmdExecutor) {
        return ICmdExecutor(
            _deploy(
                "CmdExecutor",
                abi.encode(address(weth), address(v4Pm), address(weth), address(weth))
            )
        );
    }

    function testInitializeRejectsNonOwner() public {
        ICmdExecutor ex = _freshExecutor();
        vm.deal(ATTACKER, 1 ether);
        vm.prank(ATTACKER);
        (bool ok,) = address(ex).call{value: 1}(abi.encodeWithSignature("initialize()"));
        assertFalse(ok, "non-owner initialize must revert");
    }

    function testInitializeRejectsWrongValue() public {
        ICmdExecutor ex = _freshExecutor();
        (bool ok,) = address(ex).call{value: 2}(abi.encodeWithSignature("initialize()"));
        assertFalse(ok, "msg.value == 2 must revert");
    }

    function testInitializeOneWeiSucceedsAndStrandsNoEth() public {
        ICmdExecutor ex = _freshExecutor();
        assertEq(address(ex).balance, 0, "fresh executor holds no ETH");
        ex.initialize{value: 1}();
        assertEq(address(ex).balance, 0, "initialize must not strand ETH");
    }
}
