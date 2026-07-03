#!/usr/bin/env python3
"""Generate the §4.2 parity fixture for the V3/V4 DB-aware liquidity updater
(task QJSCA5 — the Rust `apply_v3_liquidity_updates` / `apply_v4_liquidity_updates`
in `rust/crates/degenbot-db/src/liquidity_updater.rs`).

Builds two Alembic-stamped SQLite DBs — one V3, one V4 — each seeded with a
pool row + an initial `LiquidityMap` (some pre-existing positions + init maps,
so the Rust `fetch_*_liquidity_map` reconstitution + the stale-row deletion
paths are exercised), then applies an event sequence through the PYTHON
`cli/pool.py::apply_v3_liquidity_updates` / `apply_v4_liquidity_updates`
(blueprint oracle) + dumps the resulting `liquidity_positions` /
`initialization_maps` rows + the pool-row `liquidity_update_block` /
`liquidity_update_log_index` marker to JSON.

The Rust integration test `tests/liquidity_updater_parity.rs` opens the frozen
INITIAL-state DBs (copies them to a tmp path so the Rust apply mutates a throw
away, not the committed fixture), runs the SAME event sequence through the Rust
`apply_v3/v4_liquidity_updates`, + asserts byte-identical resulting rows +
markers vs the JSON oracle here.

The event sequence exercises: a positive-delta Mint (tick init + bitmap flip
on), a second Mint (gross grows, second tick region), a Burn (negative delta;
tick gross shrinks), a Burn that drives a tick to gross=0 (tick dropped, bitmap
bit flipped off), + a same-block event (the log_index tiebreaker guard). The
seeded initial state includes a stale position that NONE of the events touches
(so the `delete_stale_*_positions` path is verified to drop it).

Run once (or whenever the seed/event sequence should change)::

    uv run python rust/crates/degenbot-db/tests/fixtures/generate_liquidity_updater_parity.py

Outputs (committed):
    rust/crates/degenbot-db/tests/fixtures/liquidity_updater_v3_initial.db
    rust/crates/degenbot-db/tests/fixtures/liquidity_updater_v3_expected.json
    rust/crates/degenbot-db/tests/fixtures/liquidity_updater_v4_initial.db
    rust/crates/degenbot-db/tests/fixtures/liquidity_updater_v4_expected.json
"""

from __future__ import annotations

import json
import pathlib
import shutil

from eth_abi import encode as abi_encode
from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from sqlalchemy import select
from sqlalchemy.orm import Session
from web3.types import LogReceipt

from degenbot.checksum_cache import get_checksum_address
from degenbot.cli.pool import (
    UNISWAP_V3_BURN_EVENT_HASH,
    UNISWAP_V3_MINT_EVENT_HASH,
    apply_v3_liquidity_updates,
    apply_v4_liquidity_updates,
)
from degenbot.database.models.base import ExchangeTable
from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.database.models.pools import (
    InitializationMapTable,
    LiquidityPositionTable,
    ManagedPoolInitializationMapTable,
    ManagedPoolLiquidityPositionTable,
    PoolManagerTable,
    UniswapV3PoolTable,
    UniswapV4PoolTable,
)
from degenbot.database.operations import (
    create_new_sqlite_database,
    get_scoped_sqlite_session,
)

FIXTURE_DIR = pathlib.Path(__file__).resolve().parent


def _checkpoint(path: pathlib.Path) -> None:
    """Flush the WAL into the main `.db` file + truncate the WAL.

    The Rust `create_new_sqlite_database` opens WAL journal mode; SQLAlchemy
    writes land in the `.db-wal` sidecar until a checkpoint. `shutil.copy2`
    copies only the `.db` (missing the WAL), so checkpoint BEFORE copying so the
    committed rows land in the main file the Rust test opens.
    """
    import sqlite3

    conn = sqlite3.connect(str(path))
    conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    conn.close()

