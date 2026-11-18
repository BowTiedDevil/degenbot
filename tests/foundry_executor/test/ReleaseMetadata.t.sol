// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {TestBase, ExecutorFixture} from "../src/TestBase.sol";
import {Cmd} from "../src/Cmd.sol";
import {ExecutorErrors} from "../src/generated/Errors.sol";

/// Release identity of the built-in `cmd_executor`, composed from the committed
/// release tree and the deployed contract.
///
/// The four getters are the on-chain half of the release identity: a deployment
/// declares its release tag, protocol major, whole call-encoding version, and
/// the commitment to its capability ID list. The complete encoding description
/// (command stream, packed config, execute ABI) and the capability list itself
/// live in the off-chain release manifest, and artifact integrity stays with the
/// release artifacts' own manifest.
///
/// Both sides are pinned: the deployed contract is the authority for what it
/// declares, the committed manifest must agree with it, and the capability
/// commitment is recomputed from three independent declarations — the source's
/// `CAPABILITY_*` constants, the manifest's `ids`, and its `commitment`.
/// Changing any one of them is a release change, not a refresh.
///
/// The suite reads the committed tree rather than the lane's freshly built
/// artifacts: the lane's `--check-release` step already proves the compile is
/// byte-identical to the release, so a read of the committed files here stays
/// an assertion about the released identity instead of a self-comparison.
abstract contract ReleaseArtifacts is TestBase {
    string internal constant RELEASE_DIR =
        "../../rust/crates/foundation/degenbot-executor/releases/v2";

    /// The ASCII release tag right-padded to bytes32. The tag *is* the identity,
    /// so an operator can read a deployment's release without a lookup table.
    bytes32 internal constant RELEASE_ID = "cmd_executor/2";
    uint32 internal constant PROTOCOL_MAJOR = 1;
    uint32 internal constant CALL_ENCODING_VERSION = 1;

    /// The preimage prefix every capability commitment is derived under.
    string internal constant COMMITMENT_SCHEME = "degenbot/cmd_executor/capabilities/v1";

    /// keccak256(COMMITMENT_SCHEME ++ abi.encode(uint32[] ids)), pinned as a
    /// golden so the derivation stays checkable from any ABI toolchain.
    bytes32 internal constant CAPABILITIES_HASH =
        0x9cce6a2d69dbea3b4023e117492ff11aadc2e3feadb28a83faa172fc0823bcbf;

    uint256 internal constant CAPABILITY_COUNT = 15;

    // ── Committed release access ──────────────────────────────────────────

    function _abiJson() internal view returns (string memory) {
        return vm.readFile(string.concat(RELEASE_DIR, "/artifacts/cmd_executor.abi.json"));
    }

    function _methodIdentifiersJson() internal view returns (string memory) {
        return vm.readFile(
            string.concat(RELEASE_DIR, "/artifacts/cmd_executor.method_identifiers.json")
        );
    }

    function _manifestJson() internal view returns (string memory) {
        return vm.readFile(string.concat(RELEASE_DIR, "/manifest.json"));
    }

    function _releaseSource() internal view returns (string memory) {
        return vm.readFile(string.concat(RELEASE_DIR, "/contract.vy"));
    }

    // ── JSON access ───────────────────────────────────────────────────────
    // A missing path is a drift failure with the path named, never a silent
    // zero: the committed artifacts are the authority this suite reads.

    function _jsonString(string memory json, string memory path)
        internal
        pure
        returns (string memory)
    {
        try vm.parseJsonString(json, path) returns (string memory value) {
            return value;
        } catch {
            revert(string.concat("committed artifact has no string at JSON path ", path));
        }
    }

    function _jsonUint(string memory json, string memory path) internal pure returns (uint256) {
        try vm.parseJsonUint(json, path) returns (uint256 value) {
            return value;
        } catch {
            revert(string.concat("committed artifact has no number at JSON path ", path));
        }
    }

    function _jsonHasString(string memory json, string memory path) internal pure returns (bool) {
        try vm.parseJsonString(json, path) returns (string memory) {
            return true;
        } catch {
            return false;
        }
    }

    // ── Text / hex helpers ────────────────────────────────────────────────

    function _strEq(string memory a, string memory b) internal pure returns (bool) {
        return keccak256(bytes(a)) == keccak256(bytes(b));
    }

    function _hexNibble(bytes1 character) internal pure returns (uint8) {
        uint8 value = uint8(character);
        if (value >= 0x30 && value <= 0x39) return value - 0x30;
        if (value >= 0x61 && value <= 0x66) return value - 0x61 + 10;
        if (value >= 0x41 && value <= 0x46) return value - 0x41 + 10;
        revert(string.concat("not a hex digit: ", vm.toString(bytes.concat(character))));
    }

    /// Parse a `0x`-prefixed hex string of any width up to 8 digits.
    function _hexToUint(string memory text) internal pure returns (uint256 value) {
        bytes memory raw = bytes(text);
        uint256 start = 0;
        if (raw.length >= 2 && raw[0] == "0" && (raw[1] == "x" || raw[1] == "X")) start = 2;
        for (uint256 i = start; i < raw.length; i++) {
            value = (value << 4) | uint256(_hexNibble(raw[i]));
        }
    }

    function _hexToBytes32(string memory text) internal pure returns (bytes32) {
        bytes memory raw = bytes(text);
        if (raw.length != 66) revert(string.concat("expected a 0x-prefixed bytes32, got ", text));
        uint256 value;
        for (uint256 i = 2; i < 66; i++) {
            value = (value << 4) | uint256(_hexNibble(raw[i]));
        }
        return bytes32(value);
    }

    /// The NUL-padded tag decoded back to the ASCII string it carries.
    function _stripTrailingNul(bytes32 value) internal pure returns (string memory) {
        bytes memory raw = abi.encodePacked(value);
        uint256 length = raw.length;
        while (length > 0 && raw[length - 1] == 0) length--;
        bytes memory out = new bytes(length);
        for (uint256 i = 0; i < length; i++) out[i] = raw[i];
        return string(out);
    }

    function _slice(string memory text, uint256 start, uint256 end)
        internal
        pure
        returns (string memory)
    {
        bytes memory raw = bytes(text);
        bytes memory out = new bytes(end - start);
        for (uint256 i = start; i < end; i++) out[i - start] = raw[i];
        return string(out);
    }

    /// Split `name(type,type)` into its name and its argument type list.
    function _signatureTypes(string memory signature)
        internal
        pure
        returns (string memory name, string[] memory types)
    {
        bytes memory raw = bytes(signature);
        uint256 open = raw.length;
        for (uint256 i = 0; i < raw.length; i++) {
            if (raw[i] == "(") {
                open = i;
                break;
            }
        }
        name = _slice(signature, 0, open);
        uint256 close = raw.length;
        for (uint256 i = open + 1; i < raw.length; i++) {
            if (raw[i] == ")") {
                close = i;
                break;
            }
        }
        string memory inner = close > open + 1 ? _slice(signature, open + 1, close) : "";
        types = _splitComma(inner);
    }

    function _splitComma(string memory text) internal pure returns (string[] memory parts) {
        bytes memory raw = bytes(text);
        if (raw.length == 0) return new string[](0);
        uint256 count = 1;
        for (uint256 i = 0; i < raw.length; i++) {
            if (raw[i] == ",") count++;
        }
        parts = new string[](count);
        uint256 start = 0;
        uint256 index = 0;
        for (uint256 i = 0; i <= raw.length; i++) {
            if (i == raw.length || raw[i] == ",") {
                parts[index] = _slice(text, start, i);
                index++;
                start = i + 1;
            }
        }
    }

    // ── Vyper source scanning ─────────────────────────────────────────────
    // The source suite reads the release's pinned Vyper file directly: the
    // `CAPABILITY_*` constants and `CAPABILITIES_HASH` are auditable
    // declarations of their own, not derived from the manifest they must agree
    // with.

    function _matchesAt(bytes memory text, uint256 at, bytes memory needle)
        internal
        pure
        returns (bool)
    {
        if (at + needle.length > text.length) return false;
        for (uint256 i = 0; i < needle.length; i++) {
            if (text[at + i] != needle[i]) return false;
        }
        return true;
    }

    function _indexOf(bytes memory haystack, uint256 from, uint256 to, bytes memory needle)
        internal
        pure
        returns (uint256)
    {
        if (needle.length == 0 || to > haystack.length || needle.length > to - from) {
            return type(uint256).max;
        }
        for (uint256 i = from; i + needle.length <= to; i++) {
            if (_matchesAt(haystack, i, needle)) return i;
        }
        return type(uint256).max;
    }

    function _lineEnd(bytes memory text, uint256 start) internal pure returns (uint256) {
        uint256 index = start;
        while (index < text.length && text[index] != 0x0a) index++;
        return index;
    }

    /// Every line-leading `CAPABILITY_<NAME>: constant(uint32) = <id>` value, in
    /// declaration order.
    function _sourceCapabilityIds(string memory source) internal pure returns (uint32[] memory ids) {
        bytes memory raw = bytes(source);
        bytes memory declaration = bytes(": constant(uint32) = ");
        uint32[] memory found = new uint32[](64);
        uint256 count;

        for (uint256 i = 0; i < raw.length; i++) {
            if (i != 0 && raw[i - 1] != 0x0a) continue;
            if (!_matchesAt(raw, i, bytes("CAPABILITY_"))) continue;
            uint256 end = _lineEnd(raw, i);
            uint256 marker = _indexOf(raw, i, end, declaration);
            if (marker == type(uint256).max) continue;
            uint256 value;
            uint256 j = marker + declaration.length;
            while (j < end && raw[j] >= 0x30 && raw[j] <= 0x39) {
                value = value * 10 + (uint8(raw[j]) - 0x30);
                j++;
            }
            found[count] = uint32(value);
            count++;
        }

        ids = new uint32[](count);
        for (uint256 i = 0; i < count; i++) ids[i] = found[i];
    }

    /// The line-leading `CAPABILITIES_HASH: constant(bytes32) = 0x…` value.
    function _sourceCapabilitiesHash(string memory source)
        internal
        pure
        returns (bytes32 hash, bool found)
    {
        bytes memory raw = bytes(source);
        for (uint256 i = 0; i < raw.length; i++) {
            if (i != 0 && raw[i - 1] != 0x0a) continue;
            if (!_matchesAt(raw, i, bytes("CAPABILITIES_HASH"))) continue;
            uint256 end = _lineEnd(raw, i);
            uint256 marker = _indexOf(raw, i, end, bytes("0x"));
            if (marker == type(uint256).max || marker + 66 > end) continue;
            uint256 value;
            for (uint256 j = marker + 2; j < marker + 66; j++) {
                value = (value << 4) | uint256(_hexNibble(raw[j]));
            }
            return (bytes32(value), true);
        }
        return (bytes32(0), false);
    }

    // ── Commitment / declared ID list ─────────────────────────────────────

    function _capabilities() internal pure returns (uint32[] memory ids) {
        ids = new uint32[](CAPABILITY_COUNT);
        for (uint256 i = 0; i < CAPABILITY_COUNT; i++) ids[i] = uint32(i + 1);
    }

    function _commitmentFor(uint32[] memory ids) internal pure returns (bytes32) {
        return keccak256(bytes.concat(bytes(COMMITMENT_SCHEME), abi.encode(ids)));
    }

    function _manifestCapabilityIds(string memory manifest)
        internal
        pure
        returns (uint32[] memory ids)
    {
        uint32[] memory found = new uint32[](64);
        uint256 count;
        while (true) {
            string memory path =
                string.concat(".capabilities.ids[", vm.toString(count), "].id");
            try vm.parseJsonUint(manifest, path) returns (uint256 value) {
                found[count] = uint32(value);
            } catch {
                break;
            }
            count++;
        }
        ids = new uint32[](count);
        for (uint256 i = 0; i < count; i++) ids[i] = found[i];
    }

    // ── ABI entry access ──────────────────────────────────────────────────

    function _abiEntryCount(string memory abi) internal pure returns (uint256 count) {
        while (_jsonHasString(abi, string.concat("[", vm.toString(count), "].type"))) count++;
    }

    function _abiEntryType(string memory abi, uint256 index) internal pure returns (string memory) {
        return _jsonString(abi, string.concat("[", vm.toString(index), "].type"));
    }

    function _abiCountOfType(string memory abi, string memory entryType)
        internal
        pure
        returns (uint256 count)
    {
        uint256 entries = _abiEntryCount(abi);
        for (uint256 i = 0; i < entries; i++) {
            if (_strEq(_abiEntryType(abi, i), entryType)) count++;
        }
    }

    function _abiIndexByName(string memory abi, string memory name)
        internal
        pure
        returns (uint256 index)
    {
        uint256 entries = _abiEntryCount(abi);
        for (uint256 i = 0; i < entries; i++) {
            if (!_strEq(_abiEntryType(abi, i), "function")) continue;
            string memory namePath = string.concat("[", vm.toString(i), "].name");
            if (!_jsonHasString(abi, namePath)) continue;
            if (_strEq(_jsonString(abi, namePath), name)) return i;
        }
        revert(string.concat("the committed ABI carries no function named ", name));
    }

    /// `name(type,type)` for an ABI function or error entry.
    function _abiSignature(string memory abi, uint256 index) internal pure returns (string memory) {
        string memory prefix = string.concat("[", vm.toString(index), "]");
        string memory inputs;
        for (uint256 i = 0; ; i++) {
            string memory path = string.concat(prefix, ".inputs[", vm.toString(i), "].type");
            if (!_jsonHasString(abi, path)) break;
            inputs = i == 0 ? _jsonString(abi, path) : string.concat(inputs, ",", _jsonString(abi, path));
        }
        return string.concat(_jsonString(abi, string.concat(prefix, ".name")), "(", inputs, ")");
    }

    /// Comma-joined declared output types (empty when the entry declares none).
    function _abiOutputTypes(string memory abi, uint256 index) internal pure returns (string memory) {
        string memory prefix = string.concat("[", vm.toString(index), "]");
        string memory outputs;
        for (uint256 i = 0; ; i++) {
            string memory path = string.concat(prefix, ".outputs[", vm.toString(i), "].type");
            if (!_jsonHasString(abi, path)) break;
            outputs = i == 0
                ? _jsonString(abi, path)
                : string.concat(outputs, ",", _jsonString(abi, path));
        }
        return outputs;
    }
}

