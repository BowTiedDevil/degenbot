"""Python-side parity test for the V3/V4 DB-aware liquidity updater seam (QJSCA5 §4.3).

Loads the §4.2 fixture DBs (committed by
`rust/crates/degenbot-db/tests/fixtures/generate_liquidity_updater_parity.py`),
applies the SAME event sequence through the Python `cli/pool.py` apply shells
(now delegating to the Rust seam), + asserts the resulting
`liquidity_positions` / `initialization_maps` rows + the
`liquidity_update_block`/`log_index` marker match the committed
`*_expected.json` oracle.

This validates the seam wiring end-to-end from Python: the `LogReceipt` decode
(V3 Burn negation, V4 Modify signed delta) → `LiquidityUpdateEvent` records →
`db_apply_v3/v4_liquidity_updates` → Rust reconstitute→apply→persist→stamp.
The §4.2 Rust parity test already proves Rust-apply == old-Python-apply
(byte-identical); this test proves the Python shell decode + delegation reach
the same Rust result.
"""

from __future__ import annotations

import json
import pathlib
import shutil
from typing import TYPE_CHECKING

import pytest
from eth_abi import encode as abi_encode
from hexbytes import HexBytes
from sqlalchemy import select
from web3.types import LogReceipt

from degenbot.checksum_cache import get_checksum_address
from degenbot.cli.pool import (
    UNISWAP_V3_BURN_EVENT_HASH,
    UNISWAP_V3_MINT_EVENT_HASH,
    apply_v3_liquidity_updates,
    apply_v4_liquidity_updates,
)
from degenbot.database.models.base import ExchangeTable
from degenbot.database.models.pools import (
    InitializationMapTable,
    LiquidityPositionTable,
    ManagedPoolInitializationMapTable,
    ManagedPoolLiquidityPositionTable,
    PoolManagerTable,
    UniswapV3PoolTable,
    UniswapV4PoolTable,
)
from degenbot.database.operations import get_scoped_sqlite_session

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress
    from sqlalchemy.orm import Session, scoped_session

FIXTURE_DIR = pathlib.Path(__file__).resolve().parents[2] / (
    "rust/crates/degenbot-db/tests/fixtures"
)

# Fixture constants (must mirror `generate_liquidity_updater_parity.py`).
CHAIN = 1
V3_POOL_ADDRESS: ChecksumAddress = get_checksum_address("0x" + "a" * 40)
V4_POOL_MANAGER_ADDRESS: ChecksumAddress = get_checksum_address("0x" + "b" * 40)
V4_POOL_HASH = "0x" + "c" * 64
V4_MODIFY_TOPIC = HexBytes("0xf208f4912782fd25c7f114ca3723a2d5dd6f3bcc3ac8db5af63baa85f711d5ec")


def _v3_mint_log(
    block: int,
    log_idx: int,
    tick_lower: int,
    tick_upper: int,
    amount: int,
) -> LogReceipt:
    return LogReceipt(  # type: ignore[typeddict-item]
        {
            "blockNumber": block,
            "logIndex": log_idx,
            "address": V3_POOL_ADDRESS,
            "topics": [
                UNISWAP_V3_MINT_EVENT_HASH,
                HexBytes(b"\x00" * 12 + V3_POOL_ADDRESS.encode()),
                HexBytes(abi_encode(["int24"], [tick_lower])),
                HexBytes(abi_encode(["int24"], [tick_upper])),
            ],
            "data": HexBytes(
                abi_encode(
                    ["address", "uint128", "uint256", "uint256"],
                    ["0x" + "1" * 40, amount, 0, 0],
                )
            ),
        }
    )


def _v3_burn_log(
    block: int,
    log_idx: int,
    tick_lower: int,
    tick_upper: int,
    amount: int,
) -> LogReceipt:
    return LogReceipt(  # type: ignore[typeddict-item]
        {
            "blockNumber": block,
            "logIndex": log_idx,
            "address": V3_POOL_ADDRESS,
            "topics": [
                UNISWAP_V3_BURN_EVENT_HASH,
                HexBytes(b"\x00" * 12 + V3_POOL_ADDRESS.encode()),
                HexBytes(abi_encode(["int24"], [tick_lower])),
                HexBytes(abi_encode(["int24"], [tick_upper])),
            ],
            "data": HexBytes(
                abi_encode(
                    ["uint128", "uint256", "uint256"],
                    [amount, 0, 0],
                )
            ),
        }
    )


