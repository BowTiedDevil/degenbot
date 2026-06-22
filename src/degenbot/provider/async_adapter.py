"""Async provider adapters and the AsyncProviderAdapter facade.

Contains the async subscription support mixin, private backend adapters
(_AsyncWeb3Adapter, _AsyncAlloyAdapter), and the public AsyncProviderAdapter.
"""

from __future__ import annotations

import warnings
from typing import TYPE_CHECKING, Any, Literal, Self

from web3 import AsyncWeb3

from degenbot.degenbot_rs import AlloyProvider
from degenbot.exceptions import SubscriptionNotSupported
from degenbot.provider.alloy_errors import alloy_revert_error, is_alloy_revert
from degenbot.provider.subscription import Subscription

if TYPE_CHECKING:
    from hexbytes import HexBytes
    from web3.types import BlockData, FilterParams, LogReceipt, TxParams

    from degenbot.degenbot_rs import AsyncAlloyProvider
    from degenbot.provider.protocols import AsyncProviderBackend

# ============================================================================
# Subscription support mixin
# ============================================================================


class AsyncSubscriptionSupport:
    """Mixin providing default subscription stubs for async backends.

    For providers that don't support WS/IPC subscriptions (e.g. AsyncWeb3 over HTTP).

    Subclasses must define ``_subscription_transport`` and ``_subscription_rpc_url``
    so error messages include the transport type and endpoint.
    """

    @property
    def _subscription_transport(self) -> str:
        return "async"

    @property
    def _subscription_rpc_url(self) -> str:
        return "unknown"

    async def subscribe_blocks(self) -> Subscription:
        """Not available — requires WS/IPC transport.

        Raises:
            SubscriptionNotSupported: Async provider lacks WS/IPC transport.

        """
        raise SubscriptionNotSupported(
            transport=self._subscription_transport,
            rpc_url=self._subscription_rpc_url,
        )

    async def subscribe_full_blocks(self) -> Subscription:
        """Not available — requires WS/IPC transport.

        Raises:
            SubscriptionNotSupported: Async provider lacks WS/IPC transport.

        """
        raise SubscriptionNotSupported(
            transport=self._subscription_transport,
            rpc_url=self._subscription_rpc_url,
        )

    async def subscribe_pending_transactions(self) -> Subscription:
        """Not available — requires WS/IPC transport.

        Raises:
            SubscriptionNotSupported: Async provider lacks WS/IPC transport.

        """
        raise SubscriptionNotSupported(
            transport=self._subscription_transport,
            rpc_url=self._subscription_rpc_url,
        )

    async def subscribe_full_pending_transactions(self) -> Subscription:
        """Not available — requires WS/IPC transport.

        Raises:
            SubscriptionNotSupported: Async provider lacks WS/IPC transport.

        """
        raise SubscriptionNotSupported(
            transport=self._subscription_transport,
            rpc_url=self._subscription_rpc_url,
        )

    async def subscribe_logs(
        self,
        addresses: list[str] | None = None,  # ruff: ignore[ARG002]
        topics: list[list[str]] | None = None,  # ruff: ignore[ARG002]
    ) -> Subscription:
        """Not available — requires WS/IPC transport.

        Raises:
            SubscriptionNotSupported: Async provider lacks WS/IPC transport.

        """
        raise SubscriptionNotSupported(
            transport=self._subscription_transport,
            rpc_url=self._subscription_rpc_url,
        )


# ============================================================================
# Async backend adapters
# ============================================================================


