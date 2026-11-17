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
        // 14 * 21 = 294 bytes exceeds the Bytes[288] type bound: the ABI decoder
        // rejects it before the contract runs.
        Outcome memory o = _executeRaw(executor, commands);
        assertFalse(o.ok, "294-byte stream must be rejected at the ABI boundary");
    }

    function testMaxSetAddressesAccepted() public {
        bytes memory commands;
        for (uint256 i = 0; i < 13; i++) {
            commands = bytes.concat(commands, Cmd.setAddress(_addr(i)));
        }
        commands = bytes.concat(commands, hex"ff", Cmd.wethDepositAll());
        _execute(executor, commands);
    }
}
