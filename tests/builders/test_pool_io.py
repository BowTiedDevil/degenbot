"""Tests for PoolIO and AsyncPoolIO protocols and adapters."""

from __future__ import annotations

from typing import TYPE_CHECKING

from hexbytes import HexBytes

from degenbot.builders.pool_io import AsyncPoolIO, AsyncPoolIOProtocol, PoolIO, SyncPoolIO

if TYPE_CHECKING:
    from web3.types import BlockData, BlockIdentifier, TxParams


# --- Fake providers ---


class FakeSyncProvider:
    """Minimal fake satisfying SyncPoolIO's delegate."""

    def __init__(self) -> None:
        self.call_count = 0
        self.call_raw_count = 0

    def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.call_count += 1
        return HexBytes(b"\x00" * 32)

    def call_raw(self, tx: TxParams, block: BlockIdentifier | None = None) -> HexBytes:
        self.call_raw_count += 1
        return HexBytes(b"\x00" * 32)

    def get_block_number(self) -> int:
        return 42

    def get_block(self, block_identifier: int | str) -> BlockData | None:
        return {"number": 42}  # type: ignore[return-value]

    def get_block_timestamp(self, block: int | None = None) -> int:
        return 1_700_000_000

    def get_code(self, address: str, block: int | None = None) -> HexBytes:
        return HexBytes(b"\x01")

    def get_balance(self, address: str, block: int | None = None) -> int:
        return 10**18


class FakeAsyncProvider:
    """Minimal async fake satisfying AsyncPoolIO's delegate."""

    def __init__(self) -> None:
        self.call_count = 0

    async def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.call_count += 1
        return HexBytes(b"\x00" * 32)

    async def get_block_number(self) -> int:
        return 42

    async def get_block(self, block_identifier: int | str) -> BlockData | None:
        return {"number": 42, "timestamp": 1_700_000_000}  # type: ignore[return-value]

    async def get_code(self, address: str, block: int | None = None) -> HexBytes:
        return HexBytes(b"\x01")

    async def get_balance(self, address: str, block: int | None = None) -> int:
        return 10**18


# --- SyncPoolIO ---


class TestSyncPoolIO:
    """SyncPoolIO delegates to a sync provider."""

    def test_call_delegates(self) -> None:
        """SyncPoolIO.call() delegates to the wrapped provider."""
        provider = FakeSyncProvider()
        io = SyncPoolIO(provider)
        result = io.call(to="0x01", data=b"\x00")
        assert result == HexBytes(b"\x00" * 32)
        assert provider.call_count == 1

    def test_call_raw_delegates(self) -> None:
        """SyncPoolIO.call_raw() delegates to the wrapped provider."""
        provider = FakeSyncProvider()
        io = SyncPoolIO(provider)
        result = io.call_raw({"to": "0x01", "data": b"\x00"})
        assert result == HexBytes(b"\x00" * 32)
        assert provider.call_raw_count == 1

    def test_get_block_number_delegates(self) -> None:
        """SyncPoolIO.get_block_number() delegates to the wrapped provider."""
        provider = FakeSyncProvider()
        io = SyncPoolIO(provider)
        assert io.get_block_number() == 42

    def test_get_block_delegates(self) -> None:
        """SyncPoolIO.get_block() delegates to the wrapped provider."""
        provider = FakeSyncProvider()
        io = SyncPoolIO(provider)
        block = io.get_block(42)
        assert block is not None
        assert block["number"] == 42

    def test_get_block_timestamp_delegates(self) -> None:
        """SyncPoolIO.get_block_timestamp() delegates to the wrapped provider."""
        provider = FakeSyncProvider()
        io = SyncPoolIO(provider)
        assert io.get_block_timestamp() == 1_700_000_000

    def test_get_code_delegates(self) -> None:
        """SyncPoolIO.get_code() delegates to the wrapped provider."""
        provider = FakeSyncProvider()
        io = SyncPoolIO(provider)
        result = io.get_code(address="0x01")
        assert result == HexBytes(b"\x01")

    def test_get_balance_delegates(self) -> None:
        """SyncPoolIO.get_balance() delegates to the wrapped provider."""
        provider = FakeSyncProvider()
        io = SyncPoolIO(provider)
        assert io.get_balance(address="0x01") == 10**18

    def test_satisfies_pool_io_protocol(self) -> None:
        """SyncPoolIO satisfies the PoolIO protocol at runtime."""
        io = SyncPoolIO(FakeSyncProvider())
        assert isinstance(io, PoolIO)


# --- AsyncPoolIO ---


class TestAsyncPoolIO:
    """AsyncPoolIO delegates to an async provider."""

    async def test_call_delegates(self) -> None:
        """AsyncPoolIO.call() awaits the wrapped provider."""
        provider = FakeAsyncProvider()
        io = AsyncPoolIO(provider)
        result = await io.call(to="0x01", data=b"\x00")
        assert result == HexBytes(b"\x00" * 32)
        assert provider.call_count == 1

    async def test_get_block_number_delegates(self) -> None:
        """AsyncPoolIO.get_block_number() awaits the wrapped provider."""
        provider = FakeAsyncProvider()
        io = AsyncPoolIO(provider)
        assert await io.get_block_number() == 42

    async def test_get_block_delegates(self) -> None:
        """AsyncPoolIO.get_block() awaits the wrapped provider."""
        provider = FakeAsyncProvider()
        io = AsyncPoolIO(provider)
        block = await io.get_block(42)
        assert block is not None
        assert block["number"] == 42

    async def test_get_block_timestamp_delegates(self) -> None:
        """AsyncPoolIO.get_block_timestamp() fetches block and extracts timestamp."""
        provider = FakeAsyncProvider()
        io = AsyncPoolIO(provider)
        assert await io.get_block_timestamp() == 1_700_000_000

    async def test_get_code_delegates(self) -> None:
        """AsyncPoolIO.get_code() delegates to the wrapped provider."""
        provider = FakeAsyncProvider()
        io = AsyncPoolIO(provider)
        result = await io.get_code(address="0x01")
        assert result == HexBytes(b"\x01")

    async def test_get_balance_delegates(self) -> None:
        """AsyncPoolIO.get_balance() delegates to the wrapped provider."""
        provider = FakeAsyncProvider()
        io = AsyncPoolIO(provider)
        assert await io.get_balance(address="0x01") == 10**18

    async def test_satisfies_async_pool_io_protocol(self) -> None:
        """AsyncPoolIO satisfies the AsyncPoolIOProtocol at runtime."""
        io = AsyncPoolIO(FakeAsyncProvider())
        assert isinstance(io, AsyncPoolIOProtocol)
