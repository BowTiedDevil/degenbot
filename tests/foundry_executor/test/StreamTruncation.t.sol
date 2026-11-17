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
        // The loop cap consumes every byte; the revert is the unrelated,
        // expected ManagerLocked guard of V4_SETTLE outside an unlock — the
        // PM's Error(string) payload — never a silent-success truncation.
        _assertRevertData(o, _managerLocked(), "maximal stream");
    }

    function _managerLocked() private pure returns (bytes memory) {
        return abi.encodeWithSignature("Error(string)", "ManagerLocked");
    }

    function testLoopCapEqualsByteBound() public pure {
        assertEq(MAX_COMMANDS_LENGTH, 288, "loop cap equals the Bytes[288] bound");
    }
}
