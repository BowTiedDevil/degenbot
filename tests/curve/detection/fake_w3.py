"""Fake Web3 instances for Curve pool detection tests.

Provides a configurable fake that returns pre-programmed responses to
w3.eth.call() based on the method selector in the calldata.
"""

from typing import Any

from eth_typing import ChecksumAddress
from hexbytes import HexBytes


def _encode_address(addr: str) -> bytes:
    """ABI-encode an address for use as a call return value."""
    import eth_abi.abi

    return eth_abi.abi.encode(["address"], [addr])


def _encode_uint256(value: int) -> bytes:
    """ABI-encode a uint256 for use as a call return value."""
    import eth_abi.abi

    return eth_abi.abi.encode(["uint256"], [value])


def _encode_uint8(value: int) -> bytes:
    """ABI-encode a uint8 for use as a call return value."""
    import eth_abi.abi

    return eth_abi.abi.encode(["uint8"], [value])


def _encode_bool(value: bool) -> bytes:
    """ABI-encode a bool for use as a call return value."""
    import eth_abi.abi

    return eth_abi.abi.encode(["bool"], [value])


class FakeCurveW3Eth:
    """Fake web3.eth namespace that dispatches eth.call() by selector.

    Responses are configured via the ``call_responses`` dict passed to
    FakeCurveW3. The key is the 4-byte selector; the value is either:
    - A callable(to, data, block_identifier) -> bytes
    - A bytes value returned directly
    """

    def __init__(self, call_responses: dict[bytes, Any]) -> None:
        self._call_responses = call_responses

    def call(
        self,
        tx: dict[str, Any],
        block_identifier: int | None = None,
    ) -> HexBytes:
        data = bytes(tx["data"])
        selector = data[:4]
        handler = self._call_responses.get(selector)
        if handler is None:
            raise Exception(f"No handler for selector {selector.hex()}")
        if callable(handler):
            return HexBytes(handler(tx.get("to", ""), data, block_identifier))
        return HexBytes(handler)


class FakeCurveW3:
    """Fake Web3 instance for Curve detection tests.

    Usage:
        w3 = FakeCurveW3({
            COINS_UINT256_SELECTOR: lambda to, data, block: ...,
            BALANCES_UINT256_SELECTOR: ...,
        })
        result = discover_coins(w3, pool_address, block_identifier=18_000_000)
    """

    def __init__(self, call_responses: dict[bytes, Any]) -> None:
        self.eth = FakeCurveW3Eth(call_responses)
