"""Async Ethereum provider using Alloy.

This module provides async variants of the provider for non-blocking
Ethereum RPC operations.

Example:
    >>> import asyncio
    >>> from degenbot.provider.async_provider import AsyncAlloyProvider
    >>>
    >>> async def main():
    ...     provider = await AsyncAlloyProvider.create("https://eth.example.com")
    ...     block_number = await provider.get_block_number()
    ...     print(f"Current block: {block_number}")
    ...
    >>> asyncio.run(main())
"""

from typing import Any, Self, cast

from hexbytes import HexBytes

try:
    from degenbot.degenbot_rs import AsyncAlloyProvider as _AsyncAlloyProvider
except ImportError:
    _AsyncAlloyProvider = None  # type: ignore[assignment,misc]


class AsyncAlloyProvider:
    """
    Async Ethereum RPC provider using Alloy.

    Provides async methods for non-blocking RPC calls.

    Use `create()` to instantiate:

    Example:
        >>> import asyncio
        >>> from degenbot.provider.async_provider import AsyncAlloyProvider
        >>>
        >>> async def main():
        ...     provider = await AsyncAlloyProvider.create("https://eth-mainnet.example.com")
        ...     block_number = await provider.get_block_number()
        ...     chain_id = await provider.get_chain_id()
        ...     print(f"Chain {chain_id} at block {block_number}")
        ...
        >>> asyncio.run(main())
    """

    def __init__(self, provider: _AsyncAlloyProvider, rpc_url: str) -> None:
        """Initialize with an existing provider instance.

        Use `create()` to instantiate new providers.

        Args:
            provider: The underlying Rust provider instance
            rpc_url: RPC endpoint URL
        """
        self._provider = provider
        self._rpc_url = rpc_url

    @classmethod
    async def create(
        cls,
        rpc_url: str,
        max_retries: int = 10,
    ) -> Self:
        """Create a new async provider.

        Args:
            rpc_url: RPC endpoint URL
            max_retries: Max retry attempts

        Returns:
            A new AsyncAlloyProvider instance
        """
        provider = await _AsyncAlloyProvider.create(rpc_url, max_retries)
        return cls(provider, rpc_url)

    @property
    def rpc_url(self) -> str:
        """Get the RPC URL."""
        return self._rpc_url

    async def get_block_number(self) -> int:
        """Get current block number asynchronously.

        Returns:
            Current block number

        Raises:
            ValueError: If the RPC call fails
        """
        return await self._provider.get_block_number()

    async def get_chain_id(self) -> int:
        """Get chain ID asynchronously.

        Returns:
            Chain ID

        Raises:
            ValueError: If the RPC call fails
        """
        return await self._provider.get_chain_id()

    async def get_gas_price(self) -> int:
        """Get current gas price in wei asynchronously."""
        return await self._provider.get_gas_price()

    async def call(
        self,
        to: str,
        data: bytes,
        block_number: int | None = None,
    ) -> HexBytes:
        """Execute an eth_call to a contract asynchronously.

        Args:
            to: Contract address to call
            data: Calldata bytes (function selector + encoded arguments)
            block_number: Block number to execute call at (default: latest)

        Returns:
            Raw return data from the contract call as HexBytes
        """
        return cast("HexBytes", await self._provider.call(to, data, block_number))

    async def get_code(
        self,
        address: str,
        block_number: int | None = None,
    ) -> HexBytes:
        """Get contract code at an address asynchronously.

        Args:
            address: Contract address
            block_number: Block number to get code at (default: latest)

        Returns:
            Contract bytecode as HexBytes
        """
        return cast(
            "HexBytes",
            await self._provider.get_code(address, block_number),
        )

    async def get_logs(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> list[dict[str, Any]]:
        """Fetch logs asynchronously.

        Args:
            from_block: Starting block number
            to_block: Ending block number
            addresses: Contract addresses to filter (optional)
            topics: Event topic signatures (optional)

        Returns:
            List of log dictionaries

        Raises:
            ValueError: If the RPC call fails or filter is invalid

        Example:
            >>> logs = await provider.get_logs(
            ...     from_block=18_000_000,
            ...     to_block=18_010_000,
            ...     addresses=["0x..."],
            ... )
        """
        if addresses is None:
            addresses = []
        if topics is None:
            topics = []

        # The Rust function now returns dicts with HexBytes for hash fields
        return await self._provider.get_logs(from_block, to_block, addresses, topics)

    async def get_block(self, block_number: int) -> dict[str, Any] | None:
        """Get a block by number asynchronously.

        Returns the full block data including header and transactions.
        All field names use snake_case for Python consistency.

        Args:
            block_number: Block number to fetch

        Returns:
            Block dictionary with all fields, or None if block not found

        Raises:
            ValueError: If the RPC call fails
        """
        return await self._provider.get_block(block_number)

    async def get_transaction(self, tx_hash: str) -> dict[str, Any] | None:
        """Get a transaction by hash asynchronously.

        Args:
            tx_hash: Transaction hash as hex string

        Returns:
            Transaction data as dictionary, or None if not found.
        """
        return await self._provider.get_transaction(tx_hash)

    async def get_transaction_receipt(self, tx_hash: str) -> dict[str, Any] | None:
        """Get a transaction receipt by hash asynchronously.

        Args:
            tx_hash: Transaction hash as hex string

        Returns:
            Receipt data as dictionary, or None if not found.
        """
        return await self._provider.get_transaction_receipt(tx_hash)


__all__ = ["AsyncAlloyProvider"]
