"""
Pure on-chain release metadata for the built-in `cmd_executor`.

These getters are the on-chain half of the executor's release identity (ADR-098
D5): a deployment declares its release tag, protocol major, whole call-encoding
version, and the commitment to its capability ID list (ADR-099). The complete
encoding description (command stream, packed config, execute ABI) and the
capability list itself live in the off-chain release manifest, and artifact
integrity stays with `rust/crates/foundation/degenbot-executor/releases/v2/artifacts/manifest.json`.

Both sides are pinned here: the deployed contract is the authority for what it
declares, the committed manifest must agree with it, and the capability
commitment is recomputed from three independent declarations (the source's
`CAPABILITY_*` constants, the manifest's `ids`, and its `commitment`). Changing
any one of them is a release change, not a refresh.
"""

import json
import re
from pathlib import Path

import pytest
from eth_abi import encode as abi_encode
from eth_utils import keccak

from .conftest_shared import (
    AddressTable,
    enc_erc20_transfer,
    enc_erc20_xfer_balance,
    enc_preamble,
    enc_send_eth,
    enc_send_eth_all,
    enc_set_address,
    enc_v2_swap_calc,
    enc_v2_swap_compact,
    enc_v2_swap_direct,
    enc_v3_swap_compact,
    enc_v3_swap_delta,
    enc_v3_swap_one_for_zero,
    enc_v3_swap_zero_for_one,
    enc_v4_batch,
    enc_v4_burn_compact,
    enc_v4_mint_compact,
    enc_v4_settle,
    enc_v4_settle_all,
    enc_v4_settle_delta,
    enc_v4_swap_compact,
    enc_v4_swap_dynamic,
    enc_v4_sync,
    enc_v4_take,
    enc_v4_take_compact,
    enc_v4_take_delta,
    enc_v4_unlock,
    enc_weth_deposit,
    enc_weth_deposit_all,
    enc_weth_withdraw,
    enc_weth_withdraw_all,
    make_config,
)

REPO_ROOT = Path(__file__).resolve().parents[2]
ABI_ARTIFACT = (
    REPO_ROOT
    / "rust/crates/foundation/degenbot-executor/releases/v2/artifacts/cmd_executor.abi.json"
)
RELEASE_MANIFEST = REPO_ROOT / "rust/crates/foundation/degenbot-executor/releases/v2/manifest.json"
VYPER_SOURCE = REPO_ROOT / "rust/crates/foundation/degenbot-executor/releases/v2/contract.vy"

# "cmd_executor/2" right-padded to bytes32: the ASCII tag *is* the identity, so
# an operator can read a deployment's release without a lookup table.
RELEASE_ID = b"cmd_executor/2".ljust(32, b"\x00")
PROTOCOL_MAJOR = 1
CALL_ENCODING_VERSION = 1
CAPABILITIES = list(range(1, 16))

# The preimage prefix every capability commitment is derived under (ADR-099 D2).
COMMITMENT_SCHEME = b"degenbot/cmd_executor/capabilities/v1"

# The commitment to the declared ID list, pinned as a golden. Reproducible from
# any ABI toolchain: keccak256(scheme ++ abi.encode(uint32[] ids)).
CAPABILITIES_HASH = bytes.fromhex(
    "9cce6a2d69dbea3b4023e117492ff11aadc2e3feadb28a83faa172fc0823bcbf"
)

# getter name → declared ABI output type
GETTERS = {
    "release_id": "bytes32",
    "protocol_major": "uint32",
    "call_encoding_version": "uint32",
    "capabilities_hash": "bytes32",
}


def commitment_for(ids):
    """The ADR-099 commitment for a declared ID list."""
    return keccak(COMMITMENT_SCHEME + abi_encode(["uint32[]"], [list(ids)]))


def _abi_function(name):
    """Return the committed ABI entry for `name` (the artifact, not the source)."""
    for entry in json.loads(ABI_ARTIFACT.read_text()):
        if entry.get("type") == "function" and entry.get("name") == name:
            return entry
    raise AssertionError(f"{name}() is missing from {ABI_ARTIFACT}")


class TestDeclaredValues:
    """The deployed contract answers with the declared current values."""

    def test_release_id(self, executor):
        assert executor.release_id() == RELEASE_ID

    def test_protocol_major(self, executor):
        assert executor.protocol_major() == PROTOCOL_MAJOR

    def test_call_encoding_version(self, executor):
        assert executor.call_encoding_version() == CALL_ENCODING_VERSION

    def test_capabilities_hash(self, executor):
        assert executor.capabilities_hash() == CAPABILITIES_HASH

    def test_release_id_is_a_nul_padded_ascii_tag(self, executor):
        raw = executor.release_id()
        tag = raw.rstrip(b"\x00")
        assert tag, "release id must not be empty"
        assert all(0x20 <= byte < 0x7F for byte in tag), f"{tag!r} is not printable ASCII"
        assert raw[len(tag) :] == b"\x00" * (32 - len(tag))


