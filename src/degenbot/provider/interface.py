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

import warnings
from typing import Any, Literal, Protocol, Self, runtime_checkable

from hexbytes import HexBytes

# ruff: noqa: ERA001


# ============================================================================
# Public protocol
# ============================================================================


@runtime_checkable
class EthereumProvider(Protocol):
    """
    Protocol for Ethereum RPC providers.

    Defines the interface that both Web3 and AlloyProvider must satisfy
    for use in degenbot code.
    """

    @property
    def chain_id(self) -> int: ...

    @property
    def block_number(self) -> int: ...

    def get_block_number(self) -> int: ...

    def get_block(self, block_identifier: int | str) -> dict[str, Any] | None: ...

    def get_logs(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> list[dict[str, Any]]: ...

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes: ...

    def get_code(self, address: str, block: int | None = None) -> HexBytes: ...

    def get_balance(self, address: str, block: int | None = None) -> int: ...

    def get_storage_at(
        self,
        address: str,
        position: int,
        block: int | None = None,
    ) -> HexBytes: ...

    def get_transaction_count(
        self,
        address: str,
        block: int | None = None,
    ) -> int: ...

    def is_connected(self) -> bool: ...


# ============================================================================
# Private sync backend protocol
# ============================================================================


class _SyncProviderBackend(Protocol):
    """Private protocol for sync provider backends used by ProviderAdapter."""

    @property
    def chain_id(self) -> int: ...

    @property
    def block_number(self) -> int: ...

    def get_block_number(self) -> int: ...

    def get_block(self, block_identifier: int | str) -> dict[str, Any] | None: ...

    def get_logs(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None,
        topics: list[list[str]] | None,
    ) -> list[dict[str, Any]]: ...

    def call(self, to: str, data: bytes, block: int | None) -> HexBytes: ...

    def call_raw(self, tx: dict[str, Any], block: int | None) -> HexBytes: ...

    def get_code(self, address: str, block: int | None) -> HexBytes: ...

    def get_balance(self, address: str, block: int | None) -> int: ...

    def get_storage_at(self, address: str, position: int, block: int | None) -> HexBytes: ...

    def get_transaction_count(self, address: str, block: int | None) -> int: ...

    def is_connected(self) -> bool: ...

    def close(self) -> None: ...


# ============================================================================
# Sync backend adapters
# ============================================================================


class _Web3Adapter:
    """Adapter wrapping a web3.py Web3 instance to satisfy _SyncProviderBackend."""

    def __init__(self, w3: Any) -> None:  # noqa: ANN401
        self._w3 = w3

    @property
    def chain_id(self) -> int:
        return self._w3.eth.chain_id

    @property
    def block_number(self) -> int:
        return self._w3.eth.block_number

    def get_block_number(self) -> int:
        return self._w3.eth.get_block_number()

    def get_block(self, block_identifier: int | str) -> dict[str, Any] | None:
        return self._w3.eth.get_block(block_identifier)

    def get_logs(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None,
        topics: list[list[str]] | None,
    ) -> list[dict[str, Any]]:
        filter_param: dict[str, Any] = {"fromBlock": from_block, "toBlock": to_block}
        if addresses:
            filter_param["address"] = addresses
        if topics:
            filter_param["topics"] = topics
        return self._w3.eth.get_logs(filter_param)

    def call(self, to: str, data: bytes, block: int | None) -> HexBytes:
        tx: dict[str, Any] = {"to": to, "data": data}
        if block is not None:
            return self._w3.eth.call(tx, block)
        return self._w3.eth.call(tx)

    def call_raw(self, tx: dict[str, Any], block: int | None) -> HexBytes:
        if block is not None:
            return self._w3.eth.call(tx, block)
        return self._w3.eth.call(tx)

    def get_code(self, address: str, block: int | None) -> HexBytes:
        if block is not None:
            return self._w3.eth.get_code(address, block)
        return self._w3.eth.get_code(address)

    def get_balance(self, address: str, block: int | None) -> int:
        if block is not None:
            return self._w3.eth.get_balance(address, block)
        return self._w3.eth.get_balance(address)

    def get_storage_at(self, address: str, position: int, block: int | None) -> HexBytes:
        if block is not None:
            return self._w3.eth.get_storage_at(address, position, block)
        return self._w3.eth.get_storage_at(address, position)

    def get_transaction_count(self, address: str, block: int | None) -> int:
        if block is not None:
            return self._w3.eth.get_transaction_count(address, block)
        return self._w3.eth.get_transaction_count(address)

    def is_connected(self) -> bool:
        return self._w3.is_connected()

    def close(self) -> None:
        if hasattr(self._w3, "close"):
            self._w3.close()


class _AlloyAdapter:
    """Adapter wrapping an AlloyProvider instance to satisfy _SyncProviderBackend."""

    def __init__(self, alloy: Any) -> None:  # noqa: ANN401
        self._alloy = alloy

    @property
    def chain_id(self) -> int:
        return self._alloy.chain_id

    @property
    def block_number(self) -> int:
        return self._alloy.block_number

    def get_block_number(self) -> int:
        return self._alloy.get_block_number()

    def get_block(self, block_identifier: int | str) -> dict[str, Any] | None:
        # AlloyProvider only supports integer block numbers
        if isinstance(block_identifier, str):
            if block_identifier == "latest":
                block_identifier = self._alloy.get_block_number()
            elif block_identifier == "earliest":
                block_identifier = 0
            elif block_identifier == "pending":
                block_identifier = self._alloy.get_block_number() + 1
        return self._alloy.get_block(block_identifier)

    def get_logs(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None,
        topics: list[list[str]] | None,
    ) -> list[dict[str, Any]]:
        return self._alloy.get_logs(
            from_block=from_block,
            to_block=to_block,
            addresses=addresses,
            topics=topics,
        )

    def call(self, to: str, data: bytes, block: int | None) -> HexBytes:
        return self._alloy.call(to, data, block_number=block)

    def call_raw(self, tx: dict[str, Any], block: int | None) -> HexBytes:
        return self._alloy.call(tx["to"], tx["data"], block_number=block)

    def get_code(self, address: str, block: int | None) -> HexBytes:
        return self._alloy.get_code(address, block_number=block)

    def get_balance(self, address: str, block: int | None) -> int:
        return self._alloy.get_balance(address, block_number=block)

    def get_storage_at(self, address: str, position: int, block: int | None) -> HexBytes:
        return self._alloy.get_storage_at(address, position, block_number=block)

    def get_transaction_count(self, address: str, block: int | None) -> int:
        return self._alloy.get_transaction_count(address, block_number=block)

    def is_connected(self) -> bool:  # noqa: PLR6301
        return True

    def close(self) -> None:
        if hasattr(self._alloy, "close"):
            self._alloy.close()


class _OfflineAdapter:
    """Adapter wrapping an OfflineProvider instance to satisfy _SyncProviderBackend."""

    def __init__(self, offline: Any) -> None:  # noqa: ANN401
        self._offline = offline

    @property
    def chain_id(self) -> int:
        return self._offline.chain_id

    @property
    def block_number(self) -> int:
        return self._offline.block_number

    def get_block_number(self) -> int:
        return self._offline.get_block_number()

    def get_block(self, block_identifier: int | str) -> dict[str, Any] | None:
        return self._offline.get_block(block_identifier)

    def get_logs(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None,
        topics: list[list[str]] | None,
    ) -> list[dict[str, Any]]:
        return self._offline.get_logs(
            from_block=from_block,
            to_block=to_block,
            addresses=addresses,
            topics=topics,
        )

    def call(self, to: str, data: bytes, block: int | None) -> HexBytes:
        return self._offline.call(to, data, block_number=block)

    def call_raw(self, tx: dict[str, Any], block: int | None) -> HexBytes:
        return self._offline.call(tx["to"], tx["data"], block_number=block)

    def get_code(self, address: str, block: int | None) -> HexBytes:
        return self._offline.get_code(address, block_number=block)

    def get_balance(self, address: str, block: int | None) -> int:
        return self._offline.get_balance(address, block_number=block)

    def get_storage_at(self, address: str, position: int, block: int | None) -> HexBytes:
        return self._offline.get_storage_at(address, position, block_number=block)

    def get_transaction_count(self, address: str, block: int | None) -> int:
        return self._offline.get_transaction_count(address, block_number=block)

    def is_connected(self) -> bool:  # noqa: PLR6301
        return True

    def close(self) -> None:
        if hasattr(self._offline, "close"):
            self._offline.close()


# ============================================================================
# ProviderAdapter (sync)
# ============================================================================


class ProviderAdapter:  # noqa:PLR0904
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
        backend: _SyncProviderBackend,
        *,
        provider_type: Literal["web3", "alloy", "offline"],
        raw_provider: Any | None = None,  # noqa: ANN401
    ) -> None:
        """Initialize the adapter with a backend.

        Args:
            backend: A provider backend satisfying _SyncProviderBackend
            provider_type: The type label for the backend (used by repr and pickling)
            raw_provider: The original unwrapped provider (exposed by underlying / provider)
        """
        self._backend = backend
        self._provider_type = provider_type
        self._raw_provider = raw_provider

    # -------------------------------------------------------------------------
    # Pickle support
    # -------------------------------------------------------------------------

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

    def set_provider(self, provider: Any) -> None:  # noqa: ANN401
        """Set the underlying provider by re-wrapping it in the correct backend."""
        self._backend = _backend_for_type(self._provider_type, provider)
        self._raw_provider = provider

    # -------------------------------------------------------------------------
    # Factory methods
    # -------------------------------------------------------------------------

    @classmethod
    def from_web3(cls, w3: Any) -> Self:  # noqa: ANN401
        """Create an adapter wrapping a Web3 instance."""
        return cls(_Web3Adapter(w3), provider_type="web3", raw_provider=w3)

    @classmethod
    def from_alloy(cls, alloy: Any) -> Self:  # noqa: ANN401
        """Create an adapter wrapping an AlloyProvider instance."""
        return cls(_AlloyAdapter(alloy), provider_type="alloy", raw_provider=alloy)

    @classmethod
    def from_offline(cls, offline: Any) -> Self:  # noqa: ANN401
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
    def underlying(self) -> Any:  # noqa: ANN401
        """Get the underlying provider instance.

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
    def provider(self) -> Any:  # noqa: ANN401
        """Get the underlying provider, or None if not set (e.g., after unpickling)."""
        return self._raw_provider

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
    # Methods (delegated)
    # -------------------------------------------------------------------------

    def get_block_number(self) -> int:
        """Get the current block number."""
        return self._backend.get_block_number()

    def get_block(self, block_identifier: int | str) -> dict[str, Any] | None:
        """Get a block by number or identifier."""
        return self._backend.get_block(block_identifier)

    def get_logs(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> list[dict[str, Any]]:
        """Fetch event logs matching the filter."""
        return self._backend.get_logs(from_block, to_block, addresses, topics)

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        """Execute an eth_call."""
        return self._backend.call(to, data, block)

    def call_raw(self, tx: dict[str, Any], block: int | None = None) -> HexBytes:
        """Execute an eth_call with a raw transaction dict.

        The dict must contain ``to`` (address string) and ``data`` (bytes).
        Additional keys (e.g. ``from``, ``value``) are passed through to the
        backend.

        This is the low-level counterpart to :meth:`call`, which uses keyword
        arguments. Prefer :meth:`call` for new code; use :meth:`call_raw` when
        migrating existing ``w3.eth.call({\"to\": ..., \"data\": ...})`` call
        sites.
        """
        return self._backend.call_raw(tx, block)

    def batch_call(
        self, calls: list[dict[str, Any]], block: int | None = None
    ) -> list[HexBytes]:
        """Execute multiple eth_calls and return results in input order.

        Each entry in ``calls`` is a transaction dict with at least ``to`` and
        ``data`` keys, matching the :meth:`call_raw` signature.

        The default implementation sends requests sequentially. Backends that
        support batching (e.g. multicall3) can override this for better
        performance.
        """
        return [self._backend.call_raw(tx, block) for tx in calls]

    def get_block_timestamp(self, block: int | None = None) -> int:
        """Get the timestamp for a block.

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

    def get_code(self, address: str, block: int | None = None) -> HexBytes:
        """Get contract bytecode at an address."""
        return self._backend.get_code(address, block)

    def get_balance(self, address: str, block: int | None = None) -> int:
        """Get the balance of an address in wei."""
        return self._backend.get_balance(address, block)

    def get_storage_at(
        self,
        address: str,
        position: int,
        block: int | None = None,
    ) -> HexBytes:
        """Get storage at a given position."""
        return self._backend.get_storage_at(address, position, block)

    def get_transaction_count(
        self,
        address: str,
        block: int | None = None,
    ) -> int:
        """Get the transaction count (nonce) for an address."""
        return self._backend.get_transaction_count(address, block)

    def is_connected(self) -> bool:
        """Check if the provider is connected."""
        return self._backend.is_connected()

    def close(self) -> None:
        """Close the provider connection if supported."""
        self._backend.close()

    def make_request(self, method: str, params: list[Any]) -> Any:  # noqa: ANN401
        """Make a raw JSON-RPC request.

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
        if hasattr(self._raw_provider, "make_request"):
            return self._raw_provider.make_request(method, params)
        msg = f"Provider type '{self._provider_type}' does not support make_request"
        raise AttributeError(msg)

    def __repr__(self) -> str:
        return f"ProviderAdapter(type={self._provider_type})"


# ============================================================================
# Private async backend protocol
# ============================================================================


class _AsyncProviderBackend(Protocol):
    """Private protocol for async provider backends used by AsyncProviderAdapter."""

    async def get_block_number(self) -> int: ...

    async def get_chain_id(self) -> int: ...

    async def get_block(self, block_identifier: int | str) -> dict[str, Any] | None: ...

    async def get_logs(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None,
        topics: list[list[str]] | None,
    ) -> list[dict[str, Any]]: ...

    async def call(self, to: str, data: bytes, block: int | None) -> HexBytes: ...

    async def get_code(self, address: str, block: int | None) -> HexBytes: ...

    async def get_balance(self, address: str, block: int | None) -> int: ...

    async def get_storage_at(self, address: str, position: int, block: int | None) -> HexBytes: ...

    async def get_transaction_count(self, address: str, block: int | None) -> int: ...

    def is_connected(self) -> bool: ...

    def close(self) -> None: ...


# ============================================================================
# Async backend adapters
# ============================================================================


class _AsyncWeb3Adapter:
    """Adapter wrapping an AsyncWeb3 instance to satisfy _AsyncProviderBackend."""

    def __init__(self, w3: Any) -> None:  # noqa: ANN401
        self._w3 = w3

    async def get_block_number(self) -> int:
        return await self._w3.eth.get_block_number()

    async def get_chain_id(self) -> int:
        return await self._w3.eth.chain_id

    async def get_block(self, block_identifier: int | str) -> dict[str, Any] | None:
        return await self._w3.eth.get_block(block_identifier)

    async def get_logs(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None,
        topics: list[list[str]] | None,
    ) -> list[dict[str, Any]]:
        filter_param: dict[str, Any] = {"fromBlock": from_block, "toBlock": to_block}
        if addresses:
            filter_param["address"] = addresses
        if topics:
            filter_param["topics"] = topics
        return await self._w3.eth.get_logs(filter_param)

    async def call(self, to: str, data: bytes, block: int | None) -> HexBytes:
        tx: dict[str, Any] = {"to": to, "data": data}
        if block is not None:
            return await self._w3.eth.call(tx, block)
        return await self._w3.eth.call(tx)

    async def get_code(self, address: str, block: int | None) -> HexBytes:
        if block is not None:
            return await self._w3.eth.get_code(address, block)
        return await self._w3.eth.get_code(address)

    async def get_balance(self, address: str, block: int | None) -> int:
        if block is not None:
            return await self._w3.eth.get_balance(address, block)
        return await self._w3.eth.get_balance(address)

    async def get_storage_at(self, address: str, position: int, block: int | None) -> HexBytes:
        if block is not None:
            return await self._w3.eth.get_storage_at(address, position, block)
        return await self._w3.eth.get_storage_at(address, position)

    async def get_transaction_count(self, address: str, block: int | None) -> int:
        if block is not None:
            return await self._w3.eth.get_transaction_count(address, block)
        return await self._w3.eth.get_transaction_count(address)

    def is_connected(self) -> bool:  # noqa: PLR6301
        return True

    def close(self) -> None:
        if hasattr(self._w3, "close"):
            self._w3.close()


class _AsyncAlloyAdapter:
    """Adapter wrapping the Rust AsyncAlloyProvider directly."""

    def __init__(self, alloy: Any) -> None:  # noqa: ANN401
        self._alloy = alloy

    async def get_block_number(self) -> int:
        return await self._alloy.get_block_number()

    async def get_chain_id(self) -> int:
        return await self._alloy.get_chain_id()

    async def get_block(self, block_identifier: int | str) -> dict[str, Any] | None:
        if isinstance(block_identifier, str):
            if block_identifier == "latest":
                block_identifier = await self._alloy.get_block_number()
            elif block_identifier == "earliest":
                block_identifier = 0
            elif block_identifier == "pending":
                block_identifier = await self._alloy.get_block_number() + 1
        return await self._alloy.get_block(block_identifier)

    async def get_logs(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None,
        topics: list[list[str]] | None,
    ) -> list[dict[str, Any]]:
        return await self._alloy.get_logs(
            from_block=from_block,
            to_block=to_block,
            addresses=addresses,
            topics=topics,
        )

    async def call(self, to: str, data: bytes, block: int | None) -> HexBytes:
        return await self._alloy.call(to, data, block_number=block)

    async def get_code(self, address: str, block: int | None) -> HexBytes:
        return await self._alloy.get_code(address, block)

    async def get_balance(self, address: str, block: int | None) -> int:
        return await self._alloy.get_balance(address, block)

    async def get_storage_at(self, address: str, position: int, block: int | None) -> HexBytes:
        return await self._alloy.get_storage_at(address, position, block)

    async def get_transaction_count(self, address: str, block: int | None) -> int:
        return await self._alloy.get_transaction_count(address, block)

    def is_connected(self) -> bool:  # noqa: PLR6301
        return True

    def close(self) -> None:
        if hasattr(self._alloy, "close"):
            self._alloy.close()


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
        backend: _AsyncProviderBackend,
        *,
        provider_type: Literal["web3", "alloy"],
        raw_provider: Any | None = None,  # noqa: ANN401
    ) -> None:
        self._backend = backend
        self._provider_type = provider_type
        self._raw_provider = raw_provider

    @classmethod
    def from_web3(cls, async_w3: Any) -> Self:  # noqa: ANN401
        """Create an adapter wrapping an AsyncWeb3 instance."""
        return cls(_AsyncWeb3Adapter(async_w3), provider_type="web3", raw_provider=async_w3)

    @classmethod
    def from_alloy(cls, async_alloy: Any) -> Self:  # noqa: ANN401
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
    def underlying(self) -> Any:  # noqa: ANN401
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

    async def get_block_number(self) -> int:
        """Get the current block number."""
        return await self._backend.get_block_number()

    async def get_chain_id(self) -> int:
        """Get the chain ID."""
        return await self._backend.get_chain_id()

    async def get_block(self, block_identifier: int | str) -> dict[str, Any] | None:
        """Get a block by number or identifier."""
        return await self._backend.get_block(block_identifier)

    async def get_logs(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> list[dict[str, Any]]:
        """Fetch event logs matching the filter."""
        return await self._backend.get_logs(from_block, to_block, addresses, topics)

    async def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        """Execute an eth_call."""
        return await self._backend.call(to, data, block)

    async def get_code(self, address: str, block: int | None = None) -> HexBytes:
        """Get contract bytecode at an address."""
        return await self._backend.get_code(address, block)

    async def get_balance(self, address: str, block: int | None = None) -> int:
        """Get the balance of an address in wei."""
        return await self._backend.get_balance(address, block)

    async def get_storage_at(
        self,
        address: str,
        position: int,
        block: int | None = None,
    ) -> HexBytes:
        """Get storage at a given position."""
        return await self._backend.get_storage_at(address, position, block)

    async def get_transaction_count(
        self,
        address: str,
        block: int | None = None,
    ) -> int:
        """Get the transaction count (nonce) for an address."""
        return await self._backend.get_transaction_count(address, block)

    def is_connected(self) -> bool:
        """Check if the provider is connected."""
        return self._backend.is_connected()

    def close(self) -> None:
        """Close the provider connection if supported."""
        self._backend.close()

    def __repr__(self) -> str:
        return f"AsyncProviderAdapter(type={self._provider_type})"


# ============================================================================
# Internal helper for round-trip pickling
# ============================================================================


def _backend_for_type(
    provider_type: Literal["web3", "alloy", "offline"],
    provider: Any,  # noqa: ANN401
) -> _SyncProviderBackend:
    """Create the correct backend adapter for a provider type label."""
    match provider_type:
        case "web3":
            return _Web3Adapter(provider)
        case "alloy":
            return _AlloyAdapter(provider)
        case "offline":
            return _OfflineAdapter(provider)
        case _:
            msg = f"Unknown provider type: {provider_type}"
            raise ValueError(msg)


# Keep public API surface unchanged
__all__ = [
    "AsyncProviderAdapter",
    "EthereumProvider",
    "ProviderAdapter",
]
