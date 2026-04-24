"""
Offline provider for testing without RPC calls.

This module provides an EthereumProvider implementation that serves
pre-recorded chain data from JSON files, allowing tests to run without
requiring a live blockchain connection.

Example:
    >>> from degenbot.provider import OfflineProvider, ProviderAdapter
    >>>
    >>> # Load recorded data
    >>> offline = OfflineProvider.from_json_file(
    ...     Path("tests/fixtures/chain_data/1/multi_block.json")
    ... )
    >>> provider = ProviderAdapter.from_offline(offline)
    >>>
    >>> # Use in tests
    >>> result = provider.call(
    ...     to="0x...",
    ...     data=calldata,
    ...     block=24945700
    ... )
"""

import json
from pathlib import Path
from typing import Any

from hexbytes import HexBytes


class BlockNotRecordedError(Exception):
    """Raised when requesting data for a block that was not recorded."""

    def __init__(self, block: int, available: list[int]) -> None:
        self.block = block
        self.available = available
        available_str = ", ".join(str(b) for b in available)
        super().__init__(f"No data recorded for block {block}. Available blocks: {available_str}")


class OfflineDataMissing(Exception):
    """Raised when requested call data was not recorded."""

    def __init__(self, to: str, data: bytes) -> None:
        super().__init__(f"No recorded data for call to {to} with data 0x{data.hex()[:40]}...")


class OfflineCallReverted(Exception):
    """
    Raised when a recorded call reverted or the contract didn't exist at the block.

    This is signaled by a `null` result in the recorded data.
    """

    def __init__(self) -> None:
        super().__init__("Recorded call reverted or contract not deployed.")


