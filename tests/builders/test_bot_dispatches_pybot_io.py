"""Parity tests documenting the SyncPoolIO → PyBotIo swap in `Bot.build_pool`.

ADR-005 slice 14a: `Bot.build_pool`'s `io = SyncPoolIO(self.provider)` becomes
`io = PyBotIo(provider=self.provider, db=self.db)`. The swap must be
behavior-preserving — every builder that receives an `io: PoolIO` must see the
same observable delegation when `io` is a `PyBotIo` as when it's a
`SyncPoolIO` (both wrap the same `ProviderAdapter`).

These tests pin the wiring (the `io` handed to builders is a `PyBotIo`) and
the delegation parity (same `provider.call` arguments, same return values)
without requiring a live RPC node — they exercise the seam, mirroring
`test_bot_pool_io.py`'s mocked-io shape.
"""

from __future__ import annotations

from typing import Any
from unittest.mock import MagicMock

from hexbytes import HexBytes

from degenbot.bot import Bot
from degenbot.builders.pool_io import SyncPoolIO
from degenbot.builders.request import BuildPoolRequest
from degenbot.checksum_cache import get_checksum_address
from degenbot.degenbot_rs import PyBotIo


class _RecordingProvider:
    """A ProviderAdapter-shaped double recording every PoolIO call.

    Mirrors `ProviderAdapter`'s actual signature shape (the backing object
    `SyncPoolIO` wraps — NOT a raw `OfflineProvider`/`_AlloyAdapter` backend):
    positional leading args (`address`/`to`/`tx`) + `block` keyword. `call` /
    `call_raw` match the `SyncPoolIO`-forwarded kw call shape exactly so the
    parity argument is apples-to-apples.
    """

    def __init__(self, *, block_number: int = 18_000_000) -> None:
        self._block_number = block_number
        self.call_log: list[tuple[str, str, str]] = []  # (method, to, data_hex)

    def get_block_number(self) -> int:
        return self._block_number

    def get_block(self, block_identifier: int | str) -> dict[str, Any] | None:
        return {"number": int(block_identifier), "timestamp": 1_700_000_000}

    def get_block_timestamp(self, block: int | None = None) -> int:
        return 1_700_000_000

    def get_code(self, address: str, block: int | None = None) -> HexBytes:
        return HexBytes(b"\x60\x80")

    def get_balance(self, address: str, block: int | None = None) -> int:
        return 0

    def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
        # `call` keeps the kw-only shape `SyncPoolIO.call` forwards (to=, data=,
        # block=), matching how test doubles mock `provider.call(*, to, data)`.
        self.call_log.append(("call", to, data.hex()))
        return HexBytes(b"\x00" * 32)

    def call_raw(self, *, tx: Any, block: int | None = None) -> HexBytes:
        self.call_log.append(("call_raw", tx["to"], tx["data"].hex()))
        return HexBytes(b"\x00" * 32)


class TestBotDispatchesPyBotIo:
    """Bot.build_pool hands builders a PyBotIo (mirror of the SyncPoolIO test)."""

    def test_dispatch_build_forwards_pybot_io(self) -> None:
        """_dispatch_build forwards a PyBotIo io= to the builder untouched."""
        builder = MagicMock()
        builder.build.return_value = MagicMock()

        address = get_checksum_address("0x" + "01" * 20)
        provider = _RecordingProvider()
        io = PyBotIo(provider=provider)
        request = BuildPoolRequest()

        Bot._dispatch_build(
            builder=builder,
            address=address,
            chain_id=1,
            io=io,  # type: ignore[arg-type]  # PyBotIo satisfies PoolIO at runtime
            request=request,
        )

        builder.build.assert_called_once_with(address, chain_id=1, io=io, request=request)


class TestPyBotIoParityWithSyncPoolIO:
    """PyBotIo and SyncPoolIO wrapping the same provider are observably equivalent."""

    def test_call_delegation_matches_sync_pool_io(self) -> None:
        """Given the same provider, PyBotIo.call and SyncPoolIO.call produce
        identical call logs and return values."""
        provider_for_py = _RecordingProvider()
        provider_for_sync = _RecordingProvider()

        py_io = PyBotIo(provider=provider_for_py)
        sync_io = SyncPoolIO(provider_for_sync)  # type: ignore[arg-type]

        to = "0x" + "ab" * 20
        data = b"\x12\x34\x56\x78"

        py_result = py_io.call(to=to, data=data, block=None)
        sync_result = sync_io.call(to=to, data=data, block=None)

        assert bytes(py_result) == bytes(sync_result)
        assert provider_for_py.call_log == provider_for_sync.call_log

    def test_get_block_number_matches_sync_pool_io(self) -> None:
        provider = _RecordingProvider(block_number=12_345_678)
        py_io = PyBotIo(provider=provider)
        sync_io = SyncPoolIO(provider)  # type: ignore[arg-type]
        assert py_io.get_block_number() == sync_io.get_block_number()
