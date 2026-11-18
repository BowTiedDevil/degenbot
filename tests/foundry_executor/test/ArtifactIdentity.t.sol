// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {TestBase} from "../src/TestBase.sol";

/// Artifact identity, asserted INSIDE the suite.
///
/// `build_artifacts.py --check-release` proves at build time that the fresh
/// compile is the released bytecode, and `test/ReleaseMetadata.t.sol` proves the
/// committed release ABI/manifest/grammar are self-consistent. Neither says, in
/// a test, that the artifacts THIS suite deploys are those artifacts. These
/// cases close that loop from the files on disk:
///
///   - every artifact file matches the hash its own manifest recorded (the
///     per-artifact stamp the cache validates cold);
///   - the lane's compiled `CmdExecutor` creation and runtime bytes hash to the
///     values the manifest records AND the committed release `.hex` files
///     decode to those same hashes, so the artifact under test is the released
///     one;
///   - the deployed executor's code is the artifact runtime with only the
///     documented immutable windows substituted.
///
/// Read-only against `./artifacts` and the committed release directory, both
/// granted in `foundry.toml`; nothing is copied into the lane.
contract ArtifactIdentityTest is TestBase {
    /// Every product `build_artifacts.py` emits, in the manifest's own order.
    string[10] internal ARTIFACTS = [
        "CmdExecutor",
        "FakeErc20",
        "FakeWeth",
        "FakeUniswapV2Pair",
        "FakeUniswapV3Pool",
        "FakeUniswapV4PoolManager",
        "EchoCallee",
        "ReenteringDestination",
        "TestHarness",
        "ExttloadComparator"
    ];

    /// Vyper's immutable names in the released executor.
    string[7] internal EXECUTOR_IMMUTABLES = [
        "OWNER_ADDR",
        "WETH_ADDR",
        "POOL_MANAGER_ADDR",
        "USDC_ADDR",
        "USDT_ADDR",
        "WETH_DELTA_SLOT",
        "NATIVE_DELTA_SLOT"
    ];

    string internal constant MANIFEST = "./artifacts/manifest.json";
    string internal constant RELEASE = "../../rust/crates/foundation/degenbot-executor/releases/v2/artifacts";

    // ── Helpers ───────────────────────────────────────────────────────────

    /// `vm.toString(bytes32)` renders lowercase hex with a `0x` prefix; the
    /// manifests carry bare lowercase hex, so compare the tail.
    function _hexTail(bytes32 value) internal pure returns (string memory) {
        bytes memory rendered = bytes(vm.toString(value));
        bytes memory tail = new bytes(rendered.length - 2);
        for (uint256 i; i < tail.length; ++i) {
            tail[i] = rendered[i + 2];
        }
        return string(tail);
    }

    function _decodeHex(string memory text) internal pure returns (bytes memory out) {
        bytes memory input = bytes(text);
        uint256 start;
        if (input.length >= 2 && input[0] == "0" && (input[1] == "x" || input[1] == "X")) {
            start = 2;
        }
        // A trailing newline (the release files end with one) is not data.
        uint256 end = input.length;
        while (end > start && (input[end - 1] == "\n" || input[end - 1] == "\r")) {
            --end;
        }
        require((end - start) % 2 == 0, "hex text has an odd digit count");
        out = new bytes((end - start) / 2);
        for (uint256 i; i < out.length; ++i) {
            out[i] = bytes1((_nibble(input[start + 2 * i]) << 4) | _nibble(input[start + 2 * i + 1]));
        }
    }

    function _nibble(bytes1 c) internal pure returns (uint8) {
        uint8 v = uint8(c);
        if (v >= 48 && v <= 57) return v - 48;
        if (v >= 97 && v <= 102) return v - 87;
        if (v >= 65 && v <= 70) return v - 55;
        revert("not a hex digit");
    }

    // ── Cases ─────────────────────────────────────────────────────────────

    /// Every artifact file, and the code inside it, is what the manifest says
    /// it is.
    function testLaneArtifactsMatchTheirManifest() public {
        string memory manifest = vm.readFile(MANIFEST);
        for (uint256 i; i < ARTIFACTS.length; ++i) {
            string memory name = ARTIFACTS[i];
            string memory prefix = string.concat(".", name, ".");

            assertEq(
                _hexTail(sha256(bytes(_artifact(name)))),
                vm.parseJsonString(
                    manifest, string.concat(prefix, "artifact_sha256")
                ),
                string.concat(name, ": artifact file hash")
            );
            assertEq(
                _hexTail(sha256(vm.parseJsonBytes(_artifact(name), ".bytecode"))),
                vm.parseJsonString(manifest, string.concat(prefix, "bytecode_sha256")),
                string.concat(name, ": creation bytecode hash")
            );
            assertEq(
                _hexTail(sha256(_runtime(name))),
                vm.parseJsonString(manifest, string.concat(prefix, "runtime_sha256")),
                string.concat(name, ": runtime bytecode hash")
            );
        }
    }

    /// The lane's executor artifact is the RELEASED executor: the committed
    /// release creation/runtime `.hex` files decode to the hashes the lane
    /// manifest recorded for the artifact this suite deploys.
    function testExecutorArtifactIsTheReleasedBytecode() public {
        string memory manifest = vm.readFile(MANIFEST);

        bytes memory releasedCreation =
            _decodeHex(vm.readFile(string.concat(RELEASE, "/cmd_executor.creation.hex")));
        bytes memory releasedRuntime =
            _decodeHex(vm.readFile(string.concat(RELEASE, "/cmd_executor.runtime.hex")));

        assertGt(releasedCreation.length, 0, "release creation hex decoded");
        assertGt(releasedRuntime.length, 0, "release runtime hex decoded");

        assertEq(
            _hexTail(sha256(releasedCreation)),
            vm.parseJsonString(manifest, ".CmdExecutor.bytecode_sha256"),
            "release creation bytecode is the lane artifact's"
        );
        assertEq(
            _hexTail(sha256(releasedRuntime)),
            vm.parseJsonString(manifest, ".CmdExecutor.runtime_sha256"),
            "release runtime bytecode is the lane artifact's"
        );
        assertEq(
            keccak256(releasedRuntime),
            keccak256(_runtime("CmdExecutor")),
            "release runtime bytes are the artifact's runtime bytes"
        );
    }

    /// The deployed executor's code is exactly the artifact runtime followed by
    /// the documented immutable section: every runtime byte is identical, the
    /// section is the size the release's immutable table declares, each window
    /// is filled, and the owner window holds the deploying address.
    function testDeployedExecutorMatchesArtifactOutsideImmutableWindows() public {
        // A fresh deployment with distinct immutable values: the claim is about
        // the code shape and the immutable section, not the fixture's addresses.
        address deployedExecutor = _deploy(
            "CmdExecutor",
            abi.encode(address(0x1111), address(0x2222), address(0x3333), address(0x4444))
        );
        bytes memory deployed = deployedExecutor.code;
        bytes memory runtime = _runtime("CmdExecutor");

        string memory immutables = vm.readFile(string.concat(RELEASE, "/cmd_executor.immutables.json"));
        uint256 section;
        uint256 ownerOffset;
        for (uint256 i; i < EXECUTOR_IMMUTABLES.length; ++i) {
            string memory prefix = string.concat(".", EXECUTOR_IMMUTABLES[i], ".");
            uint256 offset = vm.parseJsonUint(immutables, string.concat(prefix, "offset"));
            uint256 length = vm.parseJsonUint(immutables, string.concat(prefix, "length"));
            if (keccak256(bytes(EXECUTOR_IMMUTABLES[i])) == keccak256("OWNER_ADDR")) {
                ownerOffset = offset;
            }
            if (offset + length > section) section = offset + length;
        }
        assertGt(section, 0, "immutable section has a size");

        // The artifact's runtime is the code region alone; the deployer appends
        // the substituted immutable section after it.
        assertEq(deployed.length, runtime.length + section, "deployed code is runtime + immutables");
        for (uint256 i; i < runtime.length; ++i) {
            assertEq(deployed[i], runtime[i], "runtime byte differs from the artifact's");
        }

        // Each declared window is filled, and the owner window holds this test
        // contract — the executor's deployer and therefore its owner.
        for (uint256 i; i < EXECUTOR_IMMUTABLES.length; ++i) {
            string memory prefix = string.concat(".", EXECUTOR_IMMUTABLES[i], ".");
            uint256 offset = vm.parseJsonUint(immutables, string.concat(prefix, "offset"));
            uint256 length = vm.parseJsonUint(immutables, string.concat(prefix, "length"));
            bytes32 window;
            for (uint256 j; j < length; ++j) {
                window |= bytes32(deployed[runtime.length + offset + j]) >> (8 * j);
            }
            assertTrue(window != bytes32(0), string.concat(EXECUTOR_IMMUTABLES[i], " window is filled"));
        }
        bytes32 ownerWindow;
        for (uint256 j; j < 32; ++j) {
            ownerWindow |= bytes32(deployed[runtime.length + ownerOffset + j]) >> (8 * j);
        }
        assertEq(
            ownerWindow,
            bytes32(uint256(uint160(address(this)))),
            "owner immutable is the deployer"
        );
    }
}
