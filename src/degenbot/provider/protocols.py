"""Public protocol seams for Ethereum RPC providers.

These runtime-checkable protocols define the sync and async backend
interfaces. Callers that only need the protocol (not the adapters)
can import from this module without pulling in adapter implementation
details.

"""

from __future__ import annotations

from typing import TYPE_CHECKING, Protocol, runtime_checkable

if TYPE_CHECKING:
    from eth_typing import BlockIdentifier
    from hexbytes import HexBytes
    from web3.types import BlockData, LogReceipt, TxParams

    from degenbot.provider.subscription import Subscription


@runtime_checkable
class ProviderBackend(Protocol):
    """Protocol for sync provider backends.

    Merged public protocol for any sync Ethereum RPC backend.
    """

    @property
    def chain_id(self) -> int:
        """Return chain id."""
        ...

    @property
    def block_number(self) -> int:
        """Return block number."""
        ...

    def get_block_number(self) -> int:
        """Return block number."""
        ...

    def get_block(self, block_identifier: int | str) -> BlockData | None:
        """Return block."""
        ...

    def get_logs(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> list[LogReceipt]:
        """Return logs."""
        ...

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        """Call."""
        ...

    def call_raw(self, tx: TxParams, block: BlockIdentifier | None = None) -> HexBytes:
        """Call raw."""
        ...

    def get_code(self, address: str, block: int | None = None) -> HexBytes:
        """Return code."""
        ...

    def get_balance(self, address: str, block: int | None = None) -> int:
        """Return balance."""
        ...

    def get_storage_at(
        self,
        address: str,
        position: int,
        block: int | None = None,
    ) -> HexBytes:
        """Return storage at."""
        ...

    def get_transaction_count(
        self,
        address: str,
        block: int | None = None,
    ) -> int:
        """Return transaction count."""
        ...

    def is_connected(self) -> bool:
        """Check whether connected."""
        ...

    def close(self) -> None:
        """Perform close."""
        ...

    # --- Subscription stubs (sync providers do not support subscriptions) ---

    def subscribe_blocks(self) -> None:
        """Not available on sync providers.

        Use AsyncProviderAdapter.subscribe_blocks() instead.
        """
        ...

    def subscribe_full_blocks(self) -> None:
        """Not available on sync providers.

        Use AsyncProviderAdapter.subscribe_full_blocks() instead.
        """
        ...

    def subscribe_pending_transactions(self) -> None:
        """Not available on sync providers.

        Use AsyncProviderAdapter.subscribe_pending_transactions() instead.
        """
        ...

    def subscribe_full_pending_transactions(self) -> None:
        """Not available on sync providers.

        Use AsyncProviderAdapter.subscribe_full_pending_transactions() instead.
        """
        ...

    def subscribe_logs(
        self,
        _addresses: list[str] | None = None,
        _topics: list[list[str]] | None = None,
    ) -> None:
        """Not available on sync providers.

        Use AsyncProviderAdapter.subscribe_logs() instead.
        """
        ...


@runtime_checkable
class AsyncProviderBackend(Protocol):
    """Protocol for async provider backends.

    Public, runtime-checkable protocol for async Ethereum RPC backends.
    """

    async def get_block_number(self) -> int:
        """Return the current block number."""
        ...

    async def get_chain_id(self) -> int:
        """Return the chain ID."""
        ...

    async def get_block(self, block_identifier: int | str) -> BlockData | None:
        """Return block data for the given identifier."""
        ...

    async def get_logs(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> list[LogReceipt]:
        """Return logs matching the filter."""
        ...

    async def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        """Perform an eth_call and return the result."""
        ...

    async def get_code(self, address: str, block: int | None = None) -> HexBytes:
        """Return the code at the given address."""
        ...

    async def get_balance(self, address: str, block: int | None = None) -> int:
        """Return the ETH balance at the given address."""
        ...

    async def get_storage_at(
        self, address: str, position: int, block: int | None = None
    ) -> HexBytes:
        """Return the storage value at the given position."""
        ...

    async def get_transaction_count(self, address: str, block: int | None = None) -> int:
        """Return the transaction count for the given address."""
        ...

    def is_connected(self) -> bool:
        """Check whether connected."""
        ...

    def close(self) -> None:
        """Perform close."""
        ...

    # --- Subscription methods (optional — override in backend) ---

    async def subscribe_blocks(self) -> Subscription:
        """Subscribe to new block headers.

        Raises SubscriptionNotSupported if transport doesn't support it.
        """
        ...

    async def subscribe_full_blocks(self) -> Subscription:
        """Subscribe to full block bodies.

        Raises SubscriptionNotSupported if transport doesn't support it.
        """
        ...

    async def subscribe_pending_transactions(self) -> Subscription:
        """Subscribe to pending transaction hashes.

        Raises SubscriptionNotSupported if transport doesn't support it.
        """
        ...

    async def subscribe_full_pending_transactions(self) -> Subscription:
        """Subscribe to full pending transaction bodies.

        Raises SubscriptionNotSupported if transport doesn't support it.
        """
        ...

    async def subscribe_logs(
        self,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> Subscription:
        """Subscribe to filtered log events.

        Raises SubscriptionNotSupported if transport doesn't support it.
        """
        ...
