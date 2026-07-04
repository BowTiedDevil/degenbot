"""Python-side §4.2 parity test for the pool discovery writers seam (WR7EA6).

Builds a fresh Alembic-stamped SQLite DB, seeds an exchange + a PoolManager,
then applies a `PoolCreated` event sequence through the Python
`cli/pool_updater_configs.py::update_v2/v3/v4_pools` shells (now delegating to
the Rust `db_upsert_v2/v3/v4_pools` seam) + asserts the resulting
`pools` / per-subclass / `managed_pools` / `uniswap_v4_pools` / `erc20_tokens`
rows + the `ExchangeTable.last_update_block` stamp match the expected
polymorphic-insert state.

The Rust-internal trajectory is pinned by `rust/crates/degenbot-db/tests/
discovery_parity.rs` (8 tests); this test proves the Python shell decode +
delegation reach the same Rust result end-to-end (the V2/V3 `PoolCreated`
ABI topic/data decode, the V4 `pool_hash`/`hooks` decode, the Aerodrome stable
flag, + the exchange stamp).
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest
from eth_abi import encode as abi_encode
from hexbytes import HexBytes
from sqlalchemy import select
from web3.types import LogReceipt

from degenbot.checksum_cache import get_checksum_address
from degenbot.cli.pool_updater_configs import (
    V2PoolUpdateConfig,
    V3PoolUpdateConfig,
    V4PoolUpdateConfig,
    update_v2_pools,
    update_v3_pools,
    update_v4_pools,
)
from degenbot.database.models.base import ExchangeTable
from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.database.models.pools import (
    LiquidityPoolTable,
    PoolManagerTable,
    UniswapV4PoolTable,
)
from degenbot.database.operations import create_new_sqlite_database, get_scoped_sqlite_session
from degenbot.degenbot_rs import (
    db_fetch_exchange,
    db_fetch_exchange_by_name,
    db_set_exchange_active,
    db_set_exchange_last_update_block,
    db_upsert_exchange,
    db_upsert_pool_manager,
)

if TYPE_CHECKING:
    import pathlib
    from collections.abc import Callable

    from eth_typing import ChecksumAddress


CHAIN = 1
UNISWAP_V2_FACTORY: ChecksumAddress = get_checksum_address("0x" + "f" * 40)
V2_POOL_MANAGER_ADDRESS: ChecksumAddress = get_checksum_address("0x" + "b" * 40)
V4_POOL_HASH = "0x" + "c" * 64

V2_POOL_CREATED_TOPIC = HexBytes(
    "0x0d3648bd0f6ba80134a33ba9275ac585d9d315f0ad8355cddefde31afa28d0e9",
)
V3_POOL_CREATED_TOPIC = HexBytes(
    "0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118",
)
V4_POOL_CREATED_TOPIC = HexBytes(
    "0xdd466e674ea557f56295e2d0218a125ea4b4f0f6f3307b95f85e6110838d6438",
)


def _v2_pool_created_log(
    pool_address: ChecksumAddress,
    token0: ChecksumAddress,
    token1: ChecksumAddress,
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
                HexBytes.fromhex(token0[2:].rjust(64, "0")),
                HexBytes.fromhex(token1[2:].rjust(64, "0")),
                HexBytes(abi_encode(["bool"], [stable])),
            ],
            "data": HexBytes(
                abi_encode(["address", "uint256"], [pool_address, 0]),
            ),
        }
    )


def _v3_pool_created_log(
    pool_address: ChecksumAddress,
    token0: ChecksumAddress,
    token1: ChecksumAddress,
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
                HexBytes.fromhex(token0[2:].rjust(64, "0")),
                HexBytes.fromhex(token1[2:].rjust(64, "0")),
                HexBytes(abi_encode(["uint24"], [fee])),
            ],
            # V3 PoolCreated data: (int24 tick_spacing, address pool_address)
            "data": HexBytes(
                abi_encode(["int24", "address"], [tick_spacing, pool_address]),
            ),
        }
    )


def _v4_pool_created_log(
    pool_hash: str,
    currency0: ChecksumAddress,
    currency1: ChecksumAddress,
    fee: int,
    tick_spacing: int,
    hooks: ChecksumAddress,
) -> LogReceipt:
    return LogReceipt(  # type: ignore[typeddict-item]
        {
            "blockNumber": 100,
            "logIndex": 2,
            "address": V2_POOL_MANAGER_ADDRESS,
            "topics": [
                V4_POOL_CREATED_TOPIC,
                HexBytes(bytes.fromhex(pool_hash[2:])),
                HexBytes.fromhex(currency0[2:].rjust(64, "0")),
                HexBytes.fromhex(currency1[2:].rjust(64, "0")),
            ],
            # V4 PoolCreated data: (uint24 fee, int24 tick_spacing, address hooks)
            "data": HexBytes(
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
    create_new_sqlite_database(db_path)
    session = get_scoped_sqlite_session(db_path)
    with session() as s:  # type: Session
        # V2/V3 exchange (factory doubles as the V3 factory + the V2 family
        # exchange). `name` doubles as the `kind` discriminator the shell
        # passes to the Rust seam.
        ex2 = ExchangeTable(
            chain_id=CHAIN,
            name="uniswap_v2",
            active=True,
            last_update_block=None,
            factory=UNISWAP_V2_FACTORY,
            deployer=None,
        )
        s.add(ex2)
        s.flush()
        exchange_id = ex2.id

        # V4 exchange + PoolManager (manager address = this exchange's factory).
        ex4 = ExchangeTable(
            chain_id=CHAIN,
            name="uniswap_v4",
            active=True,
            last_update_block=None,
            factory=V2_POOL_MANAGER_ADDRESS,
            deployer=None,
        )
        s.add(ex4)
        s.flush()
        pm = PoolManagerTable(
            address=V2_POOL_MANAGER_ADDRESS,
            chain=CHAIN,
            kind="uniswap_v4",
            state_view=None,
            exchange_id=ex4.id,
        )
        s.add(pm)
        s.flush()
        pool_manager_id = pm.id
        s.commit()
    session.get_bind().dispose()
    session.remove()
    return exchange_id, pool_manager_id


def _dispose(session) -> None:
    session.get_bind().dispose()
    session.remove()


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

    session = get_scoped_sqlite_session(seeded_db)
    with session() as s:
        exchange = s.scalar(select(ExchangeTable).where(ExchangeTable.name == "uniswap_v2"))
    _dispose(session)

    update_v2_pools(
        provider=_StubProvider(),  # type: ignore[arg-type]
        start_block=100,
        end_block=100,
        exchange=exchange,
        database_path=str(seeded_db),
        config=config,
        get_events_fn=_events_fn_factory(events),
    )

    session = get_scoped_sqlite_session(seeded_db)
    with session() as s:  # type: Session
        pool = s.scalar(select(LiquidityPoolTable))
        assert pool is not None
        assert pool.kind == "uniswap_v2"
        assert pool.address == pool_addr
        assert pool.chain == CHAIN
        assert pool.exchange_id is not None
        # tokens were get-or-create'd through the Rust seam
        tokens = s.scalars(select(Erc20TokenTable).order_by(Erc20TokenTable.id)).all()
        assert [t.address for t in tokens] == [token0, token1]
        assert pool.token0_id == tokens[0].id
        assert pool.token1_id == tokens[1].id
    _dispose(session)


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

    session = get_scoped_sqlite_session(seeded_db)
    with session() as s:
        ex = s.scalar(select(ExchangeTable).where(ExchangeTable.name == "uniswap_v2"))
        ex.name = "aerodrome_v2"  # the shell passes name→kind
    _dispose(session)

    update_v2_pools(
        provider=_StubProvider(),  # type: ignore[arg-type]
        start_block=100,
        end_block=100,
        exchange=ex,
        database_path=str(seeded_db),
        config=config,
        get_events_fn=_events_fn_factory(events),
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

    session = get_scoped_sqlite_session(seeded_db)
    with session() as s:
        ex = s.scalar(select(ExchangeTable).where(ExchangeTable.name == "uniswap_v2"))
        ex.name = "uniswap_v3"  # the shell passes name→kind (in-memory override; no commit)
    _dispose(session)

    update_v3_pools(
        provider=_StubProvider(),  # type: ignore[arg-type]
        start_block=100,
        end_block=100,
        exchange=ex,
        database_path=str(seeded_db),
        config=config,
        get_events_fn=_events_fn_factory(events),
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

    session = get_scoped_sqlite_session(seeded_db)
    with session() as s:
        ex4 = s.scalar(select(ExchangeTable).where(ExchangeTable.name == "uniswap_v4"))
    _dispose(session)

    update_v4_pools(
        provider=_StubProvider(),  # type: ignore[arg-type]
        start_block=100,
        end_block=100,
        exchange=ex4,
        database_path=str(seeded_db),
        config=config,
        get_events_fn=_events_fn_factory(events),
    )

    session = get_scoped_sqlite_session(seeded_db)
    with session() as s:  # type: Session
        v4 = s.scalar(select(UniswapV4PoolTable))
        assert v4 is not None
        assert v4.pool_hash == V4_POOL_HASH
        assert v4.hooks == hooks
        assert v4.fee_currency0 == 3000
        assert v4.fee_currency1 == 3000
        assert v4.fee_denominator == 1_000_000
        assert v4.tick_spacing == 60
        assert v4.liquidity_update_block is None  # not stamped by discovery
        # V4 does NOT write to the V2/V3 `pools` table
        v2v3_count = s.scalar(select(LiquidityPoolTable.id))  # returns None if empty
        assert v2v3_count is None
    _dispose(session)


def test_set_exchange_last_update_block_seam(seeded_db: pathlib.Path) -> None:
    """The exchange stamp routes through the Rust seam (not the SQLAlchemy
    `exchange.last_update_block = …` mutation)."""
    session = get_scoped_sqlite_session(seeded_db)
    with session() as s:
        ex = s.scalar(select(ExchangeTable).where(ExchangeTable.name == "uniswap_v2"))
        exchange_id = ex.id
    _dispose(session)

    db_set_exchange_last_update_block(
        database_path=str(seeded_db),
        chain_id=CHAIN,
        exchange_id=exchange_id,
        block=99_999,
    )

    session = get_scoped_sqlite_session(seeded_db)
    with session() as s:
        ex = s.scalar(select(ExchangeTable).where(ExchangeTable.id == exchange_id))
        assert ex.last_update_block == 99_999
    _dispose(session)


# ---------------------------------------------------------------------
# Exchange write substrate (NWU4KH — split out of HYUYTN).
# Round-trips the three PyO3 seams over a fresh Alembic-stamped DB:
# `db_upsert_exchange` → `ExchangeRow` (`active=False`); `db_set_exchange_active`
# flips active + `db_fetch_exchange` reads it back; `db_upsert_pool_manager`
# round-trips + is idempotent.

FACTORY: ChecksumAddress = get_checksum_address("0x" + "f" * 40)
DEPLOYER: ChecksumAddress = get_checksum_address("0x" + "d" * 40)
POOL_MANAGER: ChecksumAddress = get_checksum_address("0x" + "b" * 40)
STATE_VIEW: ChecksumAddress = get_checksum_address("0x" + "e" * 40)


def test_db_upsert_exchange_inserts_active_false_and_is_idempotent(
    tmp_path: pathlib.Path,
) -> None:
    """`db_upsert_exchange` inserts a new row with `active=False`,
    `last_update_block=None`, factory/deployer round-tripping; the second call
    returns the SAME id (no new insert) with factory/deployer unchanged."""
    db_path = tmp_path / "exchange.db"
    create_new_sqlite_database(db_path)
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
    create_new_sqlite_database(db_path)
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
    create_new_sqlite_database(db_path)
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
    create_new_sqlite_database(db_path)
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
    other_state_view: ChecksumAddress = get_checksum_address("0x" + "a" * 40)
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
    create_new_sqlite_database(db_path)
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
