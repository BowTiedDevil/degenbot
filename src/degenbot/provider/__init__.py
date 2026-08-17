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

from degenbot._ffi.provider import AlloyProvider as RustAlloyProvider
from degenbot._ffi.provider import AlloySubscription
from degenbot._ffi.provider import AsyncAlloyProvider as RustAsyncAlloyProvider
from degenbot.exceptions import SubscriptionNotSupported
from degenbot.provider.factory import (
    get_async_provider_from_config,
    get_provider_from_config,
)
from degenbot.provider.offline_provider import (
    OfflineProvider,
)
from degenbot.provider.subscription import LogSubscriptionFilter, Subscription
from degenbot.types.aliases import BlockNumber
from degenbot.types.rpc_types import (
    BlockData,
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

    def get_code(self, address: str, block: int | None = None) -> HexBytes:
        """Get contract code at an address.

        Args:
            address: Contract address
            block: Block number to get code at (default: latest)

        Returns:
            Contract bytecode as HexBytes

        """
        return self._provider.get_code(address, block)

    def call(
        self,
        to: str,
        data: bytes,
        block: int | None = None,
    ) -> HexBytes:
        """Execute an eth_call to a contract.

        Args:
            to: Contract address to call
            data: Calldata bytes (function selector + encoded arguments)
            block: Block number to execute call at (default: latest)

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
        return self._provider.call(to, data, block)

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
        block: int | None = None,
    ) -> int:
        """Estimate gas for a transaction.

        Args:
            to: Target address
            data: Transaction data
            from_: Sender address (optional)
            value: Value in wei (optional)
            block: Block number to estimate at (default: latest)

        Returns:
            Estimated gas as int

        """
        return self._provider.estimate_gas(to, data, from_, value, block)

    def get_storage_at(
        self,
        address: str,
        position: int,
        block: int | None = None,
    ) -> HexBytes:
        """Get storage at a given position.

        Args:
            address: Contract address
            position: Storage slot position (supports large values like mapping slots)
            block: Block number to get storage at (default: latest)

        Returns:
            Storage value at the position as HexBytes (32 bytes)

        """
        return self._provider.get_storage_at(address, position, block)

    def close(self) -> None:
        """Close connection pool and release resources."""
        self._provider.close()

    def is_connected(self) -> bool:  # ruff:ignore[no-self-use]
        """Check if the provider is connected.

        For AlloyProvider, we assume connection is valid if the provider was created.

        Returns:
            True if the provider is connected.

        """
        return True

    def get_balance(
        self,
        address: str,
        block: int | None = None,
    ) -> int:
        """Get the balance of an address in wei.

        Args:
            address: Account address
            block: Block number to get balance at (default: latest)

        Returns:
            Balance in wei as int

        """
        return self._provider.get_balance(address, block)

    def get_transaction_count(
        self,
        address: str,
        block: int | None = None,
    ) -> int:
        """Get the transaction count (nonce) for an address.

        Args:
            address: Account address
            block: Block number to get nonce at (default: latest)

        Returns:
            Transaction count as int

        """
        return self._provider.get_transaction_count(address, block)

    def make_request(
        self,
        method: str,
        params: list[Any],
    ) -> Any:  # ruff:ignore[any-type]
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

    def call_raw(self, tx: TxParams, block: int | None = None) -> HexBytes:
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
        return self._provider.call(tx["to"], HexBytes(tx["data"]), block)

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

    def to_alloy_provider(self) -> RustAlloyProvider:
        """Return the inner Rust ``AlloyProvider`` pyclass over this provider's transport.

        Rust pyclasses (e.g. ``ChainlinkPriceFeed``, ``AavePriceOracle``)
        expect the Rust ``AlloyProvider`` pyclass, not this Python wrapper.
        This shim unwraps to the inner provider so those call sites work
        unchanged.

        Returns:
            The inner Rust ``AlloyProvider`` pyclass.

        """
        return self._provider

    @property
    def provider_type(self) -> str:
        """The provider type (always 'alloy')."""
        return "alloy"

    @property
    def provider(self) -> RustAlloyProvider:
        """The underlying Rust ``AlloyProvider`` pyclass."""
        return self._provider

    @staticmethod
    def as_web3() -> None:
        """Return ``None`` — this provider has no Web3 backend."""
        return

    def as_alloy(self) -> RustAlloyProvider:
        """Return the inner Rust ``AlloyProvider`` pyclass.

        Rust pyclasses (e.g. ``ChainlinkPriceFeed``, ``AavePriceOracle``)
        expect the Rust ``AlloyProvider`` pyclass, not this Python wrapper.

        Returns:
            The inner Rust ``AlloyProvider`` pyclass.

        """
        return self._provider

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

    # ----- Subscription stubs (sync provider — always raises) -----

    def _raise_subscription_not_supported(self) -> None:  # ruff:ignore[no-self-use]
        """Raise for any sync subscription call.

        Raises:
            SubscriptionNotSupported: Always.

        """
        raise SubscriptionNotSupported(transport="sync", rpc_url="unknown")

    def subscribe_blocks(self) -> None:
        """Not available on sync providers — use AsyncAlloyProvider."""
        self._raise_subscription_not_supported()

    def subscribe_full_blocks(self) -> None:
        """Not available on sync providers — use AsyncAlloyProvider."""
        self._raise_subscription_not_supported()

    def subscribe_pending_transactions(self) -> None:
        """Not available on sync providers — use AsyncAlloyProvider."""
        self._raise_subscription_not_supported()

    def subscribe_full_pending_transactions(self) -> None:
        """Not available on sync providers — use AsyncAlloyProvider."""
        self._raise_subscription_not_supported()

    def subscribe_logs(
        self,
        addresses: list[str] | None = None,  # ruff:ignore[unused-method-argument]
        topics: list[list[str]] | None = None,  # ruff:ignore[unused-method-argument]
    ) -> None:
        """Not available on sync providers — use AsyncAlloyProvider."""
        self._raise_subscription_not_supported()


class AsyncAlloyProvider:
    """High-performance async Ethereum RPC provider using Alloy.

    A thin Python wrapper around the Rust ``AsyncAlloyProvider`` pyclass.
    Adds string block-identifier resolution, subscription wrapping, and
    adapter-compat introspection shims so it is a direct drop-in for the
    retired ``AsyncProviderAdapter``.

    Args:
        rust_provider: The underlying Rust ``AsyncAlloyProvider`` pyclass.

    Prefer :meth:`create` to construct an instance from an RPC URL.

    """

    def __init__(self, rust_provider: RustAsyncAlloyProvider) -> None:
        """Initialize the instance."""
        self._provider = rust_provider

    @staticmethod
    async def create(
        rpc_url: str,
        max_retries: int = 10,
        max_blocks_per_request: int = 5000,
        requests_per_second: int | None = None,
        burst: int | None = None,
    ) -> "AsyncAlloyProvider":
        """Create an ``AsyncAlloyProvider`` asynchronously.

        Args:
            rpc_url: HTTP/HTTPS/WS/IPC endpoint URL.
            max_retries: Maximum retry attempts.
            max_blocks_per_request: Maximum blocks per log request.
            requests_per_second: Optional rate limit.
            burst: Optional burst size for rate limiting.

        Returns:
            An ``AsyncAlloyProvider`` instance.

        """
        rust = await RustAsyncAlloyProvider.create(
            rpc_url,
            max_retries,
            max_blocks_per_request,
            requests_per_second,
            burst,
        )
        return AsyncAlloyProvider(rust)

    # ----- Properties -----

    @property
    def rpc_url(self) -> str:
        """The RPC endpoint URL."""
        return self._provider.rpc_url

    @property
    def provider_type(self) -> str:
        """The provider type (always 'alloy')."""
        return "alloy"

    @property
    def provider(self) -> "AsyncAlloyProvider":
        """The underlying provider (identity — returns ``self``).

        Returns:
            This provider instance.

        """
        return self

    # ----- Async methods -----

    async def get_block_number(self) -> int:
        """Return the current block number.

        Returns:
            The current block number.

        """
        return await self._provider.get_block_number()

    async def get_chain_id(self) -> int:
        """Return the chain ID.

        Returns:
            The chain ID.

        """
        return await self._provider.get_chain_id()

    async def get_gas_price(self) -> int:
        """Return the current gas price in wei.

        Returns:
            The current gas price in wei.

        """
        return await self._provider.get_gas_price()

    async def get_block(self, block_identifier: int | str) -> BlockData | None:
        """Get a block by number or tag.

        Args:
            block_identifier: Block number, or one of 'latest', 'earliest', 'pending'.

        Returns:
            Block data, or None if not found.

        Raises:
            ValueError: If ``block_identifier`` is an unsupported string.

        """
        if isinstance(block_identifier, str):
            if block_identifier == "latest":
                block_identifier = await self._provider.get_block_number()
            elif block_identifier == "earliest":
                block_identifier = 0
            elif block_identifier == "pending":
                block_identifier = await self._provider.get_block_number() + 1
            else:
                msg = f"Unsupported block identifier: {block_identifier!r}"
                raise ValueError(msg)
        return await self._provider.get_block(block_identifier)

    async def get_transaction(self, tx_hash: str) -> TransactionData | None:
        """Get a transaction by hash.

        Returns:
            The transaction data, or None if not found.

        """
        return await self._provider.get_transaction(tx_hash)

    async def get_transaction_receipt(self, tx_hash: str) -> TransactionReceiptData | None:
        """Get a transaction receipt by hash.

        Returns:
            The transaction receipt, or None if not found.

        """
        return await self._provider.get_transaction_receipt(tx_hash)

    async def get_logs(
        self,
        *,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> list[LogData]:
        """Fetch event logs matching the filter.

        Returns:
            A list of matching log entries.

        """
        return await self._provider.get_logs(
            from_block=from_block,
            to_block=to_block,
            addresses=addresses,
            topics=topics,
        )

    async def call(
        self,
        to: str,
        data: bytes,
        block: int | None = None,
    ) -> HexBytes:
        """Execute an eth_call.

        Returns:
            The raw return data from the contract call.

        """
        return await self._provider.call(to, data, block)

    async def call_raw(
        self,
        tx: TxParams,
        block: int | None = None,
    ) -> HexBytes:
        """Execute an eth_call with a raw transaction dict.

        Returns:
            The raw return data from the contract call.

        """
        return await self._provider.call(tx["to"], HexBytes(tx["data"]), block)

    async def batch_call(
        self,
        calls: list[TxParams],
        block: int | None = None,
    ) -> list[HexBytes]:
        """Execute multiple eth_calls sequentially.

        Returns:
            A list of raw return data from each call.

        """
        return [await self.call_raw(tx, block) for tx in calls]

    async def get_block_timestamp(self, block: int | None = None) -> int:
        """Get the timestamp for a block.

        Args:
            block: Block number, or None for latest.

        Returns:
            The block timestamp as an integer (Unix seconds).

        Raises:
            ValueError: If the block is not found.

        """
        block_data = await self.get_block(block if block is not None else "latest")
        if block_data is None:
            msg = f"Block {block} not found"
            raise ValueError(msg)
        return block_data["timestamp"]

    async def get_code(self, address: str, block: int | None = None) -> HexBytes:
        """Get the bytecode at an address.

        Returns:
            The contract bytecode, or empty bytes if not a contract.

        """
        return await self._provider.get_code(address, block)

    async def estimate_gas(
        self,
        to: str,
        data: bytes,
        from_: str | None = None,
        value: int | None = None,
        block: int | None = None,
    ) -> int:
        """Estimate gas for a transaction.

        Returns:
            The estimated gas units.

        """
        return await self._provider.estimate_gas(to, data, from_, value, block)

    async def get_storage_at(
        self,
        address: str,
        position: int,
        block: int | None = None,
    ) -> HexBytes:
        """Get storage at a given position.

        Returns:
            The storage value as bytes.

        """
        return await self._provider.get_storage_at(address, position, block)

    async def get_balance(self, address: str, block: int | None = None) -> int:
        """Get the balance of an address in wei.

        Returns:
            The balance in wei.

        """
        return await self._provider.get_balance(address, block)

    async def get_transaction_count(
        self,
        address: str,
        block: int | None = None,
    ) -> int:
        """Get the transaction count (nonce) for an address.

        Returns:
            The transaction count (nonce).

        """
        return await self._provider.get_transaction_count(address, block)

    async def make_request(self, method: str, params: list[Any]) -> Any:  # ruff:ignore[any-type]
        """Make a raw JSON-RPC request.

        Returns:
            The raw JSON-RPC response.

        """
        return await self._provider.make_request(method, params)

    def is_connected(self) -> bool:  # ruff:ignore[no-self-use]
        """Check if the provider is connected.

        Returns:
            True if connected.

        """
        return True

    def close(self) -> None:
        """Close the provider and release resources."""
        self._provider.close()

    # ----- Subscription methods -----

    async def subscribe_blocks(self) -> Subscription:
        """Subscribe to new block headers.

        Returns:
            A subscription yielding new block numbers.

        """
        return Subscription(_inner=self._provider.subscribe_blocks())

    async def subscribe_full_blocks(self) -> Subscription:
        """Subscribe to full block bodies.

        Returns:
            A subscription yielding full block data.

        """
        return Subscription(_inner=self._provider.subscribe_full_blocks())

    async def subscribe_pending_transactions(self) -> Subscription:
        """Subscribe to pending transaction hashes.

        Returns:
            A subscription yielding pending transaction hashes.

        """
        return Subscription(_inner=self._provider.subscribe_pending_transactions())

    async def subscribe_full_pending_transactions(self) -> Subscription:
        """Subscribe to full pending transaction bodies.

        Returns:
            A subscription yielding full pending transaction bodies.

        """
        return Subscription(_inner=self._provider.subscribe_full_pending_transactions())

    async def subscribe_logs(
        self,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> Subscription:
        """Subscribe to filtered log events.

        Returns:
            A subscription yielding matching log entries.

        """
        return Subscription(
            _inner=self._provider.subscribe_logs(addresses=addresses, topics=topics)
        )

    # ----- Introspection shims -----

    @staticmethod
    def to_alloy_provider() -> AlloyProvider:
        """Return an AlloyProvider. Raises (no sync transport on async provider).

        Raises:
            ValueError: Always — an AsyncAlloyProvider has no sync transport.

        """
        msg = "Cannot build a sync AlloyProvider from an AsyncAlloyProvider."
        raise ValueError(msg)

    def as_async_alloy(self) -> RustAsyncAlloyProvider:
        """Return the inner Rust ``AsyncAlloyProvider`` pyclass.

        Rust pyclasses (e.g. ``SimulateContext``, ``dispatch_and_submit_py``)
        expect the Rust ``AsyncAlloyProvider`` pyclass, not this Python wrapper.

        Returns:
            The inner Rust ``AsyncAlloyProvider`` pyclass.

        """
        return self._provider

    @staticmethod
    def as_web3() -> None:
        """Return ``None`` — this provider has no Web3 backend."""
        return

    @staticmethod
    def as_alloy() -> None:
        """Return ``None`` — use ``as_async_alloy`` for the async handle."""
        return

    @staticmethod
    def as_offline() -> None:
        """Return ``None`` — this provider is not an ``OfflineProvider``."""
        return

    def __repr__(self) -> str:
        """Return a string representation.

        Returns:
            A string representation of the provider.

        """
        return f"AsyncAlloyProvider(rpc_url={self.rpc_url!r})"


__all__ = [
    "AlloyProvider",
    "AlloySubscription",
    "AsyncAlloyProvider",
    "LogFilter",
    "LogSubscriptionFilter",
    "OfflineProvider",
    "Subscription",
    "get_async_provider_from_config",
    "get_provider_from_config",
]