class TestGetterShape:
    """Every getter is a zero-input `view` function in the committed ABI."""

    @pytest.mark.parametrize("name,output", GETTERS.items())
    def test_getter_is_a_zero_input_view_function(self, name, output):
        entry = _abi_function(name)
        assert entry["stateMutability"] == "view"
        assert entry["inputs"] == []
        assert [item["type"] for item in entry["outputs"]] == [output]

    def test_capabilities_hash_getter_is_a_zero_input_view_bytes32(self):
        # The ABI, not only the source: an operator reads the shape from the
        # committed artifact the deployment was built from.
        entry = _abi_function("capabilities_hash")
        assert entry["stateMutability"] == "view"
        assert entry["inputs"] == []
        assert entry["outputs"] == [{"name": "", "type": "bytes32"}]


class TestManifestAgreement:
    """The committed release manifest declares exactly what the contract does."""

    @pytest.fixture(scope="class")
    def manifest(self):
        return json.loads(RELEASE_MANIFEST.read_text())

    def test_release_tag(self, manifest):
        assert manifest["release_id"] == "cmd_executor/2"

    def test_schema_version(self, manifest):
        assert manifest["schema_version"] == 2

    def test_protocol_major(self, manifest):
        assert manifest["protocol"]["major"] == PROTOCOL_MAJOR

    def test_call_encoding_version(self, manifest):
        assert manifest["call_encoding"]["version"] == CALL_ENCODING_VERSION

    def test_capability_ids(self, manifest):
        declared = [capability["id"] for capability in manifest["capabilities"]["ids"]]
        assert declared == CAPABILITIES
        assert declared == sorted(set(declared)), "capability IDs must be unique and ascending"

    def test_capability_scheme(self, manifest):
        assert manifest["capabilities"]["scheme"].encode() == COMMITMENT_SCHEME

    def test_capability_commitment_matches_the_source_declaration(self, manifest):
        # The three-way drift gate: the auditable `CAPABILITY_*` constants in the
        # Vyper source, the manifest's ID list, and the manifest's commitment all
        # name one set.
        source = VYPER_SOURCE.read_text()
        source_ids = [
            int(value)
            for value in re.findall(
                r"^CAPABILITY_[A-Z0-9_]+: constant\(uint32\) = (\d+)$", source, re.MULTILINE
            )
        ]
        declared_hash = re.search(
            r"^CAPABILITIES_HASH: constant\(bytes32\) = 0x([0-9a-f]{64})$",
            source,
            re.MULTILINE,
        )
        assert declared_hash, "the source declares no CAPABILITIES_HASH constant"
        assert source_ids == CAPABILITIES, "the source list drifted from the manifest's IDs"
        assert source_ids == sorted(set(source_ids)), "the source list must be ascending"

        derived = commitment_for(source_ids)
        assert declared_hash.group(1) == derived.hex(), (
            "the source's CAPABILITIES_HASH is not the commitment its own ID constants derive"
        )
        assert manifest["capabilities"]["commitment"] == "0x" + derived.hex()
        assert [capability["id"] for capability in manifest["capabilities"]["ids"]] == source_ids
        assert CAPABILITIES_HASH == derived

    def test_manifest_commitment_is_reproducible_from_its_ids(self, manifest):
        ids = [capability["id"] for capability in manifest["capabilities"]["ids"]]
        assert manifest["capabilities"]["commitment"] == "0x" + commitment_for(ids).hex()

    def test_execute_abi_is_the_declared_one(self, manifest):
        execute_abi = manifest["call_encoding"]["execute_abi"]
        assert execute_abi["function"] == "execute"
        assert _signature_types(execute_abi["signature"]) == (
            "execute",
            ["bytes", "uint256"],
        )

    def test_manifest_agrees_with_the_deployed_contract(self, executor, manifest):
        assert executor.release_id().rstrip(b"\x00").decode() == manifest["release_id"]
        assert executor.protocol_major() == manifest["protocol"]["major"]
        assert executor.call_encoding_version() == manifest["call_encoding"]["version"]
        assert executor.capabilities_hash() == bytes.fromhex(
            manifest["capabilities"]["commitment"][2:]
        )
        assert executor.capabilities_hash() == commitment_for([
            capability["id"] for capability in manifest["capabilities"]["ids"]
        ])


def _signature_types(signature):
    """Split `name(type,type)` into (name, [types])."""
    name, _, arguments = signature.partition("(")
    return name, [item for item in arguments.rstrip(")").split(",") if item]


# field type → wire width in bytes. `bytes` is variable: its uint8 length
# prefix is a declared field of its own, so it contributes 0 here.
FIELD_WIDTH = {
    "uint8": 1,
    "bool": 1,
    "uint16": 2,
    "int16": 2,
    "uint96": 12,
    "uint256": 32,
    "address": 20,
    "bytes": 0,
}

# One V4 batch entry: (c0, c1, fee, tick_spacing, hooks_idx, zero_for_one, amount)
SWAP_ENTRY = (1, 2, 3000, 60, 0xFF, True, 1_000)


