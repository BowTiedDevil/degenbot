"""
High-performance Ethereum RPC provider using Alloy.

This module provides a Rust-based provider for fast log fetching and RPC calls.
It replaces web3.py's provider functionality with optimized Rust implementations.

Example:
    >>> from degenbot.provider import AlloyProvider, LogFilter
    >>> provider = AlloyProvider("https://eth-mainnet.example.com")
    >>>
    >>> # Web3-compatible eth namespace
    >>> chain_id = provider.eth.chain_id
    >>> block_number = provider.eth.block_number
    >>>
    >>> # Log fetching with LogFilter
    >>> logs = provider.get_logs(
    ...     LogFilter(
    ...         from_block=18_000_000,
    ...         to_block=18_010_000,
    ...         addresses=["0x..."],
    ...     )
    ... )
    >>>
    >>> # Or using keyword arguments
    >>> logs = provider.get_logs(
    ...     from_block=18_000_000,
    ...     to_block=18_010_000,
    ...     addresses=["0x..."],
    ... )
    >>>
    >>> # Or using web3.py FilterParams style dict
    >>> logs = provider.eth.get_logs({
    ...     "fromBlock": 18_000_000,
    ...     "toBlock": 18_010_000,
    ...     "address": "0x...",
    ... })
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, Self, cast

from degenbot._rs import AlloyProvider as _AlloyProvider

if TYPE_CHECKING:
    from hexbytes import HexBytes

    from degenbot.types.aliases import BlockNumber


class EthNamespace:
    """
    Web3-compatible `eth` namespace adapter for AlloyProvider.

    Provides drop-in replacement for common web3.py `w3.eth` methods.
    """

    def __init__(self, provider: AlloyProvider) -> None:
        self._provider = provider

    @property
    def chain_id(self) -> int:
        """Get the chain ID (equivalent to w3.eth.chain_id)."""
        return self._provider.get_chain_id()

    @property
    def block_number(self) -> int:
        """Get the current block number (equivalent to w3.eth.block_number)."""
        return self._provider.get_block_number()

    def get_balance(self, address: str, block_identifier: int | str = "latest") -> int:
        """
        Get the balance of an address.

        Args:
            address: Ethereum address to check
            block_identifier: Block number or "latest" (default: "latest")

        Returns:
            Balance in wei as integer
        """
        msg = "get_balance not yet implemented in AlloyProvider"
        raise NotImplementedError(msg)

    def get_code(self, address: str, block_identifier: int | str = "latest") -> HexBytes:
        """
        Get the code at an address.

        Args:
            address: Contract address
            block_identifier: Block number or "latest" (default: "latest")

        Returns:
            Contract bytecode as HexBytes
        """
        # Determine block number
        block_num: int | None = None
        if isinstance(block_identifier, str):
            if block_identifier == "latest":
                block_num = None
            else:
                msg = f"Block identifier '{block_identifier}' not supported"
                raise NotImplementedError(msg)
        else:
            block_num = block_identifier

        return self._provider.get_code(address, block_num)

    def get_transaction_count(self, address: str, block_identifier: int | str = "latest") -> int:
        """
        Get the transaction count (nonce) for an address.

        Args:
            address: Ethereum address
            block_identifier: Block number or "latest" (default: "latest")

        Returns:
            Transaction count as integer
        """
        msg = "get_transaction_count not yet implemented in AlloyProvider"
        raise NotImplementedError(msg)

    def get_logs(
        self,
        filter_param: LogFilter | dict[str, Any],
    ) -> list[dict[str, Any]]:
        """
        Fetch event logs with automatic retry and dynamic block sizing.

        Provides web3.py-compatible log fetching. Accepts either a LogFilter
        object or a FilterParams-style dictionary.

        Args:
            filter_param: LogFilter criteria or web3 FilterParams dict

        Returns:
            List of log dictionaries with web3.py-compatible format:
            - address: Contract address
            - topics: List of topic hashes
            - data: Raw log data bytes
            - blockNumber: Block number
            - blockHash: Block hash
            - transactionHash: Transaction hash
            - logIndex: Log index within block

        Example:
            >>> # Using FilterParams dict (web3.py style)
            >>> logs = provider.eth.get_logs({
            ...     "fromBlock": 18_000_000,
            ...     "toBlock": 18_010_000,
            ...     "address": "0xContractAddress...",
            ...     "topics": ["0xEventSignature..."],
            ... })
        """
        # Convert FilterParams-style dict to keyword arguments
        if isinstance(filter_param, dict):
            from_block = filter_param.get("fromBlock", 0)
            to_block = filter_param.get("toBlock", from_block)
            addresses = filter_param.get("address", [])
            if addresses and not isinstance(addresses, list):
                addresses = [addresses]

            # Convert topics to string format for Rust provider
            # web3.py FilterParams uses HexBytes, but Rust expects hex strings
            raw_topics = filter_param.get("topics", [])
            topics: list[list[str]] | None = None
            if raw_topics:
                topics = []
                for topic in raw_topics:
                    if topic is None:
                        # Wildcard - skip this position
                        continue
                    if isinstance(topic, list):
                        # OR condition - list of topics
                        topic_group = [t.hex() if hasattr(t, "hex") else str(t) for t in topic]
                        topics.append(topic_group)
                    else:
                        # Single topic
                        topic_str = topic.hex() if hasattr(topic, "hex") else str(topic)
                        topics.append([topic_str])

            return self._provider.get_logs(
                from_block=from_block,
                to_block=to_block,
                addresses=addresses or None,
                topics=topics,
            )

        # LogFilter object - pass it through
        return self._provider.get_logs(filter_param)

    def call(
        self,
        transaction: dict[str, Any],
        block_identifier: int | str | None = None,
    ) -> HexBytes:
        """
        Execute an eth_call (web3.py compatible).

        Args:
            transaction: Transaction dict with 'to' and 'data' fields
            block_identifier: Block number or "latest" (default: latest)

        Returns:
            Raw return data from the contract call as HexBytes

        Example:
            >>> result = provider.eth.call({
            ...     "to": "0xTokenAddress",
            ...     "data": "0x70a08231...",  # balanceOf(address)
            ... })
        """
        to = transaction.get("to")
        data = transaction.get("data", b"")

        if not to:
            msg = "Transaction must specify 'to' address"
            raise ValueError(msg)

        # Convert data to bytes if it's a hex string
        if isinstance(data, str):
            data = bytes.fromhex(data[2:]) if data.startswith("0x") else bytes.fromhex(data)

        # Determine block number
        block_num: int | None = None
        if block_identifier is not None:
            if isinstance(block_identifier, str):
                if block_identifier == "latest":
                    block_num = None
                else:
                    msg = f"Block identifier '{block_identifier}' not supported"
                    raise NotImplementedError(msg)
            else:
                block_num = block_identifier

        return self._provider.call(to, data, block_num)

    def get_block(
        self,
        block_identifier: int | str = "latest",
        *,
        full_transactions: bool = False,  # noqa: ARG002
    ) -> dict[str, Any] | None:
        """
        Get a block by number or hash.

        Args:
            block_identifier: Block number, "latest", "earliest", "pending", or block hash
            full_transactions: If True, return full transaction objects (default: False)

        Returns:
            Block data as dictionary, or None if block not found
        """
        # Handle special tags - convert to current block number
        if isinstance(block_identifier, str):
            if block_identifier == "latest":
                block_num = self._provider.get_block_number()
            elif block_identifier == "earliest":
                block_num = 0
            elif block_identifier == "pending":
                block_num = self._provider.get_block_number() + 1
            elif block_identifier.startswith("0x"):
                # Block hash - not yet implemented
                msg = "get_block by hash not yet implemented in AlloyProvider"
                raise NotImplementedError(msg)
            else:
                msg = f"Invalid block identifier: {block_identifier}"
                raise ValueError(msg)
        else:
            block_num = block_identifier

        return self._provider.get_block(block_num)


@dataclass
class LogFilter:
    """
    Filter criteria for log fetching.

    Args:
        from_block: Starting block number (inclusive)
        to_block: Ending block number (inclusive)
        addresses: Contract addresses to filter (optional)
        topics: Event topic signatures, nested by position (optional)

    Example:
        >>> filter = LogFilter(
        ...     from_block=18_000_000,
        ...     to_block=18_010_000,
        ...     addresses=["0xContractAddress..."],
        ...     topics=[["0xTransfer..."]],  # Match first topic
        ... )
    """

    from_block: BlockNumber
    to_block: BlockNumber
    addresses: list[str] = field(default_factory=list)
    topics: list[list[str]] = field(default_factory=list)

    def __post_init__(self) -> None:
        if self.to_block < self.from_block:
            msg = "to_block must be >= from_block"
            raise ValueError(msg)


class AlloyProvider:
    """
    High-performance Ethereum RPC provider using Alloy.

    Replaces web3.py provider for log fetching and basic RPC calls.
    Uses Rust-based HTTP client with connection pooling for optimal performance.

    Args:
        rpc_url: HTTP/HTTPS endpoint URL
        max_retries: Maximum retry attempts (default: 10)
        max_blocks_per_request: Maximum blocks per log request (default: 5000)

    Example:
        >>> provider = AlloyProvider("https://eth-mainnet.example.com")
        >>>
        >>> # Using LogFilter object
        >>> logs = provider.get_logs(
        ...     LogFilter(from_block=18_000_000, to_block=18_010_000)
        ... )
        >>>
        >>> # Using keyword arguments
        >>> logs = provider.get_logs(
        ...     from_block=18_000_000,
        ...     to_block=18_010_000,
        ...     addresses=["0x..."],
        ... )
        >>>
        >>> # Access web3-compatible eth namespace
        >>> chain_id = provider.eth.chain_id
        >>> block_number = provider.eth.block_number
    """

    def __init__(
        self,
        rpc_url: str,
        max_retries: int = 10,
        max_blocks_per_request: int = 5000,
    ) -> None:
        self._rpc_url = rpc_url
        self._max_retries = max_retries
        self._max_blocks_per_request = max_blocks_per_request

        # Initialize Rust provider
        self._provider = _AlloyProvider(
            rpc_url=rpc_url,
            max_retries=max_retries,
            max_blocks_per_request=max_blocks_per_request,
        )

        # Web3-compatible eth namespace
        self._eth = EthNamespace(self)

    def get_logs(
        self,
        filter_param: LogFilter | None = None,
        *,
        from_block: BlockNumber | None = None,
        to_block: BlockNumber | None = None,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> list[dict[str, Any]]:
        """
        Fetch event logs with automatic retry and dynamic block sizing.

        Flexible API that accepts either a LogFilter object or individual
        filter parameters as keyword arguments. Returns logs in web3.py
        compatible format.

        Args:
            filter_param: LogFilter object with filter criteria (optional)
            from_block: Starting block number (required if filter_param not provided)
            to_block: Ending block number (required if filter_param not provided)
            addresses: Contract addresses to filter (optional)
            topics: Event topic signatures (optional)

        Returns:
            List of log dictionaries with web3.py-compatible format:
            - address: Contract address
            - topics: List of topic hashes
            - data: Raw log data bytes
            - blockNumber: Block number
            - blockHash: Block hash
            - transactionHash: Transaction hash
            - logIndex: Log index within block

        Raises:
            ValueError: If neither filter_param nor from_block/to_block are provided,
                       or if from_block > to_block

        Example:
            >>> # Using LogFilter
            >>> logs = provider.get_logs(LogFilter(from_block=18_000_000, to_block=18_010_000))
            >>>
            >>> # Using keyword arguments
            >>> logs = provider.get_logs(
            ...     from_block=18_000_000,
            ...     to_block=18_010_000,
            ...     addresses=["0xContract..."],
            ...     topics=[["0xEventSignature..."]],
            ... )
        """
        # Determine filter parameters
        if filter_param is not None:
            # Use LogFilter object
            from_block_val = filter_param.from_block
            to_block_val = filter_param.to_block
            addresses_val = filter_param.addresses or None
            topics_val = filter_param.topics or None
        else:
            # Use keyword arguments
            if from_block is None or to_block is None:
                msg = "Either filter_param or from_block/to_block must be provided"
                raise ValueError(msg)
            from_block_val = from_block
            to_block_val = to_block
            addresses_val = addresses
            topics_val = topics

        # Validate block range
        if from_block_val > to_block_val:
            msg = f"from_block ({from_block_val}) must be <= to_block ({to_block_val})"
            raise ValueError(msg)

        # Call Rust provider's get_logs with keyword arguments
        # The Rust provider now returns list[dict[str, Any]] with HexBytes for hex fields
        return self._provider.get_logs(
            from_block=from_block_val,
            to_block=to_block_val,
            addresses=addresses_val,
            topics=topics_val,
        )

    def get_block_number(self) -> int:
        """Get current block number."""
        return cast("int", self._provider.get_block_number())

    def get_chain_id(self) -> int:
        """Get chain ID."""
        return cast("int", self._provider.get_chain_id())

    def get_block(self, block_number: int) -> dict[str, Any] | None:
        """Get a block by number.

        Returns:
            Block data as dictionary with HexBytes for hash fields, or None if not found.
        """
        return cast("dict[str, Any] | None", self._provider.get_block(block_number))

    def get_code(self, address: str, block_number: int | None = None) -> HexBytes:
        """Get contract code at an address.

        Args:
            address: Contract address
            block_number: Block number to get code at (default: latest)

        Returns:
            Contract bytecode as HexBytes
        """
        return cast("HexBytes", self._provider.get_code(address, block_number))

    def call(
        self,
        to: str,
        data: bytes,
        block_number: int | None = None,
    ) -> HexBytes:
        """
        Execute an eth_call to a contract.

        Args:
            to: Contract address to call
            data: Calldata bytes (function selector + encoded arguments)
            block_number: Block number to execute call at (default: latest)

        Returns:
            Raw return data from the contract call as HexBytes

        Example:
            >>> # Call ERC20 balanceOf
            >>> selector = bytes.fromhex("70a08231")  # balanceOf(address)
            >>> address = bytes.fromhex("000000000000000000000000" + "1234...")
            >>> calldata = selector + address
            >>> result = provider.call("0xTokenAddress", calldata)
            >>> balance = int.from_bytes(result, "big")
        """
        return cast("HexBytes", self._provider.call(to, data, block_number))

    def close(self) -> None:
        """Close connection pool and release resources."""
        self._provider.close()

    @property
    def rpc_url(self) -> str:
        """Get the RPC URL."""
        return self._rpc_url

    @property
    def eth(self) -> EthNamespace:
        """
        Web3-compatible `eth` namespace.

        Provides access to common Ethereum RPC methods in a web3.py style:
            >>> provider.eth.chain_id
            1
            >>> provider.eth.block_number
            18000000

        This allows AlloyProvider to be used as a drop-in replacement
        for web3.py's `w3` object for basic operations.
        """
        return self._eth

    def __enter__(self) -> Self:
        """Context manager entry."""
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc_val: BaseException | None,
        exc_tb: object,
    ) -> None:
        """Context manager exit."""
        self.close()


__all__ = [
    "AlloyProvider",
    "LogFilter",
]
