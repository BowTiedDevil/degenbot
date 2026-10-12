"""Pool discovery — the DB-backed token-pair view (``PoolRef``).

Discovery answers "which pools trade this pair?" from the database and returns
inert identity views, never instances. The three-layer resolution ladder:

- **discovery finds** — :func:`pools_for_tokens` returns ``list[PoolRef]``;
- **the registry remembers** — ``bot.pools.get(address)`` returns an already
  built instance, or ``None``;
- **construction commits** — ``bot.build_pool(address)`` materializes the pool
  (memoized: a registry hit is free, a miss is one loud, explicit build).

A token pair is **not** a pool identity: a Uniswap V2 pool, a PancakeSwap V3
pool, and a V4-managed pool can all trade the same pair, and V4 hashes its hooks
into the PoolId (so even manager + tokens + fee underdetermines it). Discovery is
therefore a *database* question whose answer is refs — the caller picks from ref
data, then builds exactly one.

Seam note (P3b scope)
---------------------

This module is built on the exposed **database discovery read**:
``degenbot.db.db_fetch_discovery_rows`` — the Python seam over the Rust
``degenbot_db::discovery_read`` rows (``DiscoveryPoolRow``). One DB read
enumerates every candidate pool for a chain, each row carrying the base pool
row, the two token rows (addresses + ids), the owning exchange row (factory),
and the per-family detail columns (fees, ``tick_spacing``, Aerodrome ``stable``,
the liquidity-update marker). :func:`pools_for_tokens` projects the token-pair
query from that read, so every :class:`PoolRef` kind param is populated from the
database and no degree >= 2 cycle filter applies.

The earlier P3 implementation ran on the graph-read seam
(``degenbot.pathfinding.build_path_graph`` over ``fetch_path_graph_edges``),
which carries only identity fields and applies a degree >= 2 cycle filter; that
seam is no longer used here.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

from degenbot.checksum_cache import get_checksum_address
from degenbot.db import db_fetch_discovery_rows
from degenbot.pathfinding import ALL_POOL_KINDS, FAMILY_TAG_TO_POOL_KIND, PoolKind

if TYPE_CHECKING:
    import pathlib
    from collections.abc import Sequence

    from degenbot.db import DiscoveryPoolRow


__all__ = ["V4_POOL_ID_OFFSET", "PoolRef", "pools_for_tokens"]


#: V4 graph-id namespace offset (mirrors ``degenbot_db::pathfinding``).
#:
#: The V2/V3 ``pools.id`` counter and the V4 ``managed_pools.id`` counter are
#: independent, so a bare numeric id can collide. :func:`pools_for_tokens`
#: namespaces every V4 graph id above this offset; a ref's :attr:`PoolRef.db_id`
#: stays the raw ``managed_pools.id``.
V4_POOL_ID_OFFSET: int = 1 << 32


@dataclass(slots=True, frozen=True)
class PoolRef:
    """One discovered pool, as an inert identity view.

    A ``PoolRef`` is enough to *choose* a pool without building it, and inert
    otherwise: it carries no state accessors, no engine handle, and no liveness
    promise. It is a frozen view over the database's stamped rows — the chain
    remains the arbiter of existence at build time.

    Identity fields:

        address: The V2/V3 checksummed pool address; ``None`` for V4.
        manager: The V4 pool-manager address; ``None`` for V2/V3.
        pool_hash: The V4 pool hash (``0x``-prefixed hex); ``None`` for V2/V3.
        kind: The coarse family discriminant (:class:`~degenbot.pathfinding.PoolKind`).
        token0: ``token0`` / ``currency0`` checksummed address, in the pool's own
            ordering.
        token1: ``token1`` / ``currency1`` checksummed address, same ordering.
        token0_id: The DB token id for :attr:`token0`.
        token1_id: The DB token id for :attr:`token1`.
        db_id: The raw DB row id (``pools.id`` for V2/V3, ``managed_pools.id``
            for V4).
        graph_id: The pathfinding graph id (``db_id`` for V2/V3, ``db_id`` plus
            :data:`V4_POOL_ID_OFFSET` for V4).

    Kind-param fields — the DB's per-family subclass columns, all populated from
    the ``discovery_read`` seam:

        kind_string: The raw polymorphic ``kind`` (e.g. ``"uniswap_v3"``).
        factory: The owning exchange's factory address.
        fee: The pool's fee (``fee_token0`` for V2/V3, ``fee_currency0`` for V4).
        tick_spacing: The V3/V4 tick spacing; ``None`` for V2.
        stable: The Aerodrome V2 stable flag; ``None`` for other families.
        update_block: The liquidity-update marker block (V3/V4); ``None`` for V2.

    """

    address: str | None
    manager: str | None
    pool_hash: str | None
    kind: PoolKind
    token0: str
    token1: str
    token0_id: int
    token1_id: int
    db_id: int
    graph_id: int
    kind_string: str | None = None
    factory: str | None = None
    fee: int | None = None
    tick_spacing: int | None = None
    stable: bool | None = None
    update_block: int | None = None


def _ref_from_row(row: DiscoveryPoolRow) -> PoolRef:
    """Build one :class:`PoolRef` from a discovery-read row.

    Args:
        row: One ``degenbot.db.DiscoveryPoolRow`` from
            :func:`~degenbot.db.db_fetch_discovery_rows`.

    Returns:
        The identity view for the row's pool, every field populated.

    """
    kind = FAMILY_TAG_TO_POOL_KIND[row.family]
    db_id = row.db_id
    graph_id = db_id + V4_POOL_ID_OFFSET if kind == PoolKind.V4 else db_id
    return PoolRef(
        address=row.address,
        manager=row.manager,
        pool_hash=row.pool_hash,
        kind=kind,
        token0=row.token0,
        token1=row.token1,
        token0_id=row.token0_id,
        token1_id=row.token1_id,
        db_id=db_id,
        graph_id=graph_id,
        kind_string=row.kind,
        factory=row.factory,
        fee=row.fee,
        tick_spacing=row.tick_spacing,
        stable=row.stable,
        update_block=row.update_block,
    )


def pools_for_tokens(
    database_path: pathlib.Path | str,
    chain_id: int,
    token_a: str,
    token_b: str,
    *,
    pool_kinds: Sequence[PoolKind] | None = None,
) -> list[PoolRef]:
    """Discover the pools that trade a token pair, as inert identity refs.

    One database read (the exposed ``db_fetch_discovery_rows`` seam over the
    Rust ``degenbot_db::discovery_read`` rows) enumerates the chain's candidate
    pools; the rows whose token pair matches are projected into refs, ordered by
    :attr:`PoolRef.graph_id` for a stable view. Nothing is constructed, and no
    degree >= 2 cycle filter applies — a token pair whose tokens each appear in
    a single pool still discovers.

    The three-layer ladder the caller composes (see the module docstring):
    discovery *finds* (this function), the registry *remembers*
    (``bot.pools.get``), construction *commits* (``bot.build_pool``, idempotent).

    Args:
        database_path: The SQLite database file path.
        chain_id: The chain whose pools are queried.
        token_a: One token's checksummed (or lower/upper) address.
        token_b: The other token's address.
        pool_kinds: Optional family filter; defaults to every family
            (:data:`~degenbot.pathfinding.ALL_POOL_KINDS`).

    Returns:
        The pools trading the pair, each an identity ref. Empty when either token
        is unknown to the database or no pool trades the pair.

    """
    wanted = {get_checksum_address(token_a), get_checksum_address(token_b)}
    allowed = set(pool_kinds) if pool_kinds is not None else set(ALL_POOL_KINDS)
    refs = [
        _ref_from_row(row)
        for row in db_fetch_discovery_rows(str(database_path), chain_id)
        if {row.token0, row.token1} == wanted and FAMILY_TAG_TO_POOL_KIND[row.family] in allowed
    ]
    refs.sort(key=lambda ref: ref.graph_id)
    return refs
