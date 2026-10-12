"""Pool discovery — ``PoolRef`` + ``Bot.pools_for_tokens`` (P3).

Discovery is a database question, not a derivation: a token pair is not a pool
identity. These tests seed a mixed V2 + V3 + V4 topology for one token pair
through the Rust write seams, then assert (a) the identity refs discovery yields,
(b) that discovery itself constructs nothing, and (c) that a discovered ref's
address is a valid key into the construction ladder (``build_pool``).

P3b extends the assertions to the discovery-read kind params (``kind_string`` /
``factory`` / ``fee`` / ``tick_spacing`` / ``stable`` / ``update_block``) and
proves the degree >= 2 cycle filter is lifted: a degree-1 pair still discovers.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest

from degenbot.bot import Bot
from degenbot.checksum_cache import get_checksum_address
from degenbot.db import (
    db_apply_v3_liquidity_updates,
    db_apply_v4_liquidity_updates,
    db_set_exchange_active,
    db_upgrade_database,
    db_upsert_exchange,
    db_upsert_v2_pools,
)
from degenbot.discovery import V4_POOL_ID_OFFSET, PoolRef, pools_for_tokens
from degenbot.pathfinding import PoolKind
from degenbot.updater import LiquidityUpdateEvent, V2PoolRowInput
from tests.fakes.pools import FakeSessionPool
from tests.helpers.database import seed_v2_topology, seed_v3_topology, seed_v4_topology

if TYPE_CHECKING:
    import pathlib


CHAIN_ID = 1
ZERO_ADDRESS = "0x0000000000000000000000000000000000000000"
SEED_BLOCK = 1_000

TOKEN_A = get_checksum_address("0x" + "aa" * 20)
TOKEN_B = get_checksum_address("0x" + "bb" * 20)
UNKNOWN_TOKEN = get_checksum_address("0x" + "77" * 20)

POOL_V2 = get_checksum_address("0x" + "11" * 20)
POOL_V3 = get_checksum_address("0x" + "22" * 20)
POOL_MANAGER = get_checksum_address("0x" + "99" * 20)
V4_POOL_HASH = "0x" + "cc" * 32


class _BoundaryProvider:
    """The external RPC boundary as a real object with an observable state."""

    def __init__(self, chain_id: int = CHAIN_ID) -> None:
        self.chain_id = chain_id
        self.closed = False

    def close(self) -> None:
        if self.closed:
            msg = "provider closed twice"
            raise RuntimeError(msg)
        self.closed = True


@pytest.fixture
def seeded_db(tmp_path: pathlib.Path) -> pathlib.Path:
    """Seed one V2 + one V3 + one V4 pool, all trading ``TOKEN_A``/``TOKEN_B``.

    The V3 + V4 pools are stamped with a liquidity-update marker (through the
    Rust apply seams) so discovery's ``update_block`` kind param is populated.
    """
    db_path = tmp_path / "discovery.db"
    seed_v2_topology(
        db_path,
        [(POOL_V2, TOKEN_A, TOKEN_B)],
        chain_id=CHAIN_ID,
        exchange_name="uniswap_v2",
    )
    seed_v3_topology(
        db_path,
        [(POOL_V3, TOKEN_A, TOKEN_B, 500, 10)],
        chain_id=CHAIN_ID,
        exchange_name="uniswap_v3",
    )
    seed_v4_topology(
        db_path,
        [(V4_POOL_HASH, TOKEN_A, TOKEN_B, ZERO_ADDRESS, 3000)],
        pool_manager_address=POOL_MANAGER,
        chain_id=CHAIN_ID,
    )
    db_apply_v3_liquidity_updates(
        str(db_path),
        CHAIN_ID,
        POOL_V3,
        [LiquidityUpdateEvent(SEED_BLOCK, 0, -120, 120, 10**18)],
    )
    db_apply_v4_liquidity_updates(
        str(db_path),
        V4_POOL_HASH,
        CHAIN_ID,
        [LiquidityUpdateEvent(SEED_BLOCK, 0, -120, 120, 10**18)],
    )
    return db_path


@pytest.fixture
def bot(seeded_db: pathlib.Path) -> Bot:
    """An offline Bot bound to the seeded database (injected RPC boundary)."""
    return Bot(chain_id=CHAIN_ID, database=str(seeded_db), provider=_BoundaryProvider())


def _by_kind(refs: list[PoolRef], kind: PoolKind) -> PoolRef:
    return next(ref for ref in refs if ref.kind == kind)


def test_pools_for_tokens_yields_one_ref_per_family(seeded_db: pathlib.Path) -> None:
    """The pair query returns an inert identity ref for every trading pool."""
    refs = pools_for_tokens(seeded_db, CHAIN_ID, TOKEN_A, TOKEN_B)

    assert [ref.kind for ref in refs] == [PoolKind.V2, PoolKind.V3, PoolKind.V4]

    v2 = _by_kind(refs, PoolKind.V2)
    assert v2.address == POOL_V2
    assert v2.manager is None
    assert v2.pool_hash is None
    assert v2.token0 == TOKEN_A
    assert v2.token1 == TOKEN_B
    assert v2.graph_id == v2.db_id

    v3 = _by_kind(refs, PoolKind.V3)
    assert v3.address == POOL_V3
    assert v3.graph_id == v3.db_id

    v4 = _by_kind(refs, PoolKind.V4)
    assert v4.address is None
    assert v4.manager == POOL_MANAGER
    assert v4.pool_hash == V4_POOL_HASH
    assert v4.graph_id == v4.db_id + V4_POOL_ID_OFFSET


def test_pool_ref_kind_params_are_filled_from_the_discovery_read(
    seeded_db: pathlib.Path,
) -> None:
    """Every kind param comes from the discovery-read rows, not the graph seam."""
    refs = pools_for_tokens(seeded_db, CHAIN_ID, TOKEN_A, TOKEN_B)

    v2 = _by_kind(refs, PoolKind.V2)
    assert v2.kind_string == "uniswap_v2"
    assert v2.factory == ZERO_ADDRESS
    assert v2.fee == 3
    assert v2.tick_spacing is None
    assert v2.stable is None
    assert v2.update_block is None

    v3 = _by_kind(refs, PoolKind.V3)
    assert v3.kind_string == "uniswap_v3"
    assert v3.factory == ZERO_ADDRESS
    assert v3.fee == 500
    assert v3.tick_spacing == 10
    assert v3.stable is None
    assert v3.update_block == SEED_BLOCK

    v4 = _by_kind(refs, PoolKind.V4)
    assert v4.kind_string == "uniswap_v4"
    assert v4.factory == ZERO_ADDRESS
    assert v4.fee == 3000
    assert v4.tick_spacing == 60
    assert v4.stable is None
    assert v4.update_block == SEED_BLOCK


def test_pool_ref_carries_no_state_or_engine_handle(seeded_db: pathlib.Path) -> None:
    """A ref is an inert view — no state accessors, no engine handle."""
    ref = pools_for_tokens(seeded_db, CHAIN_ID, TOKEN_A, TOKEN_B)[0]
    for forbidden in ("reserves", "state", "calculate_output_amount", "_py_pool"):
        assert not hasattr(ref, forbidden)


def test_pools_for_tokens_unknown_token_is_empty(seeded_db: pathlib.Path) -> None:
    """A token the database does not know yields no refs (never an error)."""
    assert pools_for_tokens(seeded_db, CHAIN_ID, TOKEN_A, UNKNOWN_TOKEN) == []


def test_pools_for_tokens_ignores_a_pool_of_another_pair(seeded_db: pathlib.Path) -> None:
    """Only pools trading the requested pair are returned."""
    token_c = get_checksum_address("0x" + "dd" * 20)
    pool_other = get_checksum_address("0x" + "33" * 20)
    seed_v2_topology(
        seeded_db,
        [(pool_other, TOKEN_A, token_c)],
        chain_id=CHAIN_ID,
        exchange_name="uniswap_v2_other",
    )
    refs = pools_for_tokens(seeded_db, CHAIN_ID, TOKEN_A, TOKEN_B)
    assert pool_other not in {ref.address for ref in refs}


def test_pools_for_tokens_discovers_a_degree_one_pair(tmp_path: pathlib.Path) -> None:
    """A general discovery read must not require a cycle (the degree filter is gone)."""
    token0 = get_checksum_address("0x" + "44" * 20)
    token1 = get_checksum_address("0x" + "55" * 20)
    pool = get_checksum_address("0x" + "66" * 20)
    db_path = tmp_path / "degree_one.db"
    seed_v2_topology(
        db_path,
        [(pool, token0, token1)],
        chain_id=CHAIN_ID,
        exchange_name="uniswap_v2",
    )

    refs = pools_for_tokens(db_path, CHAIN_ID, token0, token1)

    assert [ref.address for ref in refs] == [pool]
    assert refs[0].kind is PoolKind.V2


def test_pools_for_tokens_reports_the_aerodrome_stable_flag(tmp_path: pathlib.Path) -> None:
    """The Aerodrome V2 ``stable`` subclass column reaches the ref."""
    token0 = get_checksum_address("0x" + "88" * 20)
    token1 = get_checksum_address("0x" + "89" * 20)
    pool = get_checksum_address("0x" + "8a" * 20)
    db_path = tmp_path / "aerodrome.db"
    db_upgrade_database(str(db_path))
    exchange = db_upsert_exchange(
        database_path=str(db_path),
        chain_id=CHAIN_ID,
        name="aerodrome_v2",
        factory=ZERO_ADDRESS,
        deployer=None,
    )
    db_set_exchange_active(
        database_path=str(db_path),
        exchange_id=exchange.id,
        active=True,
    )
    db_upsert_v2_pools(
        database_path=str(db_path),
        chain_id=CHAIN_ID,
        kind="aerodrome_v2",
        exchange_id=exchange.id,
        fee_denominator=10_000,
        rows=[
            V2PoolRowInput(
                address=pool,
                token0_address=token0,
                token1_address=token1,
                fee_token0=0,
                fee_token1=0,
                stable=True,
            ),
        ],
    )

    refs = pools_for_tokens(db_path, CHAIN_ID, token0, token1)

    assert len(refs) == 1
    assert refs[0].kind_string == "aerodrome_v2"
    assert refs[0].stable is True


def test_bot_pools_for_tokens_constructs_nothing(bot: Bot, monkeypatch: pytest.MonkeyPatch) -> None:
    """Discovery never crosses the construction door (spy on ``build_pool``)."""
    build_calls: list[str] = []
    original_build = bot.build_pool

    def _recording_build(address: str, **kwargs: object) -> object:
        build_calls.append(address)
        return original_build(address, **kwargs)  # type: ignore[arg-type]

    monkeypatch.setattr(bot, "build_pool", _recording_build)

    refs = bot.pools_for_tokens(TOKEN_A, TOKEN_B)

    assert len(refs) == 3
    assert build_calls == []
    assert bot.pools.get(chain_id=CHAIN_ID, pool_address=POOL_V2) is None


def test_discovered_ref_address_is_a_valid_construction_key(bot: Bot) -> None:
    """``build_pool(ref.address)`` resolves through the registry ladder.

    A registry hit is free (the ref address is the ladder's key); the miss path
    is the one loud, explicit build the caller owns.
    """
    refs = bot.pools_for_tokens(TOKEN_A, TOKEN_B)
    v2_ref = _by_kind(refs, PoolKind.V2)
    assert v2_ref.address is not None

    sentinel = FakeSessionPool(v2_ref.address)
    bot.pools.add(chain_id=CHAIN_ID, pool_address=v2_ref.address, pool=sentinel)

    assert bot.build_pool(v2_ref.address) is sentinel
