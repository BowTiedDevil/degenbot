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

Seam note (P3 scope)
--------------------

This module is built on the **already-exposed** Python seam:
``degenbot.pathfinding.build_path_graph`` (the wrapper that choreographs the Rust
``degenbot_db::fetch_path_graph_edges`` bulk read) plus
``degenbot.db.db_resolve_token_ids``. No new Rust exposure is added.

That seam carries the pool id, the coarse family discriminant (``PoolKind``), the
V2/V3 checksummed address, the V4 ``(manager, pool_hash)`` lookup, and the edge's
token ids — but **not** the per-family kind params (``fee`` / ``tick_spacing`` /
``stable``), the raw ``kind`` string, the ``factory``, the token addresses of
pools other than the queried pair, or the liquidity-update marker. Those
``PoolRef`` fields therefore default to ``None`` here.

The Rust ``degenbot_db::discovery_read`` rows (``DiscoveryPoolRow``) already carry
every one of them (tokens, exchange/factory, fee, tick_spacing, stable, the
update marker). Exposing that read to Python collapses discovery to a single DB
read and fills the ``None`` fields — the one seam this chunk stops short of.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

from degenbot.checksum_cache import get_checksum_address
from degenbot.db import db_resolve_token_ids
from degenbot.pathfinding import ALL_POOL_KINDS, PoolKind, build_path_graph

if TYPE_CHECKING:
    import pathlib
    from collections.abc import Sequence

    from degenbot.pathfinding import PathGraph


__all__ = ["V4_POOL_ID_OFFSET", "PoolRef", "pools_for_tokens"]


#: V4 graph-id namespace offset (mirrors ``degenbot_db::pathfinding``).
#:
#: The V2/V3 ``pools.id`` counter and the V4 ``managed_pools.id`` counter are
#: independent, so a bare numeric id can collide. ``build_path_graph`` namespaces
#: every V4 graph id above this offset; :func:`pools_for_tokens` subtracts it to
#: recover the raw ``managed_pools.id``.
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
            ordering (the ordering the graph edge carries).
        token1: ``token1`` / ``currency1`` checksummed address, same ordering.
        token0_id: The DB token id for :attr:`token0`.
        token1_id: The DB token id for :attr:`token1`.
        db_id: The raw DB row id (``pools.id`` for V2/V3, ``managed_pools.id``
            for V4).
        graph_id: The pathfinding graph id (``db_id`` for V2/V3, ``db_id`` plus
            :data:`V4_POOL_ID_OFFSET` for V4) — the key that pairs this ref with
            a ``build_path_graph`` edge.

    Kind-param fields — the DB's per-family subclass columns. They default to
    ``None`` on the current graph-read seam (see the module docstring); the
    ``discovery_read`` seam fills them:

        kind_string: The raw polymorphic ``kind`` (e.g. ``"uniswap_v3"``).
        factory: The owning exchange's factory address.
        fee: The pool's fee (V2/V3/V4).
        tick_spacing: The V3/V4 tick spacing.
        stable: The Aerodrome V2 stable flag (``None`` for other families).
        update_block: The DB's stamped currency hint (the liquidity-update
            marker block).

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


def _ref_from_edge(
    graph: PathGraph,
    edge: tuple[int, int, int, PoolKind],
    id_to_address: dict[int, str],
) -> PoolRef:
    """Build one :class:`PoolRef` from a graph edge + its lookups.

    Args:
        graph: The ``build_path_graph`` value the edge came from (its address
            lookups are only valid for the read that produced the edge).
        edge: One ``(token0_id, token1_id, graph_pool_id, kind)`` graph edge.
        id_to_address: Mapping of the queried pair's token ids to checksummed
            addresses (the only token addresses the graph seam can resolve).

    Returns:
        The identity view for the edge's pool.

    """
    token0_id, token1_id, graph_id, kind = edge
    if kind == PoolKind.V4:
        manager, pool_hash = graph.v4_lookups[graph_id]
        address = None
        db_id = graph_id - V4_POOL_ID_OFFSET
    else:
        address = graph.v2v3_addresses[graph_id]
        manager = None
        pool_hash = None
        db_id = graph_id
    return PoolRef(
        address=address,
        manager=manager,
        pool_hash=pool_hash,
        kind=kind,
        token0=id_to_address[token0_id],
        token1=id_to_address[token1_id],
        token0_id=token0_id,
        token1_id=token1_id,
        db_id=db_id,
        graph_id=graph_id,
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

    One graph read (the exposed ``build_path_graph`` seam over the Rust
    ``fetch_path_graph_edges`` bulk read) answers the pair query; nothing is
    constructed. Refs are ordered by :attr:`PoolRef.graph_id` for a stable view.

    The three-layer ladder the caller composes (see the module docstring):
    discovery *finds* (this function), the registry *remembers*
    (``bot.pools.get``), construction *commits* (``bot.build_pool``, idempotent).

    Known limit of the current seam: ``build_path_graph`` keeps only tokens with
    degree >= 2 (a cycle needs two pools), so a token pair whose tokens each
    appear in fewer than two pools of the requested kinds yields no refs. The
    ``discovery_read`` seam would lift this.

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
    ids = db_resolve_token_ids(str(database_path), chain_id, [token_a, token_b])
    token_a_id = ids.get(get_checksum_address(token_a))
    token_b_id = ids.get(get_checksum_address(token_b))
    if token_a_id is None or token_b_id is None or token_a_id == token_b_id:
        return []

    kinds = tuple(pool_kinds) if pool_kinds is not None else ALL_POOL_KINDS
    graph = build_path_graph(
        database_path=str(database_path),
        chain_id=chain_id,
        pool_kinds=set(kinds),
    )

    id_to_address = {
        token_a_id: get_checksum_address(token_a),
        token_b_id: get_checksum_address(token_b),
    }
    pair = {token_a_id, token_b_id}
    refs = [
        _ref_from_edge(graph, edge, id_to_address)
        for edge in graph.edges
        if {edge[0], edge[1]} == pair
    ]
    refs.sort(key=lambda ref: ref.graph_id)
    return refs
