"""Rust-path market-activation tests (MPI6Q3).

Exercises the PyO3 seam `degenbot_rs.activate_aave_market` /
`deactivate_aave_market` against the mock JSON-RPC harness — verifies the
Rust-owned activation path (getMarketId RPC + GHO metadata fetch + the
`aave_v3_markets` / `POOL_ADDRESS_PROVIDER` contract / GHO `erc20_tokens` /
`aave_gho_tokens` row seeds) end-to-end, plus idempotent re-activation +
deactivation. No live node (canned eth_call responses via the harness's
mock server).
"""

from __future__ import annotations

import tempfile
from pathlib import Path
from typing import Any

from sqlalchemy import create_engine, text
from sqlalchemy.orm import Session

from degenbot.degenbot_rs import (
    activate_aave_market,
    db_upgrade_database,
    deactivate_aave_market,
)
from tests.aave.writer_parity.harness import mock_rpc_server

# The 4-byte selectors the activation path RPCs (keccak256 of the canonical
# signatures, first 4 bytes).
GET_MARKET_ID_SELECTOR = "0x568ef470"
NAME_SELECTOR = "0x06fdde03"
SYMBOL_SELECTOR = "0x95d89b41"
DECIMALS_SELECTOR = "0x313ce567"

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
        DECIMALS_SELECTOR: (18).to_bytes(32, "big").hex(),  # uint256, 0x-prefixed below
    }


def _fresh_db() -> Path:
    tmp = Path(tempfile.mkdtemp(prefix="activate-"))
    db_path = tmp / "act.db"
    db_upgrade_database(str(db_path))
    return db_path


def _dump_market(db_path: Path) -> dict[str, Any]:
    engine = create_engine(f"sqlite:///{db_path}")
    with Session(engine) as session:
        row = session.execute(
            text(
                "SELECT id, chain_id, name, active, last_update_block "
                "FROM aave_v3_markets"
            )
        ).one()
        out = dict(row._mapping)
    engine.dispose()
    return out


def _count_rows(db_path: Path, table_and_where: str) -> int:
    engine = create_engine(f"sqlite:///{db_path}")
    with Session(engine) as session:
        n = session.execute(text(f"SELECT COUNT(*) FROM {table_and_where}")).scalar()
    engine.dispose()
    return int(n)


def test_activate_seeds_market_contract_gho_token() -> None:
    """activate_aave_market seeds all 4 row types in one transaction."""
    db_path = _fresh_db()
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
    engine = create_engine(f"sqlite:///{db_path}")
    with Session(engine) as session:
        sym, dec = session.execute(
            text(
                "SELECT symbol, decimals FROM erc20_tokens "
                f"WHERE chain = 1 AND address = '{GHO_TOKEN_ADDRESS}'"
            )
        ).one()
    engine.dispose()
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


def test_activate_reactivation_is_idempotent() -> None:
    """Re-activating an existing market flips active=true + inserts no dup rows."""
    db_path = _fresh_db()
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


def test_deactivate_sets_active_false() -> None:
    """deactivate_aave_market flips active to 0."""
    db_path = _fresh_db()
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