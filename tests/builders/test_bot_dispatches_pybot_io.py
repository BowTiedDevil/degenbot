"""Tests documenting the PyBotIo wiring in `Bot.build_pool`.

ADR-005 slice 14a: `Bot.build_pool` hands builders a `PyBotIo` (the Rust
executor). The Python `SyncPoolIO` adapter + its parity gate are retired
(slice 14 collapse) — `PyBotIo` is the sole executor, so there is no longer
a second implementation to parity-check against.

These tests pin the wiring (the `io` handed to builders is a `PyBotIo`),
mirroring `test_bot_pool_io.py`'s mocked-io shape without requiring a live
RPC node.
"""

from __future__ import annotations

from typing import Any
from unittest.mock import MagicMock

from hexbytes import HexBytes

from degenbot.bot import Bot, PyBotIo
from degenbot.builders.request import BuildPoolRequest
from degenbot.checksum_cache import get_checksum_address


class _RecordingProvider:
    """An AlloyProvider-shaped double recording every forwarded call."""

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

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.call_log.append(("call", to, data.hex()))
        return HexBytes(b"\x00" * 32)

    def call_raw(self, tx: Any, block: int | None = None) -> HexBytes:
        self.call_log.append(("call_raw", tx["to"], tx["data"].hex()))
        return HexBytes(b"\x00" * 32)


class TestBotDispatchesPyBotIo:
    """Bot.build_pool hands builders a PyBotIo."""

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
            io=io,
            request=request,
        )

        builder.build.assert_called_once_with(address, chain_id=1, io=io, request=request)
