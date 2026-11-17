// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";

/// Port of `tests/executor/test_no_fe_prefix.py`.
contract NoFePrefixTest is PoolSetup {
    function testFePrefixedStreamReverts() public {
        bytes memory stream = hex"feff";
        Outcome memory o = _executeRaw(executor, stream);
        assertFalse(o.ok, "0xFE-prefixed stream must revert");
    }

    function testFfOnlyRevertsEmptyExecution() public {
        Outcome memory o = _executeRaw(executor, hex"ff");
        assertFalse(o.ok, "bare 0xFF must revert");
    }
}
