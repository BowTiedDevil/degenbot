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

from __future__ import annotations

from typing import Any

from degenbot._rs import AsyncAlloyProvider as _AsyncAlloyProvider


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
        max_connections: int = 10,
        timeout: float = 30.0,
        max_retries: int = 10,
    ) -> AsyncAlloyProvider:
        """Create a new async provider.

        Args:
            rpc_url: RPC endpoint URL
            max_connections: Max concurrent connections (not yet implemented)
            timeout: Request timeout (not yet implemented)
            max_retries: Max retry attempts (not yet implemented)

        Returns:
            A new AsyncAlloyProvider instance
        """
        provider = await _AsyncAlloyProvider.create(rpc_url, max_connections, timeout, max_retries)
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

        # The Rust function returns tuples, convert to dicts
        logs = await self._provider.get_logs(from_block, to_block, addresses, topics)
        return [
            {
                "address": log[0],
                "topics": log[1],
                "data": log[2],
                "blockNumber": log[3],
                "blockHash": log[4],
                "transactionHash": log[5],
                "logIndex": log[6],
            }
            for log in logs
        ]


__all__ = ["AsyncAlloyProvider"]
