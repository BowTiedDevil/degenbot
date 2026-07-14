"""Offline provider for testing without RPC calls.

This module provides a provider implementation that serves
pre-recorded chain data from JSON files, allowing tests to run without
requiring a live blockchain connection.

The heavy lifting lives in the Rust ``degenbot_rpc::offline`` module (an
in-memory alloy ``Transport`` over recorded data). This Python class is a thin
delegating shell: it parses the recorded JSON for metadata (``chain_id``,
``block_numbers``) and holds a real Rust :class:`AlloyProvider` (``PyAlloyProvider``)
that answers ``call``/``get_code``/``get_block``/``get_block_number`` via the
offline transport — so a consumer cannot distinguish an ``OfflineProvider`` from
a live provider. ``PyBotIo`` can hold it natively (O2/B1).

Example:
    >>> from degenbot.provider import OfflineProvider
    >>>
    >>> # Load recorded data
    >>> offline = OfflineProvider.from_json_file(
    ...     Path("tests/fixtures/chain_data/1/multi_block.json")
    ... )
    >>>
    >>> # Use in tests
    >>> result = offline.call(to="0x...", data=calldata, block=24945700)

"""

from __future__ import annotations

import json
from pathlib import Path
from typing import TYPE_CHECKING, Any

from hexbytes import HexBytes

from degenbot.degenbot_rs import AlloyProvider as RustAlloyProvider

if TYPE_CHECKING:
    from degenbot.types.rpc_types import BlockData, TxParams


class BlockNotRecordedError(Exception):
    """Raised when requesting data for a block that was not recorded.

    Kept importable for backwards compatibility. The delegating shell now
    surfaces unrecorded blocks as ``None`` (for ``get_block``) or an alloy
    ``ProviderError`` (for ``eth_call``/``eth_getCode`` at an unrecorded block)
    rather than this specific type.
    """

    def __init__(self, block: int, available: list[int]) -> None:
        """Initialize the instance."""
        self.block = block
        self.available = available
        available_str = ", ".join(str(b) for b in available)
        super().__init__(f"No data recorded for block {block}. Available blocks: {available_str}")


class OfflineDataMissing(Exception):
    """Raised when requested call data was not recorded.

    Kept importable for backwards compatibility. The delegating shell now
    surfaces unrecorded calls as an alloy ``ProviderError`` (``RuntimeError``)
    rather than this specific type.
    """

    def __init__(self, to: str, data: bytes) -> None:
        """Initialize the instance."""
        super().__init__(f"No recorded data for call to {to} with data 0x{data.hex()[:40]}...")


class OfflineCallReverted(Exception):
    """Raised when a recorded call reverted or the contract didn't exist at the block.

    Kept importable for backwards compatibility. The delegating shell now
    surfaces recorded reverts as a :class:`degenbot.exceptions.ContractLogicError`
    (the alloy revert path) rather than this specific type.
    """

    def __init__(self, to: str, data: bytes) -> None:
        self.to = to
        self.data = data
        msg = (
            f"Recorded call reverted or contract not deployed"
            f" for call to {to} with data 0x{data.hex()[:40]}..."
        )
        super().__init__(msg)


