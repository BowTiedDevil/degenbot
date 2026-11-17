// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";

/// Port of `tests/executor/test_preprocess_addr_count.py`.
contract PreprocessAddrCountTest is PoolSetup {
    function _addr(uint256 i) internal pure returns (address) {
        return address(uint160(uint256(keccak256(abi.encode(i))) ));
    }

    function testOverflowSetAddressesRevert() public {
        bytes memory commands;
        for (uint256 i = 0; i < 14; i++) {
            commands = bytes.concat(commands, Cmd.setAddress(_addr(i)));
        }
        commands = bytes.concat(commands, hex"ff");
        // 14 * 21 = 294 bytes exceeds the Bytes[288] type bound: the ABI
        // decoder rejects it before the contract runs, with no revert data.
        Outcome memory o = _executeRaw(executor, commands);
        _assertRevertData(o, bytes(""), "294-byte stream");
    }

    function testMaxSetAddressesAccepted() public {
        bytes memory commands;
        for (uint256 i = 0; i < 13; i++) {
            commands = bytes.concat(commands, Cmd.setAddress(_addr(i)));
        }
        commands = bytes.concat(commands, hex"ff", Cmd.wethDepositAll());
        _execute(executor, commands);
    }

    function testExactMaxByteStreamAccepted() public {
        // 13 SET_ADDRESS commands (273 bytes) + the separator + 14 one-byte
        // execution commands = exactly 288 bytes: the maximal stream the
        // Bytes[288] bound accepts. WETH_DEPOSIT_ALL with no native ETH is a
        // clean no-op, so the execution itself succeeds.
        bytes memory commands;
        for (uint256 i = 0; i < 13; i++) {
            commands = bytes.concat(commands, Cmd.setAddress(_addr(i)));
        }
        commands = bytes.concat(commands, hex"ff");
        for (uint256 i = 0; i < 14; i++) {
            commands = bytes.concat(commands, hex"14"); // WETH_DEPOSIT_ALL
        }
        assertEq(commands.length, 288, "maximal stream length");
        _execute(executor, commands);
    }

    function testOneByteOverTheBoundAbiRejected() public {
        // 289 bytes is one byte over the Bytes[288] bound: the ABI decoder
        // rejects it before the contract runs, with no revert data.
        bytes memory commands;
        for (uint256 i = 0; i < 13; i++) {
            commands = bytes.concat(commands, Cmd.setAddress(_addr(i)));
        }
        commands = bytes.concat(commands, hex"ff");
        for (uint256 i = 0; i < 15; i++) {
            commands = bytes.concat(commands, hex"14"); // WETH_DEPOSIT_ALL
        }
        assertEq(commands.length, 289, "length one over the bound");
        Outcome memory o = _executeRaw(executor, commands);
        _assertRevertData(o, bytes(""), "289-byte stream");
    }
}
