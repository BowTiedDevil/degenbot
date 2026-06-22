"""Tracer-bullet tests for PyBotIo (ADR-005 slice 14a).

`PyBotIo` is the Rust `#[pyclass]` I/O façade that builders will receive in
place of the Python `SyncPoolIO` adapter. It holds a Python provider (the
`ProviderAdapter` the `Bot` was constructed with) + an optional DB handle, and
exposes the 7-method `PoolIO` surface (`get_block_number`, `get_block`,
`get_block_timestamp`, `get_code`, `get_balance`, `call`, `call_raw`) by
delegating to the held provider.

These tests pin the *seam* -- that delegating through the Rust pyclass yields
the same observable result as calling the provider directly. They do NOT yet
route a real builder through `PyBotIo`; that's the 14a follow-on (one builder's
`build()` via `PyBotIo`), and 14b extends it to all families.
"""

from __future__ import annotations

from typing import Any

import eth_abi.abi
import pytest
from hexbytes import HexBytes

from degenbot.builders.pool_io import SyncPoolIO
from degenbot.builders.type_resolution import fetch_factory_from_chain
from degenbot.degenbot_rs import PyBotIo


class _FakeProvider:
    """A minimal ``ProviderAdapter``-shaped double for the tracer.

    Only the 7 ``PoolIO`` methods are exercised; the rest of the
    ``ProviderAdapter`` surface is irrelevant to ``PoolIO`` conformance.
    """

    def __init__(self, *, block_number: int = 18_000_000) -> None:
        self._block_number = block_number
        self.calls: list[tuple[str, str]] = []  # (to, data_hex) audit trail

    def get_block_number(self) -> int:
        return self._block_number

    def get_block(self, block_identifier: int | str) -> dict[str, Any] | None:
        return {"number": int(block_identifier), "timestamp": 1_700_000_000}

    def get_block_timestamp(self, block: int | None = None) -> int:
        return 1_700_000_000

    def get_code(self, address: str, block: int | None = None) -> HexBytes:
        return HexBytes(b"\x60\x80\x60\x40")  # plausible bytecode prefix

    def get_balance(self, address: str, block: int | None = None) -> int:
        return 10**18

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.calls.append((to, data.hex()))
        return HexBytes(b"\x00" * 32)

    def call_raw(self, tx: Any, block: int | None = None) -> HexBytes:
        return self.call(tx["to"], tx["data"], block)


class _FakeDb:
    """A ``DatabaseSessionManager``-shaped double (cannot be called; presence only)."""


def test_pybot_io_delegates_get_block_number():
    """get_block_number delegates to the held provider verbatim."""
    provider = _FakeProvider(block_number=12_345_678)
    io = PyBotIo(provider=provider)
    assert io.get_block_number() == 12_345_678


def test_pybot_io_delegates_call_records_on_provider():
    """call(to, data, block) delegates to provider.call and returns its HexBytes."""
    provider = _FakeProvider()
    io = PyBotIo(provider=provider)
    result = io.call(to="0x" + "ab" * 20, data=b"\x12\x34\x56\x78", block=None)
    assert result == HexBytes(b"\x00" * 32)
    assert provider.calls == [("0x" + "ab" * 20, "12345678")]


def test_pybot_io_delegates_get_code():
    """get_code delegates and returns HexBytes."""
    provider = _FakeProvider()
    io = PyBotIo(provider=provider)
    code = io.get_code("0x" + "cd" * 20)
    assert code == HexBytes(b"\x60\x80\x60\x40")


def test_pybot_io_delegates_get_balance():
    """get_balance delegates and returns int (not wrapped)."""
    provider = _FakeProvider()
    io = PyBotIo(provider=provider)
    assert io.get_balance("0x" + "ee" * 20) == 10**18


def test_pybot_io_holds_optional_db_handle():
    """PyBotIo stores the DB handle and exposes it back (held, not called yet)."""
    db = _FakeDb()
    io = PyBotIo(provider=_FakeProvider(), db=db)
    # The held handle round-trips through the pyclass.
    assert io.db is db