/// The executor-side view of the release: a deployed fixture plus the manifest's
/// declared grammar that the suite encodes against.
abstract contract ReleaseMetadataBase is ReleaseArtifacts, ExecutorFixture {
    struct Sample {
        string name;
        bytes data;
    }

    /// Field type → wire width in bytes. `bytes` is variable: its uint8 length
    /// prefix is a declared field of its own, so it contributes 0 here.
    function _fieldWidth(
        string memory fieldType,
        string memory opcodeName,
        string memory fieldPath,
        uint256 fieldIndex
    ) internal pure returns (uint256) {
        bytes32 tag = keccak256(bytes(fieldType));
        if (tag == keccak256("uint8")) return 1;
        if (tag == keccak256("bool")) return 1;
        if (tag == keccak256("uint16")) return 2;
        if (tag == keccak256("int16")) return 2;
        if (tag == keccak256("uint96")) return 12;
        if (tag == keccak256("uint256")) return 32;
        if (tag == keccak256("address")) return 20;
        if (tag == keccak256("bytes")) return 0;
        revert(
            string.concat(
                "opcode ",
                opcodeName,
                " ",
                fieldPath,
                "[",
                vmToString(fieldIndex),
                "] declares type ",
                fieldType,
                ", which the suite's width table does not carry"
            )
        );
    }

    function _declaredFieldWidths(
        string memory manifest,
        string memory opcodePath,
        string memory fieldPath,
        string memory opcodeName
    ) internal pure returns (uint256 width, string memory summary) {
        for (uint256 i = 0; ; i++) {
            string memory path =
                string.concat(opcodePath, ".", fieldPath, "[", vmToString(i), "].type");
            if (!_jsonHasString(manifest, path)) break;
            string memory fieldType = _jsonString(manifest, path);
            width += _fieldWidth(fieldType, opcodeName, fieldPath, i);
            summary = i == 0 ? fieldType : string.concat(summary, ",", fieldType);
        }
    }

    function _declaredOpcodeWidth(
        string memory manifest,
        string memory opcodePath,
        string memory opcodeName
    ) internal pure returns (uint256 width, string memory summary) {
        uint256 fieldsWidth;
        string memory fieldsSummary;
        (fieldsWidth, fieldsSummary) =
            _declaredFieldWidths(manifest, opcodePath, "fields", opcodeName);
        width = 1 + fieldsWidth;
        summary = fieldsSummary;
        if (_jsonHasString(manifest, string.concat(opcodePath, ".repeat.entry.fields[0].type"))) {
            uint256 repeatWidth;
            string memory repeatSummary;
            (repeatWidth, repeatSummary) =
                _declaredFieldWidths(manifest, opcodePath, "repeat.entry.fields", opcodeName);
            width += repeatWidth;
            summary = string.concat(summary, " + repeat(", repeatSummary, ")");
        }
    }

    function _declaredOpcodeCount(string memory manifest) internal pure returns (uint256 count) {
        while (
            _jsonHasString(
                manifest,
                string.concat(
                    ".call_encoding.command_stream.opcodes[", vmToString(count), "].name"
                )
            )
        ) {
            count++;
        }
    }

    function _sampleNamed(Sample[] memory samples, string memory name)
        internal
        pure
        returns (bool found, bytes memory data)
    {
        for (uint256 i = 0; i < samples.length; i++) {
            if (_strEq(samples[i].name, name)) return (true, samples[i].data);
        }
        return (false, bytes(""));
    }

    /// One encoded sample per declared opcode, with empty variable tails.
    function _samples() internal pure returns (Sample[] memory samples) {
        samples = new Sample[](31);

        Cmd.BatchSwap[] memory swaps = new Cmd.BatchSwap[](1);
        swaps[0] = Cmd.BatchSwap(1, 2, 3000, 60, 0xFF, true, 1000);
        bytes memory batch = Cmd.v4Batch(swaps);

        samples[0] = Sample(
            "SET_ADDRESS",
            Cmd.setAddress(address(uint160(0x1111111111111111111111111111111111111111)))
        );
        samples[1] = Sample("ERC20_TRANSFER", Cmd.erc20Transfer(1, 2, 1000));
        samples[2] = Sample("ERC20_XFER_BALANCE", Cmd.erc20XferBalance(1, 2));
        samples[3] = Sample("WETH_DEPOSIT", Cmd.wethDeposit(1));
        samples[4] = Sample("WETH_WITHDRAW", Cmd.wethWithdraw(1));
        samples[5] = Sample("WETH_DEPOSIT_ALL", Cmd.wethDepositAll());
        samples[6] = Sample("WETH_WITHDRAW_ALL", Cmd.wethWithdrawAll());
        samples[7] = Sample("SEND_ETH", Cmd.sendEth(1, 1000));
        samples[8] = Sample("SEND_ETH_ALL", Cmd.sendEthAll(1));
        samples[9] = Sample("V2_SWAP_COMPACT", Cmd.v2SwapCompact(1, true, 1000, 2, 30, ""));
        samples[10] = Sample("V2_SWAP_CALC", Cmd.v2SwapCalc(1, true, 2, 30));
        samples[11] = Sample("V2_SWAP_DIRECT", Cmd.v2SwapDirect(1, true, 1000, 2));
        // The direction-baked 0x33/0x34 forms come from the shared V3 helper, so
        // the raw 0x30 compact form is built through its own encoder: the
        // direction travels in-band as a flag byte, one byte wider.
        samples[12] = Sample("V3_SWAP_COMPACT", Cmd.v3SwapCompactRaw(1, true, 1000, 2, ""));
        samples[13] = Sample("V3_SWAP_DELTA", Cmd.v3SwapDelta(1, true, 2));
        samples[14] = Sample("V3_SWAP_ONE_FOR_ZERO", Cmd.v3SwapCompact(1, false, 1000, 2, ""));
        samples[15] = Sample("V3_SWAP_ZERO_FOR_ONE", Cmd.v3SwapCompact(1, true, 1000, 2, ""));
        samples[16] = Sample("V4_SWAP_COMPACT", Cmd.v4SwapCompact(1, 2, 3000, 60, 0xFF, true, 1000));
        samples[17] = Sample("V4_SWAP_DYNAMIC", Cmd.v4SwapDynamic(1, 2, 3000, 60, 0xFF, true));
        samples[18] = Sample("V4_BATCH", batch);
        samples[19] = Sample("V4_BATCH_OPEN_WETH", Cmd.v4BatchOpenWeth(swaps));
        samples[20] = Sample("V4_UNLOCK", Cmd.v4Unlock(""));
        samples[21] = Sample("V4_TAKE", Cmd.v4Take(1, 2, 1000));
        samples[22] = Sample("V4_TAKE_COMPACT", Cmd.v4TakeCompact(1, 2, 1000));
        samples[23] = Sample("V4_TAKE_DELTA", Cmd.v4TakeDelta(1, 2));
        samples[24] = Sample("V4_SYNC", Cmd.v4Sync(1));
        samples[25] = Sample("V4_SETTLE", Cmd.v4Settle());
        samples[26] = Sample("V4_SETTLE_DELTA", Cmd.v4SettleDelta(1));
        samples[27] = Sample("V4_SETTLE_ALL", Cmd.v4SettleAll());
        samples[28] = Sample("V4_MINT_COMPACT", Cmd.v4MintCompact(1, 2, 1000));
        samples[29] = Sample("V4_BURN_COMPACT", Cmd.v4BurnCompact(1, 1000));
        samples[30] = Sample(
            "BEGIN_EXECUTION",
            Cmd.preamble(Cmd.newTable(address(0), address(0), address(0)))
        );
    }
}

