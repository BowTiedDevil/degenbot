"""Rust-path market-activation tests.

Exercises the PyO3 seam `degenbot._ffi.activate_aave_market` /
`deactivate_aave_market` against the mock JSON-RPC harness — verifies the
Rust-owned activation path (getMarketId RPC + GHO metadata fetch + the
`aave_v3_markets` / `POOL_ADDRESS_PROVIDER` contract / GHO `erc20_tokens` /
`aave_gho_tokens` row seeds) end-to-end, plus idempotent re-activation +
deactivation. No live node (canned eth_call responses via the harness's
mock server).
"""

from __future__ import annotations

import sqlite3
from pathlib import Path
from typing import Any

import pytest

from degenbot.crypto import function_selector
from degenbot.db import db_upgrade_database
from tests.aave.writer_parity.harness import mock_rpc_server
from tests.helpers.database import sqlite_connection
from tests.helpers.seam import activate_aave_market, deactivate_aave_market

# The 4-byte selectors the activation path RPCs, derived from the canonical
# signatures so a transposed digit cannot survive review.
GET_MARKET_ID_SELECTOR = "0x" + function_selector("getMarketId()").hex()
GET_MARKET_ID_UPPER_SELECTOR = "0x" + function_selector("GETMARKETID()").hex()
NAME_SELECTOR = "0x" + function_selector("name()").hex()
SYMBOL_SELECTOR = "0x" + function_selector("symbol()").hex()
DECIMALS_SELECTOR = "0x" + function_selector("decimals()").hex()
DECIMALS_UPPER_SELECTOR = "0x" + function_selector("DECIMALS()").hex()

# Canonical Aave V3 Ethereum mainnet addresses (check the real deployments
# module).
POOL_ADDRESS_PROVIDER_ADDRESS = "0x2f39d218133AFaB8F2B819B1066c7E434Ad94E9e"
GHO_TOKEN_ADDRESS = "0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f"


def _encode_dynamic_string(s: str) -> str:
    """ABI-encode a `string` return: offset(0x20) + length + data (padded to
    32-byte multiple). Mirrors `decode_dynamic_string`'s input shape."""
    data = s.encode("utf-8")
    length = len(data)
    padded = data + b"\x00" * ((32 - length % 32) % 32)
    return (
        "0x"
        + (32).to_bytes(32, "big").hex()  # offset = 0x20
        + length.to_bytes(32, "big").hex()
        + padded.hex()
    )


def _activate_responses(market_name: str = "Aave Ethereum Market") -> dict[str, str]:
    """The canned eth_call responses the activation path needs."""
    return {
        GET_MARKET_ID_SELECTOR: _encode_dynamic_string(market_name),
        NAME_SELECTOR: _encode_dynamic_string("GHO Token"),
        SYMBOL_SELECTOR: _encode_dynamic_string("GHO"),
        DECIMALS_SELECTOR: "0x" + (18).to_bytes(32, "big").hex(),
    }


def _fresh_db(tmp_path: Path) -> Path:
    db_path = tmp_path / "act.db"
    db_upgrade_database(str(db_path))
    return db_path


def _dump_market(db_path: Path) -> dict[str, Any]:
    with sqlite_connection(db_path) as connection:
        connection.row_factory = sqlite3.Row
        row = connection.execute(
            "SELECT id, chain_id, name, active, last_update_block FROM aave_v3_markets"
        ).fetchone()
        assert row is not None
        return dict(row)


def _count_rows(db_path: Path, table_and_where: str) -> int:
    with sqlite_connection(db_path) as connection:
        row = connection.execute(f"SELECT COUNT(*) FROM {table_and_where}").fetchone()
        assert row is not None
        return int(row[0])


def test_activate_seeds_market_contract_gho_token(tmp_path: Path) -> None:
    """activate_aave_market seeds all 4 row types in one transaction."""
    db_path = _fresh_db(tmp_path)
    responses = _activate_responses()
    with mock_rpc_server(logs=[], block_number=100, eth_call_responses=responses) as rpc_url:
        result = activate_aave_market(
            database_path=str(db_path),
            chain_id=1,
            pool_address_provider=POOL_ADDRESS_PROVIDER_ADDRESS,
            gho_token_address=GHO_TOKEN_ADDRESS,
            rpc_url=rpc_url,
        )

    assert result["market_name"] == "Aave Ethereum Market", result
    assert result["created"] is True, result
    market_id = result["market_id"]
    assert market_id > 0, result

    market = _dump_market(db_path)
    assert market["active"] == 1, market
    assert market["name"] == "Aave Ethereum Market", market
    # The bootstrap stamp (the parent of the PoolAddressProvider deploy block).
    assert market["last_update_block"] == 16_291_070, market

    # POOL_ADDRESS_PROVIDER contract row.
    assert (
        _count_rows(
            db_path,
            f"aave_v3_contracts WHERE market_id = {market_id} "
            f"AND name = 'POOL_ADDRESS_PROVIDER' AND address = "
            f"'{POOL_ADDRESS_PROVIDER_ADDRESS}'",
        )
        == 1
    )

    # GHO erc20 row with metadata.
    with sqlite_connection(db_path) as connection:
        row = connection.execute(
            "SELECT symbol, decimals FROM erc20_tokens "
            f"WHERE chain = 1 AND address = '{GHO_TOKEN_ADDRESS}'"
        ).fetchone()
        assert row is not None
        sym, dec = row
    assert sym == "GHO", sym
    assert dec == 18, dec

    # aave_gho_tokens row referencing the GHO token.
    assert (
        _count_rows(
            db_path,
            "aave_gho_tokens g JOIN erc20_tokens e ON e.id = g.token_id "
            f"WHERE e.chain = 1 AND e.address = '{GHO_TOKEN_ADDRESS}'",
        )
        == 1
    )