# Event-topic hashes (must match `cli/pool.py`'s module constants exactly).
V3_MINT_TOPIC = UNISWAP_V3_MINT_EVENT_HASH
V3_BURN_TOPIC = UNISWAP_V3_BURN_EVENT_HASH
# V4 PoolManager.ModifyLiquidity event topic.
V4_MODIFY_TOPIC = HexBytes(
    "0xf208f4912782fd25c7f114ca3723a2d5dd6f3bcc3ac8db5af63baa85f711d5ec"
)

CHAIN = 1
TICK_SPACING = 10

V3_POOL_ADDRESS: ChecksumAddress = get_checksum_address("0x" + "a" * 40)
V3_FACTORY: ChecksumAddress = get_checksum_address("0x" + "f" * 40)
V4_POOL_MANAGER_ADDRESS: ChecksumAddress = get_checksum_address("0x" + "b" * 40)
# V4 pool-hash as a 0x-prefixed hex string (matches `pool_id.to_0x_hex()` in the
# Python `apply_v4_liquidity_updates` lookup). Must be exactly 66 chars.
V4_POOL_HASH = "0x" + "c" * 64
V4_HOOKS = "0x" + "0" * 40

# Seed ticks: a pre-existing initial state (tick -10, tick 10) carrying gross
# liquidity, PLUS a STALE tick (tick 12345) that no event touches — its
# presence verifies the Rust `delete_stale_*_positions` path drops it.
SEED_TICKS: list[tuple[int, int, int]] = [
    # (tick, liquidity_net, liquidity_gross)
    (-10, 1_000_000, 1_000_000),  # touched by events
    (10, -1_000_000, 1_000_000),  # touched by events
    (12345, 500, 500),  # STALE — no event touches it; must be dropped
]
# Seed init-map words. The cl-math `get_tick_word_and_bit_position` uses EVM
# truncated division: word = (tick // tick_spacing) // 256, with the bit set at
# (tick // tick_spacing) % 256. For tick_spacing=10:
#   tick -10 → word -1, bit_pos = (-1) % 256 = 255  ⟹ bitmap word = 1 << 255
#   tick  10 → word  0, bit_pos =   1  % 256 =   1  ⟹ bitmap word = 1 << 1
#   tick 12345 → word 4, bit_pos = 1234 % 256 = 210 ⟹ bitmap word = 1 << 210
SEED_INIT_MAPS: list[tuple[int, int]] = [
    (-1, 1 << 255),
    (0, 1 << 1),
    (4, 1 << 210),
]


def _v3_mint_log(block: int, log_idx: int, tick_lower: int, tick_upper: int, amount: int) -> LogReceipt:
    """Build a V3 Mint log receipt (topics[0]=Mint, topics[2..3]=tick_lower/upper)."""
    return LogReceipt(  # type: ignore[typeddict-item]
        {
            "blockNumber": block,
            "logIndex": log_idx,
            "address": V3_POOL_ADDRESS,
            "topics": [
                V3_MINT_TOPIC,
                HexBytes(b"\x00" * 12 + V3_POOL_ADDRESS.encode()),  # sender
                HexBytes(abi_encode(["int24"], [tick_lower])),
                HexBytes(abi_encode(["int24"], [tick_upper])),
            ],
            # Mint data: (address sender, uint128 amount, uint256 amount0, uint256 amount1)
            "data": HexBytes(abi_encode(
                ["address", "uint128", "uint256", "uint256"],
                ["0x" + "1" * 40, amount, 0, 0],
            )),
        }
    )


def _v3_burn_log(block: int, log_idx: int, tick_lower: int, tick_upper: int, amount: int) -> LogReceipt:
    """Build a V3 Burn log receipt (Burn: amount is the unsigned magnitude; the
    Python path negates it for the liquidity_delta)."""
    return LogReceipt(  # type: ignore[typeddict-item]
        {
            "blockNumber": block,
            "logIndex": log_idx,
            "address": V3_POOL_ADDRESS,
            "topics": [
                V3_BURN_TOPIC,
                HexBytes(b"\x00" * 12 + V3_POOL_ADDRESS.encode()),
                HexBytes(abi_encode(["int24"], [tick_lower])),
                HexBytes(abi_encode(["int24"], [tick_upper])),
            ],
            # Burn data: (uint128 amount, uint256 amount0, uint256 amount1)
            "data": HexBytes(abi_encode(
                ["uint128", "uint256", "uint256"],
                [amount, 0, 0],
            )),
        }
    )


