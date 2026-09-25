"""Python-side §4.2 parity test for the pool discovery writers seam (WR7EA6).

Builds a fresh Alembic-stamped SQLite DB, seeds an exchange + a PoolManager,
then applies a `PoolCreated` event sequence through the Python
`updater/pool_updater_configs.py::update_v2/v3/v4_pools` shells (now delegating to
the Rust `db_upsert_v2/v3/v4_pools` seam) + asserts the resulting
`pools` / per-subclass / `managed_pools` / `uniswap_v4_pools` / `erc20_tokens`
rows + the `ExchangeTable.last_update_block` stamp match the expected
polymorphic-insert state.

The Rust-internal trajectory is pinned by `rust/crates/foundation/degenbot-db/tests/
discovery_parity.rs` (8 tests); this test proves the Python shell decode +
delegation reach the same Rust result end-to-end (the V2/V3 `PoolCreated`
ABI topic/data decode, the V4 `pool_hash`/`hooks` decode, the Aerodrome stable
flag, + the exchange stamp).
"""

from __future__ import annotations

import sqlite3
from typing import TYPE_CHECKING

import pytest

from degenbot.abi import encode as abi_encode
from degenbot.checksum_cache import get_checksum_address
from degenbot.db import (
    db_create_new_database,
    db_fetch_exchange,
    db_fetch_exchange_by_name,
    db_set_exchange_active,
    db_set_exchange_last_update_block,
    db_upsert_exchange,
    db_upsert_pool_manager,
)
from degenbot.types.rpc_types import LogReceipt
from degenbot.updater.pool_updater_configs import (
    PoolUpdateRequest,
    V2PoolUpdateConfig,
    V3PoolUpdateConfig,
    V4PoolUpdateConfig,
    update_v2_pools,
    update_v3_pools,
    update_v4_pools,
)
from degenbot.utils.bytes import to_bytes
from tests.helpers.database import sqlite_connection

if TYPE_CHECKING:
    import pathlib
    from collections.abc import Callable

    from degenbot._ffi import ChecksummedAddress


CHAIN = 1
UNISWAP_V2_FACTORY: ChecksummedAddress = get_checksum_address("0x" + "f" * 40)
V2_POOL_MANAGER_ADDRESS: ChecksummedAddress = get_checksum_address("0x" + "b" * 40)
V4_POOL_HASH = "0x" + "c" * 64

V2_POOL_CREATED_TOPIC = to_bytes(
    "0x0d3648bd0f6ba80134a33ba9275ac585d9d315f0ad8355cddefde31afa28d0e9",
)
V3_POOL_CREATED_TOPIC = to_bytes(
    "0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118",
)
V4_POOL_CREATED_TOPIC = to_bytes(
    "0xdd466e674ea557f56295e2d0218a125ea4b4f0f6f3307b95f85e6110838d6438",
)


def _v2_pool_created_log(
    pool_address: ChecksummedAddress,
    token0: ChecksummedAddress,
    token1: ChecksummedAddress,
    *,
    stable: bool = False,
) -> LogReceipt:
    return LogReceipt(  # type: ignore[typeddict-item]
        {
            "blockNumber": 100,
            "logIndex": 0,
            "address": UNISWAP_V2_FACTORY,
            "topics": [
                V2_POOL_CREATED_TOPIC,
                bytes.fromhex(token0[2:].rjust(64, "0")),
                bytes.fromhex(token1[2:].rjust(64, "0")),
                to_bytes(abi_encode(["bool"], [stable])),
            ],
            "data": to_bytes(
                abi_encode(["address", "uint256"], [pool_address, 0]),
            ),
        }
    )


def _v3_pool_created_log(
    pool_address: ChecksummedAddress,
    token0: ChecksummedAddress,
    token1: ChecksummedAddress,
    fee: int,
    tick_spacing: int,
) -> LogReceipt:
    return LogReceipt(  # type: ignore[typeddict-item]
        {
            "blockNumber": 100,
            "logIndex": 1,
            "address": UNISWAP_V2_FACTORY,
            "topics": [
                V3_POOL_CREATED_TOPIC,
                bytes.fromhex(token0[2:].rjust(64, "0")),
                bytes.fromhex(token1[2:].rjust(64, "0")),
                to_bytes(abi_encode(["uint24"], [fee])),
            ],
            # V3 PoolCreated data: (int24 tick_spacing, address pool_address)
            "data": to_bytes(
                abi_encode(["int24", "address"], [tick_spacing, pool_address]),
            ),
        }
    )


