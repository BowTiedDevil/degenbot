"""Offline §4.2 parity tests for the submission signing PyO3 seam.

Drives the full ``PyTxSigner`` / ``PyTxParams`` pyclasses + the
``finalize_fees`` pyfunction through the Rust core signing path and asserts
**byte-exact parity vs `eth_account.Account.sign_transaction`** — the Python
oracle (`examples/eth_backrun_v2_v3_v4_rust.py` L2623–L2640).

Because both ``eth_account`` and the Rust ``alloy-signer-local`` use RFC 6979
deterministic ECDSA over secp256k1, a pinned key + ``tx_params`` produces
byte-for-byte identical raw signed bytes in Rust and Python. This is the §4.2
HARD gate.
"""

from __future__ import annotations

import eth_account
from eth_account import Account

from degenbot._ffi.submission import finalize_fees as rs_finalize_fees
from degenbot._ffi.submission import PyTxParams, PyTxSigner

# Anvil account 0 — canonical well-known test private key + address.
ANVIL_KEY_HEX = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
ANVIL_ADDR = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"

# The exact raw signed bytes eth_account produces for this key + tx_params
# (captured once, pinned here — the Rust seam must reproduce them exactly).
ETH_ACCOUNT_RAW_HEX = (
    "02f870010784773594008506fc23ac008303d09094"
    "000000000000000000000000000000000000000080"
    "8412345678c001a0e4d0045433dd450a5f3f754b622551da4ccad4626e10e044d140960f39b37ad8a0"
    "724c6ade60f7cae109ccff66ec35e7387977d0bd2e0e94cad3f5809e966ca3f4"
)

# Oracle fee fields: maxFeePerGas=30 gwei, maxPriorityFeePerGas=2 gwei.
# finalize_fees(params, base_fee_next, priority) sets
#   maxFeePerGas = int(1.5 * base_fee_next) + priority.
# To land exactly on 30 gwei with priority=2 gwei, int(1.5 * base_fee_next)
# must equal 28 gwei. base_fee_next=19_000_000_000 (19 gwei) →
# 1.5*19 = 28.5 → int() = 28 (gwei) → +2 = 30 (gwei). Exact.
ORACLE_MAX_FEE = 30_000_000_000
ORACLE_PRIORITY_FEE = 2_000_000_000
BASE_FEE_NEXT_FOR_30_GWEI = 18_666_666_667

ZERO_ADDR = "0x0000000000000000000000000000000000000000"


# ---------------------------------------------------------------------------
# Construction + address derivation
# ---------------------------------------------------------------------------


def test_py_tx_signer_address_from_hex() -> None:
    signer = PyTxSigner(ANVIL_KEY_HEX, 1)
    assert signer.address.lower() == ANVIL_ADDR.lower()
    assert signer.chain_id == 1


def test_py_tx_signer_address_from_bytes() -> None:
    key_bytes = bytes.fromhex(ANVIL_KEY_HEX.removeprefix("0x"))
    signer = PyTxSigner(key_bytes, 1)
    assert signer.address.lower() == ANVIL_ADDR.lower()


def test_py_tx_signer_address_from_hex_without_prefix() -> None:
    signer = PyTxSigner(ANVIL_KEY_HEX.removeprefix("0x"), 1)
    assert signer.address.lower() == ANVIL_ADDR.lower()


def test_py_tx_signer_rejects_bad_key() -> None:
    import pytest

    zero_key = (0).to_bytes(32, "big")
    with pytest.raises(ValueError):
        PyTxSigner(zero_key, 1)
    with pytest.raises(ValueError):
        PyTxSigner("nothex", 1)
    with pytest.raises(ValueError):
        PyTxSigner("0xdeadbeef", 1)  # too short


# ---------------------------------------------------------------------------
# §4.2 byte-exact parity vs eth_account
# ---------------------------------------------------------------------------


def test_signed_bytes_match_eth_account_oracle_byte_for_byte() -> None:
    """PyTxSigner.sign_eip1559 must produce eth_account's exact raw bytes.

    Pinned fixture: anvil key 0, chain 1, nonce 7, gas 250_000,
    maxFeePerGas 30 gwei, maxPriorityFeePerGas 2 gwei, to=zero, value=0,
    data=[0x12,0x34,0x56,0x78], empty access list. The fees are finalized via
    finalization (base_fee_next=18_666_666_667 → 28 gwei headroom + 2 gwei =
    30 gwei) so the test exercises the full finalize_fees + sign path, not
    just signing.
    """
    signer = PyTxSigner(ANVIL_KEY_HEX, 1)
    params = PyTxParams(
        to=ZERO_ADDR,
        data=bytes([0x12, 0x34, 0x56, 0x78]),
        gas_limit=250_000,
        nonce=7,
        value=0,
        access_list=None,
    )
    rs_finalize_fees(params, BASE_FEE_NEXT_FOR_30_GWEI, ORACLE_PRIORITY_FEE)
    # Verify the finalized fees landed exactly on the oracle's values.
    assert params.max_fee_per_gas == ORACLE_MAX_FEE
    assert params.max_priority_fee_per_gas == ORACLE_PRIORITY_FEE

    raw = signer.sign_eip1559(params)
    assert raw.hex() == ETH_ACCOUNT_RAW_HEX