@pytest.mark.parametrize(
    "method",
    [
        "get_block_number",
        "get_block",
        "get_block_timestamp",
        "get_code",
        "get_balance",
        "call",
        "call_raw",
    ],
)
def test_pybot_io_satisfies_pool_io_protocol(method: str):
    """PyBotIo exposes the full 7-method PoolIO surface (runtime protocol check).

    This is the acceptance criterion for 14a: every method a builder may call
    on its ``io: PoolIO`` parameter is reachable on ``PyBotIo``.
    """
    io = PyBotIo(provider=_FakeProvider())
    assert hasattr(io, method), f"PyBotIo missing PoolIO method {method!r}"


# === I/O choreography methods (slice 14b) ===
#
# `fetch_factory_address` is the first choreography method moved into `PyBotIo`:
# the multi-step (encode `factory()` selector -> `eth_call` -> ABI-decode `address`
# -> EIP-55 checksum), previously `fetch_factory_from_chain` in
# `type_resolution.py`, now reachable as a single Rust-owned method. The RPC
# primitive (`call`) still delegates to the held provider (the native-alloy
# swap is a later slice); the *choreography* -- the orchestration of those 4
# steps -- now lives in Rust, satisfying slice 14's "methods for the builder
# I/O choreography … moved here, called from Python via PyBotIo".

class _FactoryCallProvider:
    """Provider double that returns an ABI-encoded factory address for `factory()`.

    Mirrors ``ProviderAdapter.call(*, to, data, block)`` (kw-only) so it stays
    compatible with ``PyBotIo``'s kw-only forward contract.
    """

    def __init__(self, factory_raw: str) -> None:
        # factory_raw is the 40-hex-char lowercase address (no 0x prefix),
        # ABI-encoded right-aligned in a 32-byte word -- what a real
        # `factory()` call returns.
        self._encoded = eth_abi.abi.encode(types=["address"], args=[factory_raw])
        self.calls: list[tuple[str, bytes]] = []  # (to, data)

    def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.calls.append((to, data))
        return HexBytes(self._encoded)


def test_pybot_io_fetch_factory_address_decodes_and_checksums():
    """PyBotIo.fetch_factory_address runs encode->call->decode->checksum in Rust
    and returns an EIP-55 checksummed address.
    """
    factory_raw = "66f9664f97f2b50f62d13ea064982f936de76657"  # arbitrary 20-byte
    expected = "0x66f9664f97F2b50F62D13eA064982f936dE76657"  # EIP-55 of above
    pool_address = "0x" + "ab" * 20
    provider = _FactoryCallProvider(factory_raw)
    io = PyBotIo(provider=provider)

    result = io.fetch_factory_address(pool_address)

    assert result == expected
    # Exactly one call made, to the pool address, with the 4-byte `factory()`
    # selector (keccak256("factory()")[:4] = 0xc45a0155).
    assert len(provider.calls) == 1
    to, data = provider.calls[0]
    assert to == pool_address
    assert data[:4] == bytes.fromhex("c45a0155")


def test_pybot_io_fetch_factory_address_returns_none_on_revert():
    """On a provider-side error (revert / call failure), return None -- mirrors
    `fetch_factory_from_chain`'s `except (Web3Exception, DecodingError): return None`."""
    class _RevertingProvider:
        def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
            msg = "eth_call reverted"
            raise RuntimeError(msg)

    io = PyBotIo(provider=_RevertingProvider())
    assert io.fetch_factory_address("0x" + "ab" * 20) is None


def test_pybot_io_fetch_factory_parity_with_python_impl():
    """`PyBotIo.fetch_factory_address` returns the exact same EIP-55 checksum
    as the original Python `fetch_factory_from_chain` for the same provider
    `call` result.

    Two independent implementations (Rust on PyBotIo, Python on SyncPoolIO)
    against identical backends must agree -- this is the parity gate that lets
    `Bot.build_pool` route through `PyBotIo.fetch_factory_address` without
    behavior change. The SyncPoolIO path exercises the original Python
    decode/checksum; the PyBotIo path exercises the Rust impl."""
    factory_raw = "66f9664f97f2b50f62d13ea064982f936de76657"
    pool_address = "0x" + "ab" * 20

    rust_result = PyBotIo(provider=_FactoryCallProvider(factory_raw)).fetch_factory_address(
        pool_address
    )
    py_result = fetch_factory_from_chain(
        pool_address, chain_id=1, io=SyncPoolIO(_FactoryCallProvider(factory_raw))
    )

    assert rust_result == py_result