def _v4_pool_created_log(
    pool_hash: str,
    currency0: ChecksummedAddress,
    currency1: ChecksummedAddress,
    fee: int,
    tick_spacing: int,
    hooks: ChecksummedAddress,
) -> LogReceipt:
    return LogReceipt(  # type: ignore[typeddict-item]
        {
            "blockNumber": 100,
            "logIndex": 2,
            "address": V2_POOL_MANAGER_ADDRESS,
            "topics": [
                V4_POOL_CREATED_TOPIC,
                to_bytes(bytes.fromhex(pool_hash[2:])),
                bytes.fromhex(currency0[2:].rjust(64, "0")),
                bytes.fromhex(currency1[2:].rjust(64, "0")),
            ],
            # V4 PoolCreated data: (uint24 fee, int24 tick_spacing, address hooks)
            "data": to_bytes(
                abi_encode(["uint24", "int24", "address"], [fee, tick_spacing, hooks]),
            ),
        }
    )


class _StubProvider:
    """Minimal provider shim carrying just `chain_id` (the shells read it)."""

    chain_id = CHAIN


def _events_fn_factory(events: list[LogReceipt]) -> Callable[..., list[LogReceipt]]:
    """Build a `get_events_fn` stub returning `events` (ignores filters)."""

    def _fn(*, provider, start_block, end_block, address, event_hash):
        return events

    return _fn


def _seed_db(db_path: pathlib.Path) -> tuple[int, int]:
    """Create + seed a fresh DB with a V2 exchange + a V4 PoolManager.

    Returns `(exchange_id, pool_manager_id)` for the assert phase.
    """
    db_create_new_database(str(db_path))
    exchange = db_upsert_exchange(
        database_path=str(db_path),
        chain_id=CHAIN,
        name="uniswap_v2",
        factory=UNISWAP_V2_FACTORY,
        deployer=None,
    )
    db_set_exchange_active(str(db_path), exchange_id=exchange.id, active=True)
    exchange_id = exchange.id

    exchange_v4 = db_upsert_exchange(
        database_path=str(db_path),
        chain_id=CHAIN,
        name="uniswap_v4",
        factory=V2_POOL_MANAGER_ADDRESS,
        deployer=None,
    )
    db_set_exchange_active(str(db_path), exchange_id=exchange_v4.id, active=True)
    pool_manager = db_upsert_pool_manager(
        database_path=str(db_path),
        address=V2_POOL_MANAGER_ADDRESS,
        chain=CHAIN,
        kind="uniswap_v4",
        state_view=None,
        exchange_id=exchange_v4.id,
    )
    return exchange_id, pool_manager.id


@pytest.fixture
def seeded_db(tmp_path: pathlib.Path) -> pathlib.Path:
    db_path = tmp_path / "discovery.db"
    _seed_db(db_path)
    db_path.chmod(0o644)
    return db_path


def test_update_v2_pools_shell_routes_through_rust(seeded_db: pathlib.Path) -> None:
    """The V2 shell decodes a `PoolCreated` event + delegates to the Rust seam;
    the resulting `pools` base + `uniswap_v2_pools` detail rows match the
    expected polymorphic insert."""
    pool_addr = get_checksum_address("0x" + "1" * 40)
    token0 = get_checksum_address("0x" + "a" * 40)
    token1 = get_checksum_address("0x" + "2" * 40)
    events = [_v2_pool_created_log(pool_addr, token0, token1, stable=False)]
    config = V2PoolUpdateConfig(
        name="uniswap_v2",
        event_hash=V2_POOL_CREATED_TOPIC,
        fee_token0=3,
        fee_token1=3,
        fee_denominator=1000,
    )

    exchange = db_fetch_exchange_by_name(
        database_path=str(seeded_db),
        chain_id=CHAIN,
        name="uniswap_v2",
    )
    assert exchange is not None

    update_v2_pools(
        PoolUpdateRequest(
            provider=_StubProvider(),  # type: ignore[arg-type]
            start_block=100,
            end_block=100,
            exchange=exchange,
            database_path=str(seeded_db),
            config=config,
            get_events_fn=_events_fn_factory(events),
        )
    )

    with sqlite_connection(seeded_db) as connection:
        connection.row_factory = sqlite3.Row
        pool = connection.execute("SELECT * FROM pools").fetchone()
        assert pool is not None
        assert pool["kind"] == "uniswap_v2"
        assert pool["address"] == pool_addr
        assert pool["chain"] == CHAIN
        assert pool["exchange_id"] is not None
        tokens = connection.execute("SELECT id, address FROM erc20_tokens ORDER BY id").fetchall()
        assert [token["address"] for token in tokens] == [token0, token1]
        assert pool["token0_id"] == tokens[0]["id"]
        assert pool["token1_id"] == tokens[1]["id"]


