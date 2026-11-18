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

    function expectRevert(bytes4 revertSelector) external;

    // ── FFI lane ──────────────────────────────────────────────────────────
    // Consumer: the production-calldata port runs the Rust `encode_three_hop`
    // binary and executes the returned bytes verbatim. `ffi` returns stdout and
    // reverts on a non-zero exit carrying stderr; `tryFfi` reports the exit
    // code and both streams so the encoder's CLI refusals can be pinned
    // exactly. Requires `forge test --ffi`; the default gate never sets it, and
    // `skip` is how that lane's suite stays out of it.

    struct FfiResult {
        uint8 exitCode;
        bytes stdout;
        bytes stderr;
    }

    function ffi(string[] calldata command) external returns (bytes memory);

    function tryFfi(string[] calldata command) external returns (FfiResult memory);

    function skip(bool skipTest) external;

    // ── RPC / fork lanes ──────────────────────────────────────────────────
    // Consumers: the fake-PM parity fork port pins a real PoolManager at an
    // archive height with createFork/selectFork + rpc `eth_getCode`
    // fail-closed probes; the access-list gas-evidence replacement
    // benchmarks `eth_createAccessList` through rpc.

    function rpc(string calldata method, string calldata params) external returns (bytes memory);

    function createFork(string calldata urlOrAlias, uint256 blockNumber)
        external
        returns (uint256 forkId);

    function selectFork(uint256 forkId) external;

    function activeFork() external returns (uint256);

    // ── JSON lane ─────────────────────────────────────────────────────────
    // Consumer: the release-metadata port reads the release manifest's
    // declared values out of its JSON document.

    function parseJson(string calldata json, string calldata key) external pure returns (bytes memory);

    function parseJsonString(string calldata json, string calldata key)
        external
        pure
        returns (string memory);

    function parseJsonUint(string calldata json, string calldata key) external pure returns (uint256);

    // ── Hex lanes ─────────────────────────────────────────────────────────
    // Consumers: the sentinel/fail-closed and NoFePrefix ports render raw
    // revert payloads and stream bytes as `0x…` hex in failure messages.

    function toString(bytes32 value) external pure returns (string memory);

    function toString(bytes calldata value) external pure returns (string memory);
}