def _v4_modify_log(block: int, log_idx: int, tick_lower: int, tick_upper: int, delta: int) -> LogReceipt:
    """Build a V4 ModifyLiquidity log receipt (data carries tick_lower/upper/delta)."""
    return LogReceipt(  # type: ignore[typeddict-item]
        {
            "blockNumber": block,
            "logIndex": log_idx,
            "address": V4_POOL_MANAGER_ADDRESS,
            "topics": [
                V4_MODIFY_TOPIC,
                HexBytes(b"\x00" * 12 + V4_POOL_MANAGER_ADDRESS.encode()),  # caller
                HexBytes(b"\x00" * 12 + V4_POOL_HASH[2:].encode()),  # poolId
            ],
            # ModifyLiquidity data: (int24 tickLower, int24 tickUpper, int256 liquidityDelta, bytes32 salt)
            "data": HexBytes(abi_encode(
                ["int24", "int24", "int256", "bytes32"],
                [tick_lower, tick_upper, delta, b"\x00" * 32],
            )),
        }
    )


def _build_v3_db(path: pathlib.Path) -> None:
    path.unlink(missing_ok=True)
    create_new_sqlite_database(path)
    session = get_scoped_sqlite_session(path)
    with session() as s:  # type: Session
        exchange = ExchangeTable(
            chain_id=CHAIN,
            name="uniswap_v3",
            active=True,
            last_update_block=None,
            factory=V3_FACTORY,
            deployer=None,
        )
        s.add(exchange)
        s.flush()

        token0 = Erc20TokenTable(
            address=get_checksum_address("0x" + "1" * 40),
            chain=CHAIN,
            symbol="T0",
            name="Token0",
            decimals=18,
        )
        token1 = Erc20TokenTable(
            address=get_checksum_address("0x" + "2" * 40),
            chain=CHAIN,
            symbol="T1",
            name="Token1",
            decimals=18,
        )
        s.add_all([token0, token1])
        s.flush()

        pool = UniswapV3PoolTable(
            address=V3_POOL_ADDRESS,
            chain=CHAIN,
            token0_id=token0.id,
            token1_id=token1.id,
            exchange_id=exchange.id,
            fee_token0=0,
            fee_token1=0,
            fee_denominator=1_000_000,
            tick_spacing=TICK_SPACING,
            liquidity_update_block=None,
            liquidity_update_log_index=None,
        )
        s.add(pool)
        s.flush()

        for tick, net, gross in SEED_TICKS:
            s.add(
                LiquidityPositionTable(
                    pool_id=pool.id, tick=tick, liquidity_net=net, liquidity_gross=gross
                )
            )
        for word, bitmap in SEED_INIT_MAPS:
            s.add(
                InitializationMapTable(pool_id=pool.id, word=word, bitmap=bitmap)
            )
        s.commit()
    _checkpoint(path)