def _sample_commands():
    """One encoded sample per declared opcode, with empty variable tails."""
    batch = enc_v4_batch([SWAP_ENTRY])
    return {
        "SET_ADDRESS": enc_set_address("0x" + "11" * 20),
        "ERC20_TRANSFER": enc_erc20_transfer(1, 2, 1_000),
        "ERC20_XFER_BALANCE": enc_erc20_xfer_balance(1, 2),
        "WETH_DEPOSIT": enc_weth_deposit(1),
        "WETH_WITHDRAW": enc_weth_withdraw(1),
        "WETH_DEPOSIT_ALL": enc_weth_deposit_all(),
        "WETH_WITHDRAW_ALL": enc_weth_withdraw_all(),
        "SEND_ETH": enc_send_eth(1, 1_000),
        "SEND_ETH_ALL": enc_send_eth_all(1),
        "V2_SWAP_COMPACT": enc_v2_swap_compact(1, True, 1_000, 2),
        "V2_SWAP_CALC": enc_v2_swap_calc(1, True, 2),
        "V2_SWAP_DIRECT": enc_v2_swap_direct(1, True, 1_000, 2),
        # The shared V3 helper emits the direction-baked 0x33/0x34 forms, so
        # the 0x30 compact form is built inline: [0x30][pool_idx][zfo][amount:12]
        # [recipient_idx][forward_len].
        "V3_SWAP_COMPACT": (bytes([0x30, 1, 1]) + (1_000).to_bytes(12, "big") + bytes([2, 0])),
        "V3_SWAP_DELTA": enc_v3_swap_delta(1, True, 2),
        "V3_SWAP_ONE_FOR_ZERO": enc_v3_swap_one_for_zero(1, 1_000, 2),
        "V3_SWAP_ZERO_FOR_ONE": enc_v3_swap_zero_for_one(1, 1_000, 2),
        "V4_SWAP_COMPACT": enc_v4_swap_compact(1, 2, 3000, 60, 0xFF, True, 1_000),
        "V4_SWAP_DYNAMIC": enc_v4_swap_dynamic(1, 2, 3000, 60, 0xFF, True),
        "V4_BATCH": batch,
        "V4_BATCH_OPEN_WETH": bytes([0x43]) + batch[1:],
        "V4_UNLOCK": enc_v4_unlock(b""),
        "V4_TAKE": enc_v4_take(1, 2, 1_000),
        "V4_TAKE_COMPACT": enc_v4_take_compact(1, 2, 1_000),
        "V4_TAKE_DELTA": enc_v4_take_delta(1, 2),
        "V4_SYNC": enc_v4_sync(1),
        "V4_SETTLE": enc_v4_settle(),
        "V4_SETTLE_DELTA": enc_v4_settle_delta(1),
        "V4_SETTLE_ALL": enc_v4_settle_all(),
        "V4_MINT_COMPACT": enc_v4_mint_compact(1, 2, 1_000),
        "V4_BURN_COMPACT": enc_v4_burn_compact(1, 1_000),
        "BEGIN_EXECUTION": enc_preamble(AddressTable()),
    }


def _declared_size(declared):
    size = 1 + sum(FIELD_WIDTH[field["type"]] for field in declared["fields"])
    if "repeat" in declared:
        size += sum(FIELD_WIDTH[field["type"]] for field in declared["repeat"]["entry"]["fields"])
    return size


class TestDeclaredGrammar:
    """The manifest's grammar is the grammar this suite encodes against."""

    @pytest.fixture(scope="class")
    def manifest(self):
        return json.loads(RELEASE_MANIFEST.read_text())

    def test_declared_opcode_sizes_match_the_encoding_helpers(self, manifest):
        declared = {
            opcode["name"]: opcode
            for opcode in manifest["call_encoding"]["command_stream"]["opcodes"]
        }
        samples = _sample_commands()

        assert set(samples) == set(declared), "every declared opcode needs a sample"

        for name, command in samples.items():
            assert command[0] == int(declared[name]["opcode"], 16), name
            assert len(command) == _declared_size(declared[name]), name

    def test_declared_config_layout_matches_the_packer(self, manifest):
        fields = manifest["call_encoding"]["config"]["fields"]
        next_bit = 0
        for field in fields:
            low, high = field["bits"]
            assert low == next_bit, f"{field['name']} leaves a hole"
            assert high >= low
            next_bit = high + 1
        assert next_bit == 256, "the config word must be fully described"

        packed = make_config(profit_mode=2, bribe_bips=500, bribe_recipient_idx=1, min_profit=1_000)
        for field in fields:
            low, high = field["bits"]
            value = (packed >> low) & ((1 << (high - low + 1)) - 1)
            if field["name"] == "profit_mode":
                assert value == 2
            elif field["name"] == "bribe_bips":
                assert value == 500
            elif field["name"] == "bribe_recipient_idx":
                assert value == 1
            else:
                assert value == 1_000
