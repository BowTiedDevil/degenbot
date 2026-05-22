"""PoolIO and AsyncPoolIO protocols and adapters for builder I/O.

These protocols define the narrow I/O surface that pool builders need:
`call`, `call_raw`, `get_block_number`, `get_block`, `get_block_timestamp`,
`get_code`, and `get_balance`. By parameterizing builders on `PoolIO` (sync)
or `AsyncPoolIO` (async), the same builder logic can be reused by both Bot
and AsyncBot.

`chain_id` is intentionally excluded — it is a configuration value, not an
I/O operation. Builders already receive `chain_id` as a `build()` kwarg.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Protocol, runtime_checkable

if TYPE_CHECKING:
    from hexbytes import HexBytes
    from web3.types import BlockData, BlockIdentifier, TxParams

    from degenbot.provider.interface import AsyncProviderAdapter, ProviderAdapter


@runtime_checkable
class PoolIO(Protocol):
    """I/O primitives needed by pool builders (sync).

    Encapsulates the RPC surface builders use so they are agnostic
    to sync vs async execution. Two adapters satisfy this protocol:
    SyncPoolIO (wrapping ProviderAdapter) and AsyncPoolIO (wrapping
    AsyncProviderAdapter).
    """

    def get_block_number(self) -> int: ...

    def get_block(self, block_identifier: int | str) -> BlockData | None: ...

    def get_block_timestamp(self, block: int | None = None) -> int: ...

    def get_code(self, address: str, block: int | None = None) -> HexBytes: ...

    def get_balance(self, address: str, block: int | None = None) -> int: ...

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes: ...

    def call_raw(self, tx: TxParams, block: BlockIdentifier | None = None) -> HexBytes: ...


@runtime_checkable
class AsyncPoolIOProtocol(Protocol):
    """Async counterpart of PoolIO.

    All methods are async. Builders that use AsyncPoolIO must be
    async builders.
    """

    async def get_block_number(self) -> int: ...

    async def get_block(self, block_identifier: int | str) -> BlockData | None: ...

    async def get_block_timestamp(self, block: int | None = None) -> int: ...

    async def get_code(self, address: str, block: int | None = None) -> HexBytes: ...

    async def get_balance(self, address: str, block: int | None = None) -> int: ...

    async def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes: ...


class SyncPoolIO:
    """PoolIO adapter wrapping a sync ProviderAdapter."""

    def __init__(self, provider: ProviderAdapter) -> None:
        self._provider = provider

    def get_block_number(self) -> int:
        return self._provider.get_block_number()

    def get_block(self, block_identifier: int | str) -> BlockData | None:
        return self._provider.get_block(block_identifier)

    def get_block_timestamp(self, block: int | None = None) -> int:
        return self._provider.get_block_timestamp(block=block)

    def get_code(self, address: str, block: int | None = None) -> HexBytes:
        return self._provider.get_code(address, block=block)

    def get_balance(self, address: str, block: int | None = None) -> int:
        return self._provider.get_balance(address, block=block)

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        return self._provider.call(to=to, data=data, block=block)

    def call_raw(self, tx: TxParams, block: BlockIdentifier | None = None) -> HexBytes:
        return self._provider.call_raw(tx, block=block)  # ty: ignore


class AsyncPoolIO:
    """PoolIO adapter wrapping an AsyncProviderAdapter.

    All methods are async. Used by AsyncBot's async builders.
    Satisfies the AsyncPoolIOProtocol at runtime.
    """

    def __init__(self, provider: AsyncProviderAdapter) -> None:
        self._provider = provider

    async def get_block_number(self) -> int:
        return await self._provider.get_block_number()

    async def get_block(self, block_identifier: int | str) -> BlockData | None:
        return await self._provider.get_block(block_identifier)

    async def get_block_timestamp(self, block: int | None = None) -> int:
        block_data = await self._provider.get_block(block if block is not None else "latest")
        if block_data is None:
            msg = f"Block {block} not found"
            raise ValueError(msg)
        return block_data["timestamp"]

    async def get_code(self, address: str, block: int | None = None) -> HexBytes:
        return await self._provider.get_code(address, block=block)

    async def get_balance(self, address: str, block: int | None = None) -> int:
        return await self._provider.get_balance(address, block=block)

    async def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        return await self._provider.call(to=to, data=data, block=block)
