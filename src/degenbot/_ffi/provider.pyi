from collections.abc import Coroutine
from typing import Any

from degenbot.types.rpc_types import (
    BlockData,
    LogData,
    TransactionData,
    TransactionReceiptData,
)

class AlloyProvider:
    r"""Synchronous Ethereum RPC provider.

    Automatically detects connection type from URL:
    - HTTP/HTTPS URLs use HTTP transport with connection pooling
    - WS/WSS URLs use WebSocket transport
    - File paths (Unix: /path, Windows: \\.\pipe\...) use IPC transport

    Rate limiting is opt-in: pass ``requests_per_second`` and ``burst``
    together to enable transport-level throttling on HTTP connections.
    """

    def __init__(
        self,
        rpc_url: str,
        max_retries: int = 10,
        max_blocks_per_request: int = 5000,
        requests_per_second: int | None = None,
        burst: int | None = None,
    ) -> None: ...
    @property
    def rpc_url(self) -> str: ...
    def get_block_number(self) -> int: ...
    def get_chain_id(self) -> int: ...
    def get_gas_price(self) -> int: ...
    def get_block(self, block_number: int) -> BlockData | None: ...
    def get_transaction(self, tx_hash: str) -> TransactionData | None: ...
    def get_transaction_receipt(self, tx_hash: str) -> TransactionReceiptData | None: ...
    def get_logs(
        self,
        *,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> list[LogData]: ...
    def call(
        self,
        to: str,
        data: bytes,
        block_number: int | None = None,
    ) -> bytes: ...
    def get_code(
        self,
        address: str,
        block_number: int | None = None,
    ) -> bytes: ...
    def estimate_gas(
        self,
        to: str,
        data: bytes,
        from_: str | None = None,
        value: int | None = None,
        block_number: int | None = None,
    ) -> int: ...
    def get_storage_at(
        self,
        address: str,
        position: int,
        block_number: int | None = None,
    ) -> bytes: ...
    def get_balance(
        self,
        address: str,
        block_number: int | None = None,
    ) -> int: ...
    def get_transaction_count(
        self,
        address: str,
        block_number: int | None = None,
    ) -> int: ...
    @staticmethod
    def offline_from_json_file(path: str) -> AlloyProvider: ...
    @staticmethod
    def offline_from_json_string(s: str) -> AlloyProvider: ...
    def make_request(
        self,
        method: str,
        params: list[str | bool | int | bytes | None],
    ) -> str | bool | int | bytes | list[Any] | dict[str, Any] | None: ...
    def close(self) -> None: ...
    def subscribe_blocks(self) -> AlloySubscription: ...
    def subscribe_full_blocks(self) -> AlloySubscription: ...
    def subscribe_pending_transactions(self) -> AlloySubscription: ...
    def subscribe_full_pending_transactions(self) -> AlloySubscription: ...
    def subscribe_logs(
        self,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> AlloySubscription: ...

class LogFilter:
    """Filter for log queries."""

    def __init__(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> None: ...
    @property
    def from_block(self) -> int | None: ...
    @property
    def to_block(self) -> int | None: ...
    @property
    def addresses(self) -> list[str]: ...
    @property
    def topics(self) -> list[list[str]]: ...

class AsyncAlloyProvider:
    """Async wrapper for AlloyProvider operations."""

    def __init__(self, sync_provider: AlloyProvider) -> None: ...
    @staticmethod
    def create(
        rpc_url: str,
        max_retries: int = 10,
        max_blocks_per_request: int = 5000,
        requests_per_second: int | None = None,
        burst: int | None = None,
    ) -> Coroutine[Any, Any, AsyncAlloyProvider]: ...
    @property
    def rpc_url(self) -> str: ...
    def get_block_number(self) -> Coroutine[Any, Any, int]: ...
    def get_chain_id(self) -> Coroutine[Any, Any, int]: ...
    def get_gas_price(self) -> Coroutine[Any, Any, int]: ...
    def get_block(self, block_number: int) -> Coroutine[Any, Any, BlockData | None]: ...
    def get_transaction(self, tx_hash: str) -> Coroutine[Any, Any, TransactionData | None]: ...
    def get_transaction_receipt(
        self,
        tx_hash: str,
    ) -> Coroutine[Any, Any, TransactionReceiptData | None]: ...
    def get_logs(
        self,
        *,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> Coroutine[Any, Any, list[LogData]]: ...
    def call(
        self,
        to: str,
        data: bytes,
        block_number: int | None = None,
    ) -> Coroutine[Any, Any, bytes]: ...
    def get_code(
        self,
        address: str,
        block_number: int | None = None,
    ) -> Coroutine[Any, Any, bytes]: ...
    def estimate_gas(
        self,
        to: str,
        data: bytes,
        from_: str | None = None,
        value: int | None = None,
        block_number: int | None = None,
    ) -> Coroutine[Any, Any, int]: ...
    def get_storage_at(
        self,
        address: str,
        position: int,
        block_number: int | None = None,
    ) -> Coroutine[Any, Any, bytes]: ...
    def get_balance(
        self,
        address: str,
        block_number: int | None = None,
    ) -> Coroutine[Any, Any, int]: ...
    def get_transaction_count(
        self,
        address: str,
        block_number: int | None = None,
    ) -> Coroutine[Any, Any, int]: ...
    def make_request(
        self,
        method: str,
        params: list[str | bool | int | bytes | None],
    ) -> Coroutine[Any, Any, str | bool | int | bytes | list[Any] | dict[str, Any] | None]: ...
    def close(self) -> None: ...
    def subscribe_blocks(self) -> AlloySubscription: ...
    def subscribe_full_blocks(self) -> AlloySubscription: ...
    def subscribe_pending_transactions(self) -> AlloySubscription: ...
    def subscribe_full_pending_transactions(self) -> AlloySubscription: ...
    def subscribe_logs(
        self,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> AlloySubscription: ...

class AlloySubscription:
    """Python wrapper for a subscription from the Rust layer."""

    def __aiter__(self) -> AlloySubscription: ...
    def __anext__(self) -> Coroutine[Any, Any, BlockData | LogData | str]: ...
    def drain(self) -> list[BlockData | LogData | str]: ...
    async def started(self) -> None: ...
    def unsubscribe(self) -> None: ...

__all__ = [
    "AlloyProvider",
    "AlloySubscription",
    "AsyncAlloyProvider",
    "LogFilter",
]