class OfflineProvider:
    """
    Ethereum provider that serves pre-recorded chain data.

    This class implements the EthereumProvider Protocol (minus async methods)
    by loading recorded RPC responses from JSON files. It allows tests to
    run without requiring a live blockchain connection.

    Attributes:
        chain_id: The chain ID this provider serves data for
        blocks: Dictionary of recorded block data keyed by block number string

    Example:
        >>> offline = OfflineProvider(
        ...     chain_id=1,
        ...     blocks={
        ...         "24945700": {
        ...             "timestamp": 1776984059,
        ...             "calls": {
        ...                 "0x...:0x...": "0x..."
        ...             },
        ...             "code": {
        ...                 "0x...": "0x..."
        ...             }
        ...         }
        ...     }
        ... )
        >>> result = offline.call(to="0x...", data=b"...", block=24945700)
    """

    def __init__(
        self,
        chain_id: int,
        blocks: dict[str, dict[str, Any]],
    ) -> None:
        """
        Initialize the offline provider.

        Args:
            chain_id: The chain ID this provider serves data for
            blocks: Dictionary of recorded block data. Each block should have:
                - "timestamp": int
                - "calls": dict[str, str] - keyed by "address:data"
                - "code": dict[str, str] - keyed by address
        """
        self._chain_id = chain_id
        self._blocks = blocks
        self._block_numbers = sorted(int(b) for b in blocks)

        if not self._block_numbers:
            msg = "No blocks recorded in provider data"
            raise ValueError(msg)

    @classmethod
    def from_json_file(cls, path: Path) -> "OfflineProvider":
        """
        Load recorded data from a JSON file.

        Supports both old multi-block format (with "blocks" key) and new single-block
        format (with "block_number" key).

        Args:
            path: Path to the JSON file containing recorded data

        Returns:
            An OfflineProvider instance with the loaded data

        Raises:
            FileNotFoundError: If the file doesn't exist
            ValueError: If the JSON is malformed
        """
        with Path(path).open(encoding="utf-8") as f:
            data = json.load(f)

        # Check if this is new single-block format
        if "block_number" in data:
            # Single block format - wrap in blocks dict
            block_number = str(data["block_number"])
            return cls(
                chain_id=data["chain_id"],
                blocks={block_number: data},
            )

        # Old multi-block format
        return cls(
            chain_id=data["chain_id"],
            blocks=data["blocks"],
        )

    @classmethod
    def from_json_string(cls, json_str: str) -> "OfflineProvider":
        """
        Load recorded data from a JSON string.

        Args:
            json_str: JSON string containing recorded data

        Returns:
            An OfflineProvider instance with the loaded data
        """
        data = json.loads(json_str)
        return cls(
            chain_id=data["chain_id"],
            blocks=data["blocks"],
        )

    @property
    def chain_id(self) -> int:
        """Get the chain ID."""
        return self._chain_id

    @property
    def block_number(self) -> int:
        """Get the latest recorded block number."""
        return max(self._block_numbers)

    @property
    def block_numbers(self) -> list[int]:
        """Get list of all recorded block numbers."""
        return self._block_numbers.copy()

    def get_block_number(self) -> int:
        """Get the current (latest recorded) block number."""
        return self.block_number

    def _get_block_key(self, block: int | None) -> str:
        """
        Validate block number and return string key.

        Args:
            block: Block number, or None for latest

        Returns:
            String block key

        Raises:
            BlockNotRecordedError: If the block was not recorded
        """
        if block is None:
            block = self.block_number

        block_key = str(block)
        if block_key not in self._blocks:
            raise BlockNotRecordedError(block, self._block_numbers)

        return block_key

    def call(
        self,
        to: str,
        data: bytes,
        *,
        block_number: int | None = None,
    ) -> HexBytes:
        """
        Execute a contract call using recorded data.

        Args:
            to: Contract address to call
            data: Calldata bytes
            block_number: Block number, or None for latest

        Returns:
            Raw return data from the contract call

        Raises:
            BlockNotRecordedError: If the block was not recorded
            OfflineDataMissing: If the specific call was not recorded
            OfflineCallReverted: If the recorded call reverted (result is null)
        """
        block_key = self._get_block_key(block_number)

        call_key = f"{to.lower()}:0x{data.hex()}"
        block_data = self._blocks[block_key]

        if call_key not in block_data["calls"]:
            raise OfflineDataMissing(to, data)

        result = block_data["calls"][call_key]

        # Handle null/reverted calls
        if result is None:
            raise OfflineCallReverted(to, data)

        return HexBytes(result)

    def get_code(
        self,
        address: str,
        block_number: int | None = None,
    ) -> HexBytes:
        """
        Get contract bytecode at an address.

        Args:
            address: Contract address
            block_number: Block number, or None for latest

        Returns:
            Contract bytecode, or empty bytes if not recorded
        """
        block_key = self._get_block_key(block_number)
        block_data = self._blocks[block_key]

        code = block_data.get("code", {}).get(address.lower(), "")
        return HexBytes(code)

    def get_block(
        self,
        block_identifier: int | str,
    ) -> dict[str, Any] | None:
        """
        Get a block by number or identifier.

        Args:
            block_identifier: Block number or "latest"

        Returns:
            Block data dict with "number", "timestamp", or None if not found
        """
        if block_identifier == "latest":
            block_num = self.block_number
        else:
            try:
                block_num = int(block_identifier)
            except (ValueError, TypeError):
                return None

        try:
            block_key = self._get_block_key(block_num)
        except BlockNotRecordedError:
            return None

        block_data = self._blocks[block_key]
        return {
            "number": block_num,
            "timestamp": block_data.get("timestamp", 0),
        }

    def get_balance(
        self,
        address: str,
        block_number: int | None = None,
    ) -> int:
        """
        Get the balance of an address.

        Note: Balance tracking is not implemented in OfflineProvider.
        This method always raises NotImplementedError.

        Args:
            address: Ethereum address
            block_number: Block number, or None for latest

        Raises:
            NotImplementedError: Balances are not recorded
        """
        msg = (
            "OfflineProvider does not support get_balance. "
            "Use a live provider for balance-dependent tests."
        )
        raise NotImplementedError(msg)

    def get_storage_at(
        self,
        address: str,
        position: int,
        block_number: int | None = None,
    ) -> HexBytes:
        """
        Get storage at a given position.

        Note: Storage tracking is not implemented in OfflineProvider.
        This method always raises NotImplementedError.

        Args:
            address: Contract address
            position: Storage slot position
            block_number: Block number, or None for latest

        Raises:
            NotImplementedError: Storage is not recorded
        """
        msg = (
            "OfflineProvider does not support get_storage_at. "
            "Use a live provider for storage-dependent tests."
        )
        raise NotImplementedError(msg)

    def get_transaction_count(
        self,
        address: str,
        block_number: int | None = None,
    ) -> int:
        """
        Get the transaction count (nonce) for an address.

        Note: Transaction counts are not implemented in OfflineProvider.
        This method always raises NotImplementedError.

        Args:
            address: Ethereum address
            block_number: Block number, or None for latest

        Raises:
            NotImplementedError: Transaction counts are not recorded
        """
        msg = (
            "OfflineProvider does not support get_transaction_count. "
            "Use a live provider for nonce-dependent tests."
        )
        raise NotImplementedError(msg)

    def get_logs(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None = None,
        topics: list[list[str]] | None = None,
    ) -> list[dict[str, Any]]:
        """
        Fetch event logs.

        Note: Log fetching is not implemented in OfflineProvider.
        This method always raises NotImplementedError.

        Raises:
            NotImplementedError: Logs are not recorded
        """

        msg = (
            "OfflineProvider does not support get_logs. "
            "Use a live provider for event log-dependent tests."
        )
        raise NotImplementedError(msg)

    @staticmethod
    def is_connected() -> bool:
        """Check if the provider is connected.

        OfflineProvider is always considered connected.
        """
        return True

    def __repr__(self) -> str:
        return f"OfflineProvider(chain_id={self._chain_id}, blocks={len(self._block_numbers)})"


__all__ = [
    "BlockNotRecordedError",
    "OfflineDataMissing",
    "OfflineProvider",
]
