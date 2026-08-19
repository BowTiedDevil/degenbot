"""degenbot.types.chain — the eth_typing replacement home.

Pins the ChainId IntEnum members/values and the EVM type aliases that the
codebase consumes, so the eth_typing dependency can be dropped without the
hash-equality / enum semantics drifting.
"""

import typing

import pytest

from degenbot.types.chain import (
    ABIEvent,
    Address,
    BlockParams,
    ChainId,
    Hash32,
    HexAddress,
    HexStr,
)


@pytest.mark.parametrize(
    ("member", "value"),
    [
        ("ETH", 1),
        ("FTM", 250),
        ("AVAX", 43114),
        ("ARB1", 42161),
        ("BASE", 8453),
    ],
)
def test_chain_id_members(member: str, value: int) -> None:
    assert getattr(ChainId, member).value == value


def test_chain_id_is_intenum_and_hash_equals_int():
    base = ChainId.BASE
    assert isinstance(base, int)
    # dict-key identity with the plain int (deployments lookups depend on it)
    assert {ChainId.BASE: "x"}[8453] == "x"
    assert hash(ChainId.ARB1) == hash(42161)


def test_chain_id_membership():
    assert ChainId.ETH.name == "ETH"
    assert ChainId(8453) is ChainId.BASE


def test_str_aliases_resolve_to_str():
    # PEP 695 type statements; the alias VALUES are plain str (annotations
    # resolve to str and callers may treat them as such).
    for alias in (HexStr, HexAddress, Hash32, Address):
        assert alias.__value__ is str, alias.__name__


def test_block_params_literal_members():
    members = typing.get_args(BlockParams)
    assert members == ("latest", "earliest", "pending", "safe", "finalized")


def test_abi_event_typeddict_shape():
    # The ABIEvent TypedDict must model the shape crypto.event_topic consumes.
    entry: ABIEvent = {
        "type": "event",
        "name": "Transfer",
        "inputs": [
            {"name": "from", "type": "address", "indexed": True},
            {"name": "to", "type": "address", "indexed": True},
            {"name": "value", "type": "uint256"},
        ],
    }
    assert entry["type"] == "event"