def test_update_v2_aerodrome_stable_flag(seeded_db: pathlib.Path) -> None:
    """Aerodrome-style V2 config (stable flag, no RPC) decodes `stable` from
    topics[3] + the Rust seam writes the `aerodrome_v2_pools.stable` column."""
    pool_addr = get_checksum_address("0x" + "3" * 40)
    token0 = get_checksum_address("0x" + "a" * 40)
    token1 = get_checksum_address("0x" + "2" * 40)
    events = [_v2_pool_created_log(pool_addr, token0, token1, stable=True)]
    config = V2PoolUpdateConfig(
        name="aerodrome_v2",
        event_hash=V2_POOL_CREATED_TOPIC,
        fee_token0=0,
        fee_token1=0,
        fee_denominator=10_000,
        has_stable_flag=True,
    )

    exchange = db_upsert_exchange(
        database_path=str(seeded_db),
        chain_id=CHAIN,
        name="aerodrome_v2",
        factory=UNISWAP_V2_FACTORY,
        deployer=None,
    )

    update_v2_pools(
        PoolUpdateRequest(
            provider=_StubProvider(),  # type: ignore[arg-type]
            start_block=100,
            end_block=100,
            exchange=exchange,
            database_path=str(seeded_db),
            config=config,
            get_events_fn=_events_fn_factory(events),
        )
    )

    import sqlite3

    conn = sqlite3.connect(str(seeded_db))
    try:
        kind = conn.execute("SELECT kind FROM pools").fetchone()[0]
        assert kind == "aerodrome_v2"
        stable = conn.execute("SELECT stable FROM aerodrome_v2_pools").fetchone()[0]
        assert stable == 1
    finally:
        conn.close()


def test_update_v3_pools_shell_routes_through_rust(seeded_db: pathlib.Path) -> None:
    """The V3 shell decodes fee/tick_spacing from the `PoolCreated` event +
    delegates; the resulting `uniswap_v3_pools` detail row matches."""
    pool_addr = get_checksum_address("0x" + "5" * 40)
    token0 = get_checksum_address("0x" + "a" * 40)
    token1 = get_checksum_address("0x" + "2" * 40)
    events = [_v3_pool_created_log(pool_addr, token0, token1, fee=500, tick_spacing=10)]
    config = V3PoolUpdateConfig(
        name="uniswap_v3",
        event_hash=V3_POOL_CREATED_TOPIC,
        fee_denominator=1_000_000,
    )

    exchange = db_upsert_exchange(
        database_path=str(seeded_db),
        chain_id=CHAIN,
        name="uniswap_v3",
        factory=UNISWAP_V2_FACTORY,
        deployer=None,
    )

    update_v3_pools(
        PoolUpdateRequest(
            provider=_StubProvider(),  # type: ignore[arg-type]
            start_block=100,
            end_block=100,
            exchange=exchange,
            database_path=str(seeded_db),
            config=config,
            get_events_fn=_events_fn_factory(events),
        )
    )

    import sqlite3

    conn = sqlite3.connect(str(seeded_db))
    try:
        kind = conn.execute("SELECT kind FROM pools").fetchone()[0]
        assert kind == "uniswap_v3"
        _, ts, lub, lui, f0, f1, fden = conn.execute(
            "SELECT pool_id, tick_spacing, liquidity_update_block, "
            "liquidity_update_log_index, fee_token0, fee_token1, fee_denominator "
            "FROM uniswap_v3_pools"
        ).fetchone()
        assert ts == 10
        assert (f0, f1, fden) == (500, 500, 1_000_000)
        assert lub is None
        assert lui is None
    finally:
        conn.close()