def _build_v4_db(path: pathlib.Path) -> None:
    path.unlink(missing_ok=True)
    create_new_sqlite_database(path)
    session = get_scoped_sqlite_session(path)
    with session() as s:  # type: Session
        exchange = ExchangeTable(
            chain_id=CHAIN,
            name="uniswap_v4",
            active=True,
            last_update_block=None,
            factory=V4_POOL_MANAGER_ADDRESS,
            deployer=None,
        )
        s.add(exchange)
        s.flush()

        manager = PoolManagerTable(
            address=V4_POOL_MANAGER_ADDRESS,
            chain=CHAIN,
            kind="uniswap_v4",
            state_view=None,
            exchange_id=exchange.id,
        )
        s.add(manager)
        s.flush()

        token0 = Erc20TokenTable(
            address=get_checksum_address("0x" + "1" * 40),
            chain=CHAIN,
            symbol="T0",
            name="Token0",
            decimals=18,
        )
        token1 = Erc20TokenTable(
            address=get_checksum_address("0x" + "2" * 40),
            chain=CHAIN,
            symbol="T1",
            name="Token1",
            decimals=18,
        )
        s.add_all([token0, token1])
        s.flush()

        pool = UniswapV4PoolTable(
            manager_id=manager.id,
            pool_hash=V4_POOL_HASH,
            hooks=V4_HOOKS,
            currency0_id=token0.id,
            currency1_id=token1.id,
            fee_currency0=0,
            fee_currency1=0,
            fee_denominator=1_000_000,
            tick_spacing=TICK_SPACING,
            liquidity_update_block=None,
            liquidity_update_log_index=None,
        )
        s.add(pool)
        s.flush()

        for tick, net, gross in SEED_TICKS:
            s.add(
                ManagedPoolLiquidityPositionTable(
                    managed_pool_id=pool.managed_pool_id, tick=tick, liquidity_net=net, liquidity_gross=gross
                )
            )
        for word, bitmap in SEED_INIT_MAPS:
            s.add(
                ManagedPoolInitializationMapTable(
                    managed_pool_id=pool.managed_pool_id, word=word, bitmap=bitmap
                )
            )
        s.commit()
    _checkpoint(path)


def _dump_v3_state(s: Session, pool_id: int) -> dict[str, object]:
    positions = sorted(
        s.scalars(select(LiquidityPositionTable).where(LiquidityPositionTable.pool_id == pool_id)).all(),
        key=lambda r: r.tick,
    )
    init_maps = sorted(
        s.scalars(select(InitializationMapTable).where(InitializationMapTable.pool_id == pool_id)).all(),
        key=lambda r: r.word,
    )
    pool = s.scalar(select(UniswapV3PoolTable).where(UniswapV3PoolTable.id == pool_id))
    assert pool is not None
    return {
        "positions": [
            {"tick": r.tick, "liquidity_net": str(r.liquidity_net), "liquidity_gross": str(r.liquidity_gross)}
            for r in positions
        ],
        "initialization_maps": [
            {"word": r.word, "bitmap": str(r.bitmap)}
            for r in init_maps
        ],
        "liquidity_update_block": pool.liquidity_update_block,
        "liquidity_update_log_index": pool.liquidity_update_log_index,
    }


def _dump_v4_state(s: Session, managed_pool_id: int) -> dict[str, object]:
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
    return {
        "positions": [
            {"tick": r.tick, "liquidity_net": str(r.liquidity_net), "liquidity_gross": str(r.liquidity_gross)}
            for r in positions
        ],
        "initialization_maps": [
            {"word": r.word, "bitmap": str(r.bitmap)}
            for r in init_maps
        ],
        "liquidity_update_block": pool.liquidity_update_block,
        "liquidity_update_log_index": pool.liquidity_update_log_index,
    }


class _DummyProvider:
    """A minimal provider shim carrying just the chain_id (the V3 apply fn
    only reads `provider.chain_id`)."""

    chain_id = CHAIN


def _apply_v3_python_blueprint(db_path: pathlib.Path) -> dict[str, object]:
    events = [
        _v3_mint_log(block=100, log_idx=0, tick_lower=-10, tick_upper=10, amount=500_000),
        _v3_mint_log(block=100, log_idx=1, tick_lower=100, tick_upper=110, amount=250_000),
        _v3_burn_log(block=101, log_idx=0, tick_lower=-10, tick_upper=10, amount=200_000),
        _v3_burn_log(block=102, log_idx=0, tick_lower=-10, tick_upper=10, amount=1_300_000),
    ]
    session = get_scoped_sqlite_session(db_path)
    with session() as s:  # type: Session
        pool = s.scalar(
            select(UniswapV3PoolTable).where(
                UniswapV3PoolTable.address == V3_POOL_ADDRESS,
                UniswapV3PoolTable.chain == CHAIN,
            )
        )
        assert pool is not None
        exchange = s.scalar(select(ExchangeTable).where(ExchangeTable.chain_id == CHAIN))
        assert exchange is not None

        apply_v3_liquidity_updates(
            provider=_DummyProvider(),  # type: ignore[arg-type]
            pool_address=V3_POOL_ADDRESS,
            liquidity_events=events,
            exchanges_in_scope={exchange},
            session=s,
        )
        s.commit()
        return _dump_v3_state(s, pool.id)


