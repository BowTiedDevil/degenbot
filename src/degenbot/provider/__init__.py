"""High-performance Ethereum RPC provider using Alloy.

This module provides a Rust-based provider for fast log fetching and RPC calls.
It replaces web3.py's provider functionality with optimized Rust implementations.

Example:
    >>> from degenbot.provider import AlloyProvider, LogFilter
    >>> provider = AlloyProvider("https://eth-mainnet.example.com")
    >>>
    >>> # Direct property access
    >>> chain_id = provider.chain_id
    >>> block_number = provider.block_number
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

"""

from dataclasses import dataclass, field
from typing import Any, Self

from hexbytes import HexBytes

from degenbot.degenbot_rs import AlloyProvider as RustAlloyProvider
from degenbot.degenbot_rs import AsyncAlloyProvider
from degenbot.provider.async_adapter import AsyncProviderAdapter
from degenbot.provider.factory import (
    get_async_provider_from_config,
    get_provider_from_config,
)
from degenbot.provider.offline_provider import (
    BlockNotRecordedError,
    OfflineDataMissing,
    OfflineProvider,
)
from degenbot.provider.protocols import AsyncProviderBackend, ProviderBackend
from degenbot.provider.subscription import LogSubscriptionFilter, Subscription
from degenbot.provider.sync_adapter import ProviderAdapter
from degenbot.types.aliases import BlockNumber
from degenbot.types.rpc_types import (
    BlockData,
    BlockIdentifier,
    LogData,
    TransactionData,
    TransactionReceiptData,
    TxParams,
)