/// The deployed contract answers with the declared current values.
contract ReleaseDeclaredValuesTest is ReleaseMetadataBase {
    function testReleaseId() public view {
        assertEq(executor.release_id(), RELEASE_ID, "release_id");
    }

    function testProtocolMajor() public view {
        assertEq(uint256(executor.protocol_major()), uint256(PROTOCOL_MAJOR), "protocol_major");
    }

    function testCallEncodingVersion() public view {
        assertEq(
            uint256(executor.call_encoding_version()),
            uint256(CALL_ENCODING_VERSION),
            "call_encoding_version"
        );
    }

    function testCapabilitiesHash() public view {
        assertEq(executor.capabilities_hash(), CAPABILITIES_HASH, "capabilities_hash");
    }

    function testReleaseIdIsANulPaddedAsciiTag() public view {
        bytes memory raw = abi.encodePacked(executor.release_id());
        uint256 length = raw.length;
        while (length > 0 && raw[length - 1] == 0) length--;
        assertTrue(length > 0, "release id must not be empty");
        for (uint256 i = 0; i < length; i++) {
            uint8 byteValue = uint8(raw[i]);
            assertTrue(
                byteValue >= 0x20 && byteValue < 0x7f,
                string.concat("release id byte ", vmToString(i), " is not printable ASCII")
            );
        }
        for (uint256 i = length; i < raw.length; i++) {
            assertEq(
                uint256(uint8(raw[i])),
                0,
                string.concat("release id byte ", vmToString(i), " is not NUL padding")
            );
        }
    }
}