# ---------------------------------------------------------------------------
# Round-trip: Rust signs → Rust recovers → matches signer address
# ---------------------------------------------------------------------------


def test_signed_tx_recovers_to_signer_address() -> None:
    signer = PyTxSigner(ANVIL_KEY_HEX, 1)
    params = PyTxParams(
        to=ZERO_ADDR,
        data=bytes([0xDE, 0xAD, 0xBE, 0xEF]),
        gas_limit=100_000,
        nonce=42,
        value=0,
        access_list=None,
    )
    rs_finalize_fees(params, 10_000_000_000, 1_000_000_000)
    raw = signer.sign_eip1559(params)
    assert raw[0] == 0x02  # type-2 EIP-1559 prefix
    recovered = PyTxSigner.recover_sender(bytes(raw))
    assert recovered.lower() == signer.address.lower()


# ---------------------------------------------------------------------------
# Fee-math parity: Rust finalize_fees vs Python int(1.5 * bf) + pf
# ---------------------------------------------------------------------------


def test_finalize_fees_matches_python_int_truncate() -> None:
    """Rust finalize_fees must match Python's int(1.5 * bf) + pf exactly."""
    for base_fee_next, priority_fee in [
        (10_000_000_000, 2_000_000_000),  # 15+2=17 gwei
        (7, 1),  # int(10.5)+1 = 10+1 = 11
        (1, 0),  # int(1.5)+0 = 1+0 = 1
        (0, 5),  # 0+5
    ]:
        params = PyTxParams(
            to=ZERO_ADDR,
            data=b"\x01",
            gas_limit=21_000,
            nonce=0,
            value=0,
            access_list=None,
        )
        rs_finalize_fees(params, base_fee_next, priority_fee)
        expected_max = int(1.5 * base_fee_next) + priority_fee
        assert params.max_fee_per_gas == expected_max, (
            f"base={base_fee_next}, prio={priority_fee}: "
            f"Rust={params.max_fee_per_gas}, Python={expected_max}"
        )
        assert params.max_priority_fee_per_gas == priority_fee


# ---------------------------------------------------------------------------
# Chain-id replay protection: same key + params, different chain → different
# bytes, same recovered address
# ---------------------------------------------------------------------------


def test_chain_id_replay_protection() -> None:
    signer_mainnet = PyTxSigner(ANVIL_KEY_HEX, 1)
    signer_sepolia = PyTxSigner(ANVIL_KEY_HEX, 11_155_111)

    params = PyTxParams(
        to=ZERO_ADDR,
        data=bytes([0x01]),
        gas_limit=210_000,
        nonce=1,
        value=0,
        access_list=None,
    )
    rs_finalize_fees(params, 10_000_000_000, 1_000_000_000)

    raw_mainnet = signer_mainnet.sign_eip1559(params)
    raw_sepolia = signer_sepolia.sign_eip1559(params)
    assert raw_mainnet != raw_sepolia
    # Same key → same recovered address
    assert (
        PyTxSigner.recover_sender(bytes(raw_mainnet)).lower()
        == PyTxSigner.recover_sender(bytes(raw_sepolia)).lower()
    )


# ---------------------------------------------------------------------------
# Sanity: eth_account still produces the pinned oracle bytes (version drift
# guard — if eth_account changes its RFC 6979 nonce, the fixture breaks here
# rather than silently passing the Rust test)
# ---------------------------------------------------------------------------


def test_eth_account_oracle_still_produces_pinned_bytes() -> None:
    tx_params = {
        "chainId": 1,
        "nonce": 7,
        "gas": 250_000,
        "maxFeePerGas": ORACLE_MAX_FEE,
        "maxPriorityFeePerGas": ORACLE_PRIORITY_FEE,
        "to": ZERO_ADDR,
        "value": 0,
        "data": bytes([0x12, 0x34, 0x56, 0x78]),
        "type": "0x2",
        "accessList": [],
    }
    signed = Account.sign_transaction(transaction_dict=tx_params, private_key=ANVIL_KEY_HEX)
    assert signed.raw_transaction.hex() == ETH_ACCOUNT_RAW_HEX
    assert eth_account.__version__  # import surface check
