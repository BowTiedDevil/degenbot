"""Tuple-ABI record path: canonical selectors, real answers.

A tuple-typed ABI input must yield the canonical selector. A bare ``tuple``
in the signature hashes to a selector no contract serves, and the encoder
rejects the malformed type — so the record path captures the exception as a
synthetic ``reverted`` golden entry instead of the on-chain answer.
"""

from __future__ import annotations

import json
from typing import TYPE_CHECKING, Any

from degenbot.abi import encode as abi_encode
from degenbot.crypto import keccak256
from tests.golden.oracle import GoldenOracle
from tests.helpers.contract_compat import ContractCompat

if TYPE_CHECKING:
    from pathlib import Path

# The Uniswap V4 quoter's single-pool quote: one dynamic tuple argument whose
# members span a nested static tuple plus dynamic bytes, and a
# ``(uint256, uint256)`` answer — the tuple-ABI record shape.
_QUOTE_ABI: list[dict[str, Any]] = [
    {
        "inputs": [
            {
                "components": [
                    {
                        "components": [
                            {"name": "currency0", "type": "address"},
                            {"name": "currency1", "type": "address"},
                            {"name": "fee", "type": "uint24"},
                            {"name": "tickSpacing", "type": "int24"},
                            {"name": "hooks", "type": "address"},
                        ],
                        "internalType": "struct PoolKey",
                        "name": "poolKey",
                        "type": "tuple",
                    },
                    {"name": "zeroForOne", "type": "bool"},
                    {"name": "amountSpecified", "type": "uint128"},
                    {"name": "hookData", "type": "bytes"},
                ],
                "internalType": "struct QuoteExactSingleParams",
                "name": "params",
                "type": "tuple",
            }
        ],
        "name": "quoteExactInputSingle",
        "outputs": [
            {"name": "amountOut", "type": "uint256"},
            {"name": "gasEstimate", "type": "uint256"},
        ],
        "stateMutability": "nonpayable",
        "type": "function",
    },
]

_ERC20_TRANSFER_ABI: list[dict[str, Any]] = [
    {
        "inputs": [
            {"name": "to", "type": "address"},
            {"name": "amount", "type": "uint256"},
        ],
        "name": "transfer",
        "outputs": [{"name": "", "type": "bool"}],
        "stateMutability": "nonpayable",
        "type": "function",
    },
]

_CANONICAL_SIGNATURE = (
    "quoteExactInputSingle(((address,address,uint24,int24,address),bool,uint128,bytes))"
)

_QUOTER_ADDRESS = "0x52F0E24D1c21C8A0cB1e5a5dD6198556BD9E1203"
_USDC_ADDRESS = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
_CURRENCY0 = "0x0000000000000000000000000000000000000000"

_AMOUNT_OUT = 123_456_789
_GAS_ESTIMATE = 47_110


class _CannedAnswerProvider:
    """Serves one canned answer — but only for the expected selector.

    A node dispatches on the four-byte selector: a request carrying one no
    contract recognizes cannot return this answer. Enforcing that here (not in
    the test) is what makes the record capture below prove the selector rather
    than merely the absence of an exception.
    """

    def __init__(self, address: str, expected_selector: bytes, answer: bytes) -> None:
        self._address = address
        self._expected_selector = expected_selector
        self._answer = answer
        self.selectors_seen: list[bytes] = []

    def call(self, address: str, calldata: bytes, block_identifier: int | None = None) -> bytes:
        self.selectors_seen.append(calldata[:4])
        if address != self._address or calldata[:4] != self._expected_selector:
            msg = f"no function with selector {calldata[:4].hex()} at {address}"
            raise LookupError(msg)
        return self._answer


def _quote_contract(provider: _CannedAnswerProvider) -> ContractCompat:
    return ContractCompat(_QUOTER_ADDRESS, _QUOTE_ABI, provider)


def test_tuple_abi_record_case_records_the_real_answer(tmp_path: Path) -> None:
    """A tuple-ABI record capture stores the real answer, not a revert entry.

    The golden oracle is driven exactly as a record-mode parity test drives it:
    the deferred callable raises or returns, and whatever happened is what the
    golden file must hold.
    """
    canonical_selector = keccak256(_CANONICAL_SIGNATURE.encode())[:4]
    provider = _CannedAnswerProvider(
        _QUOTER_ADDRESS,
        canonical_selector,
        abi_encode(types=["uint256", "uint256"], args=[_AMOUNT_OUT, _GAS_ESTIMATE]),
    )
    oracle = GoldenOracle(path=tmp_path / "quote.json", chain_id=1, block_number=1, mode="record")
    contract = _quote_contract(provider)

    def _quote() -> tuple[int, ...]:
        return contract.functions.quoteExactInputSingle(
            ((_CURRENCY0, _USDC_ADDRESS, 500, 10, _CURRENCY0), True, 12_345, b""),
        ).call()

    result = oracle.check("quote", contract=_quote)

    assert provider.selectors_seen == [canonical_selector]
    assert not result.reverted, f"recorded {result.exception_type}: {result.message}"
    assert result.value == (_AMOUNT_OUT, _GAS_ESTIMATE)

    entry = json.loads((tmp_path / "quote.json").read_text())["entries"]["quote"]
    assert entry == {"value": [_AMOUNT_OUT, _GAS_ESTIMATE]}


def test_plain_abi_selector_path_unchanged(tmp_path: Path) -> None:
    """Non-tuple signatures keep the canonical ``transfer`` selector."""
    provider = _CannedAnswerProvider(
        _USDC_ADDRESS,
        bytes.fromhex("a9059cbb"),
        abi_encode(types=["bool"], args=[True]),
    )
    oracle = GoldenOracle(
        path=tmp_path / "transfer.json", chain_id=1, block_number=1, mode="record"
    )
    contract = ContractCompat(_USDC_ADDRESS, _ERC20_TRANSFER_ABI, provider)
    result = oracle.check(
        "transfer", contract=lambda: contract.functions.transfer(_CURRENCY0, 1).call()
    )

    assert not result.reverted, f"recorded {result.exception_type}: {result.message}"
    assert result.value is True
