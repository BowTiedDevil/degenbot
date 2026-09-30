"""Async Alloy provider: the mixin trees and :class:`AsyncAlloyProvider`.

The sync twin lives in :mod:`degenbot.provider.sync`. The two trees are
deliberately parallel: 20 of the 22 methods they share are logic-identical
modulo ``await``, and nearly all of those are single-line pass-throughs to
the wrapped Rust pyclass -- the sync/await duality IS the code. The
multi-line logic both trees need (block-tag resolution, block-timestamp
extraction) lives once in :mod:`degenbot.provider.block_helpers`.
``get_logs`` genuinely differs: the sync surface accepts a ``LogFilter`` or
individual keyword arguments with range validation; this surface takes
keyword arguments only. The sync tree also carries the context-manager and
scalar-property surface this tree exposes as awaited methods.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

from degenbot.provider._rust import RustAsyncAlloyProvider
from degenbot.provider.block_helpers import block_timestamp_from, resolve_block_tag
from degenbot.utils.bytes import to_bytes

if TYPE_CHECKING:
    from degenbot.types.rpc_types import (
        BlockData,
        LogData,
        TransactionData,
        TransactionReceiptData,
        TxParams,
    )


class _AsyncAlloyBacked:
    """Structural base for the async alloy mixins: the wrapped Rust provider."""

    _provider: RustAsyncAlloyProvider
    rpc_url: str


class _AsyncAlloyEndpointMixin(_AsyncAlloyBacked):
    """Endpoint identity and chain-state scalars for ``AsyncAlloyProvider``."""

    # ----- Properties -----

    @property
    def rpc_url(self) -> str:
        """The RPC endpoint URL."""
        return self._provider.rpc_url

    @property
    def provider_type(self) -> str:
        """The provider type (always 'alloy')."""
        return "alloy"

    # ----- Async methods -----

    async def get_block_number(self) -> int:
        """Return the current block number.

        Returns:
            The current block number.

        """
        return await self._provider.get_block_number()

    async def get_chain_id(self) -> int:
        """Return the chain ID.

        Returns:
            The chain ID.

        """
        return await self._provider.get_chain_id()

    async def get_gas_price(self) -> int:
        """Return the current gas price in wei.

        Returns:
            The current gas price in wei.

        """
        return await self._provider.get_gas_price()


class _AsyncAlloyQueryMixin(_AsyncAlloyBacked):
    """Async block/log/transaction queries and call helpers."""

    async def get_block(self, block_identifier: int | str) -> BlockData | None:
        """Get a block by number or tag.

        Args:
            block_identifier: Block number, or one of 'latest', 'earliest', 'pending'.

        Returns:
            Block data, or None if not found.

        An unsupported tag string raises ``ValueError`` (:meth:`BlockTag.parse`).

        """
        return await self._provider.get_block(
            resolve_block_tag(block_identifier, await self._provider.get_block_number())
        )

    async def get_transaction(self, tx_hash: str) -> TransactionData | None:
        """Get a transaction by hash.

        Returns:
            The transaction data, or None if not found.

        """
        return await self._provider.get_transaction(tx_hash)

    async def get_transaction_receipt(self, tx_hash: str) -> TransactionReceiptData | None:
        """Get a transaction receipt by hash.

        Returns:
            The transaction receipt, or None if not found.

        """
        return await self._provider.get_transaction_receipt(tx_hash)

    async def get_logs(
        self,
        *,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> list[LogData]:
        """Fetch event logs matching the filter.

        Returns:
            A list of matching log entries.

        """
        return await self._provider.get_logs(
            from_block=from_block,
            to_block=to_block,
            addresses=addresses,
            topics=topics,
        )

    async def call(
        self,
        to: str,
        data: bytes,
        block: int | None = None,
    ) -> bytes:
        """Execute an eth_call.

        Returns:
            The raw return data from the contract call.

        """
        return await self._provider.call(to, data, block)

    async def call_raw(
        self,
        tx: TxParams,
        block: int | None = None,
    ) -> bytes:
        """Execute an eth_call with a raw transaction dict.

        Returns:
            The raw return data from the contract call.

        """
        return await self._provider.call(tx["to"], to_bytes(tx["data"]), block)

    async def batch_call(
        self,
        calls: list[TxParams],
        block: int | None = None,
    ) -> list[bytes]:
        """Execute multiple eth_calls sequentially.

        Returns:
            A list of raw return data from each call.

        """
        return [await self.call_raw(tx, block) for tx in calls]

    async def get_block_timestamp(self, block: int | None = None) -> int:
        """Get the timestamp for a block.

        Args:
            block: Block number, or None for latest.

        Returns:
            The block timestamp as an integer (Unix seconds).

        """
        block_data = await self.get_block(block if block is not None else "latest")
        return block_timestamp_from(block_data, block)

    async def get_code(self, address: str, block: int | None = None) -> bytes:
        """Get the bytecode at an address.

        Returns:
            The contract bytecode, or empty bytes if not a contract.

        """
        return await self._provider.get_code(address, block)

    async def estimate_gas(
        self,
        to: str,
        data: bytes,
        from_: str | None = None,
        value: int | None = None,
        block: int | None = None,
    ) -> int:
        """Estimate gas for a transaction.

        Returns:
            The estimated gas units.

        """
        return await self._provider.estimate_gas(to, data, from_, value, block)

    async def get_storage_at(
        self,
        address: str,
        position: int,
        block: int | None = None,
    ) -> bytes:
        """Get storage at a given position.

        Returns:
            The storage value as bytes.

        """
        return await self._provider.get_storage_at(address, position, block)

    async def get_balance(self, address: str, block: int | None = None) -> int:
        """Get the balance of an address in wei.

        Returns:
            The balance in wei.

        """
        return await self._provider.get_balance(address, block)

    async def get_transaction_count(
        self,
        address: str,
        block: int | None = None,
    ) -> int:
        """Get the transaction count (nonce) for an address.

        Returns:
            The transaction count (nonce).

        """
        return await self._provider.get_transaction_count(address, block)

    async def make_request(self, method: str, params: list[Any]) -> Any:  # ruff:ignore[any-type]
        """Make a raw JSON-RPC request.

        Returns:
            The raw JSON-RPC response.

        """
        return await self._provider.make_request(method, params)

    def is_connected(self) -> bool:  # ruff:ignore[no-self-use]
        """Check if the provider is connected.

        Returns:
            True if connected.

        """
        return True

    def close(self) -> None:
        """Close the provider and release resources."""
        self._provider.close()


class _AsyncAlloyIntrospectionMixin(_AsyncAlloyBacked):
    """Introspection surface for ``AsyncAlloyProvider``."""

    # ----- Introspection -----

    def as_async_alloy(self) -> RustAsyncAlloyProvider:
        """Return the inner Rust ``AsyncAlloyProvider`` pyclass.

        Rust pyclasses (e.g. ``SimulateContext``, ``dispatch_and_submit_py``)
        expect the Rust ``AsyncAlloyProvider`` pyclass, not this Python wrapper.

        Returns:
            The inner Rust ``AsyncAlloyProvider`` pyclass.

        """
        return self._provider

    def __repr__(self) -> str:
        """Return a string representation.

        Returns:
            A string representation of the provider.

        """
        return f"AsyncAlloyProvider(rpc_url={self.rpc_url!r})"


class AsyncAlloyProvider(
    _AsyncAlloyEndpointMixin,
    _AsyncAlloyQueryMixin,
    _AsyncAlloyIntrospectionMixin,
):
    """High-performance async Ethereum RPC provider using Alloy.

    A thin Python wrapper around the Rust ``AsyncAlloyProvider`` pyclass.
    Adds string block-identifier resolution (``'latest'``, ``'earliest'``,
    ``'pending'``) and exposes the inner Rust pyclass via
    :meth:`as_async_alloy` for Rust-side call seams. The public surface is
    assembled from the async query mixins.

    Args:
        rust_provider: The underlying Rust ``AsyncAlloyProvider`` pyclass.

    Prefer :meth:`create` to construct an instance from an RPC URL.

    """

    def __init__(self, rust_provider: RustAsyncAlloyProvider) -> None:
        """Initialize the instance."""
        self._provider = rust_provider

    # Six arguments, each a distinct transport or binding knob: the endpoint,
    # the two tuning values, the opt-in rate-limit pair, and the chain to bind
    # the endpoint to. Folding any two of them would hide one behind another.
    @staticmethod
    async def create(  # ruff: ignore[too-many-arguments]
        rpc_url: str,
        max_retries: int = 10,
        max_blocks_per_request: int = 5000,
        *,
        requests_per_second: int | None = None,
        burst: int | None = None,
        chain_id: int | None = None,
    ) -> AsyncAlloyProvider:
        # The optional arguments are keyword-only: a positional sixth
        # argument is a rate limit in one call and a chain in the next.
        """Create an ``AsyncAlloyProvider`` asynchronously.

        Args:
            rpc_url: HTTP/HTTPS/WS/IPC endpoint URL.
            max_retries: Maximum retry attempts.
            max_blocks_per_request: Maximum blocks per log request.
            requests_per_second: Optional rate limit.
            burst: Optional burst size for rate limiting.
            chain_id: Chain to bind the endpoint to; the Rust core raises
                :class:`ValueError` when the endpoint serves another chain.

        Returns:
            An ``AsyncAlloyProvider`` instance.

        """
        rust = await RustAsyncAlloyProvider.create(
            rpc_url,
            max_retries,
            max_blocks_per_request,
            requests_per_second,
            burst,
            chain_id,
        )
        return AsyncAlloyProvider(rust)