def _v4_modify_log(
    block: int,
    log_idx: int,
    tick_lower: int,
    tick_upper: int,
    delta: int,
) -> LogReceipt:
    return LogReceipt(  # type: ignore[typeddict-item]
        {
            "blockNumber": block,
            "logIndex": log_idx,
            "address": V4_POOL_MANAGER_ADDRESS,
            "topics": [
                V4_MODIFY_TOPIC,
                HexBytes(b"\x00" * 12 + V4_POOL_MANAGER_ADDRESS.encode()),
                HexBytes(b"\x00" * 12 + V4_POOL_HASH[2:].encode()),
            ],
            "data": HexBytes(
                abi_encode(
                    ["int24", "int24", "int256", "bytes32"],
                    [tick_lower, tick_upper, delta, b"\x00" * 32],
                )
            ),
        }
    )


class _DummyProvider:
    """Minimal provider shim carrying just the `chain_id` the V3 apply reads."""

    chain_id = CHAIN


def _dispose(session: scoped_session[Session]) -> None:
    """Dispose the scoped session + its engine (avoids leaking a pooled
    sqlite connection — `get_scoped_sqlite_session` builds a fresh engine per
    call)."""
    bind = session.get_bind()
    # `get_bind()` may return an `Engine` or `Connection`; only dispose an engine
    # (the SQLAlchemy engine pool caches the pooled sqlite connection).
    if hasattr(bind, "dispose"):
        bind.dispose()  # type: ignore[union-attr]
    session.remove()


def _checkpoint(path: pathlib.Path) -> None:
    import sqlite3

    conn = sqlite3.connect(str(path))
    try:
        conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        conn.commit()
    finally:
        conn.close()


def _dump_v3(db_path: pathlib.Path, pool_id: int) -> dict[str, object]:
    session = get_scoped_sqlite_session(db_path)
    with session() as s:  # type: Session
        positions = sorted(
            s.scalars(
                select(LiquidityPositionTable).where(LiquidityPositionTable.pool_id == pool_id)
            ).all(),
            key=lambda r: r.tick,
        )
        init_maps = sorted(
            s.scalars(
                select(InitializationMapTable).where(InitializationMapTable.pool_id == pool_id)
            ).all(),
            key=lambda r: r.word,
        )
        pool = s.scalar(select(UniswapV3PoolTable).where(UniswapV3PoolTable.id == pool_id))
        assert pool is not None
        result = {
            "positions": [
                {
                    "tick": r.tick,
                    "liquidity_net": str(r.liquidity_net),
                    "liquidity_gross": str(r.liquidity_gross),
                }
                for r in positions
            ],
            "initialization_maps": [{"word": r.word, "bitmap": str(r.bitmap)} for r in init_maps],
            "liquidity_update_block": pool.liquidity_update_block,
            "liquidity_update_log_index": pool.liquidity_update_log_index,
        }
    _dispose(session)
    return result


def _dump_v4(db_path: pathlib.Path, managed_pool_id: int) -> dict[str, object]:
    session = get_scoped_sqlite_session(db_path)
    with session() as s:  # type: Session
        positions = sorted(
            s.scalars(
                select(ManagedPoolLiquidityPositionTable).where(
                    ManagedPoolLiquidityPositionTable.managed_pool_id == managed_pool_id
                )
            ).all(),
            key=lambda r: r.tick,
        )
        init_maps = sorted(
            s.scalars(
                select(ManagedPoolInitializationMapTable).where(
                    ManagedPoolInitializationMapTable.managed_pool_id == managed_pool_id
                )
            ).all(),
            key=lambda r: r.word,
        )
        pool = s.scalar(
            select(UniswapV4PoolTable).where(UniswapV4PoolTable.managed_pool_id == managed_pool_id)
        )
        assert pool is not None
        result = {
            "positions": [
                {
                    "tick": r.tick,
                    "liquidity_net": str(r.liquidity_net),
                    "liquidity_gross": str(r.liquidity_gross),
                }
                for r in positions
            ],
            "initialization_maps": [{"word": r.word, "bitmap": str(r.bitmap)} for r in init_maps],
            "liquidity_update_block": pool.liquidity_update_block,
            "liquidity_update_log_index": pool.liquidity_update_log_index,
        }
    _dispose(session)
    return result
    _dispose(session)
    return result