/// Every getter is a zero-input `view` function in the committed ABI.
contract ReleaseGetterShapeTest is ReleaseMetadataBase {
    function _assertGetterShape(string memory abi, string memory name, string memory outputType)
        internal
        pure
    {
        uint256 index = _abiIndexByName(abi, name);
        string memory prefix = string.concat("[", vmToString(index), "]");

        assertEq(
            _jsonString(abi, string.concat(prefix, ".stateMutability")),
            "view",
            string.concat(name, " state mutability")
        );
        assertFalse(
            _jsonHasString(abi, string.concat(prefix, ".inputs[0].type")),
            string.concat(name, " must take no inputs")
        );
        assertEq(
            _jsonString(abi, string.concat(prefix, ".outputs[0].type")),
            outputType,
            string.concat(name, " output type")
        );
        assertFalse(
            _jsonHasString(abi, string.concat(prefix, ".outputs[1].type")),
            string.concat(name, " declares more than one output")
        );
    }

    function testReleaseIdGetterIsAZeroInputViewFunction() public view {
        _assertGetterShape(_abiJson(), "release_id", "bytes32");
    }

    function testProtocolMajorGetterIsAZeroInputViewFunction() public view {
        _assertGetterShape(_abiJson(), "protocol_major", "uint32");
    }

    function testCallEncodingVersionGetterIsAZeroInputViewFunction() public view {
        _assertGetterShape(_abiJson(), "call_encoding_version", "uint32");
    }

    function testCapabilitiesHashGetterIsAZeroInputViewFunction() public view {
        _assertGetterShape(_abiJson(), "capabilities_hash", "bytes32");
    }

    function testCapabilitiesHashGetterIsAZeroInputViewBytes32() public view {
        // The ABI, not only the source: an operator reads the shape from the
        // committed artifact the deployment was built from.
        string memory abi = _abiJson();
        _assertGetterShape(abi, "capabilities_hash", "bytes32");
        uint256 index = _abiIndexByName(abi, "capabilities_hash");
        assertEq(
            _jsonString(abi, string.concat("[", vmToString(index), "].outputs[0].name")),
            "",
            "capabilities_hash output name must be empty"
        );
    }
}