class _AsyncWeb3Adapter(AsyncSubscriptionSupport):
    """Adapter wrapping an AsyncWeb3 instance to satisfy AsyncProviderBackend."""

    def __init__(self, w3: AsyncWeb3[Any]) -> None:
        self._w3 = w3

    # --- Subscription support override ---

    @property
    def _subscription_transport(self) -> str:
        return "web3"

    @property
    def _subscription_rpc_url(self) -> str:
        return str(self._w3.provider)

    async def get_block_number(self) -> int:
        return await self._w3.eth.get_block_number()

    async def get_chain_id(self) -> int:
        return await self._w3.eth.chain_id

    async def get_block(self, block_identifier: int | str) -> BlockData | None:
        return await self._w3.eth.get_block(block_identifier)  # ty:ignore[invalid-argument-type]

    async def get_logs(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> list[LogReceipt]:
        filter_param: FilterParams = {"fromBlock": from_block, "toBlock": to_block}
        if addresses:
            filter_param["address"] = addresses  # ty:ignore[invalid-assignment]
        if topics:
            filter_param["topics"] = topics
        return await self._w3.eth.get_logs(filter_param)

    async def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        tx: TxParams = {"to": to, "data": data}
        return await self._w3.eth.call(tx, block)

    async def get_code(self, address: str, block: int | None = None) -> HexBytes:
        return await self._w3.eth.get_code(address, block)  # ty:ignore[invalid-argument-type]

    async def get_balance(self, address: str, block: int | None = None) -> int:
        return await self._w3.eth.get_balance(address, block)  # ty:ignore[invalid-argument-type]

    async def get_storage_at(
        self, address: str, position: int, block: int | None = None
    ) -> HexBytes:
        return await self._w3.eth.get_storage_at(address, position, block)  # ty:ignore[invalid-argument-type]

    async def get_transaction_count(self, address: str, block: int | None = None) -> int:
        return await self._w3.eth.get_transaction_count(address, block)  # ty:ignore[invalid-argument-type]

    def is_connected(self) -> bool:  # noqa: PLR6301
        return True

    def close(self) -> None:
        if hasattr(self._w3, "close"):
            self._w3.close()  # ty:ignore[call-non-callable]


class _AsyncAlloyAdapter:
    """Adapter wrapping the Rust AsyncAlloyProvider directly."""

    def __init__(self, alloy: AsyncAlloyProvider) -> None:
        self._alloy = alloy

    async def get_block_number(self) -> int:
        return await self._alloy.get_block_number()

    async def get_chain_id(self) -> int:
        return await self._alloy.get_chain_id()

    async def get_block(self, block_identifier: int | str) -> BlockData | None:
        if isinstance(block_identifier, str):
            if block_identifier == "latest":
                block_identifier = await self._alloy.get_block_number()
            elif block_identifier == "earliest":
                block_identifier = 0
            elif block_identifier == "pending":
                block_identifier = await self._alloy.get_block_number() + 1
        return await self._alloy.get_block(block_identifier)  # ty:ignore[invalid-argument-type, invalid-return-type]

    async def get_logs(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> list[LogReceipt]:
        return await self._alloy.get_logs(
            from_block=from_block,
            to_block=to_block,
            addresses=addresses,
            topics=topics,
        )  # ty:ignore[invalid-return-type]

    async def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        try:
            return await self._alloy.call(to, data, block_number=block)
        except RuntimeError as exc:
            if is_alloy_revert(exc):
                raise alloy_revert_error(exc, to=to) from exc
            raise

    async def get_code(self, address: str, block: int | None = None) -> HexBytes:
        return await self._alloy.get_code(address, block)

    async def get_balance(self, address: str, block: int | None = None) -> int:
        return await self._alloy.get_balance(address, block)

    async def get_storage_at(
        self, address: str, position: int, block: int | None = None
    ) -> HexBytes:
        return await self._alloy.get_storage_at(address, position, block)

    async def get_transaction_count(self, address: str, block: int | None = None) -> int:
        return await self._alloy.get_transaction_count(address, block)

    def is_connected(self) -> bool:  # noqa: PLR6301
        return True

    def close(self) -> None:
        if hasattr(self._alloy, "close"):
            self._alloy.close()

    async def subscribe_blocks(self) -> Subscription:
        """Subscribe to new block headers via Alloy WS/IPC connection.

        Returns:
            A Subscription for new block events.

        """
        raw_sub = self._alloy.subscribe_blocks()
        return Subscription(_inner=raw_sub)

    async def subscribe_full_blocks(self) -> Subscription:
        """Subscribe to full block bodies via Alloy WS/IPC connection.

        Returns:
            A Subscription for full block data.

        """
        raw_sub = self._alloy.subscribe_full_blocks()
        return Subscription(_inner=raw_sub)

    async def subscribe_pending_transactions(self) -> Subscription:
        """Subscribe to pending transaction hashes via Alloy WS/IPC connection.

        Returns:
            A Subscription for pending transactions.

        """
        raw_sub = self._alloy.subscribe_pending_transactions()
        return Subscription(_inner=raw_sub)

    async def subscribe_full_pending_transactions(self) -> Subscription:
        """Subscribe to full pending transaction bodies via Alloy WS/IPC connection.

        Returns:
            A Subscription for full pending transaction data.

        """
        raw_sub = self._alloy.subscribe_full_pending_transactions()
        return Subscription(_inner=raw_sub)

    async def subscribe_logs(
        self,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> Subscription:
        """Subscribe to filtered log events via Alloy WS/IPC connection.

        Returns:
            A Subscription for event logs.

        """
        raw_sub = self._alloy.subscribe_logs(addresses=addresses or [], topics=topics or [])
        return Subscription(_inner=raw_sub)


# ============================================================================
# AsyncProviderAdapter
# ============================================================================


class AsyncProviderAdapter:
    """Async adapter that wraps either AsyncWeb3 or AsyncAlloyProvider.

    Provides a uniform async interface for Ethereum RPC operations,
    allowing existing code to work with either backend.

    Use factory methods to create:
        - AsyncProviderAdapter.from_web3(async_w3)
        - AsyncProviderAdapter.from_alloy(async_alloy_provider)
    """

    def __init__(
        self,
        backend: AsyncProviderBackend,
        *,
        provider_type: Literal["web3", "alloy"],
        raw_provider: (AsyncWeb3[Any] | AlloyProvider | AsyncAlloyProvider | None) = None,
    ) -> None:
        """Initialize the instance."""
        self._backend = backend
        self._provider_type = provider_type
        self._raw_provider = raw_provider

    @classmethod
    def from_web3(cls, async_w3: AsyncWeb3[Any]) -> Self:
        """Create an adapter wrapping an AsyncWeb3 instance.

        Returns:
            An AsyncProviderAdapter wrapping the given AsyncWeb3.

        """
        return cls(_AsyncWeb3Adapter(async_w3), provider_type="web3", raw_provider=async_w3)

    @classmethod
    def from_alloy(cls, async_alloy: AsyncAlloyProvider) -> Self:
        """Create an adapter wrapping a Rust AsyncAlloyProvider.

        Returns:
            An AsyncProviderAdapter wrapping the given AsyncAlloyProvider.

        """
        return cls(
            _AsyncAlloyAdapter(async_alloy),
            provider_type="alloy",
            raw_provider=async_alloy,
        )

    @property
    def provider_type(self) -> Literal["web3", "alloy"]:
        """Get the type of the underlying provider."""
        return self._provider_type

    @property
    def underlying(
        self,
    ) -> AsyncWeb3[Any] | AlloyProvider | AsyncAlloyProvider | None:
        """Get the underlying provider instance.

        .. deprecated:: 0.x
            This escape hatch will be removed in a future release.
            Use AsyncProviderAdapter methods directly instead.
        """
        warnings.warn(
            "AsyncProviderAdapter.underlying is deprecated "
            "— use AsyncProviderAdapter methods directly.",
            DeprecationWarning,
            stacklevel=2,
        )
        return self._raw_provider

    def as_web3(self) -> AsyncWeb3[Any] | None:
        """Return the underlying provider as AsyncWeb3, or None if not a Web3 provider.

        Returns:
            The underlying AsyncWeb3 instance, or None.

        """
        if self._provider_type == "web3" and isinstance(self._raw_provider, AsyncWeb3):
            return self._raw_provider
        return None

    def as_alloy(self) -> AlloyProvider | None:
        """Return the underlying provider as AlloyProvider, or None if not an Alloy provider.

        Returns:
            The underlying AlloyProvider, or None.

        """
        if self._provider_type == "alloy" and isinstance(self._raw_provider, AlloyProvider):
            return self._raw_provider
        return None

    # Note: Async provider properties raise NotImplementedError intentionally.
    # Callers must use the async get_* methods instead.

    @property
    def chain_id(self) -> int:
        """Synchronous property not supported; use get_chain_id()."""
        msg = "Use await get_chain_id() for async provider"
        raise NotImplementedError(msg)

    @property
    def block_number(self) -> int:
        """Synchronous property not supported; use get_block_number()."""
        msg = "Use await get_block_number() for async provider"
        raise NotImplementedError(msg)

    async def get_chain_id(self) -> int:
        """Get the chain ID.

        Returns:
            The chain ID as integer.

        """
        return await self._backend.get_chain_id()

    async def get_block_number(self) -> int:
        """Get the current block number.

        Returns:
            The current block number as integer.

        """
        return await self._backend.get_block_number()

    async def get_block(self, block_identifier: int | str) -> BlockData | None:
        """Get block data for a given block identifier.

        Args:
            block_identifier: Block number or string ('latest', 'earliest', 'pending').

        Returns:
            Block data dict or None if block not found.

        """
        return await self._backend.get_block(block_identifier)

    async def get_logs(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> list[LogReceipt]:
        """Get logs matching the filter parameters.

        Args:
            from_block: Starting block number (inclusive).
            to_block: Ending block number (inclusive).
            addresses: Optional list of contract addresses to filter.
            topics: Optional list of topic filters.

        Returns:
            List of log receipts matching the filter.

        """
        return await self._backend.get_logs(from_block, to_block, addresses, topics)

    async def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        """Execute an eth_call.

        Args:
            to: Contract address.
            data: Call data bytes.
            block: Block number, or None for latest.

        Returns:
            Call result as HexBytes.

        """
        return await self._backend.call(to, data, block)

    async def get_code(self, address: str, block: int | None = None) -> HexBytes:
        """Get the bytecode at an address.

        Args:
            address: Contract address.
            block: Block number, or None for latest.

        Returns:
            Contract bytecode as HexBytes.

        """
        return await self._backend.get_code(address, block)

    async def get_balance(self, address: str, block: int | None = None) -> int:
        """Get the ETH balance at an address.

        Args:
            address: Account address.
            block: Block number, or None for latest.

        Returns:
            Balance in wei as integer.

        """
        return await self._backend.get_balance(address, block)

    async def get_storage_at(
        self,
        address: str,
        position: int,
        block: int | None = None,
    ) -> HexBytes:
        """Get storage at a given position.

        Args:
            address: Contract address.
            position: Storage slot position.
            block: Block number, or None for latest.

        Returns:
            32-byte storage value as HexBytes.

        """
        return await self._backend.get_storage_at(address, position, block)

    async def get_transaction_count(self, address: str, block: int | None = None) -> int:
        """Get the transaction count (nonce) for an address.

        Args:
            address: Account address.
            block: Block number, or None for latest.

        Returns:
            Transaction count as integer.

        """
        return await self._backend.get_transaction_count(address, block)

    def is_connected(self) -> bool:
        """Check if the provider is connected.

        Returns:
            True if connected, False otherwise.

        """
        return self._backend.is_connected()

    def close(self) -> None:
        """Close the provider connection and release resources."""
        self._backend.close()

    def __repr__(self) -> str:
        """Return a string representation.

        Returns:
            A string representation of the adapter.

        """
        return f"AsyncProviderAdapter(type={self._provider_type})"

    # -----------------------------------------------------------------------
    # Subscription methods
    # -----------------------------------------------------------------------

    async def subscribe_blocks(self) -> Subscription:
        """Subscribe to new block headers.

        Requires a WS or IPC transport. Raises SubscriptionNotSupported
        if the underlying provider doesn't support eth_subscribe.

        Returns:
            A Subscription for new block events.

        """
        return await self._backend.subscribe_blocks()

    async def subscribe_full_blocks(self) -> Subscription:
        """Subscribe to full block bodies (with full transactions).

        Requires a WS or IPC transport. Raises SubscriptionNotSupported
        if the underlying provider doesn't support eth_subscribe.

        Returns:
            A Subscription for full block data.

        """
        return await self._backend.subscribe_full_blocks()

    async def subscribe_pending_transactions(self) -> Subscription:
        """Subscribe to pending transaction hashes.

        Requires a WS or IPC transport. Raises SubscriptionNotSupported
        if the underlying provider doesn't support eth_subscribe.

        Returns:
            A Subscription for pending transactions.

        """
        return await self._backend.subscribe_pending_transactions()

    async def subscribe_full_pending_transactions(self) -> Subscription:
        """Subscribe to full pending transaction bodies.

        Requires a WS or IPC transport. Raises SubscriptionNotSupported
        if the underlying provider doesn't support eth_subscribe.

        Returns:
            A Subscription for full pending transaction data.

        """
        return await self._backend.subscribe_full_pending_transactions()

    async def subscribe_logs(
        self,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> Subscription:
        """Subscribe to filtered log events.

        Args:
            addresses: Contract addresses to filter.
            topics: Event topic filters (nested: [[topic0], [topic1], ...]).

        Requires a WS or IPC transport. Raises SubscriptionNotSupported
        if the underlying provider doesn't support eth_subscribe.

        Returns:
            A Subscription for event logs.

        """
        return await self._backend.subscribe_logs(addresses=addresses, topics=topics)