def _apply_v4_python_blueprint(db_path: pathlib.Path) -> dict[str, object]:
    events = [
        _v4_modify_log(block=200, log_idx=0, tick_lower=-10, tick_upper=10, delta=500_000),
        _v4_modify_log(block=200, log_idx=1, tick_lower=100, tick_upper=110, delta=250_000),
        _v4_modify_log(block=201, log_idx=0, tick_lower=-10, tick_upper=10, delta=-200_000),
        _v4_modify_log(block=202, log_idx=0, tick_lower=-10, tick_upper=10, delta=-1_300_000),
    ]
    session = get_scoped_sqlite_session(db_path)
    with session() as s:  # type: Session
        manager = s.scalar(select(PoolManagerTable).where(PoolManagerTable.chain == CHAIN))
        assert manager is not None
        pool = s.scalar(
            select(UniswapV4PoolTable).where(UniswapV4PoolTable.pool_hash == V4_POOL_HASH)
        )
        assert pool is not None
        apply_v4_liquidity_updates(
            pool_id=HexBytes(V4_POOL_HASH),
            liquidity_events=events,
            pool_manager=manager,
            session=s,
        )
        s.commit()
        return _dump_v4_state(s, pool.managed_pool_id)


def main() -> None:
    v3_db = FIXTURE_DIR / "liquidity_updater_v3_initial.db"
    v4_db = FIXTURE_DIR / "liquidity_updater_v4_initial.db"

    # Build the INITIAL-state DBs (the Rust test copies these to a tmp path
    # before applying, so the committed initial state stays pristine).
    _build_v3_db(v3_db)
    _build_v4_db(v4_db)

    # Apply the PYTHON blueprint (on copies — keep the initial DBs pristine for
    # the Rust test).
    v3_copy = FIXTURE_DIR / "_liquidity_updater_v3_apply_copy.db"
    v4_copy = FIXTURE_DIR / "_liquidity_updater_v4_apply_copy.db"
    v3_copy.unlink(missing_ok=True)
    v4_copy.unlink(missing_ok=True)
    shutil.copy2(v3_db, v3_copy)
    shutil.copy2(v4_db, v4_copy)

    v3_expected = _apply_v3_python_blueprint(v3_copy)
    v4_expected = _apply_v4_python_blueprint(v4_copy)

    (FIXTURE_DIR / "liquidity_updater_v3_expected.json").write_text(
        json.dumps({"chain_id": CHAIN, "pool_address": V3_POOL_ADDRESS, "tick_spacing": TICK_SPACING, **v3_expected}, indent=2, sort_keys=True)
        + "\n"
    )
    (FIXTURE_DIR / "liquidity_updater_v4_expected.json").write_text(
        json.dumps({"chain_id": CHAIN, "pool_hash": V4_POOL_HASH, "pool_manager_chain": CHAIN, "tick_spacing": TICK_SPACING, **v4_expected}, indent=2, sort_keys=True)
        + "\n"
    )

    v3_copy.unlink(missing_ok=True)
    v4_copy.unlink(missing_ok=True)

    print(f"Built {v3_db.name} ({v3_db.stat().st_size} bytes)")
    print(f"Built {v4_db.name} ({v4_db.stat().st_size} bytes)")
    print(f"Wrote liquidity_updater_v3_expected.json ({len(v3_expected['positions'])} positions, {len(v3_expected['initialization_maps'])} init-maps, marker=({v3_expected['liquidity_update_block']},{v3_expected['liquidity_update_log_index']}))")  # type: ignore[arg-type]
    print(f"Wrote liquidity_updater_v4_expected.json ({len(v4_expected['positions'])} positions, {len(v4_expected['initialization_maps'])} init-maps, marker=({v4_expected['liquidity_update_block']},{v4_expected['liquidity_update_log_index']}))")  # type: ignore[arg-type]


if __name__ == "__main__":
    main()