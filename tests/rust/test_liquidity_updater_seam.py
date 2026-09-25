"""Python-side parity test for the V3/V4 DB-aware liquidity updater seam (QJSCA5 §4.3).

Loads the §4.2 fixture DBs (committed by
`rust/crates/foundation/degenbot-db/tests/fixtures/generate_liquidity_updater_parity.py`),
applies the SAME event sequence through the Python `updater/pool_updater_configs.py` apply shells
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
import sqlite3
from typing import TYPE_CHECKING

import pytest

from degenbot.abi import encode as abi_encode
from degenbot.checksum_cache import get_checksum_address
from degenbot.db import db_fetch_exchange, db_upsert_pool_manager
from degenbot.types.rpc_types import LogReceipt
from degenbot.updater.pool_updater_configs import (
    UNISWAP_V3_BURN_EVENT_HASH,
    UNISWAP_V3_MINT_EVENT_HASH,
    apply_v3_liquidity_updates,
    apply_v4_liquidity_updates,
)
from degenbot.utils.bytes import to_bytes
from tests.helpers.database import sqlite_connection

if TYPE_CHECKING:
    from degenbot._ffi import ChecksummedAddress

FIXTURE_DIR = pathlib.Path(__file__).resolve().parents[2] / (
    "rust/crates/foundation/degenbot-db/tests/fixtures"
)

# Fixture constants (must mirror `generate_liquidity_updater_parity.py`).
CHAIN = 1
V3_POOL_ADDRESS: ChecksummedAddress = get_checksum_address("0x" + "a" * 40)
V4_POOL_MANAGER_ADDRESS: ChecksummedAddress = get_checksum_address("0x" + "b" * 40)
V4_POOL_HASH = "0x" + "c" * 64
V4_MODIFY_TOPIC = to_bytes("0xf208f4912782fd25c7f114ca3723a2d5dd6f3bcc3ac8db5af63baa85f711d5ec")


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
                to_bytes(b"\x00" * 12 + V3_POOL_ADDRESS.encode()),
                to_bytes(abi_encode(["int24"], [tick_lower])),
                to_bytes(abi_encode(["int24"], [tick_upper])),
            ],
            "data": to_bytes(
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
                to_bytes(b"\x00" * 12 + V3_POOL_ADDRESS.encode()),
                to_bytes(abi_encode(["int24"], [tick_lower])),
                to_bytes(abi_encode(["int24"], [tick_upper])),
            ],
            "data": to_bytes(
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
                to_bytes(b"\x00" * 12 + V4_POOL_MANAGER_ADDRESS.encode()),
                to_bytes(b"\x00" * 12 + V4_POOL_HASH[2:].encode()),
            ],
            "data": to_bytes(
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


def _checkpoint(path: pathlib.Path) -> None:
    connection = sqlite3.connect(path)
    try:
        connection.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        connection.commit()
    finally:
        connection.close()


def _dump_v3(db_path: pathlib.Path, pool_id: int) -> dict[str, object]:
    with sqlite_connection(db_path) as connection:
        positions = connection.execute(
            "SELECT tick, liquidity_net, liquidity_gross FROM liquidity_positions "
            "WHERE pool_id = ? ORDER BY tick",
            (pool_id,),
        ).fetchall()
        init_maps = connection.execute(
            "SELECT word, bitmap FROM initialization_maps WHERE pool_id = ? ORDER BY word",
            (pool_id,),
        ).fetchall()
        pool = connection.execute(
            "SELECT liquidity_update_block, liquidity_update_log_index "
            "FROM uniswap_v3_pools WHERE pool_id = ?",
            (pool_id,),
        ).fetchone()
        assert pool is not None
        return {
            "positions": [
                {
                    "tick": row[0],
                    "liquidity_net": str(row[1]),
                    "liquidity_gross": str(row[2]),
                }
                for row in positions
            ],
            "initialization_maps": [{"word": row[0], "bitmap": str(row[1])} for row in init_maps],
            "liquidity_update_block": pool[0],
            "liquidity_update_log_index": pool[1],
        }


def _dump_v4(db_path: pathlib.Path, managed_pool_id: int) -> dict[str, object]:
    with sqlite_connection(db_path) as connection:
        positions = connection.execute(
            "SELECT tick, liquidity_net, liquidity_gross "
            "FROM managed_pool_liquidity_positions WHERE managed_pool_id = ? ORDER BY tick",
            (managed_pool_id,),
        ).fetchall()
        init_maps = connection.execute(
            "SELECT word, bitmap FROM managed_pool_initialization_maps "
            "WHERE managed_pool_id = ? ORDER BY word",
            (managed_pool_id,),
        ).fetchall()
        pool = connection.execute(
            "SELECT liquidity_update_block, liquidity_update_log_index "
            "FROM uniswap_v4_pools WHERE managed_pool_id = ?",
            (managed_pool_id,),
        ).fetchone()
        assert pool is not None
        return {
            "positions": [
                {
                    "tick": row[0],
                    "liquidity_net": str(row[1]),
                    "liquidity_gross": str(row[2]),
                }
                for row in positions
            ],
            "initialization_maps": [{"word": row[0], "bitmap": str(row[1])} for row in init_maps],
            "liquidity_update_block": pool[0],
            "liquidity_update_log_index": pool[1],
        }


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
    with sqlite_connection(v3_apply_copy) as connection:
        pool = connection.execute(
            "SELECT p.id, p.exchange_id FROM pools p WHERE p.address = ? AND p.chain = ?",
            (V3_POOL_ADDRESS, CHAIN),
        ).fetchone()
        assert pool is not None
        pool_id = pool[0]
        exchange = db_fetch_exchange(
            database_path=str(v3_apply_copy),
            exchange_id=pool[1],
        )
        assert exchange is not None
        exchanges_in_scope = {exchange}

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
    with sqlite_connection(v4_apply_copy) as connection:
        manager = connection.execute(
            "SELECT address, chain, kind, state_view, exchange_id "
            "FROM pool_managers WHERE chain = ?",
            (CHAIN,),
        ).fetchone()
        assert manager is not None
        pool = connection.execute(
            "SELECT managed_pool_id FROM uniswap_v4_pools WHERE pool_hash = ?",
            (V4_POOL_HASH,),
        ).fetchone()
        assert pool is not None
        managed_pool_id = pool[0]
        manager_row = db_upsert_pool_manager(
            database_path=str(v4_apply_copy),
            address=manager[0],
            chain=manager[1],
            kind=manager[2],
            state_view=manager[3],
            exchange_id=manager[4],
        )

    apply_v4_liquidity_updates(
        pool_id=to_bytes(V4_POOL_HASH),
        liquidity_events=events,
        pool_manager=manager_row,
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
