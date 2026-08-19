"""Golden-vector parity tests for the Rust keccak256 / event_topic FFI.

Vectors pinned from eth_utils before its removal (5JKNQH): keccak256 is
SHA-3 variant of Keccak (NOT SHA-3), and event topics are keccak256 of the
canonical event signature (all inputs, declared order; struct params
expanded to ``tuple(...)``).
"""

import pytest
from hexbytes import HexBytes

from degenbot._ffi import event_topic as ffi_event_topic
from degenbot._ffi import keccak256 as ffi_keccak256
from degenbot.crypto import event_topic, keccak256

KECCAK_EMPTY = "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
KECCAK_ABC = "4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45"


def _event(name: str, inputs: list[tuple[str, bool, str]]) -> dict:
    return {
        "type": "event",
        "name": name,
        "anonymous": False,
        "inputs": [
            {"indexed": indexed, "name": nm, "type": typ}
            for typ, indexed, nm in inputs
        ],
    }


def test_keccak256_ff_vectors():
    assert ffi_keccak256(b"").hex() == KECCAK_EMPTY
    assert ffi_keccak256(b"abc").hex() == KECCAK_ABC
    assert len(ffi_keccak256(b"")) == 32


def test_keccak256_python_wrapper():
    assert keccak256(b"") == HexBytes("0x" + KECCAK_EMPTY)
    assert keccak256(b"abc").to_0x_hex() == "0x" + KECCAK_ABC


def test_event_topic_transfer():
    entry = _event(
        "Transfer",
        [("address", True, "a"), ("address", True, "b"), ("uint256", False, "c")],
    )
    assert ffi_event_topic(entry).hex() == (
        "ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
    )


def test_event_topic_swap():
    entry = _event(
        "Swap",
        [t for t in (
            ("address", True, "a"), ("address", True, "b"),
            ("uint256", False, "c"), ("uint256", False, "d"),
            ("uint256", False, "e"), ("uint256", False, "f"),
            ("uint160", False, "g"), ("uint128", False, "h"), ("int24", False, "i"),
        )],
    )
    assert ffi_event_topic(entry).hex() == (
        "87f457ecc0a194190886d8de23312851442f9facf5ad395725721f767371db98"
    )


def test_event_topic_sync():
    entry = _event("Sync", [("uint112", False, "a"), ("uint112", False, "b")])
    assert ffi_event_topic(entry).hex() == (
        "1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1"
    )


def test_event_topic_struct_expansion():
    entry = {
        "type": "event",
        "name": "Deposit",
        "anonymous": False,
        "inputs": [
            {"indexed": True, "name": "who", "type": "address"},
            {
                "indexed": False,
                "name": "info",
                "type": "tuple",
                "components": [
                    {"name": "t", "type": "address"},
                    {"name": "v", "type": "uint256"},
                ],
            },
        ],
    }
    assert ffi_event_topic(entry).hex() == (
        "17b4373027c43177e6ff5e6173d14795114b365763b1aa8d70ec8f42ef0792a8"
    )
    # ``Deposit(address,(address,uint256))`` — canonical signature form.


def test_event_topic_python_wrapper():
    entry = _event("Sync", [("uint112", False, "a"), ("uint112", False, "b")])
    assert event_topic(entry) == HexBytes(
        "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1"
    )


def test_event_topic_rejects_non_event():
    with pytest.raises(ValueError):
        ffi_event_topic({"type": "function", "name": "foo", "inputs": []})