class OfflineProvider:
    """Ethereum provider that serves pre-recorded chain data.

    This class delegates every data-serving RPC (``call``, ``get_code``,
    ``get_block``, ``get_block_number``, ``get_block_timestamp``) to a real
    Rust :class:`AlloyProvider` built over an in-memory offline transport, so it is
    indistinguishable from a live provider to any consumer (including
    :class:`PyBotIo`). Recorded-JSON metadata (``chain_id``, ``block_numbers``)
    is parsed in Python for the convenience accessors.

    Attributes:
        chain_id: The chain ID this provider serves data for
        blocks: Dictionary of recorded block data keyed by block number string

    Example:
        >>> offline = OfflineProvider(
        ...     chain_id=1,
        ...     blocks={
        ...         "24945700": {
        ...             "timestamp": 1776984059,
        ...             "calls": {"0x...:0x...": "0x..."},
        ...             "code": {"0x...": "0x..."},
        ...         }
        ...     },
        ... )
        >>> result = offline.call(to="0x...", data=b"...", block=24945700)

    """

    def __init__(
        self,
        chain_id: int,
        blocks: dict[str, dict[str, Any]],
    ) -> None:
        """Initialize the offline provider.

        Args:
            chain_id: The chain ID this provider serves data for
            blocks: Dictionary of recorded block data. Each block should have:
                - "timestamp": int
                - "calls": dict[str, str] - keyed by "address:data"
                - "code": dict[str, str] - keyed by address

        Raises:
            ValueError: If no blocks are recorded in provider data.

        """
        self._chain_id = chain_id
        self._blocks = blocks
        self._block_numbers = sorted(int(b) for b in blocks)

        if not self._block_numbers:
            msg = "No blocks recorded in provider data"
            raise ValueError(msg)

        # Build the Rust offline transport from the same recorded data. The
        # multi-block `{"chain_id", "blocks": {...}}` envelope matches the
        # `RecordedData` wire format parsed by `OfflineProvider::from_json_bytes`.
        recorded_json = json.dumps({"chain_id": chain_id, "blocks": blocks})
        self._alloy: RustAlloyProvider = RustAlloyProvider.offline_from_json_string(recorded_json)

    @classmethod
    def from_json_file(cls, path: Path) -> OfflineProvider:
        """Load recorded data from a JSON file.

        Supports both old multi-block format (with "blocks" key) and new single-block
        format (with "block_number" key).

        Args:
            path: Path to the JSON file containing recorded data

        Returns:
            An OfflineProvider instance loaded from the file.

        """
        # Build the Rust transport from the raw file (it handles both formats
        # natively), then parse the JSON again for the Python-side metadata.
        raw = Path(path).read_text(encoding="utf-8")
        data = json.loads(raw)

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
    def from_json_string(cls, json_str: str) -> OfflineProvider:
        """Load recorded data from a JSON string.

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
        """The chain ID."""
        return self._chain_id

    @property
    def block_number(self) -> int:
        """The latest recorded block number."""
        return max(self._block_numbers)

    @property
    def block_numbers(self) -> list[int]:
        """List of all recorded block numbers.

        Returns:
            A copy of the list of recorded block numbers.

        """
        return self._block_numbers.copy()

    def get_block_number(self) -> int:
        """Get the current (latest recorded) block number.

        Returns:
            The latest recorded block number.

        """
        return self._alloy.get_block_number()

    def call(
        self,
        to: str,
        data: bytes,
        block: int | None = None,
    ) -> HexBytes:
        """Execute a contract call using recorded data.

        Delegates to the Rust offline transport. A recorded revert (``null``
        result) surfaces as a :class:`ContractLogicError`; an unrecorded call
        surfaces as a provider ``RuntimeError``.

        Args:
            to: Contract address to call
            data: Calldata bytes
            block: Block number, or None for latest

        Returns:
            Raw return data from the contract call

        """
        return self._alloy.call(to, HexBytes(data), block_number=block)

    def get_code(
        self,
        address: str,
        block: int | None = None,
    ) -> HexBytes:
        """Get contract bytecode at an address.

        Args:
            address: Contract address
            block: Block number, or None for latest

        Returns:
            Contract bytecode, or empty bytes if not recorded

        """
        return self._alloy.get_code(address, block_number=block)

    def get_block(
        self,
        block_identifier: int | str,
    ) -> BlockData | None:
        """Get a block by number or identifier.

        Args:
            block_identifier: Block number or "latest"

        Returns:
            Block data dict (including ``number`` and ``timestamp``), or
            ``None`` if the block is not recorded.

        """
        if block_identifier == "latest":
            block_num = self._alloy.get_block_number()
        else:
            try:
                block_num = int(block_identifier)
            except (ValueError, TypeError):
                return None
        return self._alloy.get_block(block_num)

    def get_balance(
        self,
        address: str,
        block: int | None = None,
    ) -> int:
        """Get the balance of an address.

        Note: Balance tracking is not implemented in OfflineProvider.
        This method always raises NotImplementedError.

        Args:
            address: Ethereum address
            block: Block number, or None for latest

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
        block: int | None = None,
    ) -> HexBytes:
        """Get storage at a given position.

        Note: Storage tracking is not implemented in OfflineProvider.
        This method always raises NotImplementedError.

        Args:
            address: Contract address
            position: Storage slot position
            block: Block number, or None for latest

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
        block: int | None = None,
    ) -> int:
        """Get the transaction count (nonce) for an address.

        Note: Transaction counts are not implemented in OfflineProvider.
        This method always raises NotImplementedError.

        Args:
            address: Ethereum address
            block: Block number, or None for latest

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
        """Fetch event logs.

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

        Returns:
            True, since the offline provider is always connected.

        """
        return True

    def call_raw(self, tx: TxParams, block: int | None = None) -> HexBytes:
        """Execute an eth_call with a raw transaction dict.

        Args:
            tx: Transaction dict with at least ``to`` and ``data`` keys.
            block: Block number, or None for latest.

        Returns:
            The raw return data from the contract call.

        """
        return self.call(tx["to"], HexBytes(tx["data"]), block=block)

    def batch_call(self, calls: list[TxParams], block: int | None = None) -> list[HexBytes]:
        """Execute multiple eth_calls sequentially.

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
        block_data = self.get_block(block if block is not None else self.get_block_number())
        if block_data is None:
            msg = f"Block {block} not found"
            raise ValueError(msg)
        return block_data["timestamp"]

    def make_request(self, method: str, params: list[Any]) -> Any:  # noqa: ANN401
        """Raw JSON-RPC request — not supported by OfflineProvider."""
        msg = f"OfflineProvider does not support make_request (method={method!r})"
        raise NotImplementedError(msg)

    def close(self) -> None:  # noqa: PLR6301
        """Close the provider — no resources to release."""
        return

    def to_alloy_provider(self) -> RustAlloyProvider:
        """Return the underlying Rust :class:`AlloyProvider`.

        The offline provider *is* a real Rust ``AlloyProvider`` over an
        in-memory transport, so this returns the held pyclass.

        Returns:
            The wrapped :class:`degenbot.degenbot_rs.AlloyProvider`.

        """
        return self._alloy

    @property
    def provider_type(self) -> str:
        """The provider type (always 'offline')."""
        return "offline"

    @property
    def provider(self) -> RustAlloyProvider:
        """The underlying Rust ``AlloyProvider`` (the offline pyclass)."""
        return self._alloy

    @staticmethod
    def as_web3() -> None:
        """Return ``None`` — this provider has no Web3 backend."""
        return

    def as_alloy(self) -> RustAlloyProvider:
        """Return the underlying Rust :class:`AlloyProvider`.

        Returns:
            The wrapped :class:`degenbot.degenbot_rs.AlloyProvider`.

        """
        return self._alloy

    def as_offline(self) -> OfflineProvider:
        """Return ``self`` as an ``OfflineProvider``.

        Returns:
            This provider instance.

        """
        return self

    def __repr__(self) -> str:
        """Return a string representation.

        Returns:
            A string representation of the offline provider.

        """
        return f"OfflineProvider(chain_id={self._chain_id}, blocks={len(self._block_numbers)})"


__all__ = [
    "BlockNotRecordedError",
    "OfflineDataMissing",
    "OfflineProvider",
]
