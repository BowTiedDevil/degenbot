"""Async smart contract interface with automatic ABI encoding/decoding.

This module provides async variants of the Contract class for non-blocking
smart contract interactions.

Example:
    >>> import asyncio
    >>> from degenbot.contract.async_contract import AsyncContract
    >>>
    >>> async def main():
    ...     contract = AsyncContract(
    ...         "0xA0b86a33E6441e3D4e4b8b8b8b8b8b8b8b8b8b8",
    ...         "https://eth.example.com"
    ...     )
    ...     balance = await contract.call("balanceOf(address)", ["0x1234..."])
    ...     print(f"Balance: {balance[0]}")
    ...
    >>> asyncio.run(main())
"""

from collections.abc import Sequence

from degenbot.degenbot_rs import AsyncContract as _AsyncContract


class AsyncContract:
    """
    Async contract interface with automatic ABI encoding/decoding.

    Provides async methods for non-blocking smart contract calls with
    automatic ABI encoding of arguments and decoding of return values.

    Args:
        address: Contract address
        provider_url: RPC provider URL

    Example:
        >>> import asyncio
        >>> from degenbot.contract.async_contract import AsyncContract
        >>>
        >>> async def main():
        ...     # Create contract for an ERC20 token
        ...     token = AsyncContract(
        ...         address="0xA0b86a33E6441e3D4e4b8b8b8b8b8b8b8b8b8b8",
        ...         provider_url="https://eth-mainnet.example.com",
        ...     )
        ...
        ...     # Call balanceOf function asynchronously
        ...     balance = await token.call(
        ...         "balanceOf(address)",
        ...         ["0x742d35Cc6634C0532925a3b8D4C9db96590d6B75"],
        ...     )
        ...     print(f"Balance: {balance[0]}")
        >>> asyncio.run(main())
    """

    def __init__(
        self,
        address: str,
        provider_url: str,
    ) -> None:
        """Create a new async contract instance.

        Args:
            address: Contract address
            provider_url: RPC provider URL
        """
        self._address = address
        self._contract = _AsyncContract(address, provider_url)

    @property
    def address(self) -> str:
        """Get the contract address."""
        return self._contract.address

    async def call(
        self,
        function_signature: str,
        args: Sequence[str] | None = None,
        block_number: int | None = None,
    ) -> list[str]:
        """
        Execute a contract call asynchronously.

        Args:
            function_signature: Function signature like "balanceOf(address)" or
                "transfer(address,uint256) returns (bool)"
            args: Function arguments as strings (optional)
            block_number: Block to query (default: latest)

        Returns:
            List of decoded return values as strings

        Raises:
            ValueError: If the call fails or encoding/decoding fails

        Example:
            >>> # Simple call without arguments
            >>> name = await contract.call("name()")
            >>>
            >>> # Call with arguments
            >>> balance = await contract.call(
            ...     "balanceOf(address)",
            ...     ["0x742d35Cc6634C0532925a3b8D4C9db96590d6B75"],
            ... )
        """
        if args is None:
            args = []

        return await self._contract.call(function_signature, list(args), block_number)

    async def batch_call(
        self,
        calls: Sequence[tuple[str, Sequence[str] | None]],
        block_number: int | None = None,
    ) -> list[list[str]]:
        """
        Execute multiple contract calls asynchronously.

        Args:
            calls: List of (function_signature, args) tuples
            block_number: Block to query (default: latest)

        Returns:
            List of results, where each result is a list of decoded return values

        Raises:
            ValueError: If any call fails

        Example:
            >>> # Fetch multiple token properties concurrently
            >>> results = await contract.batch_call([
            ...     ("name()", []),
            ...     ("symbol()", []),
            ...     ("decimals()", []),
            ...     ("totalSupply()", []),
            ... ])
            >>> name, symbol, decimals, total_supply = [r[0] for r in results]
        """
        # Convert calls to the format expected by Rust
        rust_calls: list[tuple[str, list[str]]] = []
        for func_sig, args in calls:
            rust_calls.append((func_sig, list(args) if args else []))

        return await self._contract.batch_call(rust_calls, block_number)


__all__ = ["AsyncContract"]
