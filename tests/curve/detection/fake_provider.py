"""Fake ProviderAdapter for Curve pool detection tests.

Provides a configurable fake that returns pre-programmed responses to
provider.call_raw() based on the method selector in the calldata.
"""

from __future__ import annotations

from typing import Any

from hexbytes import HexBytes
from web3.exceptions import Web3Exception

from degenbot.builders.pool_io import SyncPoolIO
from degenbot.provider.sync_adapter import ProviderAdapter


class FakeCurveBackend:
    """Fake _SyncProviderBackend that dispatches call_raw by selector.

    Responses are configured via the ``call_responses`` dict. The key is the
    4-byte selector; the value is either:
    - A callable(to, data, block) -> bytes
    - A bytes value returned directly
    """

    def __init__(self, call_responses: dict[bytes, Any]) -> None:
        self._call_responses = call_responses
        self._block_timestamp = 1700000000

    @property
    def chain_id(self) -> int:
        return 1

    @property
    def block_number(self) -> int:
        return 18_000_000

    def get_block_number(self) -> int:
        return 18_000_000

    def get_block(self, block_identifier: int | str) -> dict[str, Any] | None:
        return {"number": 18_000_000, "timestamp": self._block_timestamp}

    def get_logs(
        self,
        from_block: int,
        to_block: int,
        addresses: list[str] | None,
        topics: list[list[str]] | None,
    ) -> list[dict[str, Any]]:
        return []

    def call(self, to: str, data: bytes, block: int | None) -> HexBytes:
        return self.call_raw({"to": to, "data": data}, block)

    def call_raw(self, tx: dict[str, Any], block: int | None) -> HexBytes:
        data = bytes(tx["data"])
        selector = data[:4]
        handler = self._call_responses.get(selector)
        if handler is None:
            msg = f"No handler for selector {selector.hex()}"
            raise Web3Exception(msg)
        if callable(handler):
            return HexBytes(handler(tx.get("to", ""), data, block))
        return HexBytes(handler)

    def get_code(self, address: str, block: int | None) -> HexBytes:
        return HexBytes(b"")

    def get_balance(self, address: str, block: int | None) -> int:
        return 0

    def get_storage_at(self, address: str, position: int, block: int | None) -> HexBytes:
        return HexBytes(b"\x00" * 32)

    def get_transaction_count(self, address: str, block: int | None) -> int:
        return 0

    def is_connected(self) -> bool:
        return True

    def close(self) -> None:
        pass


def make_fake_curve_provider(call_responses: dict[bytes, Any]) -> ProviderAdapter:
    """Create a ProviderAdapter backed by a FakeCurveBackend.

    Usage:
        provider = make_fake_curve_provider({
            COINS_UINT256_SELECTOR: lambda to, data, block: ...,
            BALANCES_UINT256_SELECTOR: ...,
        })
        result = discover_coins(io=SyncPoolIO(provider), pool_address, block_identifier=18_000_000)
    """
    backend = FakeCurveBackend(call_responses)
    # Bypass factory methods — inject the backend directly
    adapter = ProviderAdapter.__new__(ProviderAdapter)
    adapter._backend = backend
    adapter._provider_type = "alloy"  # not "web3" — we're testing the abstracted path
    adapter._raw_provider = None
    return adapter


def make_fake_pool_io(call_responses: dict[bytes, Any]) -> SyncPoolIO:
    """Create a SyncPoolIO backed by a FakeCurveBackend.

    Thin wrapper around make_fake_curve_provider that returns a SyncPoolIO
    instead of a bare ProviderAdapter. Preferred for detection module tests
    that now accept io: PoolIO instead of provider: ProviderAdapter.

    Usage:
        io = make_fake_pool_io({
            COINS_UINT256_SELECTOR: lambda to, data, block: ...,
            BALANCES_UINT256_SELECTOR: ...,
        })
        result = discover_coins(io, pool_address, block_identifier=18_000_000)
    """
    return SyncPoolIO(make_fake_curve_provider(call_responses))