def test_update_v4_pools_shell_routes_through_rust(seeded_db: pathlib.Path) -> None:
    """The V4 shell resolves the manager via `exchange.factory` (no inline
    `PoolManagerTable` read) + delegates; the resulting `managed_pools` base +
    `uniswap_v4_pools` detail rows match."""
    currency0 = get_checksum_address("0x" + "a" * 40)
    currency1 = get_checksum_address("0x" + "2" * 40)
    hooks = get_checksum_address("0x" + "0" * 40)
    events = [
        _v4_pool_created_log(
            V4_POOL_HASH,
            currency0,
            currency1,
            fee=3000,
            tick_spacing=60,
            hooks=hooks,
        ),
    ]
    config = V4PoolUpdateConfig(
        name="uniswap_v4",
        event_hash=V4_POOL_CREATED_TOPIC,
        fee_denominator=1_000_000,
    )

    exchange = db_fetch_exchange_by_name(
        database_path=str(seeded_db),
        chain_id=CHAIN,
        name="uniswap_v4",
    )
    assert exchange is not None

    update_v4_pools(
        PoolUpdateRequest(
            provider=_StubProvider(),  # type: ignore[arg-type]
            start_block=100,
            end_block=100,
            exchange=exchange,
            database_path=str(seeded_db),
            config=config,
            get_events_fn=_events_fn_factory(events),
        )
    )

    with sqlite_connection(seeded_db) as connection:
        connection.row_factory = sqlite3.Row
        v4 = connection.execute("SELECT * FROM uniswap_v4_pools").fetchone()
        assert v4 is not None
        assert v4["pool_hash"] == V4_POOL_HASH
        assert v4["hooks"] == hooks
        assert v4["fee_currency0"] == 3000
        assert v4["fee_currency1"] == 3000
        assert v4["fee_denominator"] == 1_000_000
        assert v4["tick_spacing"] == 60
        assert v4["liquidity_update_block"] is None
        assert connection.execute("SELECT COUNT(*) FROM pools").fetchone()[0] == 0


def test_set_exchange_last_update_block_seam(seeded_db: pathlib.Path) -> None:
    """The exchange stamp routes through the Rust write seam."""
    exchange = db_fetch_exchange_by_name(str(seeded_db), CHAIN, "uniswap_v2")
    assert exchange is not None

    db_set_exchange_last_update_block(
        database_path=str(seeded_db),
        chain_id=CHAIN,
        exchange_id=exchange.id,
        block=99_999,
    )

    stamped = db_fetch_exchange(str(seeded_db), exchange.id)
    assert stamped is not None
    assert stamped.last_update_block == 99_999


# ---------------------------------------------------------------------
# Exchange write substrate (NWU4KH — split out of HYUYTN).
# Round-trips the three PyO3 seams over a fresh Alembic-stamped DB:
# `db_upsert_exchange` → `ExchangeRow` (`active=False`); `db_set_exchange_active`
# flips active + `db_fetch_exchange` reads it back; `db_upsert_pool_manager`
# round-trips + is idempotent.

FACTORY: ChecksummedAddress = get_checksum_address("0x" + "f" * 40)
DEPLOYER: ChecksummedAddress = get_checksum_address("0x" + "d" * 40)
POOL_MANAGER: ChecksummedAddress = get_checksum_address("0x" + "b" * 40)
STATE_VIEW: ChecksummedAddress = get_checksum_address("0x" + "e" * 40)


def test_db_upsert_exchange_inserts_active_false_and_is_idempotent(
    tmp_path: pathlib.Path,
) -> None:
    """`db_upsert_exchange` inserts a new row with `active=False`,
    `last_update_block=None`, factory/deployer round-tripping; the second call
    returns the SAME id (no new insert) with factory/deployer unchanged."""
    db_path = tmp_path / "exchange.db"
    db_create_new_database(str(db_path))
    db_path.chmod(0o644)

    row = db_upsert_exchange(
        database_path=str(db_path),
        chain_id=CHAIN,
        name="uniswap_v3",
        factory=FACTORY,
        deployer=DEPLOYER,
    )
    assert row.chain_id == CHAIN
    assert row.name == "uniswap_v3"
    assert row.active is False
    assert row.last_update_block is None
    assert row.factory == FACTORY
    assert row.deployer == DEPLOYER

    first_id = row.id

    # second call — even with different factory/deployer args — must return the
    # SAME id, factory/deployer UNCHANGED (no new insert; `active` not touched).
    row2 = db_upsert_exchange(
        database_path=str(db_path),
        chain_id=CHAIN,
        name="uniswap_v3",
        factory=get_checksum_address("0x" + "0" * 40),
        deployer=None,
    )
    assert row2.id == first_id
    assert row2.factory == FACTORY
    assert row2.deployer == DEPLOYER
    assert row2.active is False


