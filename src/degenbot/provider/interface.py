"""
Provider interface for abstracting Web3 and AlloyProvider.

This module defines a Protocol for Ethereum RPC providers and an adapter
that can delegate to either web3.py's Web3 or degenbot's AlloyProvider.

Example:
    >>> from degenbot.provider import AlloyProvider, ProviderAdapter
    >>> from web3 import Web3
    >>>
    >>> # Create adapter for Web3
    >>> w3 = Web3(...)
    >>> provider = ProviderAdapter.from_web3(w3)
    >>>
    >>> # Create adapter for AlloyProvider
    >>> alloy = AlloyProvider("https://eth-mainnet.example.com")
    >>> provider = ProviderAdapter.from_alloy(alloy)
    >>>
    >>> # Use uniformly
    >>> chain_id = provider.chain_id
    >>> block = provider.get_block(18_000_000)
    >>> result = provider.call(to="0x...", data=calldata)

"""

from __future__ import annotations

import warnings
from typing import TYPE_CHECKING, Any, Literal, Protocol, Self, cast, runtime_checkable

from web3 import AsyncWeb3, Web3

from degenbot.degenbot_rs import AlloyProvider
from degenbot.exceptions import SubscriptionNotSupported
from degenbot.provider.offline_provider import OfflineProvider
from degenbot.provider.subscription import Subscription

if TYPE_CHECKING:
    from eth_typing import BlockIdentifier
    from hexbytes import HexBytes
    from web3.types import BlockData, FilterParams, LogReceipt, TxParams

    from degenbot.degenbot_rs import AsyncAlloyProvider

# ruff: noqa: ERA001


# ============================================================================
# Public protocol
# ============================================================================


@runtime_checkable
class ProviderBackend(Protocol):
    """
    Protocol for sync provider backends.

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
        """
        Not available on sync providers.

        Use AsyncProviderAdapter.subscribe_blocks() instead.
        """
        ...

    def subscribe_full_blocks(self) -> None:
        """
        Not available on sync providers.

        Use AsyncProviderAdapter.subscribe_full_blocks() instead.
        """
        ...

    def subscribe_pending_transactions(self) -> None:
        """
        Not available on sync providers.

        Use AsyncProviderAdapter.subscribe_pending_transactions() instead.
        """
        ...

    def subscribe_full_pending_transactions(self) -> None:
        """
        Not available on sync providers.

        Use AsyncProviderAdapter.subscribe_full_pending_transactions() instead.
        """
        ...

    def subscribe_logs(
        self,
        _addresses: list[str] | None = None,
        _topics: list[list[str]] | None = None,
    ) -> None:
        """
        Not available on sync providers.

        Use AsyncProviderAdapter.subscribe_logs() instead.
        """
        ...


# ============================================================================
# Subscription support mixins
# ============================================================================


class SyncSubscriptionSupport:
    """
    Mixin providing default subscription stubs for sync backends.

    Sync providers never support subscriptions. This mixin satisfies
    the ProviderBackend protocol requirement without duplicating 5 identical
    stubs per adapter.

    Subclasses must define ``_subscription_transport`` and ``_subscription_rpc_url``
    so error messages include the transport type and endpoint.
    """

    @property
    def _subscription_transport(self) -> str:  # type: ignore[override]
        return "sync"

    @property
    def _subscription_rpc_url(self) -> str:
        return "unknown"

    def subscribe_blocks(self) -> None:
        """Not available on sync providers."""
        raise SubscriptionNotSupported(
            transport=self._subscription_transport,
            rpc_url=self._subscription_rpc_url,
        )

    def subscribe_full_blocks(self) -> None:
        """Not available on sync providers."""
        raise SubscriptionNotSupported(
            transport=self._subscription_transport,
            rpc_url=self._subscription_rpc_url,
        )

    def subscribe_pending_transactions(self) -> None:
        """Not available on sync providers."""
        raise SubscriptionNotSupported(
            transport=self._subscription_transport,
            rpc_url=self._subscription_rpc_url,
        )

    def subscribe_full_pending_transactions(self) -> None:
        """Not available on sync providers."""
        raise SubscriptionNotSupported(
            transport=self._subscription_transport,
            rpc_url=self._subscription_rpc_url,
        )

    def subscribe_logs(
        self,
        _addresses: list[str] | None = None,
        _topics: list[list[str]] | None = None,
    ) -> None:
        """Not available on sync providers."""
        raise SubscriptionNotSupported(
            transport=self._subscription_transport,
            rpc_url=self._subscription_rpc_url,
        )


class AsyncSubscriptionSupport:
    """
    Mixin providing default subscription stubs for async backends.

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
        """Not available — requires WS/IPC transport."""
        raise SubscriptionNotSupported(
            transport=self._subscription_transport,
            rpc_url=self._subscription_rpc_url,
        )

    async def subscribe_full_blocks(self) -> Subscription:
        """Not available — requires WS/IPC transport."""
        raise SubscriptionNotSupported(
            transport=self._subscription_transport,
            rpc_url=self._subscription_rpc_url,
        )

    async def subscribe_pending_transactions(self) -> Subscription:
        """Not available — requires WS/IPC transport."""
        raise SubscriptionNotSupported(
            transport=self._subscription_transport,
            rpc_url=self._subscription_rpc_url,
        )

    async def subscribe_full_pending_transactions(self) -> Subscription:
        """Not available — requires WS/IPC transport."""
        raise SubscriptionNotSupported(
            transport=self._subscription_transport,
            rpc_url=self._subscription_rpc_url,
        )

    async def subscribe_logs(
        self,
        addresses: list[str] | None = None,  # ruff: ignore[ARG002]
        topics: list[list[str]] | None = None,  # ruff: ignore[ARG002]
    ) -> Subscription:
        """Not available — requires WS/IPC transport."""
        raise SubscriptionNotSupported(
            transport=self._subscription_transport,
            rpc_url=self._subscription_rpc_url,
        )


