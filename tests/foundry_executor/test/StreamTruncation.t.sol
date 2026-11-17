// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";

/// Port of `tests/executor/test_stream_truncation.py`.
contract StreamTruncationTest is PoolSetup {
    uint256 internal constant MAX_COMMANDS_LENGTH = 288;

    function testMaxLengthStreamOfSingleByteCommandsFitsLoopCap() public {
        bytes memory stream = new bytes(MAX_COMMANDS_LENGTH);
        for (uint256 i = 0; i < MAX_COMMANDS_LENGTH; i++) {
            stream[i] = bytes1(Cmd.V4_SETTLE);
        }
        Outcome memory o = _executeRaw(executor, stream);
        assertFalse(o.ok, "V4_SETTLE outside an unlock must revert, never silently truncate");
    }

    function testLoopCapEqualsByteBound() public pure {
        assertEq(MAX_COMMANDS_LENGTH, 288, "loop cap equals the Bytes[288] bound");
    }
}
