"""Pool discovery — ``PoolRef`` + ``Bot.pools_for_tokens`` (P3).

Discovery is a database question, not a derivation: a token pair is not a pool
identity. These tests seed a mixed V2 + V3 + V4 topology for one token pair
through the Rust write seams, then assert (a) the identity refs discovery yields,
(b) that discovery itself constructs nothing, and (c) that a discovered ref's
address is a valid key into the construction ladder (``build_pool``).
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest

from degenbot.bot import Bot
from degenbot.checksum_cache import get_checksum_address
from degenbot.discovery import V4_POOL_ID_OFFSET, PoolRef, pools_for_tokens
from degenbot.pathfinding import PoolKind
from tests.fakes.pools import FakeSessionPool
from tests.helpers.database import seed_v2_topology, seed_v3_topology, seed_v4_topology

if TYPE_CHECKING:
    import pathlib


CHAIN_ID = 1
ZERO_ADDRESS = "0x0000000000000000000000000000000000000000"

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
    """Seed one V2 + one V3 + one V4 pool, all trading ``TOKEN_A``/``TOKEN_B``."""
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