# ============================================================================
# Sync backend adapters
# ============================================================================


class _Web3Adapter(SyncSubscriptionSupport):
    """Adapter wrapping a web3.py Web3 instance to satisfy ProviderBackend."""

    def __init__(self, w3: Web3) -> None:
        self._w3 = w3

    # --- Subscription support override ---

    @property
    def _subscription_transport(self) -> str:  # type: ignore[override]
        return "web3"

    @property
    def _subscription_rpc_url(self) -> str:
        return str(self._w3.provider)

    # --- Core methods ---

    @property
    def chain_id(self) -> int:
        return self._w3.eth.chain_id

    @property
    def block_number(self) -> int:
        return self._w3.eth.block_number

    def get_block_number(self) -> int:
        return self._w3.eth.get_block_number()

    def get_block(self, block_identifier: int | str) -> BlockData | None:
        return self._w3.eth.get_block(block_identifier)  # ty:ignore[invalid-argument-type]

    def get_logs(
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
        return self._w3.eth.get_logs(filter_param)

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        tx: TxParams = {"to": to, "data": data}
        return self._w3.eth.call(tx, block)

    def call_raw(self, tx: TxParams, block: BlockIdentifier | None = None) -> HexBytes:
        return self._w3.eth.call(tx, block)

    def get_code(self, address: str, block: int | None = None) -> HexBytes:
        return self._w3.eth.get_code(address, block)  # ty:ignore[invalid-argument-type]

    def get_balance(self, address: str, block: int | None = None) -> int:
        return self._w3.eth.get_balance(address, block)  # ty:ignore[invalid-argument-type]

    def get_storage_at(self, address: str, position: int, block: int | None = None) -> HexBytes:
        return self._w3.eth.get_storage_at(address, position, block)  # ty:ignore[invalid-argument-type]

    def get_transaction_count(self, address: str, block: int | None = None) -> int:
        return self._w3.eth.get_transaction_count(address, block)  # ty:ignore[invalid-argument-type]

    def is_connected(self) -> bool:
        return self._w3.is_connected()

    def close(self) -> None:
        if hasattr(self._w3, "close"):
            self._w3.close()  # ty:ignore[call-non-callable]


class _AlloyAdapter(SyncSubscriptionSupport):
    """Adapter wrapping an AlloyProvider instance to satisfy ProviderBackend."""

    def __init__(self, alloy: AlloyProvider) -> None:
        self._alloy = alloy

    # --- Subscription support override ---

    @property
    def _subscription_rpc_url(self) -> str:
        return self._alloy.rpc_url

    @property
    def chain_id(self) -> int:
        return self._alloy.get_chain_id()

    @property
    def block_number(self) -> int:
        return self._alloy.get_block_number()

    def get_block_number(self) -> int:
        return self._alloy.get_block_number()

    def get_block(self, block_identifier: int | str) -> BlockData | None:
        # AlloyProvider only supports integer block numbers
        if isinstance(block_identifier, str):
            if block_identifier == "latest":
                block_identifier = self._alloy.get_block_number()
            elif block_identifier == "earliest":
                block_identifier = 0
            elif block_identifier == "pending":
                block_identifier = self._alloy.get_block_number() + 1
        return self._alloy.get_block(block_identifier)  # ty:ignore[invalid-argument-type, invalid-return-type]

    def get_logs(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> list[LogReceipt]:
        return self._alloy.get_logs(
            from_block=from_block,
            to_block=to_block,
            addresses=addresses,
            topics=topics,
        )  # ty:ignore[invalid-return-type]

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        return self._alloy.call(to, data, block_number=block)

    def call_raw(self, tx: TxParams, block: BlockIdentifier | None = None) -> HexBytes:
        return self._alloy.call(tx["to"], tx["data"], block_number=block)  # ty:ignore[invalid-argument-type]

    def get_code(self, address: str, block: int | None = None) -> HexBytes:
        return self._alloy.get_code(address, block_number=block)

    def get_balance(self, address: str, block: int | None = None) -> int:
        return self._alloy.get_balance(address, block_number=block)

    def get_storage_at(self, address: str, position: int, block: int | None = None) -> HexBytes:
        return self._alloy.get_storage_at(address, position, block_number=block)

    def get_transaction_count(self, address: str, block: int | None = None) -> int:
        return self._alloy.get_transaction_count(address, block_number=block)

    def is_connected(self) -> bool:  # noqa: PLR6301
        return True

    def close(self) -> None:
        if hasattr(self._alloy, "close"):
            self._alloy.close()


class _OfflineAdapter(SyncSubscriptionSupport):
    """Adapter wrapping an OfflineProvider instance to satisfy ProviderBackend."""

    def __init__(self, offline: OfflineProvider) -> None:
        self._offline = offline

    # --- Subscription support override ---

    @property
    def _subscription_transport(self) -> str:  # type: ignore[override]
        return "offline"

    @property
    def _subscription_rpc_url(self) -> str:
        return "offline"

    @property
    def chain_id(self) -> int:
        return self._offline.chain_id

    @property
    def block_number(self) -> int:
        return self._offline.block_number

    def get_block_number(self) -> int:
        return self._offline.get_block_number()

    def get_block(self, block_identifier: int | str) -> BlockData | None:
        return self._offline.get_block(block_identifier)  # ty:ignore[invalid-return-type]

    def get_logs(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> list[LogReceipt]:
        return self._offline.get_logs(
            from_block=from_block,
            to_block=to_block,
            addresses=addresses,
            topics=topics,
        )  # ty:ignore[invalid-return-type]

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        return self._offline.call(to, data, block_number=block)

    def call_raw(self, tx: TxParams, block: BlockIdentifier | None = None) -> HexBytes:
        return self._offline.call(tx["to"], tx["data"], block_number=block)  # ty:ignore[invalid-argument-type]

    def get_code(self, address: str, block: int | None = None) -> HexBytes:
        return self._offline.get_code(address, block_number=block)

    def get_balance(self, address: str, block: int | None = None) -> int:
        return self._offline.get_balance(address, block_number=block)

    def get_storage_at(self, address: str, position: int, block: int | None = None) -> HexBytes:
        return self._offline.get_storage_at(address, position, block_number=block)

    def get_transaction_count(self, address: str, block: int | None = None) -> int:
        return self._offline.get_transaction_count(address, block_number=block)

    def is_connected(self) -> bool:  # noqa: PLR6301
        return True

    def close(self) -> None:
        if hasattr(self._offline, "close"):
            self._offline.close()  # ty:ignore[call-non-callable]


# ============================================================================
# ProviderAdapter (sync)
# ============================================================================


class ProviderAdapter(SyncSubscriptionSupport):
    """
    Adapter that wraps Web3, AlloyProvider, or OfflineProvider.

    Provides a uniform interface for Ethereum RPC operations,
    allowing existing code to work with any backend.

    Use factory methods to create:
        - ProviderAdapter.from_web3(w3)
        - ProviderAdapter.from_alloy(alloy_provider)
        - ProviderAdapter.from_offline(offline_provider)
    """

    def __init__(
        self,
        backend: ProviderBackend,
        *,
        provider_type: Literal["web3", "alloy", "offline"],
        raw_provider: AlloyProvider | OfflineProvider | Web3 | None = None,
    ) -> None:
        """
        Initialize the adapter with a backend.

        Args:
            backend: A provider backend satisfying ProviderBackend
            provider_type: The type label for the backend (used by repr and pickling)
            raw_provider: The original unwrapped provider (exposed by underlying / provider)

        """
        self._backend = backend
        self._provider_type = provider_type
        self._raw_provider = raw_provider

    # --- Subscription support override ---

    @property
    def _subscription_rpc_url(self) -> str:
        return str(self._raw_provider)

    # --- Pickle support ---

    def __getstate__(self) -> dict[str, Any]:
        """Pickle by storing only the type label; the provider must be re-acquired."""
        return {
            "_provider_type": self._provider_type,
            "_backend": None,
            "_raw_provider": None,
        }

    def __setstate__(self, state: dict[str, Any]) -> None:
        """Restore the type label. The backend must be set externally via set_provider."""
        self.__dict__ = state

    def set_provider(self, provider: AlloyProvider | OfflineProvider | Web3) -> None:
        """Set the underlying provider by re-wrapping it in the correct backend."""
        self._backend = _backend_for_type(self._provider_type, provider)
        self._raw_provider = provider

    # -------------------------------------------------------------------------
    # Factory methods
    # -------------------------------------------------------------------------

    @classmethod
    def from_web3(cls, w3: Web3) -> Self:
        """Create an adapter wrapping a Web3 instance."""
        return cls(_Web3Adapter(w3), provider_type="web3", raw_provider=w3)

    @classmethod
    def from_alloy(cls, alloy: AlloyProvider) -> Self:
        """Create an adapter wrapping an AlloyProvider instance."""
        return cls(_AlloyAdapter(alloy), provider_type="alloy", raw_provider=alloy)

    @classmethod
    def from_offline(cls, offline: OfflineProvider) -> Self:
        """Create an adapter wrapping an OfflineProvider instance."""
        return cls(_OfflineAdapter(offline), provider_type="offline", raw_provider=offline)

    # -------------------------------------------------------------------------
    # Introspection
    # -------------------------------------------------------------------------

    @property
    def provider_type(self) -> Literal["web3", "alloy", "offline"]:
        """Get the type of the underlying provider."""
        return self._provider_type

    @property
    def underlying(self) -> AlloyProvider | OfflineProvider | Web3 | None:
        """
        Get the underlying provider instance.

        .. deprecated:: 0.x
            This escape hatch will be removed in a future release.
            Use ProviderAdapter methods directly instead.
        """
        warnings.warn(
            "ProviderAdapter.underlying is deprecated — use ProviderAdapter methods directly.",
            DeprecationWarning,
            stacklevel=2,
        )
        return self._raw_provider

    @property
    def provider(self) -> AlloyProvider | OfflineProvider | Web3 | None:
        """Get the underlying provider, or None if not set (e.g., after unpickling)."""
        return self._raw_provider

    def as_web3(self) -> Web3 | None:
        """Return the underlying provider as Web3, or None if not a Web3 provider."""
        if self._provider_type == "web3" and isinstance(self._raw_provider, Web3):
            return self._raw_provider
        return None

    def as_alloy(self) -> AlloyProvider | None:
        """Return the underlying provider as AlloyProvider, or None if not an Alloy provider."""
        if self._provider_type == "alloy" and isinstance(self._raw_provider, AlloyProvider):
            return self._raw_provider
        return None

    def as_offline(self) -> OfflineProvider | None:
        """Return the underlying provider as OfflineProvider, or None if not an Offline provider."""
        if self._provider_type == "offline" and isinstance(self._raw_provider, OfflineProvider):
            return self._raw_provider
        return None

    # -------------------------------------------------------------------------
    # Properties (delegated)
    # -------------------------------------------------------------------------

    @property
    def chain_id(self) -> int:
        """Get the chain ID."""
        return self._backend.chain_id

    @property
    def block_number(self) -> int:
        """Get the current block number."""
        return self._backend.block_number

    # -------------------------------------------------------------------------
    # Methods with extra logic (kept explicit)
    # -------------------------------------------------------------------------

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        """Execute an eth_call."""
        return self._backend.call(to, data, block)

    def call_raw(self, tx: TxParams, block: BlockIdentifier | None = None) -> HexBytes:
        """
        Execute an eth_call with a raw transaction dict.

        The dict must contain ``to`` (address string) and ``data`` (bytes).
        Additional keys (e.g. ``from``, ``value``) are passed through to the
        backend.

        This is the low-level counterpart to :meth:`call`, which uses keyword
        arguments. Prefer :meth:`call` for new code; use :meth:`call_raw` when
        migrating existing ``w3.eth.call({"to": ..., "data": ...})`` call
        sites.
        """
        return self._backend.call_raw(tx, block)

    def batch_call(self, calls: list[TxParams], block: int | None = None) -> list[HexBytes]:
        """
        Execute multiple eth_calls and return results in input order.

        Each entry in ``calls`` is a transaction dict with at least ``to`` and
        ``data`` keys, matching the :meth:`call_raw` signature.

        The default implementation sends requests sequentially. Backends that
        support batching (e.g. multicall3) can override this for better
        performance.
        """
        return [self._backend.call_raw(tx, block) for tx in calls]

    def get_block_timestamp(self, block: int | None = None) -> int:
        """
        Get the timestamp for a block.

        Args:
            block: Block number, or None for latest.

        Returns:
            The block timestamp as an integer (Unix seconds).

        """
        block_data = self._backend.get_block(block if block is not None else "latest")
        if block_data is None:
            msg = f"Block {block} not found"
            raise ValueError(msg)
        return block_data["timestamp"]

    def make_request(self, method: str, params: list[Any]) -> Any:  # noqa: ANN401
        """
        Make a raw JSON-RPC request.

        This allows calling arbitrary RPC methods that don't have typed wrappers.
        Only available for AlloyProvider backends; raises AttributeError for others.

        Args:
            method: The RPC method name (e.g., "debug_traceTransaction")
            params: The parameters as a list

        Returns:
            The raw result (deserialized from JSON)

        Raises:
            AttributeError: If the underlying provider doesn't support make_request

        """
        raw = self._raw_provider
        if raw is not None and hasattr(raw, "make_request"):
            return raw.make_request(method, params)  # ty:ignore[call-non-callable]
        msg = f"Provider type '{self._provider_type}' does not support make_request"
        raise AttributeError(msg)

    def get_block_number(self) -> int:
        """Get the current block number."""
        return self._backend.get_block_number()

    def get_block(self, block_identifier: int | str) -> BlockData | None:
        """
        Get block data for a given block identifier.

        Args:
            block_identifier: Block number or string ('latest', 'earliest', 'pending').

        Returns:
            Block data dict or None if block not found.

        """
        return self._backend.get_block(block_identifier)

    def get_logs(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> list[LogReceipt]:
        """
        Get logs matching the filter parameters.

        Args:
            from_block: Starting block number (inclusive).
            to_block: Ending block number (inclusive).
            addresses: Optional list of contract addresses to filter.
            topics: Optional list of topic filters.

        Returns:
            List of log receipts matching the filter.

        """
        return self._backend.get_logs(from_block, to_block, addresses, topics)

    def get_code(self, address: str, block: int | None = None) -> HexBytes:
        """
        Get the bytecode at an address.

        Args:
            address: Contract address.
            block: Block number, or None for latest.

        Returns:
            Contract bytecode as HexBytes.

        """
        return self._backend.get_code(address, block)

    def get_balance(self, address: str, block: int | None = None) -> int:
        """
        Get the ETH balance at an address.

        Args:
            address: Account address.
            block: Block number, or None for latest.

        Returns:
            Balance in wei as integer.

        """
        return self._backend.get_balance(address, block)

    def get_storage_at(
        self,
        address: str,
        position: int,
        block: int | None = None,
    ) -> HexBytes:
        """
        Get storage at a given position.

        Args:
            address: Contract address.
            position: Storage slot position.
            block: Block number, or None for latest.

        Returns:
            32-byte storage value as HexBytes.

        """
        return self._backend.get_storage_at(address, position, block)

    def get_transaction_count(self, address: str, block: int | None = None) -> int:
        """
        Get the transaction count (nonce) for an address.

        Args:
            address: Account address.
            block: Block number, or None for latest.

        Returns:
            Transaction count as integer.

        """
        return self._backend.get_transaction_count(address, block)

    def is_connected(self) -> bool:
        """
        Check if the provider is connected.

        Returns:
            True if connected, False otherwise.

        """
        return self._backend.is_connected()

    def close(self) -> None:
        """Close the provider connection and release resources."""
        self._backend.close()

    def __repr__(self) -> str:
        """Return a string representation."""
        return f"ProviderAdapter(type={self._provider_type})"


# ============================================================================
# Private async backend protocol
# ============================================================================


@runtime_checkable
class AsyncProviderBackend(Protocol):
    """
    Protocol for async provider backends.

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
        """
        Subscribe to new block headers.

        Raises SubscriptionNotSupported if transport doesn't support it.
        """
        ...

    async def subscribe_full_blocks(self) -> Subscription:
        """
        Subscribe to full block bodies.

        Raises SubscriptionNotSupported if transport doesn't support it.
        """
        ...

    async def subscribe_pending_transactions(self) -> Subscription:
        """
        Subscribe to pending transaction hashes.

        Raises SubscriptionNotSupported if transport doesn't support it.
        """
        ...

    async def subscribe_full_pending_transactions(self) -> Subscription:
        """
        Subscribe to full pending transaction bodies.

        Raises SubscriptionNotSupported if transport doesn't support it.
        """
        ...

    async def subscribe_logs(
        self,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> Subscription:
        """
        Subscribe to filtered log events.

        Raises SubscriptionNotSupported if transport doesn't support it.
        """
        ...


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
        return await self._alloy.call(to, data, block_number=block)

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
        """Subscribe to new block headers via Alloy WS/IPC connection."""
        raw_sub = self._alloy.subscribe_blocks()
        return Subscription(_inner=raw_sub)

    async def subscribe_full_blocks(self) -> Subscription:
        """Subscribe to full block bodies via Alloy WS/IPC connection."""
        raw_sub = self._alloy.subscribe_full_blocks()
        return Subscription(_inner=raw_sub)

    async def subscribe_pending_transactions(self) -> Subscription:
        """Subscribe to pending transaction hashes via Alloy WS/IPC connection."""
        raw_sub = self._alloy.subscribe_pending_transactions()
        return Subscription(_inner=raw_sub)

    async def subscribe_full_pending_transactions(self) -> Subscription:
        """Subscribe to full pending transaction bodies via Alloy WS/IPC connection."""
        raw_sub = self._alloy.subscribe_full_pending_transactions()
        return Subscription(_inner=raw_sub)

    async def subscribe_logs(
        self,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> Subscription:
        """Subscribe to filtered log events via Alloy WS/IPC connection."""
        raw_sub = self._alloy.subscribe_logs(addresses=addresses or [], topics=topics or [])
        return Subscription(_inner=raw_sub)


# ============================================================================
# AsyncProviderAdapter
# ============================================================================


class AsyncProviderAdapter:
    """
    Async adapter that wraps either AsyncWeb3 or AsyncAlloyProvider.

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
        raw_provider: (
            AsyncWeb3[Any] | AlloyProvider | AsyncAlloyProvider | OfflineProvider | None
        ) = None,
    ) -> None:
        """Initialize the instance."""
        self._backend = backend
        self._provider_type = provider_type
        self._raw_provider = raw_provider

    @classmethod
    def from_web3(cls, async_w3: AsyncWeb3[Any]) -> Self:
        """Create an adapter wrapping an AsyncWeb3 instance."""
        return cls(_AsyncWeb3Adapter(async_w3), provider_type="web3", raw_provider=async_w3)

    @classmethod
    def from_alloy(cls, async_alloy: AsyncAlloyProvider) -> Self:
        """Create an adapter wrapping a Rust AsyncAlloyProvider."""
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
    ) -> AsyncWeb3[Any] | AlloyProvider | AsyncAlloyProvider | OfflineProvider | None:
        """
        Get the underlying provider instance.

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
        """Return the underlying provider as AsyncWeb3, or None if not a Web3 provider."""
        if self._provider_type == "web3" and isinstance(self._raw_provider, AsyncWeb3):
            return self._raw_provider
        return None

    def as_alloy(self) -> AlloyProvider | None:
        """Return the underlying provider as AlloyProvider, or None if not an Alloy provider."""
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
        """
        Get the chain ID.

        Returns:
            The chain ID as integer.

        """
        return await self._backend.get_chain_id()

    async def get_block_number(self) -> int:
        """
        Get the current block number.

        Returns:
            The current block number as integer.

        """
        return await self._backend.get_block_number()

    async def get_block(self, block_identifier: int | str) -> BlockData | None:
        """
        Get block data for a given block identifier.

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
        """
        Get logs matching the filter parameters.

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
        """
        Execute an eth_call.

        Args:
            to: Contract address.
            data: Call data bytes.
            block: Block number, or None for latest.

        Returns:
            Call result as HexBytes.

        """
        return await self._backend.call(to, data, block)

    async def get_code(self, address: str, block: int | None = None) -> HexBytes:
        """
        Get the bytecode at an address.

        Args:
            address: Contract address.
            block: Block number, or None for latest.

        Returns:
            Contract bytecode as HexBytes.

        """
        return await self._backend.get_code(address, block)

    async def get_balance(self, address: str, block: int | None = None) -> int:
        """
        Get the ETH balance at an address.

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
        """
        Get storage at a given position.

        Args:
            address: Contract address.
            position: Storage slot position.
            block: Block number, or None for latest.

        Returns:
            32-byte storage value as HexBytes.

        """
        return await self._backend.get_storage_at(address, position, block)

    async def get_transaction_count(self, address: str, block: int | None = None) -> int:
        """
        Get the transaction count (nonce) for an address.

        Args:
            address: Account address.
            block: Block number, or None for latest.

        Returns:
            Transaction count as integer.

        """
        return await self._backend.get_transaction_count(address, block)

    def is_connected(self) -> bool:
        """
        Check if the provider is connected.

        Returns:
            True if connected, False otherwise.

        """
        return self._backend.is_connected()

    def close(self) -> None:
        """Close the provider connection and release resources."""
        self._backend.close()

    def __repr__(self) -> str:
        """Return a string representation."""
        return f"AsyncProviderAdapter(type={self._provider_type})"

    # -------------------------------------------------------------------------
    # Pickle support
    # -------------------------------------------------------------------------

    def __getstate__(self) -> dict[str, Any]:
        """Pickle by storing only the type label; subscriptions are not picklable."""
        state = self.__dict__.copy()
        # Remove unpicklable subscription manager
        # Backend and raw_provider are also not reliably picklable
        state["_backend"] = None
        state["_raw_provider"] = None
        return state

    def __setstate__(self, state: dict[str, Any]) -> None:
        """Restore state; the backend must be set externally via set_provider."""
        self.__dict__ = state

    # --- Subscription methods ---

    async def subscribe_blocks(self) -> Subscription:
        """
        Subscribe to new block headers.

        Requires a WS or IPC transport. Raises SubscriptionNotSupported
        if the underlying provider doesn't support eth_subscribe.
        """
        return await self._backend.subscribe_blocks()

    async def subscribe_full_blocks(self) -> Subscription:
        """
        Subscribe to full block bodies (with full transactions).

        Requires a WS or IPC transport. Raises SubscriptionNotSupported
        if the underlying provider doesn't support eth_subscribe.
        """
        return await self._backend.subscribe_full_blocks()

    async def subscribe_pending_transactions(self) -> Subscription:
        """
        Subscribe to pending transaction hashes.

        Requires a WS or IPC transport. Raises SubscriptionNotSupported
        if the underlying provider doesn't support eth_subscribe.
        """
        return await self._backend.subscribe_pending_transactions()

    async def subscribe_full_pending_transactions(self) -> Subscription:
        """
        Subscribe to full pending transaction bodies.

        Requires a WS or IPC transport. Raises SubscriptionNotSupported
        if the underlying provider doesn't support eth_subscribe.
        """
        return await self._backend.subscribe_full_pending_transactions()

    async def subscribe_logs(
        self,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> Subscription:
        """
        Subscribe to filtered log events.

        Args:
            addresses: Contract addresses to filter.
            topics: Event topic filters (nested: [[topic0], [topic1], ...]).

        Requires a WS or IPC transport. Raises SubscriptionNotSupported
        if the underlying provider doesn't support eth_subscribe.

        """
        return await self._backend.subscribe_logs(addresses=addresses, topics=topics)


# ============================================================================
# Internal helper for round-trip pickling
# ============================================================================


def _backend_for_type(
    provider_type: Literal["web3", "alloy", "offline"],
    provider: AlloyProvider | OfflineProvider | Web3,
) -> ProviderBackend:
    """Create the correct backend adapter for a provider type label."""
    match provider_type:
        case "web3":
            return _Web3Adapter(cast("Web3", provider))
        case "alloy":
            return _AlloyAdapter(cast("AlloyProvider", provider))
        case "offline":
            return _OfflineAdapter(cast("OfflineProvider", provider))
        case _:
            msg = f"Unknown provider type: {provider_type}"
            raise ValueError(msg)


# Public API
__all__ = [
    "AsyncProviderAdapter",
    "AsyncProviderBackend",
    "ProviderAdapter",
    "ProviderBackend",
]