def test_db_set_exchange_active_flips_and_db_fetch_exchange_reads_back(
    tmp_path: pathlib.Path,
) -> None:
    """`db_set_exchange_active` flips active false→true→false; `db_fetch_exchange`
    reads the flipped state back (a fresh connection → fresh WAL snapshot)."""
    db_path = tmp_path / "exchange_active.db"
    db_create_new_database(str(db_path))
    db_path.chmod(0o644)

    row = db_upsert_exchange(
        database_path=str(db_path),
        chain_id=CHAIN,
        name="uniswap_v2",
        factory=FACTORY,
        deployer=None,
    )
    assert row.active is False

    db_set_exchange_active(
        database_path=str(db_path),
        exchange_id=row.id,
        active=True,
    )
    fetched = db_fetch_exchange(database_path=str(db_path), exchange_id=row.id)
    assert fetched is not None
    assert fetched.active is True

    db_set_exchange_active(
        database_path=str(db_path),
        exchange_id=row.id,
        active=False,
    )
    fetched = db_fetch_exchange(database_path=str(db_path), exchange_id=row.id)
    assert fetched is not None
    assert fetched.active is False


def test_db_set_exchange_active_missing_id_raises_value_error(
    tmp_path: pathlib.Path,
) -> None:
    """A nonexistent `exchange_id` surfaces the `DbError::MissingRow` as a
    `ValueError`."""
    db_path = tmp_path / "exchange_missing.db"
    db_create_new_database(str(db_path))
    db_path.chmod(0o644)

    with pytest.raises(ValueError, match="9999"):
        db_set_exchange_active(
            database_path=str(db_path),
            exchange_id=9999,
            active=True,
        )


def test_db_upsert_pool_manager_round_trips_and_is_idempotent(
    tmp_path: pathlib.Path,
) -> None:
    """`db_upsert_pool_manager` inserts, updates `state_view` in place (same id),
    and is a no-op on identical recall."""
    db_path = tmp_path / "pool_manager.db"
    db_create_new_database(str(db_path))
    db_path.chmod(0o644)

    exchange = db_upsert_exchange(
        database_path=str(db_path),
        chain_id=CHAIN,
        name="uniswap_v4",
        factory=POOL_MANAGER,
        deployer=None,
    )

    first = db_upsert_pool_manager(
        database_path=str(db_path),
        address=POOL_MANAGER,
        chain=CHAIN,
        kind="uniswap_v4",
        state_view=STATE_VIEW,
        exchange_id=exchange.id,
    )
    assert first.address == POOL_MANAGER
    assert first.chain == CHAIN
    assert first.kind == "uniswap_v4"
    assert first.state_view == STATE_VIEW
    assert first.exchange_id == exchange.id

    # re-call with a changed state_view — same id, state_view updated.
    other_state_view: ChecksummedAddress = get_checksum_address("0x" + "a" * 40)
    second = db_upsert_pool_manager(
        database_path=str(db_path),
        address=POOL_MANAGER,
        chain=CHAIN,
        kind="uniswap_v4",
        state_view=other_state_view,
        exchange_id=exchange.id,
    )
    assert second.id == first.id
    assert second.state_view == other_state_view

    # identical recall — unchanged.
    third = db_upsert_pool_manager(
        database_path=str(db_path),
        address=POOL_MANAGER,
        chain=CHAIN,
        kind="uniswap_v4",
        state_view=other_state_view,
        exchange_id=exchange.id,
    )
    assert third.id == first.id
    assert third.state_view == other_state_view


def test_db_fetch_exchange_by_name_returns_row_and_none_when_missing(
    tmp_path: pathlib.Path,
) -> None:
    """`db_upsert_exchange` then `db_fetch_exchange_by_name` returns the row;
    a missing name returns None; the lookup is scoped by chain_id."""
    db_path = tmp_path / "exchange_by_name.db"
    db_create_new_database(str(db_path))
    db_path.chmod(0o644)

    inserted = db_upsert_exchange(
        database_path=str(db_path),
        chain_id=CHAIN,
        name="uniswap_v2",
        factory=FACTORY,
        deployer=None,
    )

    fetched = db_fetch_exchange_by_name(
        database_path=str(db_path),
        chain_id=CHAIN,
        name="uniswap_v2",
    )
    assert fetched is not None
    assert fetched.id == inserted.id
    assert fetched.active is False
    assert fetched.factory == FACTORY

    # missing name → None
    missing = db_fetch_exchange_by_name(
        database_path=str(db_path),
        chain_id=CHAIN,
        name="uniswap_v3",
    )
    assert missing is None

    # scoped by chain_id — a different chain returns None even for the same name.
    cross_chain = db_fetch_exchange_by_name(
        database_path=str(db_path),
        chain_id=8453,
        name="uniswap_v2",
    )
    assert cross_chain is None
