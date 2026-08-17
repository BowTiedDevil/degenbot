"""Parity + functional tests for the `BotIo` construction-time DB seam (QVMWQC).

`BotIo.fetch_erc20_token` / `update_erc20_token_metadata` route the erc20
builder's construction-time DB read + write-back through `degenbot-db` (the
`database_path`-per-call pattern, matching `db_apply_v3_liquidity_updates`).
These tests drive the seam against a temp DB and assert §4.2 parity vs the
prior SQLAlchemy `session.scalar(select(Erc20TokenTable)...)` /
`session.commit()` path:

- `fetch_erc20_token` returns an `Erc20TokenRow` whose attributes match the ORM
  row (id/address/chain/name/symbol/decimals), including the `None`-metadata
  trajectory that triggers the builder's write-back.
- `update_erc20_token_metadata` lands an `UPDATE` whose result is observable by
  a subsequent SQLAlchemy read (Rust write ≡ ORM commit).
- the no-`database_path` skip returns `None` / no-ops (mirrors the prior
  `contextlib.suppress(Exception)` skip when the `Bot` has no DB).
"""

from __future__ import annotations

from sqlalchemy import create_engine, select
from sqlalchemy.orm import Session

from degenbot._ffi.provider import AlloyProvider as RustAlloyProvider
from degenbot._ffi import BotIo
from degenbot.checksum_cache import get_checksum_address
from degenbot.database.models import Base
from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.db import db_create_new_database

CHAIN = 1
# Production stores addresses checksummed (the builder checksums before insert);
# the Rust seam queries with `to_checksum(None)`, so the test seed must match.
ADDR = get_checksum_address("0x" + "ab" * 20)

# A trivial real provider for the DB seam. After B1, `BotIo` extracts a
# native `Arc<AlloyProvider>` when the provider is `PyAlloyProvider`-backed;
# the DB methods never touch the provider, but a valid provider keeps the seam
# honest (no Python `object()` double — see O3).
_MINIMAL_OFFLINE_JSON = '{"chain_id":1,"block_number":1,"timestamp":1,"calls":{},"code":{}}'


def _offline_provider() -> RustAlloyProvider:
    """A one-block `OfflineProvider`-backed `AlloyProvider` (no RPC)."""
    return RustAlloyProvider.offline_from_json_string(_MINIMAL_OFFLINE_JSON)


def _seed_token_row(database_path: str) -> int:
    """Insert an erc20_tokens row with NULL metadata; return its id."""
    engine = create_engine(f"sqlite:///{database_path}")
    Base.metadata.create_all(engine)
    with Session(engine) as session:
        token = Erc20TokenTable(chain=CHAIN, address=ADDR)
        session.add(token)
        session.commit()
        session.refresh(token)
        token_id = token.id
    engine.dispose()
    return token_id


def _read_token_row(database_path: str) -> Erc20TokenTable:
    """Re-read the erc20_tokens row via SQLAlchemy (the §4.2 oracle)."""
    engine = create_engine(f"sqlite:///{database_path}")
    try:
        with Session(engine) as session:
            row = session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.address == ADDR,
                    Erc20TokenTable.chain == CHAIN,
                )
            )
            assert row is not None
            # Detach so attribute reads survive session close.
            session.expunge(row)
            return row
    finally:
        engine.dispose()


def test_fetch_erc20_token_parity(tmp_path):
    """`BotIo.fetch_erc20_token` returns a row matching the ORM read."""
    database_path = str(tmp_path / "erc20_seam.db")
    db_create_new_database(database_path)
    expected_id = _seed_token_row(database_path)

    io = BotIo(provider=_offline_provider(), database_path=database_path)
    row = io.fetch_erc20_token(chain_id=CHAIN, address=ADDR)

    assert row is not None
    assert row.id == expected_id
    assert row.chain == CHAIN
    # The checksummed address must round-trip the stored (lowercase) address.
    assert row.address.lower() == ADDR.lower()
    # NULL metadata (the trajectory that triggers the builder write-back).
    assert row.name is None
    assert row.symbol is None
    assert row.decimals is None

    # Parity: the SQLAlchemy oracle reads the same row.
    oracle = _read_token_row(database_path)
    assert oracle.id == row.id
    assert oracle.chain == row.chain
    assert oracle.name is row.name is None
    assert oracle.symbol is row.symbol is None
    assert oracle.decimals is row.decimals is None


def test_fetch_erc20_token_missing_row_returns_none(tmp_path):
    """A non-existent (chain, address) returns None (mirrors `get_token_from_database`)."""
    database_path = str(tmp_path / "erc20_seam_missing.db")
    db_create_new_database(database_path)

    io = BotIo(provider=_offline_provider(), database_path=database_path)
    assert io.fetch_erc20_token(chain_id=CHAIN, address="0x" + "00" * 20) is None


def test_update_erc20_token_metadata_lands_update(tmp_path):
    """`update_erc20_token_metadata` lands an UPDATE observable by SQLAlchemy."""
    database_path = str(tmp_path / "erc20_seam_writeback.db")
    db_create_new_database(database_path)
    _seed_token_row(database_path)

    io = BotIo(provider=_offline_provider(), database_path=database_path)
    # The builder's write-back: row existed with NULL metadata, now populated.
    result = io.update_erc20_token_metadata(
        chain_id=CHAIN, address=ADDR, name="Test", symbol="TST", decimals=6
    )
    assert result is None  # the Rust seam returns None (mirrors commit() → None)

    # Parity: a SQLAlchemy re-read observes the UPDATE.
    oracle = _read_token_row(database_path)
    assert oracle.name == "Test"
    assert oracle.symbol == "TST"
    assert oracle.decimals == 6


def test_no_database_path_skips_db_read():
    """No `database_path` → `fetch_erc20_token` returns None (the skip path)."""
    io = BotIo(provider=_offline_provider(), database_path=None)
    assert io.database_path is None
    assert io.fetch_erc20_token(chain_id=CHAIN, address=ADDR) is None
    # And the write-back is a no-op.
    assert io.update_erc20_token_metadata(CHAIN, ADDR, "x", "y", 1) is None
