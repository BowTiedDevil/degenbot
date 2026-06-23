"""TypedDict definitions for Ethereum RPC response data returned by the Rust extension provider.

Field types correspond to the Rust converters in ``rust/src/py_converters.rs``. All hash/data fields
use ``HexBytes``, all addresses are EIP-55 checksummed strings, and all numeric values are Python
ints.

Key naming: Block/transaction dicts use snake_case (produced by the typed Rust converters). Log
dicts use camelCase (matching the web3.py convention, as produced by ``log_to_py_dict``).
"""

from typing import TypedDict

from hexbytes import HexBytes


class _AccessListEntry(TypedDict, total=True):
    address: str
    storageKeys: list[HexBytes]


class _AuthorizationEntry(TypedDict, total=True):
    chain_id: int
    address: str
    nonce: int
    r: HexBytes
    s: HexBytes
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
        "hash": HexBytes,
        "type": int,
        "from": str,
        "block_number": int | None,
        "block_hash": HexBytes | None,
        "transaction_index": int | None,
        "effective_gas_price": int | None,
        "block_timestamp": int | None,
        "chain_id": int | None,
        "nonce": int,
        "gas_price": int,
        "gas": int,
        "to": str | None,
        "value": int,
        "input": HexBytes,
        "max_fee_per_gas": int,
        "max_priority_fee_per_gas": int,
        "access_list": list[_AccessListEntry],
        "max_fee_per_blob_gas": int,
        "blob_versioned_hashes": list[HexBytes],
        "r": HexBytes,
        "s": HexBytes,
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

    hash: HexBytes
    parent_hash: HexBytes
    sha3_uncles: HexBytes
    miner: str
    state_root: HexBytes
    transactions_root: HexBytes
    receipts_root: HexBytes
    logs_bloom: HexBytes
    difficulty: int
    number: int
    gas_limit: int
    gas_used: int
    timestamp: int
    extra_data: HexBytes
    mix_hash: HexBytes
    nonce: HexBytes
    base_fee_per_gas: int | None
    withdrawals_root: HexBytes | None
    blob_gas_used: int | None
    excess_blob_gas: int | None
    parent_beacon_block_root: HexBytes | None
    requests_hash: HexBytes | None
    block_access_list_hash: HexBytes | None
    slot_number: int | None
    total_difficulty: int | None
    size: int | None
    uncles: list[HexBytes]
    transactions: list[HexBytes] | list[TransactionData] | None
    withdrawals: list[_WithdrawalData] | None


# TransactionReceiptData also includes 'from', so the functional
# TypedDict form is used.
TransactionReceiptData = TypedDict(
    "TransactionReceiptData",
    {
        "transaction_hash": HexBytes,
        "block_hash": HexBytes | None,
        "block_number": int | None,
        "transaction_index": int | None,
        "from": str,
        "to": str | None,
        "cumulative_gas_used": int,
        "effective_gas_price": int,
        "gas_used": int,
        "contract_address": str | None,
        "logs": list["LogData"],
        "logs_bloom": HexBytes,
        "root": HexBytes,
        "status": int,
        "transaction_type": int,
    },
    total=False,
)


class LogData(TypedDict, total=False):
    """Log entry data returned by ``get_logs()`` and embedded in receipts.

    Uses camelCase keys matching the web3.py convention.
    """

    address: str
    topics: list[HexBytes]
    data: HexBytes
    blockNumber: int | None
    blockHash: HexBytes | None
    transactionHash: HexBytes | None
    logIndex: int | None