def test_activate_reactivation_is_idempotent(tmp_path: Path) -> None:
    """Re-activating an existing market flips active=true + inserts no dup rows."""
    db_path = _fresh_db(tmp_path)
    responses = _activate_responses()
    with mock_rpc_server(logs=[], block_number=100, eth_call_responses=responses) as rpc_url:
        first = activate_aave_market(
            database_path=str(db_path),
            chain_id=1,
            pool_address_provider=POOL_ADDRESS_PROVIDER_ADDRESS,
            gho_token_address=GHO_TOKEN_ADDRESS,
            rpc_url=rpc_url,
        )

    # Deactivate (via the Rust seam).
    deactivate_aave_market(database_path=str(db_path), market_id=first["market_id"])
    market = _dump_market(db_path)
    assert market["active"] == 0, market

    # Re-activate: same market_id, no duplicate rows.
    with mock_rpc_server(logs=[], block_number=100, eth_call_responses=responses) as rpc_url:
        second = activate_aave_market(
            database_path=str(db_path),
            chain_id=1,
            pool_address_provider=POOL_ADDRESS_PROVIDER_ADDRESS,
            gho_token_address=GHO_TOKEN_ADDRESS,
            rpc_url=rpc_url,
        )
    assert second["market_id"] == first["market_id"], (first, second)
    assert second["created"] is False, second

    market = _dump_market(db_path)
    assert market["active"] == 1, market

    # No duplicate contract / gho rows.
    assert (
        _count_rows(
            db_path,
            f"aave_v3_contracts WHERE market_id = {first['market_id']} "
            "AND name = 'POOL_ADDRESS_PROVIDER'",
        )
        == 1
    )
    assert (
        _count_rows(
            db_path,
            "aave_gho_tokens g JOIN erc20_tokens e ON e.id = g.token_id "
            f"WHERE e.chain = 1 AND e.address = '{GHO_TOKEN_ADDRESS}'",
        )
        == 1
    )


def test_deactivate_sets_active_false(tmp_path: Path) -> None:
    """deactivate_aave_market flips active to 0."""
    db_path = _fresh_db(tmp_path)
    responses = _activate_responses()
    with mock_rpc_server(logs=[], block_number=100, eth_call_responses=responses) as rpc_url:
        result = activate_aave_market(
            database_path=str(db_path),
            chain_id=1,
            pool_address_provider=POOL_ADDRESS_PROVIDER_ADDRESS,
            gho_token_address=GHO_TOKEN_ADDRESS,
            rpc_url=rpc_url,
        )
    assert result["created"] is True, result

    deactivate_aave_market(database_path=str(db_path), market_id=result["market_id"])
    market = _dump_market(db_path)
    assert market["active"] == 0, market


def _assert_activation_refused(
    db_path: Path, responses: dict[str, str], code: dict[str, str]
) -> str:
    """Drive an activation that must refuse; return its message.

    The mock serves every read the activation needs except the one under test, so
    a refusal can only come from the evidence that read carried.
    """
    with (
        mock_rpc_server(
            logs=[], block_number=100, eth_call_responses=responses, code=code
        ) as rpc_url,
        pytest.raises(ValueError, match="activation metadata") as refused,
    ):
        activate_aave_market(
            database_path=str(db_path),
            chain_id=1,
            pool_address_provider=POOL_ADDRESS_PROVIDER_ADDRESS,
            gho_token_address=GHO_TOKEN_ADDRESS,
            rpc_url=rpc_url,
        )
    with sqlite_connection(db_path) as connection:
        markets = connection.execute("SELECT COUNT(*) FROM aave_v3_markets").fetchone()[0]
        gho_tokens = connection.execute("SELECT COUNT(*) FROM aave_gho_tokens").fetchone()[0]
    assert markets == 0, "a refused activation writes no market row"
    assert gho_tokens == 0, "a refused activation writes no GHO row"
    return str(refused.value)


def test_activate_refuses_a_market_id_that_carries_no_string(tmp_path: Path) -> None:
    """A malformed `getMarketId()` answer is a refusal, not an uppercase retry.

    The uppercase spelling is canned and would succeed, so the assertion is that
    the refusal happens WITHOUT spending it: only an execution revert says the
    contract lacks the method.
    """
    db_path = _fresh_db(tmp_path)
    responses = _activate_responses()
    responses[GET_MARKET_ID_SELECTOR] = "0x0102"
    # The uppercase spelling ANSWERS, so a fallback that should never happen would
    # turn this refusal into a successful activation.
    responses[GET_MARKET_ID_UPPER_SELECTOR] = _encode_dynamic_string("Aave Ethereum Market")

    message = _assert_activation_refused(db_path, responses, code={})

    assert "market id" in message, message


def test_activate_refuses_a_gho_token_without_decimals(tmp_path: Path) -> None:
    """Decimals are required: an unanswered `decimals()` never persists NULL."""
    db_path = _fresh_db(tmp_path)
    responses = _activate_responses()
    # The harness answers ERC-20 metadata underneath every fixture, so the
    # unanswered spellings are stated as empty returns rather than deleted keys.
    responses[DECIMALS_SELECTOR] = "0x"
    responses[DECIMALS_UPPER_SELECTOR] = "0x"

    message = _assert_activation_refused(db_path, responses, code={})

    assert "decimals" in message, message


def test_activate_refuses_a_gho_address_holding_no_code(tmp_path: Path) -> None:
    """A code-less GHO address is evidence, not an absent identity fact."""
    db_path = _fresh_db(tmp_path)

    message = _assert_activation_refused(
        db_path, _activate_responses(), code={GHO_TOKEN_ADDRESS: "0x"}
    )

    assert "no code" in message, message