/// The committed release manifest declares exactly what the contract does.
contract ReleaseManifestAgreementTest is ReleaseMetadataBase {
    function testReleaseTag() public view {
        assertEq(
            _jsonString(_manifestJson(), ".release_id"), "cmd_executor/2", "manifest release_id"
        );
    }

    function testSchemaVersion() public view {
        assertEq(_jsonUint(_manifestJson(), ".schema_version"), 2, "manifest schema_version");
    }

    function testManifestProtocolMajor() public view {
        assertEq(
            _jsonUint(_manifestJson(), ".protocol.major"),
            uint256(PROTOCOL_MAJOR),
            "manifest protocol.major"
        );
    }

    function testManifestCallEncodingVersion() public view {
        assertEq(
            _jsonUint(_manifestJson(), ".call_encoding.version"),
            uint256(CALL_ENCODING_VERSION),
            "manifest call_encoding.version"
        );
    }

    function testCapabilityIds() public view {
        uint32[] memory declared = _manifestCapabilityIds(_manifestJson());
        uint32[] memory expected = _capabilities();

        assertEq(declared.length, expected.length, "manifest capability count");
        for (uint256 i = 0; i < declared.length; i++) {
            assertEq(
                uint256(declared[i]),
                uint256(expected[i]),
                string.concat("manifest capability id at index ", vmToString(i))
            );
        }
        for (uint256 i = 1; i < declared.length; i++) {
            assertGt(
                uint256(declared[i]),
                uint256(declared[i - 1]),
                "capability IDs must be unique and ascending"
            );
        }
    }

    function testCapabilityScheme() public view {
        assertEq(
            _jsonString(_manifestJson(), ".capabilities.scheme"),
            COMMITMENT_SCHEME,
            "manifest capabilities.scheme"
        );
    }

    function testCapabilityCommitmentMatchesTheSourceDeclaration() public view {
        // The three-way drift gate: the auditable `CAPABILITY_*` constants in
        // the pinned Vyper source, the manifest's ID list, and the manifest's
        // commitment all name one set.
        string memory manifest = _manifestJson();
        string memory source = _releaseSource();
        uint32[] memory sourceIds = _sourceCapabilityIds(source);
        (bytes32 declaredHash, bool found) = _sourceCapabilitiesHash(source);
        assertTrue(found, "the source declares no CAPABILITIES_HASH constant");

        uint32[] memory expected = _capabilities();
        assertEq(
            sourceIds.length,
            expected.length,
            "the source's CAPABILITY_* count drifted from the manifest's IDs"
        );
        for (uint256 i = 0; i < sourceIds.length; i++) {
            assertEq(
                uint256(sourceIds[i]),
                uint256(expected[i]),
                string.concat(
                    "CAPABILITY_* value at index ", vmToString(i), " drifted from the manifest's IDs"
                )
            );
        }
        for (uint256 i = 1; i < sourceIds.length; i++) {
            assertGt(
                uint256(sourceIds[i]),
                uint256(sourceIds[i - 1]),
                "the source's CAPABILITY_* list must be ascending"
            );
        }

        bytes32 derived = _commitmentFor(sourceIds);
        assertEq(
            declaredHash,
            derived,
            "the source's CAPABILITIES_HASH is not the commitment its own ID constants derive"
        );

        uint32[] memory manifestIds = _manifestCapabilityIds(manifest);
        assertEq(
            manifestIds.length,
            sourceIds.length,
            "the manifest's ID list length drifted from the source's"
        );
        for (uint256 i = 0; i < sourceIds.length; i++) {
            assertEq(
                uint256(manifestIds[i]),
                uint256(sourceIds[i]),
                string.concat(
                    "manifest capability id at index ", vmToString(i), " drifted from the source"
                )
            );
        }

        assertEq(
            _hexToBytes32(_jsonString(manifest, ".capabilities.commitment")),
            derived,
            "the manifest's commitment is not the commitment its IDs derive"
        );
        assertEq(derived, CAPABILITIES_HASH, "the derived commitment drifted from the pinned golden");
    }

    function testManifestCommitmentIsReproducibleFromItsIds() public view {
        string memory manifest = _manifestJson();
        assertEq(
            _hexToBytes32(_jsonString(manifest, ".capabilities.commitment")),
            _commitmentFor(_manifestCapabilityIds(manifest)),
            "the manifest's commitment must be reproducible from its declared IDs"
        );
    }

    function testExecuteAbiIsTheDeclaredOne() public view {
        string memory manifest = _manifestJson();
        assertEq(
            _jsonString(manifest, ".call_encoding.execute_abi.function"),
            "execute",
            "declared execute function name"
        );
        (string memory name, string[] memory types) =
            _signatureTypes(_jsonString(manifest, ".call_encoding.execute_abi.signature"));
        assertEq(name, "execute", "execute signature name");
        assertEq(types.length, 2, "execute signature arity");
        assertEq(types[0], "bytes", "execute signature parameter 0");
        assertEq(types[1], "uint256", "execute signature parameter 1");
    }

    function testManifestAgreesWithTheDeployedContract() public view {
        string memory manifest = _manifestJson();
        assertEq(
            _stripTrailingNul(executor.release_id()),
            _jsonString(manifest, ".release_id"),
            "deployed release_id vs manifest"
        );
        assertEq(
            uint256(executor.protocol_major()),
            _jsonUint(manifest, ".protocol.major"),
            "deployed protocol_major vs manifest"
        );
        assertEq(
            uint256(executor.call_encoding_version()),
            _jsonUint(manifest, ".call_encoding.version"),
            "deployed call_encoding_version vs manifest"
        );
        bytes32 commitment = _hexToBytes32(_jsonString(manifest, ".capabilities.commitment"));
        assertEq(
            executor.capabilities_hash(),
            commitment,
            "deployed capabilities_hash vs manifest commitment"
        );
        assertEq(
            executor.capabilities_hash(),
            _commitmentFor(_manifestCapabilityIds(manifest)),
            "deployed capabilities_hash vs recomputed commitment"
        );
    }
}

