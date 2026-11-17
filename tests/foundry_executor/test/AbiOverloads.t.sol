// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {PoolSetup} from "../src/Setup.sol";
import {Cmd} from "../src/Cmd.sol";

/// Vyper emits one selector per default-argument permutation: `config: uint256 = 1`
/// produces BOTH `execute(bytes)` and `execute(bytes,uint256)`. This pins the
/// shape and proves the two entry points agree — the assertion a port needs,
/// because a Solidity caller must name a selector where Python got a default.
contract AbiOverloadsTest is PoolSetup {
    uint256 internal constant PREFUND = 10 ether;

    bytes4 internal constant EXECUTE_ONE_ARG = bytes4(keccak256("execute(bytes)"));
    bytes4 internal constant EXECUTE_TWO_ARG = bytes4(keccak256("execute(bytes,uint256)"));

    function _stream() internal pure returns (bytes memory) {
        return bytes.concat(Cmd.preamble(_emptyTable()), Cmd.wethDepositAll());
    }

    function testOverloadSelectorsAreDistinct() public pure {
        assertTrue(EXECUTE_ONE_ARG != EXECUTE_TWO_ARG, "two selectors for the one defaulted param");
        assertEq(uint256(uint32(EXECUTE_ONE_ARG)), uint256(uint32(bytes4(0x09c5eabe))), "execute(bytes)");
        assertEq(
            uint256(uint32(EXECUTE_TWO_ARG)), uint256(uint32(bytes4(0xab5898e8))), "execute(bytes,uint256)"
        );
    }

    /// The omitted-argument form and the fully-specified form are the same call.
    function testDefaultArgumentAndExplicitArgumentAgree() public {
        weth.mint(address(executor), PREFUND);

        bytes memory oneArg = abi.encodeWithSelector(EXECUTE_ONE_ARG, _stream());
        bytes memory twoArg = abi.encodeWithSelector(EXECUTE_TWO_ARG, _stream(), uint256(1));

        uint256 snapOne = vm.snapshotState();
        (bool okOne, bytes memory retOne) = address(executor).call(oneArg);
        vm.revertToState(snapOne);

        uint256 snapTwo = vm.snapshotState();
        (bool okTwo, bytes memory retTwo) = address(executor).call(twoArg);
        vm.revertToState(snapTwo);

        assertTrue(okOne, string.concat("execute(bytes) failed: ", _hexPreview(retOne)));
        assertTrue(okTwo, string.concat("execute(bytes,1) failed: ", _hexPreview(retTwo)));
        assertEq(keccak256(retOne), keccak256(retTwo), "both entry points return the same receipt");
    }

    /// The declared default is `1`, not `0`: mode 0 is rejected outright, so a
    /// port that substitutes 0 for the omitted argument fails loudly instead of
    /// silently skipping the profit check.
    function testOmittedArgumentIsNotModeZero() public {
        weth.mint(address(executor), PREFUND);

        Outcome memory omitted =
            _rawSimulated(abi.encodeWithSelector(EXECUTE_ONE_ARG, _stream()));
        assertTrue(omitted.ok, "the omitted-argument form executes");

        Outcome memory modeZero =
            _rawSimulated(abi.encodeWithSelector(EXECUTE_TWO_ARG, _stream(), uint256(0)));
        assertFalse(modeZero.ok, "an explicit config = 0 is rejected (mode 0 is not legal)");
    }

    function _rawSimulated(bytes memory data) internal returns (Outcome memory o) {
        uint256 snapshot = vm.snapshotState();
        (o.ok, o.revertData) = address(executor).call(data);
        vm.revertToState(snapshot);
    }
}
