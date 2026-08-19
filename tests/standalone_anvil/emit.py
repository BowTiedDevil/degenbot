"""Real-transaction emitter for the standalone-anvil test tier (web3-free).

Anvil's default dev accounts are unlocked server-side, so a plain
``eth_sendTransaction`` works: the anvil node signs, and the Python side sends
no keys. The tests/rust provider suites need one real, mined transaction (a
Ping log) before they can assert on real block/log/tx shapes, so they all
funnel through ``emit_tx``.
"""

from __future__ import annotations

import time
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from degenbot.utils.bytes import to_0x_hex

if TYPE_CHECKING:
    from degenbot.provider import AlloyProvider


@dataclass
class EmittedTx:
    """A real emitted transaction: its block number + hash (for shape assertions)."""

    block: int
    tx_hash: str


def _to_int(value: Any) -> int:
    """Normalize a provider quantity result (int or 0x-str) to int."""
    if isinstance(value, int):
        return value
    return int(_as_0x_str(value), 16)


def _as_0x_str(value: Any) -> str:
    """Normalize a provider hex result (bytes or 0x-str) to a 0x-prefixed str."""
    if isinstance(value, bytes):
        return "0x" + value.hex()
    s = str(value)
    return s if s.startswith("0x") else "0x" + s


def emit_tx(
    provider: AlloyProvider,
    *,
    to: str,
    data: bytes,
    chain_id: int,
    coinbase: str | None = None,
    timeout: float = 10.0,
) -> EmittedTx:
    """Send one real, mined transaction (``data`` to ``to``) and wait.

    Sends from the first account in ``eth_accounts`` (unlocked server-side, so
    the node signs the transaction). ``coinbase``, when given, is set first
    via ``anvil_setCoinbase``.
    """
    if coinbase is not None:
        provider.make_request("anvil_setCoinbase", [coinbase])
    sender = _as_0x_str(provider.make_request("eth_accounts", [])[0])
    txh = _as_0x_str(
        provider.make_request(
            "eth_sendTransaction",
            [{"from": sender, "to": to, "data": to_0x_hex(data), "chainId": chain_id}],
        )
    )
    deadline = time.monotonic() + timeout
    while True:
        receipt = provider.get_transaction_receipt(txh)
        if receipt is not None:
            return EmittedTx(block=_to_int(receipt["blockNumber"]), tx_hash=txh[2:])
        if time.monotonic() > deadline:
            msg = f"tx not mined within {timeout:.0f}s: {txh}"
            raise TimeoutError(msg)
        time.sleep(0.2)
