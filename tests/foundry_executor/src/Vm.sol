// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

/// Minimal subset of the Foundry cheatcode interface (no forge-std dependency).
interface Vm {
    struct Log {
        bytes32[] topics;
        bytes data;
        address emitter;
    }

    function readFile(string calldata path) external view returns (string memory);

    function parseJsonBytes(string calldata json, string calldata key)
        external
        pure
        returns (bytes memory);

    function deal(address who, uint256 amount) external;

    function prank(address msgSender) external;

    function startPrank(address msgSender) external;

    function stopPrank() external;

    function expectRevert() external;

    function expectRevert(bytes calldata revertData) external;

    function recordLogs() external;

    function getRecordedLogs() external returns (Log[] memory);

    function snapshotState() external returns (uint256);

    function revertToState(uint256 snapshotId) external;

    function label(address account, string calldata newLabel) external;

    function warp(uint256 timestamp) external;

    function envOr(string calldata name, string calldata defaultValue)
        external
        view
        returns (string memory);

    function chainId() external view returns (uint256);

    function toString(uint256 value) external pure returns (string memory);

    function toString(address value) external pure returns (string memory);

    function expectEmit(bool checkTopic1, bool checkTopic2, bool checkTopic3, bool checkData)
        external;
}