@dataclass(frozen=True, slots=True)
class LogFilter:
    """Filter criteria for log fetching.

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
        """Post-initialization hook.

        Raises:
            ValueError: If to_block is less than from_block.

        """
        if self.to_block < self.from_block:
            msg = "to_block must be >= from_block"
            raise ValueError(msg)


class AlloyProvider:
    """High-performance Ethereum RPC provider using Alloy.

    Replaces web3.py provider for log fetching and basic RPC calls.
    Uses Rust-based HTTP client with connection pooling for optimal performance.

    Args:
        rpc_url: HTTP/HTTPS endpoint URL
        max_retries: Maximum retry attempts (default: 10)
        max_blocks_per_request: Maximum blocks per log request (default: 5000)

    Example:
        >>> provider = AlloyProvider("https://eth-mainnet.example.com")
        >>>
        >>> # Properties
        >>> chain_id = provider.chain_id
        >>> block_number = provider.block_number
        >>>
        >>> # Methods
        >>> block = provider.get_block(18_000_000)
        >>> logs = provider.get_logs(from_block=18_000_000, to_block=18_010_000)
        >>> code = provider.get_code("0x...")
        >>> result = provider.call("0x...", calldata)

    """

    def __init__(
        self,
        rpc_url: str,
        max_retries: int = 10,
        max_blocks_per_request: int = 5000,
    ) -> None:
        """Initialize the instance."""
        self._rpc_url = rpc_url
        self._max_retries = max_retries
        self._max_blocks_per_request = max_blocks_per_request

        # Initialize Rust provider
        self._provider = RustAlloyProvider(
            rpc_url=rpc_url,
            max_retries=max_retries,
            max_blocks_per_request=max_blocks_per_request,
        )

    # =========================================================================
    # Properties
    # =========================================================================

    @property
    def rpc_url(self) -> str:
        """The RPC URL."""
        return self._rpc_url

    @property
    def chain_id(self) -> int:
        """The chain ID."""
        return self._provider.get_chain_id()

    @property
    def block_number(self) -> int:
        """The current block number."""
        return self._provider.get_block_number()

    # =========================================================================
    # Methods
    # =========================================================================

    def get_block_number(self) -> int:
        """Get current block number.

        Returns:
            The current block number.

        """
        return self._provider.get_block_number()

    def get_chain_id(self) -> int:
        """Get chain ID.

        Returns:
            The chain ID.

        """
        return self._provider.get_chain_id()

    def get_gas_price(self) -> int:
        """Get current gas price in wei.

        Returns:
            The current gas price in wei.

        """
        return self._provider.get_gas_price()

    def get_block(self, block_identifier: int | str) -> BlockData | None:
        """Get a block by number or tag.

        Args:
            block_identifier: Block number, or one of 'latest', 'earliest', 'pending'.

        Returns:
            Block data as dictionary with HexBytes for hash fields, or None if not found.

        Raises:
            ValueError: If ``block_identifier`` is an unsupported string.

        """
        if isinstance(block_identifier, str):
            if block_identifier == "latest":
                block_identifier = self._provider.get_block_number()
            elif block_identifier == "earliest":
                block_identifier = 0
            elif block_identifier == "pending":
                block_identifier = self._provider.get_block_number() + 1
            else:
                msg = f"Unsupported block identifier: {block_identifier!r}"
                raise ValueError(msg)
        return self._provider.get_block(block_identifier)

    def get_code(self, address: str, block_number: int | None = None) -> HexBytes:
        """Get contract code at an address.

        Args:
            address: Contract address
            block_number: Block number to get code at (default: latest)

        Returns:
            Contract bytecode as HexBytes

        """
        return self._provider.get_code(address, block_number)

    def call(
        self,
        to: str,
        data: bytes,
        block_number: int | None = None,
    ) -> HexBytes:
        """Execute an eth_call to a contract.

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
        return self._provider.call(to, data, block_number)

    def get_logs(
        self,
        filter_param: LogFilter | None = None,
        *,
        from_block: BlockNumber | None = None,
        to_block: BlockNumber | None = None,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> list[LogData]:
        """Fetch event logs with automatic retry and dynamic block sizing.

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
        # The Rust provider returns list[LogData] with HexBytes for hex fields
        return self._provider.get_logs(
            from_block=from_block_val,
            to_block=to_block_val,
            addresses=addresses_val,
            topics=topics_val,
        )

    def get_transaction(self, tx_hash: str) -> TransactionData | None:
        """Get a transaction by hash.

        Args:
            tx_hash: Transaction hash as hex string

        Returns:
            Transaction data as dictionary with HexBytes for hash fields,
            or None if not found.

        """
        return self._provider.get_transaction(tx_hash)

    def get_transaction_receipt(self, tx_hash: str) -> TransactionReceiptData | None:
        """Get a transaction receipt by hash.

        Args:
            tx_hash: Transaction hash as hex string

        Returns:
            Receipt data as dictionary with HexBytes for hash fields,
            or None if not found.

        """
        return self._provider.get_transaction_receipt(tx_hash)

    def estimate_gas(
        self,
        to: str,
        data: bytes,
        from_: str | None = None,
        value: int | None = None,
        block_number: int | None = None,
    ) -> int:
        """Estimate gas for a transaction.

        Args:
            to: Target address
            data: Transaction data
            from_: Sender address (optional)
            value: Value in wei (optional)
            block_number: Block number to estimate at (default: latest)

        Returns:
            Estimated gas as int

        """
        return self._provider.estimate_gas(to, data, from_, value, block_number)

    def get_storage_at(
        self,
        address: str,
        position: int,
        block_number: int | None = None,
    ) -> HexBytes:
        """Get storage at a given position.

        Args:
            address: Contract address
            position: Storage slot position (supports large values like mapping slots)
            block_number: Block number to get storage at (default: latest)

        Returns:
            Storage value at the position as HexBytes (32 bytes)

        """
        return self._provider.get_storage_at(address, position, block_number)

    def close(self) -> None:
        """Close connection pool and release resources."""
        self._provider.close()

    def is_connected(self) -> bool:  # noqa: PLR6301
        """Check if the provider is connected.

        For AlloyProvider, we assume connection is valid if the provider was created.

        Returns:
            True if the provider is connected.

        """
        return True

    def get_balance(
        self,
        address: str,
        block_number: int | None = None,
    ) -> int:
        """Get the balance of an address in wei.

        Args:
            address: Account address
            block_number: Block number to get balance at (default: latest)

        Returns:
            Balance in wei as int

        """
        return self._provider.get_balance(address, block_number)

    def get_transaction_count(
        self,
        address: str,
        block_number: int | None = None,
    ) -> int:
        """Get the transaction count (nonce) for an address.

        Args:
            address: Account address
            block_number: Block number to get nonce at (default: latest)

        Returns:
            Transaction count as int

        """
        return self._provider.get_transaction_count(address, block_number)

    def make_request(
        self,
        method: str,
        params: list[Any],
    ) -> Any:  # noqa: ANN401
        """Make a raw JSON-RPC request.

        This allows calling arbitrary RPC methods that don't have typed wrappers,
        such as debug methods, trace methods, or node-specific APIs.

        Args:
            method: The RPC method name (e.g., "debug_traceTransaction")
            params: The parameters as a list

        Returns:
            The raw result (deserialized from JSON with HexBytes for hex values)

        Example:
            >>> # Call debug_traceTransaction
            >>> result = provider.make_request(
            ...     "debug_traceTransaction", ["0x...", {"tracer": "callTracer"}]
            ... )

        """
        return self._provider.make_request(method, params)

    def call_raw(self, tx: TxParams, block: BlockIdentifier | None = None) -> HexBytes:
        """Execute an eth_call with a raw transaction dict.

        This is the low-level counterpart to :meth:`call`. The dict must
        contain ``to`` (address string) and ``data`` (bytes). Additional keys
        (e.g. ``from``, ``value``) are accepted but ignored — alloy's
        ``eth_call`` uses the latest block by default.

        Args:
            tx: Transaction dict with at least ``to`` and ``data`` keys.
            block: Block number, or None for latest.

        Returns:
            The raw return data from the contract call.

        """
        return self._provider.call(tx["to"], tx["data"], block)

    def batch_call(self, calls: list[TxParams], block: int | None = None) -> list[HexBytes]:
        """Execute multiple eth_calls sequentially and return results in order.

        Args:
            calls: List of transaction dicts, each with ``to`` and ``data``.
            block: Block number, or None for latest.

        Returns:
            A list of raw return data from each call.

        """
        return [self.call_raw(tx, block) for tx in calls]

    def get_block_timestamp(self, block: int | None = None) -> int:
        """Get the timestamp for a block.

        Args:
            block: Block number, or None for latest.

        Returns:
            The block timestamp as an integer (Unix seconds).

        Raises:
            ValueError: If the block is not found.

        """
        block_data = self.get_block(block if block is not None else "latest")
        if block_data is None:
            msg = f"Block {block} not found"
            raise ValueError(msg)
        return block_data["timestamp"]

    # --- Introspection / adapter-compat shims ---

    def to_alloy_provider(self) -> "AlloyProvider":
        """Return an ``AlloyProvider`` over this provider's transport.

        This provider *is* already an ``AlloyProvider``, so return ``self``.
        Kept for adapter-compatibility with call sites that previously held a
        ``ProviderAdapter``.

        Returns:
            This provider instance (already an ``AlloyProvider``).

        """
        return self

    @property
    def provider_type(self) -> str:
        """The provider type (always 'alloy')."""
        return "alloy"

    @property
    def provider(self) -> "AlloyProvider":
        """The underlying provider (identity — returns ``self``)."""
        return self

    @staticmethod
    def as_web3() -> None:
        """Return ``None`` — this provider has no Web3 backend."""
        return

    def as_alloy(self) -> "AlloyProvider":
        """Return ``self`` as an ``AlloyProvider``.

        Returns:
            This provider instance.

        """
        return self

    @staticmethod
    def as_offline() -> None:
        """Return ``None`` — this provider is not an ``OfflineProvider``."""
        return

    def __repr__(self) -> str:
        """Return a string representation.

        Returns:
            A string representation of the provider.

        """
        return f"AlloyProvider(rpc_url={self._rpc_url!r})"

    def __enter__(self) -> Self:
        """Context manager entry.

        Returns:
            The provider instance.

        """
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
    "AsyncAlloyProvider",
    "AsyncProviderAdapter",
    "AsyncProviderBackend",
    "BlockNotRecordedError",
    "LogFilter",
    "LogSubscriptionFilter",
    "OfflineDataMissing",
    "OfflineProvider",
    "ProviderAdapter",
    "ProviderBackend",
    "Subscription",
    "get_async_provider_from_config",
    "get_provider_from_config",
]