/// The manifest's grammar is the grammar this suite encodes against.
contract ReleaseDeclaredGrammarTest is ReleaseMetadataBase {
    function testDeclaredOpcodeSizesMatchTheEncodingHelpers() public view {
        string memory manifest = _manifestJson();
        string memory base = ".call_encoding.command_stream.opcodes";
        Sample[] memory samples = _samples();

        uint256 declared = _declaredOpcodeCount(manifest);
        assertEq(
            declared,
            samples.length,
            "every declared opcode needs a sample and every sample a declaration"
        );

        for (uint256 i = 0; i < declared; i++) {
            string memory opcodePath = string.concat(base, "[", vmToString(i), "]");
            string memory name = _jsonString(manifest, string.concat(opcodePath, ".name"));
            uint256 opcode =
                _hexToUint(_jsonString(manifest, string.concat(opcodePath, ".opcode")));

            (bool found, bytes memory data) = _sampleNamed(samples, name);
            assertTrue(
                found,
                string.concat("the manifest declares ", name, " but the suite has no sample for it")
            );
            if (!found) continue;

            assertEq(
                uint256(uint8(data[0])),
                opcode,
                string.concat(
                    "opcode ", name, ": the encoded sample does not start with the declared opcode"
                )
            );

            (uint256 width, string memory summary) =
                _declaredOpcodeWidth(manifest, opcodePath, name);
            assertEq(
                data.length,
                width,
                string.concat(
                    "opcode ",
                    name,
                    ": declared width ",
                    vmToString(width),
                    " from fields [",
                    summary,
                    "] != sample width ",
                    vmToString(data.length)
                )
            );
        }

        for (uint256 i = 0; i < samples.length; i++) {
            bool declaredName;
            for (uint256 j = 0; j < declared; j++) {
                string memory declaredPath = string.concat(base, "[", vmToString(j), "].name");
                if (_strEq(_jsonString(manifest, declaredPath), samples[i].name)) {
                    declaredName = true;
                    break;
                }
            }
            assertTrue(
                declaredName,
                string.concat("the suite's sample ", samples[i].name, " is not a declared opcode")
            );
        }
    }

    function testDeclaredConfigLayoutMatchesThePacker() public view {
        string memory manifest = _manifestJson();
        string memory base = ".call_encoding.config.fields";

        uint256 nextBit = 0;
        uint256 fieldCount;
        while (_jsonHasString(manifest, string.concat(base, "[", vmToString(fieldCount), "].name"))) {
            fieldCount++;
        }
        assertTrue(fieldCount > 0, "the manifest declares no config fields");

        for (uint256 i = 0; i < fieldCount; i++) {
            string memory path = string.concat(base, "[", vmToString(i), "]");
            string memory name = _jsonString(manifest, string.concat(path, ".name"));
            uint256 low = _jsonUint(manifest, string.concat(path, ".bits[0]"));
            uint256 high = _jsonUint(manifest, string.concat(path, ".bits[1]"));

            assertEq(low, nextBit, string.concat("config field ", name, " leaves a bit hole"));
            assertGe(high, low, string.concat("config field ", name, " has inverted bits"));
            nextBit = high + 1;
        }
        assertEq(nextBit, 256, "the config word must be fully described");

        uint256 packed = Cmd.makeConfig(2, 500, 1, 1000);
        for (uint256 i = 0; i < fieldCount; i++) {
            string memory path = string.concat(base, "[", vmToString(i), "]");
            string memory name = _jsonString(manifest, string.concat(path, ".name"));
            uint256 low = _jsonUint(manifest, string.concat(path, ".bits[0]"));
            uint256 high = _jsonUint(manifest, string.concat(path, ".bits[1]"));
            uint256 value = (packed >> low) & ((uint256(1) << (high - low + 1)) - 1);

            if (_strEq(name, "profit_mode")) {
                assertEq(value, 2, "packed profit_mode");
            } else if (_strEq(name, "bribe_bips")) {
                assertEq(value, 500, "packed bribe_bips");
            } else if (_strEq(name, "bribe_recipient_idx")) {
                assertEq(value, 1, "packed bribe_recipient_idx");
            } else if (_strEq(name, "min_profit")) {
                assertEq(value, 1000, "packed min_profit");
            } else {
                revert(string.concat("config field ", name, " is not one the packer covers"));
            }
        }
    }
}

