// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {ExecutorErrors} from "../src/generated/Errors.sol";

/// Port of `tests/executor/test_no_fe_prefix.py`.
contract NoFePrefixTest is PoolSetup {
    function testFePrefixedStreamReverts() public {
        bytes memory stream = hex"feff";
        Outcome memory o = _executeRaw(executor, stream);
        // 0xFE is not a valid execution opcode: the dispatcher raises
        // InvalidCommand(0xFE).
        _assertRevertData(
            o, _executorRevert(ExecutorErrors.INVALID_COMMAND, 0xFE), "0xFE-prefixed stream"
        );
    }

    function testFfOnlyRevertsEmptyExecution() public {
        Outcome memory o = _executeRaw(executor, hex"ff");
        // The dispatch loop executes before its break check, so offset == len
        // hits the out-of-bounds slice: a fail-closed revert carrying no
        // identity (the source's NO_REVERT_DATA).
        _assertRevertData(o, bytes(""), "bare 0xFF");
    }
}