@pytest.fixture
def v3_apply_copy(tmp_path: pathlib.Path) -> pathlib.Path:
    src = FIXTURE_DIR / "liquidity_updater_v3_initial.db"
    dst = tmp_path / "v3_apply.db"
    shutil.copy2(src, dst)
    dst.chmod(0o644)
    return dst


@pytest.fixture
def v4_apply_copy(tmp_path: pathlib.Path) -> pathlib.Path:
    src = FIXTURE_DIR / "liquidity_updater_v4_initial.db"
    dst = tmp_path / "v4_apply.db"
    shutil.copy2(src, dst)
    dst.chmod(0o644)
    return dst


def test_apply_v3_seam_matches_expected_oracle(v3_apply_copy: pathlib.Path) -> None:
    """Apply the §4.2 V3 event sequence via the Python shell → matches the JSON oracle."""
    events = [
        _v3_mint_log(block=100, log_idx=0, tick_lower=-10, tick_upper=10, amount=500_000),
        _v3_mint_log(block=100, log_idx=1, tick_lower=100, tick_upper=110, amount=250_000),
        _v3_burn_log(block=101, log_idx=0, tick_lower=-10, tick_upper=10, amount=200_000),
        _v3_burn_log(block=102, log_idx=0, tick_lower=-10, tick_upper=10, amount=1_300_000),
    ]
    # Read the pool id + the in-scope exchange (the shell uses both).
    session = get_scoped_sqlite_session(v3_apply_copy)
    with session() as s:
        pool = s.scalar(
            select(UniswapV3PoolTable).where(
                UniswapV3PoolTable.address == V3_POOL_ADDRESS,
                UniswapV3PoolTable.chain == CHAIN,
            )
        )
        assert pool is not None
        pool_id = pool.id
        exchange = s.get(ExchangeTable, pool.exchange_id)
        assert exchange is not None
        exchanges_in_scope = {exchange}
    _dispose(session)

    apply_v3_liquidity_updates(
        provider=_DummyProvider(),  # type: ignore[arg-type]
        pool_address=V3_POOL_ADDRESS,
        liquidity_events=events,
        exchanges_in_scope=exchanges_in_scope,
        database_path=str(v3_apply_copy),
    )
    _checkpoint(v3_apply_copy)

    expected = json.loads((FIXTURE_DIR / "liquidity_updater_v3_expected.json").read_text())
    actual = _dump_v3(v3_apply_copy, pool_id)
    assert actual == {
        "positions": expected["positions"],
        "initialization_maps": expected["initialization_maps"],
        "liquidity_update_block": expected["liquidity_update_block"],
        "liquidity_update_log_index": expected["liquidity_update_log_index"],
    }


def test_apply_v4_seam_matches_expected_oracle(v4_apply_copy: pathlib.Path) -> None:
    """Apply the §4.2 V4 event sequence via the Python shell → matches the JSON oracle."""
    events = [
        _v4_modify_log(block=200, log_idx=0, tick_lower=-10, tick_upper=10, delta=500_000),
        _v4_modify_log(block=200, log_idx=1, tick_lower=100, tick_upper=110, delta=250_000),
        _v4_modify_log(block=201, log_idx=0, tick_lower=-10, tick_upper=10, delta=-200_000),
        _v4_modify_log(block=202, log_idx=0, tick_lower=-10, tick_upper=10, delta=-1_300_000),
    ]
    session = get_scoped_sqlite_session(v4_apply_copy)
    with session() as s:
        manager = s.scalar(select(PoolManagerTable).where(PoolManagerTable.chain == CHAIN))
        assert manager is not None
        pool = s.scalar(
            select(UniswapV4PoolTable).where(UniswapV4PoolTable.pool_hash == V4_POOL_HASH)
        )
        assert pool is not None
        managed_pool_id = pool.managed_pool_id
    _dispose(session)

    apply_v4_liquidity_updates(
        pool_id=HexBytes(V4_POOL_HASH),
        liquidity_events=events,
        pool_manager=manager,
        database_path=str(v4_apply_copy),
    )
    _checkpoint(v4_apply_copy)

    expected = json.loads((FIXTURE_DIR / "liquidity_updater_v4_expected.json").read_text())
    actual = _dump_v4(v4_apply_copy, managed_pool_id)
    assert actual == {
        "positions": expected["positions"],
        "initialization_maps": expected["initialization_maps"],
        "liquidity_update_block": expected["liquidity_update_block"],
        "liquidity_update_log_index": expected["liquidity_update_log_index"],
    }
