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

from typing import Any, Literal, Protocol, Self, runtime_checkable

from hexbytes import HexBytes


@runtime_checkable
class EthereumProvider(Protocol):
    """
    Protocol for Ethereum RPC providers.

    Defines the interface that both Web3 and AlloyProvider must satisfy
    for use in degenbot code.
    """

    @property
    def chain_id(self) -> int:
        """Get the chain ID."""
        ...

    @property
    def block_number(self) -> int:
        """Get the current block number."""
        ...

    def get_block_number(self) -> int:
        """Get the current block number."""
        ...

    def get_block(
        self,
        block_identifier: int | str,
    ) -> dict[str, Any] | None:
        """Get a block by number or identifier."""
        ...

    def get_logs(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> list[dict[str, Any]]:
        """Fetch event logs matching the filter."""
        ...

    def call(
        self,
        to: str,
        data: bytes,
        block: int | None = None,
    ) -> HexBytes:
        """Execute an eth_call."""
        ...

    def get_code(
        self,
        address: str,
        block: int | None = None,
    ) -> HexBytes:
        """Get contract bytecode at an address."""
        ...

    def get_balance(
        self,
        address: str,
        block: int | None = None,
    ) -> int:
        """Get the balance of an address in wei."""
        ...

    def get_storage_at(
        self,
        address: str,
        position: int,
        block: int | None = None,
    ) -> HexBytes:
        """Get storage at a given position."""
        ...

    def get_transaction_count(
        self,
        address: str,
        block: int | None = None,
    ) -> int:
        """Get the transaction count (nonce) for an address."""
        ...

    def is_connected(self) -> bool:
        """Check if the provider is connected."""
        ...


class ProviderAdapter:
    """
    Adapter that wraps either Web3 or AlloyProvider.

    Provides a uniform interface for Ethereum RPC operations,
    allowing existing code to work with either backend.

    Use factory methods to create:
        - ProviderAdapter.from_web3(w3)
        - ProviderAdapter.from_alloy(alloy_provider)
    """

    def __init__(
        self,
        provider: Any,  # noqa: ANN401
        *,
        provider_type: Literal["web3", "alloy"],
    ) -> None:
        """Initialize the adapter.

        Args:
            provider: The underlying provider (Web3 or AlloyProvider)
            provider_type: "web3" or "alloy" to indicate the backend type
        """
        self._provider = provider
        self._provider_type = provider_type

    @classmethod
    def from_web3(cls, w3: Any) -> Self:  # noqa: ANN401
        """Create an adapter wrapping a Web3 instance.

        Args:
            w3: A web3.py Web3 instance

        Returns:
            A ProviderAdapter wrapping the Web3 instance
        """
        return cls(provider=w3, provider_type="web3")

    @classmethod
    def from_alloy(cls, alloy: Any) -> Self:  # noqa: ANN401
        """Create an adapter wrapping an AlloyProvider instance.

        Args:
            alloy: An AlloyProvider instance

        Returns:
            A ProviderAdapter wrapping the AlloyProvider instance
        """
        return cls(provider=alloy, provider_type="alloy")

    @property
    def provider_type(self) -> Literal["web3", "alloy"]:
        """Get the type of the underlying provider."""
        return self._provider_type

    @property
    def underlying(self) -> Any:  # noqa: ANN401
        """Get the underlying provider instance."""
        return self._provider

    # =========================================================================
    # Properties
    # =========================================================================

    @property
    def chain_id(self) -> int:
        """Get the chain ID."""
        if self._provider_type == "web3":
            return self._provider.eth.chain_id
        return self._provider.chain_id

    @property
    def block_number(self) -> int:
        """Get the current block number."""
        if self._provider_type == "web3":
            return self._provider.eth.block_number
        return self._provider.block_number

    # =========================================================================
    # Methods
    # =========================================================================

    def get_block_number(self) -> int:
        """Get the current block number."""
        if self._provider_type == "web3":
            return self._provider.eth.get_block_number()
        return self._provider.get_block_number()

    def get_block(
        self,
        block_identifier: int | str,
    ) -> dict[str, Any] | None:
        """Get a block by number or identifier.

        Args:
            block_identifier: Block number or "latest", "earliest", "pending"

        Returns:
            Block data dict, or None if not found
        """
        if self._provider_type == "web3":
            return self._provider.eth.get_block(block_identifier)
        # AlloyProvider only supports integer block numbers
        if isinstance(block_identifier, str):
            if block_identifier == "latest":
                block_identifier = self._provider.get_block_number()
            elif block_identifier == "earliest":
                block_identifier = 0
            elif block_identifier == "pending":
                block_identifier = self._provider.get_block_number() + 1
        return self._provider.get_block(block_identifier)

    def get_logs(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> list[dict[str, Any]]:
        """Fetch event logs matching the filter.

        Args:
            from_block: Starting block number (inclusive)
            to_block: Ending block number (inclusive)
            addresses: Contract addresses to filter (optional)
            topics: Event topic signatures (optional)

        Returns:
            List of log dictionaries
        """
        if self._provider_type == "web3":
            filter_param: dict[str, Any] = {
                "fromBlock": from_block,
                "toBlock": to_block,
            }
            if addresses:
                filter_param["address"] = addresses
            if topics:
                filter_param["topics"] = topics
            return self._provider.eth.get_logs(filter_param)

        return self._provider.get_logs(
            from_block=from_block,
            to_block=to_block,
            addresses=addresses,
            topics=topics,
        )

    def call(
        self,
        to: str,
        data: bytes,
        block: int | None = None,
    ) -> HexBytes:
        """Execute an eth_call.

        Args:
            to: Contract address to call
            data: Calldata bytes
            block: Block number (default: latest)

        Returns:
            Raw return data from the contract call
        """
        if self._provider_type == "web3":
            tx: dict[str, Any] = {"to": to, "data": data}
            if block is not None:
                return self._provider.eth.call(tx, block)
            return self._provider.eth.call(tx)
        return self._provider.call(to, data, block)

    def get_code(
        self,
        address: str,
        block: int | None = None,
    ) -> HexBytes:
        """Get contract bytecode at an address.

        Args:
            address: Contract address
            block: Block number (default: latest)

        Returns:
            Contract bytecode
        """
        if self._provider_type == "web3":
            if block is not None:
                return self._provider.eth.get_code(address, block)
            return self._provider.eth.get_code(address)
        return self._provider.get_code(address, block)

    def get_balance(
        self,
        address: str,
        block: int | None = None,
    ) -> int:
        """Get the balance of an address in wei.

        Args:
            address: Ethereum address
            block: Block number (default: latest)

        Returns:
            Balance in wei
        """
        if self._provider_type == "web3":
            if block is not None:
                return self._provider.eth.get_balance(address, block)
            return self._provider.eth.get_balance(address)
        # AlloyProvider doesn't have get_balance yet
        msg = "get_balance not implemented for AlloyProvider"
        raise NotImplementedError(msg)

    def get_storage_at(
        self,
        address: str,
        position: int,
        block: int | None = None,
    ) -> HexBytes:
        """Get storage at a given position.

        Args:
            address: Contract address
            position: Storage slot position
            block: Block number (default: latest)

        Returns:
            Storage value at the position
        """
        if self._provider_type == "web3":
            if block is not None:
                return self._provider.eth.get_storage_at(address, position, block)
            return self._provider.eth.get_storage_at(address, position)
        return self._provider.get_storage_at(address, position, block)

    def get_transaction_count(
        self,
        address: str,
        block: int | None = None,
    ) -> int:
        """Get the transaction count (nonce) for an address.

        Args:
            address: Ethereum address
            block: Block number (default: latest)

        Returns:
            Transaction count
        """
        if self._provider_type == "web3":
            if block is not None:
                return self._provider.eth.get_transaction_count(address, block)
            return self._provider.eth.get_transaction_count(address)
        # AlloyProvider doesn't have get_transaction_count yet
        msg = "get_transaction_count not implemented for AlloyProvider"
        raise NotImplementedError(msg)

    def is_connected(self) -> bool:
        """Check if the provider is connected."""
        if self._provider_type == "web3":
            return self._provider.is_connected()
        # AlloyProvider doesn't have is_connected - assume connected if created
        return True

    def close(self) -> None:
        """Close the provider connection (AlloyProvider only)."""
        if self._provider_type == "alloy" and hasattr(self._provider, "close"):
            self._provider.close()

    def __repr__(self) -> str:
        return f"ProviderAdapter(type={self._provider_type})"


__all__ = [
    "EthereumProvider",
    "ProviderAdapter",
]
