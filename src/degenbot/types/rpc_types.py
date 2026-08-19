"""TypedDict definitions for Ethereum RPC response data returned by the Rust extension provider.

Field types correspond to the Rust converters in ``rust/src/py_converters.rs``. All hash/data fields
use ``bytes``, all addresses are EIP-55 checksummed strings, and all numeric values are Python
ints.

Key naming: Block/transaction dicts use snake_case (produced by the typed Rust converters). Log
dicts use camelCase (matching the web3.py convention, as produced by ``log_to_py_dict``).

C1 (YMDOZL): the ``web3.types`` imports (``BlockIdentifier``, ``BlockData``, ``FilterParams``,
``LogReceipt``, ``RPCEndpoint``, ``RPCResponse``, ``TxParams``) are reproduced here natively
so the codebase carries zero web3 type imports (no ``web3.types.*`` anywhere). The shapes are deliberately
loose (``total=False`` TypedDicts with the fields actually produced/consumed) so they survive the
full web3py retirement epic (Pass C) without type-check churn.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, TypedDict

from degenbot.types.chain import HexStr

if TYPE_CHECKING:
    from degenbot._ffi import ChecksummedAddress

# ── RPC primitive aliases (C1 — replaces ``web3.types`` primitives) ───────

#: Block identifier: a number, hex hash, or one of the canonical tags.
#: Matches every call site (``block_identifier: BlockIdentifier | None = None``);
#: web3's ``Union[int, Literal["earliest", "latest", "pending"], Hash32]`` is.
#: subsumed by the ``str`` arm.
BlockIdentifier = int | str


#: Transaction params for ``eth_call`` / ``eth_sendTransaction`` — was ``web3.types.TxParams``.
#:
#: The degenbot call sites construct ``{"to": addr, "data": bytes}`` and
#: sometimes ``{"to": addr, "data": bytes, "from": addr}``; the full param
#: set (gas/value/nonce/etc.) lives in ``TransactionData`` for typed receipts.
#: Uses the functional ``TypedDict`` form because ``from`` is a Python keyword
#: (same pattern as ``TransactionData`` below).
TxParams = TypedDict(
    "TxParams",
    {
        "from": str,
        "to": str,
        "gas": int,
        "gasPrice": int,
        "maxFeePerGas": int,
        "maxPriorityFeePerGas": int,
        "value": int,
        "data": bytes | HexStr,
        "nonce": int,
        "chainId": int,
        "accessList": list[Any],
        "input": bytes | HexStr,
        "type": int,
    },
    total=False,
)


class _AccessListEntry(TypedDict, total=True):
    address: str
    storageKeys: list[bytes]


class _AuthorizationEntry(TypedDict, total=True):
    chain_id: int
    address: str
    nonce: int
    r: bytes
    s: bytes
    v: int


class _WithdrawalData(TypedDict, total=True):
    index: int
    validator_index: int
    address: str
    amount: int


# TransactionData includes 'from' (a Python keyword), so the functional
# TypedDict form is used. All fields are optional at the type level
# because the set of present fields varies by transaction type
# (legacy, EIP-2930, EIP-1559, EIP-4844, EIP-7702). In practice,
# 'hash', 'type', 'from', 'nonce', 'gas', 'value', 'input', 'r',
# 's', and 'v' are always present.
TransactionData = TypedDict(
    "TransactionData",
    {
        "hash": bytes,
        "type": int,
        "from": str,
        "block_number": int | None,
        "block_hash": bytes | None,
        "transaction_index": int | None,
        "effective_gas_price": int | None,
        "block_timestamp": int | None,
        "chain_id": int | None,
        "nonce": int,
        "gas_price": int,
        "gas": int,
        "to": str | None,
        "value": int,
        "input": bytes,
        "max_fee_per_gas": int,
        "max_priority_fee_per_gas": int,
        "access_list": list[_AccessListEntry],
        "max_fee_per_blob_gas": int,
        "blob_versioned_hashes": list[bytes],
        "r": bytes,
        "s": bytes,
        "v": int,
        "y_parity": bool,
        "authorization_list": list[_AuthorizationEntry],
    },
    total=False,
)


class BlockData(TypedDict, total=False):
    """Full block data returned by ``get_block()``.

    All fields are optional at the type level because the set of
    present fields varies by network and hardfork. In practice,
    ``hash``, ``parent_hash``, ``sha3_uncles``, ``miner``,
    ``state_root``, ``transactions_root``, ``receipts_root``,
    ``logs_bloom``, ``difficulty``, ``number``, ``gas_limit``,
    ``gas_used``, ``timestamp``, ``extra_data``, ``mix_hash``, and
    ``nonce`` are always present for post-merge blocks.
    """

    hash: bytes
    parent_hash: bytes
    sha3_uncles: bytes
    miner: str
    state_root: bytes
    transactions_root: bytes
    receipts_root: bytes
    logs_bloom: bytes
    difficulty: int
    number: int
    gas_limit: int
    gas_used: int
    timestamp: int
    extra_data: bytes
    mix_hash: bytes
    nonce: bytes
    base_fee_per_gas: int | None
    withdrawals_root: bytes | None
    blob_gas_used: int | None
    excess_blob_gas: int | None
    parent_beacon_block_root: bytes | None
    requests_hash: bytes | None
    block_access_list_hash: bytes | None
    slot_number: int | None
    total_difficulty: int | None
    size: int | None
    uncles: list[bytes]
    transactions: list[bytes] | list[TransactionData] | None
    withdrawals: list[_WithdrawalData] | None


# TransactionReceiptData also includes 'from', so the functional
# TypedDict form is used.
TransactionReceiptData = TypedDict(
    "TransactionReceiptData",
    {
        "transaction_hash": bytes,
        "block_hash": bytes | None,
        "block_number": int | None,
        "transaction_index": int | None,
        "from": str,
        "to": str | None,
        "cumulative_gas_used": int,
        "effective_gas_price": int,
        "gas_used": int,
        "contract_address": str | None,
        "logs": list["LogData"],
        "logs_bloom": bytes,
        "root": bytes,
        "status": int,
        "transaction_type": int,
    },
    total=False,
)


class LogData(TypedDict, total=False):
    """Loose log entry data — ``eth_getLogs`` / subscription stream entries.

    Some fields may be ``None`` for pending (un-mined) entries. Uses
    camelCase keys matching the web3.py convention.
    """

    address: str
    topics: list[bytes]
    data: bytes
    blockNumber: int | None
    blockHash: bytes | None
    transactionHash: bytes | None
    transactionIndex: int | None
    logIndex: int | None
    removed: bool


class LogReceipt(TypedDict, total=True):
    """Confirmed-log entry shape — was ``web3.types.LogReceipt``.

    Strict TypedDict (all fields required) for log entries consumed from
    confirmed blocks: consumers expect plain ``int`` (not ``int | None``),
    matching web3's exact shape without the looseness the pending-aware
    ``LogData`` carries.
    """

    address: ChecksummedAddress
    blockHash: bytes
    blockNumber: int
    data: bytes
    logIndex: int
    removed: bool
    topics: list[bytes]
    transactionHash: bytes
    transactionIndex: int