/// The committed ABI is the executor's complete external surface: every callable
/// selector, both `execute` overloads, and every parameter and return type.
///
/// A hand-written interface cannot answer this — the generated `ICmdExecutor`
/// comes from the same ABI, so an interface-shaped assertion would agree by
/// construction. This reads the artifact a deployment is built from and
/// cross-checks every selector against the release's own method-identifier
/// table.
contract ExecutorAbiShapeTest is ReleaseArtifacts {
    struct FunctionShape {
        string signature;
        string mutability;
        string outputs;
    }

    uint256 internal constant EXPECTED_ENTRY_COUNT = 27;
    uint256 internal constant EXPECTED_FUNCTION_COUNT = 15;
    uint256 internal constant EXPECTED_ERROR_COUNT = 10;

    function _quote() internal pure returns (string memory) {
        return "\x22";
    }

    function _expectedFunctions() internal pure returns (FunctionShape[] memory shapes) {
        shapes = new FunctionShape[](EXPECTED_FUNCTION_COUNT);
        shapes[0] = FunctionShape("initialize()", "payable", "");
        shapes[1] = FunctionShape("release_id()", "view", "bytes32");
        shapes[2] = FunctionShape("protocol_major()", "view", "uint32");
        shapes[3] = FunctionShape("call_encoding_version()", "view", "uint32");
        shapes[4] = FunctionShape("capabilities_hash()", "view", "bytes32");
        shapes[5] = FunctionShape("withdraw(uint256,address)", "nonpayable", "");
        shapes[6] = FunctionShape("sweep(address)", "nonpayable", "");
        shapes[7] = FunctionShape("execute(bytes)", "payable", "tuple");
        shapes[8] = FunctionShape("execute(bytes,uint256)", "payable", "tuple");
        shapes[9] = FunctionShape("uniswapV2Call(address,uint256,uint256,bytes)", "payable", "");
        shapes[10] = FunctionShape("hook(address,uint256,uint256,bytes)", "payable", "");
        shapes[11] = FunctionShape("pancakeCall(address,uint256,uint256,bytes)", "payable", "");
        shapes[12] = FunctionShape("uniswapV3SwapCallback(int256,int256,bytes)", "payable", "");
        shapes[13] = FunctionShape("pancakeV3SwapCallback(int256,int256,bytes)", "payable", "");
        shapes[14] = FunctionShape("unlockCallback(bytes)", "payable", "bytes");
    }

    function _singleEntryOfType(string memory abi, string memory entryType)
        internal
        pure
        returns (uint256 index)
    {
        bool found;
        uint256 entries = _abiEntryCount(abi);
        for (uint256 i = 0; i < entries; i++) {
            if (!_strEq(_abiEntryType(abi, i), entryType)) continue;
            assertFalse(
                found, string.concat("the committed ABI declares ", entryType, " more than once")
            );
            index = i;
            found = true;
        }
        assertTrue(found, string.concat("the committed ABI declares no ", entryType));
    }

    function testCommittedAbiCarriesTheCompleteExecutorSurface() public view {
        string memory abi = _abiJson();
        string memory identifiers = _methodIdentifiersJson();
        FunctionShape[] memory expected = _expectedFunctions();
        uint256 entries = _abiEntryCount(abi);

        assertEq(entries, EXPECTED_ENTRY_COUNT, "committed ABI entry count");
        assertEq(
            _abiCountOfType(abi, "function"),
            EXPECTED_FUNCTION_COUNT,
            "committed ABI function count"
        );
        assertEq(_abiCountOfType(abi, "error"), EXPECTED_ERROR_COUNT, "committed ABI error count");
        assertEq(_abiCountOfType(abi, "fallback"), 1, "committed ABI fallback count");
        assertEq(_abiCountOfType(abi, "constructor"), 1, "committed ABI constructor count");

        for (uint256 i = 0; i < expected.length; i++) {
            FunctionShape memory shape = expected[i];

            uint256 index;
            uint256 matches;
            for (uint256 j = 0; j < entries; j++) {
                if (!_strEq(_abiEntryType(abi, j), "function")) continue;
                if (!_strEq(_abiSignature(abi, j), shape.signature)) continue;
                matches++;
                index = j;
            }
            assertEq(
                matches,
                1,
                string.concat("the committed ABI must carry ", shape.signature, " exactly once")
            );

            string memory prefix = string.concat("[", vmToString(index), "]");
            assertEq(
                _jsonString(abi, string.concat(prefix, ".stateMutability")),
                shape.mutability,
                string.concat(shape.signature, " state mutability")
            );
            assertEq(
                _abiOutputTypes(abi, index),
                shape.outputs,
                string.concat(shape.signature, " return types")
            );

            if (_strEq(shape.outputs, "tuple")) {
                // `execute` returns the execution receipt: two named uint256 fields.
                assertEq(
                    _jsonString(abi, string.concat(prefix, ".outputs[0].components[0].name")),
                    "gross_profit",
                    string.concat(shape.signature, " receipt field 0 name")
                );
                assertEq(
                    _jsonString(abi, string.concat(prefix, ".outputs[0].components[0].type")),
                    "uint256",
                    string.concat(shape.signature, " receipt field 0 type")
                );
                assertEq(
                    _jsonString(abi, string.concat(prefix, ".outputs[0].components[1].name")),
                    "bribe_paid",
                    string.concat(shape.signature, " receipt field 1 name")
                );
                assertEq(
                    _jsonString(abi, string.concat(prefix, ".outputs[0].components[1].type")),
                    "uint256",
                    string.concat(shape.signature, " receipt field 1 type")
                );
                assertFalse(
                    _jsonHasString(abi, string.concat(prefix, ".outputs[0].components[2].type")),
                    string.concat(shape.signature, " receipt declares more than two fields")
                );
            }

            bytes4 selector = bytes4(keccak256(bytes(shape.signature)));
            string memory quoted = string.concat("$[", _quote(), shape.signature, _quote(), "]");
            assertEq(
                _hexToUint(_jsonString(identifiers, quoted)),
                uint256(uint32(selector)),
                string.concat("method identifier for ", shape.signature)
            );
        }

        // Nothing else is callable: every function entry is in the expected table.
        for (uint256 i = 0; i < entries; i++) {
            if (!_strEq(_abiEntryType(abi, i), "function")) continue;
            string memory signature = _abiSignature(abi, i);
            bool known;
            for (uint256 j = 0; j < expected.length; j++) {
                if (_strEq(expected[j].signature, signature)) {
                    known = true;
                    break;
                }
            }
            assertTrue(
                known,
                string.concat("the committed ABI carries an unexpected function ", signature)
            );
        }

        uint256 constructorIndex = _singleEntryOfType(abi, "constructor");
        string memory constructorPrefix = string.concat("[", vmToString(constructorIndex), "]");
        assertEq(
            _jsonString(abi, string.concat(constructorPrefix, ".stateMutability")),
            "nonpayable",
            "constructor state mutability"
        );
        assertEq(_jsonString(abi, string.concat(constructorPrefix, ".inputs[0].name")), "weth", "constructor parameter 0 name");
        assertEq(_jsonString(abi, string.concat(constructorPrefix, ".inputs[1].name")), "pool_manager", "constructor parameter 1 name");
        assertEq(_jsonString(abi, string.concat(constructorPrefix, ".inputs[2].name")), "usdc", "constructor parameter 2 name");
        assertEq(_jsonString(abi, string.concat(constructorPrefix, ".inputs[3].name")), "usdt", "constructor parameter 3 name");
        for (uint256 i = 0; i < 4; i++) {
            assertEq(
                _jsonString(
                    abi, string.concat(constructorPrefix, ".inputs[", vmToString(i), "].type")
                ),
                "address",
                string.concat("constructor parameter ", vmToString(i), " type")
            );
        }
        assertFalse(
            _jsonHasString(abi, string.concat(constructorPrefix, ".inputs[4].type")),
            "constructor declares more than four parameters"
        );
        assertFalse(
            _jsonHasString(abi, string.concat(constructorPrefix, ".outputs[0].type")),
            "constructor declares outputs"
        );

        uint256 fallbackIndex = _singleEntryOfType(abi, "fallback");
        string memory fallbackPrefix = string.concat("[", vmToString(fallbackIndex), "]");
        assertEq(
            _jsonString(abi, string.concat(fallbackPrefix, ".stateMutability")),
            "payable",
            "fallback state mutability"
        );
        assertFalse(
            _jsonHasString(abi, string.concat(fallbackPrefix, ".inputs[0].type")),
            "fallback declares inputs"
        );
        assertFalse(
            _jsonHasString(abi, string.concat(fallbackPrefix, ".outputs[0].type")),
            "fallback declares outputs"
        );

        // The default-function selector is Vyper's `__default__`, which the ABI
        // carries as its fallback and the donation gate uses as the ETH receipt
        // surface; the method-identifier table must name it too.
        assertEq(
            _hexToUint(
                _jsonString(
                    identifiers, string.concat("$[", _quote(), "__default__()", _quote(), "]")
                )
            ),
            0x89402a72,
            "method identifier for __default__()"
        );

        // Every declared error's selector must round-trip through the generated
        // revert vocabulary, so the release ABI and `Errors.sol` cannot drift.
        for (uint256 i = 0; i < entries; i++) {
            if (!_strEq(_abiEntryType(abi, i), "error")) continue;
            string memory signature = _abiSignature(abi, i);
            bytes4 selector = bytes4(keccak256(bytes(signature)));
            assertEq(
                ExecutorErrors.nameOf(selector),
                _jsonString(abi, string.concat("[", vmToString(i), "].name")),
                string.concat("generated revert vocabulary for ", signature)
            );
        }
    }
}
