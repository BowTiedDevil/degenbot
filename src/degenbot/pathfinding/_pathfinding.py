"""Pathfinding utilities for discovering arbitrage routes."""

from __future__ import annotations

import asyncio
import enum
import itertools
import time
from dataclasses import dataclass
from typing import TYPE_CHECKING, cast

from degenbot.checksum_cache import get_checksum_address
from degenbot.database.models.pools import (
    LiquidityPoolTable,
    UniswapV2PoolTableBase,
    UniswapV3PoolTableBase,
    UniswapV4PoolTable,
)
from degenbot.database.operations import resolve_token_ids
from degenbot.exceptions.base import DegenbotValueError
from degenbot.logging import logger
from degenbot.pathfinding import build_path_graph, find_paths_rust

if TYPE_CHECKING:
    from collections.abc import AsyncIterator, Iterable, Iterator, Sequence

    from sqlalchemy.orm import Session

    from degenbot._ffi import ChecksummedAddress
    from degenbot.database.session_manager import DatabaseSessionManager

type PoolId = int
type TokenId = int

# Pool-kind discriminants passed to the Rust `find_paths_rust` function.
# These match the `PoolKind::from_u8` mapping in the Rust core leaf.
_POOL_KIND_V2: int = 0
_POOL_KIND_V3: int = 1
_POOL_KIND_V4: int = 2

# The family-base class for each pool-kind u8 (AF7OEL option B — family-only
# `PathStep.type` parity). The Rust seam returns `pool_id → pool_kind_u8`
# (V2/V3/V4 family); the concrete subclass (UniswapV2 vs SushiswapV2) is not
# recoverable from the family alone. Every known consumer of `PathStep.type`
# uses `issubclass(step.type, {UniswapV2,V3,V4}PoolTableBase)` — family
# detection — so the family base class is functionally equivalent. Strict
# concrete-class parity (option A) would require the Rust read to expose the
# `kind` STRING per pool_id (a follow-on `degenbot-db` task).
_POOL_KIND_TO_BASE: dict[int, type] = {
    _POOL_KIND_V2: UniswapV2PoolTableBase,
    _POOL_KIND_V3: UniswapV3PoolTableBase,
    _POOL_KIND_V4: UniswapV4PoolTable,
}

# Minimum elapsed wall-clock between `find_paths_async` progress heartbeats
# (NY4EFN). Picked so a long search stays quiet but a hang surfaces within
# the bounded-time target. The Rust DFS emits a separate GIL-free stderr
# heartbeat for the zero-yield grind that blocks this coroutine.
_DISCOVERY_HEARTBEAT_INTERVAL_S: float = 15.0


@dataclass(slots=True, frozen=True)
class PathStep:
    """PathStep class."""

    address: ChecksummedAddress
    type: type[LiquidityPoolTable | UniswapV4PoolTable]
    hash: str | None = None


class Direction(enum.Enum):
    """Direction class."""

    FORWARD = enum.auto()
    FORWARD_AND_REVERSE = enum.auto()


def _pool_kind_for_type(pool_type: type) -> int:
    """Map a pool-table class to its u8 discriminant for the Rust core.

    Checking V3 before V2 is required because both are subclasses of
    ``LiquidityPoolTable`` — the more specific ``UniswapV3PoolTableBase``
    must be tested before the general ``LiquidityPoolTable`` fallback.

    Returns:
        0 for V2 pools, 1 for V3 pools, 2 for V4 pools.

    Raises:
        DegenbotValueError: If ``pool_type`` is not a recognized pool-table class.

    """
    if issubclass(pool_type, UniswapV4PoolTable):
        return _POOL_KIND_V4
    if issubclass(pool_type, UniswapV3PoolTableBase):
        return _POOL_KIND_V3
    if issubclass(pool_type, UniswapV2PoolTableBase):
        return _POOL_KIND_V2
    msg = f"Unsupported pool type: {pool_type}"
    raise DegenbotValueError(message=msg)


@dataclass(slots=True)
class _PreparedGraph:
    """The flat edge list + address lookups produced by ``_prepare_graph``.

    Attributes:
        edges: Flat ``(token0, token1, pool_id, pool_kind_u8)`` tuples for Rust.
        v2v3_addresses: Maps V2/V3 pool IDs to their on-chain addresses.
        v4_lookups: Maps V4 pool IDs to ``(manager_address, pool_hash)``.
        pool_id_to_type: Maps ``(pool_id, pool_kind_u8)`` to the concrete table
            class, used to reconstruct ``PathStep.type``. Keyed by the pair so
            a V2 pool and a V4 pool sharing a ``pool_id`` (independent id
            counters — see `test_pool_id_collision`) do NOT collapse to one.

    """

    edges: list[tuple[TokenId, TokenId, PoolId, int]]
    v2v3_addresses: dict[PoolId, ChecksummedAddress]
    v4_lookups: dict[PoolId, tuple[ChecksummedAddress, str]]
    pool_id_to_type: dict[tuple[PoolId, int], type[LiquidityPoolTable | UniswapV4PoolTable]]


def _prepare_graph(
    chain_id: int,
    pool_types: Sequence[type],
    session: Session,
    allowed_intermediate_tokens: set[TokenId] | None = None,
) -> _PreparedGraph:
    """Build the flat edge list + address lookup dicts for the Rust DFS.

    Delegates the bulk DB read + candidate-token edge filter to the Rust core
    via `build_path_graph` (AF7OEL) — the sole graph-build path (ZNWXNC).
    The DB must be file-backed: the Rust seam opens its own connection on
    `database_path`, so an in-memory `:memory:` session cannot be shared
    (test fixtures use temp files).

    Returns:
        A ``_PreparedGraph`` with flat edges + address lookup dicts.

    Raises:
        DegenbotValueError: The session is not file-backed
            (``:memory:`` is not supported - the Rust seam opens its
            own connection on the database path).

    """
    engine = session.bind
    db_path = getattr(getattr(engine, "url", None), "database", None) if engine else None

    if not db_path or db_path == ":memory:":
        msg = (
            "Pathfinding requires a file-backed database "
            "(the Rust build_path_graph seam opens its own "
            "connection); :memory: sessions are not supported."
        )
        raise DegenbotValueError(message=msg)
    return _prepare_graph_rust(
        chain_id=chain_id,
        pool_types=pool_types,
        db_path=db_path,
        allowed_intermediate_tokens=allowed_intermediate_tokens,
    )


def _prepare_graph_rust(
    *,
    chain_id: int,
    pool_types: Sequence[type],
    db_path: str,
    allowed_intermediate_tokens: set[TokenId] | None = None,
) -> _PreparedGraph:
    """Rust seam path: `build_path_graph` does the bulk read + filter.

    Resolves the `pool_kinds` u8 set from the Python `pool_types` classes,
    invokes the Rust `build_path_graph` seam (which runs
    `fetch_tokens_with_min_degree` → `fetch_path_graph_edges` → candidate-
    token edge filter, all in one GIL-released span), then rebuilds the exact
    concrete `PathStep.type` class per pool_id from the DB's raw `kind` STRING
    (AF7OEL strict parity, option A).

    Returns:
        A ``_PreparedGraph`` with flat edges + address lookup dicts.

    """
    start = time.perf_counter()

    # Map the Python `pool_types` classes to the Rust `pool_kind` u8 set
    # (deduped — e.g. UniswapV2PoolTable + SushiswapV2PoolTable → {0}). The
    # base `LiquidityPoolTable` (the default `pool_types` entry, used for
    # single-table-inheritance selects that return BOTH V2 + V3 rows) expands
    # to {V2, V3} — it is neither a V2-base nor V3-base subclass itself, so
    # `_pool_kind_for_type` would raise on it; expand it explicitly.
    pool_kinds: set[int] = set()
    for pt in pool_types:
        if pt is LiquidityPoolTable or issubclass(pt, LiquidityPoolTable):
            if issubclass(pt, UniswapV3PoolTableBase):
                pool_kinds.add(_POOL_KIND_V3)
            elif issubclass(pt, UniswapV2PoolTableBase):
                pool_kinds.add(_POOL_KIND_V2)
            elif pt is LiquidityPoolTable:
                # The base covers both V2 + V3 (single-table-inheritance).
                pool_kinds.update({_POOL_KIND_V2, _POOL_KIND_V3})
            else:
                # A LiquidityPoolTable subclass that is neither V2 nor V3
                # base — unknown family; skip (matches the old silent skip).
                pass
        if issubclass(pt, UniswapV4PoolTable):
            pool_kinds.add(_POOL_KIND_V4)

    raw = build_path_graph(
        database_path=str(db_path),
        chain_id=chain_id,
        pool_kinds=pool_kinds,
        allowed_intermediate_token_ids=allowed_intermediate_tokens,
    )

    candidate_tokens: set[TokenId] = set(raw["candidate_tokens"])
    logger.debug(f"Found {len(candidate_tokens)} candidate tokens held by 2 or more pools")
    if allowed_intermediate_tokens is not None:
        logger.debug(
            f"Token whitelist applied: {len(candidate_tokens)} candidate tokens",
        )

    # Build a `kind_string → concrete_class` registry from the `pool_types`
    # input, via each class's `__mapper__.polymorphic_identity` (the
    # single-table-inheritance discriminator the DB stores as the `kind`
    # column). This rebuilds the exact concrete `PathStep.type` class per
    # pool_id (AF7OEL strict parity, option A) — e.g. a pool whose `kind` is
    # `"sushiswap_v3"` maps to `SushiswapV3PoolTable`, not the family base.
    kind_string_to_class: dict[str, type[LiquidityPoolTable | UniswapV4PoolTable]] = {}
    for pt in pool_types:
        # `__mapper__.polymorphic_identity` is a runtime attribute SQLAlchemy
        # attaches to declarative classes; ty can't see it statically, so
        # resolve via `getattr` + skip when absent (base classes without a
        # polymorphic_identity, e.g. the abstract `UniswapV2PoolTableBase`).
        mapper = getattr(pt, "__mapper__", None)
        identity = getattr(mapper, "polymorphic_identity", None) if mapper is not None else None
        if identity is None:
            continue
        if isinstance(identity, str):
            kind_string_to_class[identity] = cast(
                "type[LiquidityPoolTable | UniswapV4PoolTable]", pt
            )

    # Rebuild `pool_id_to_type` from the DB's raw `kind` STRING, falling back
    # to the family-base class if no `pool_types` entry matches the kind (a
    # pool whose concrete class wasn't in the `pool_types` input).
    fallback_class: type | None = None
    kind_str_map: dict = raw["pool_id_to_kind_string"]
    kind_u8_map: dict = raw["pool_id_to_kind"]
    pool_id_to_type: dict[tuple[PoolId, int], type[LiquidityPoolTable | UniswapV4PoolTable]] = {}
    for pool_id_u8, kind_u8 in kind_u8_map.items():
        pool_id = int(pool_id_u8)
        kind_str = kind_str_map.get(pool_id_u8)
        cls = kind_string_to_class.get(kind_str) if kind_str else None
        if cls is None:
            # Lazily cache + reuse the family-base fallback.
            if fallback_class is None:
                fallback_class = _POOL_KIND_TO_BASE.get(int(kind_u8))
            cls = fallback_class
        if cls is not None:
            pool_id_to_type[pool_id, int(kind_u8)] = cast(
                "type[LiquidityPoolTable | UniswapV4PoolTable]", cls
            )

    logger.debug(
        f"Built graph at +{time.perf_counter() - start:.1f}s: {len(raw['edges'])} edges",
    )

    return _PreparedGraph(
        edges=list(raw["edges"]),
        v2v3_addresses={int(pid): addr for pid, addr in raw["v2v3_addresses"].items()},
        v4_lookups={int(pid): (mgr, hsh) for pid, (mgr, hsh) in raw["v4_lookups"].items()},
        pool_id_to_type=pool_id_to_type,
    )


def _build_path_steps(
    path: list[tuple[PoolId, int]],
    v2v3_addresses: dict[PoolId, ChecksummedAddress],
    v4_lookups: dict[PoolId, tuple[ChecksummedAddress, str]],
    pool_id_to_type: dict[tuple[PoolId, int], type[LiquidityPoolTable | UniswapV4PoolTable]],
) -> list[PathStep]:
    """Convert a raw Rust path ``(pool_id, pool_kind_u8)`` into ``PathStep`` objects.

    The ``pool_kind_u8`` discriminant tells us V2 / V3 / V4 and selects which
    address lookup to use. The exact concrete table class for each pool is
    recovered from ``pool_id_to_type`` — a map built during ``_prepare_graph``
    that records the specific subclass (e.g. ``SushiswapV3PoolTable``) for
    every pool ID encountered.

    Returns:
        A list of ``PathStep`` objects with resolved addresses + hashes.

    """
    steps: list[PathStep] = []
    for pool_id, pool_kind_u8 in path:
        # Key by ``(pool_id, pool_kind_u8)`` and fall back to the family base
        # (`_POOL_KIND_TO_BASE`) so a V2/V4 pool-id collision (independent id
        # counters) never collapses a pool to the wrong family — the Rust
        # seam's `pool_id_to_kind*` maps can only keep ONE family per id.
        pool_type = pool_id_to_type.get((pool_id, pool_kind_u8))
        if pool_type is None:
            pool_base = _POOL_KIND_TO_BASE.get(pool_kind_u8)
            pool_type = cast("type[LiquidityPoolTable | UniswapV4PoolTable]", pool_base)
        if pool_kind_u8 == _POOL_KIND_V4:
            manager_address, pool_hash = v4_lookups[pool_id]
            steps.append(PathStep(address=manager_address, hash=pool_hash, type=pool_type))
        else:
            steps.append(PathStep(address=v2v3_addresses[pool_id], type=pool_type))

    return steps


def _prepare_traversal_plan(
    start_tokens: set[ChecksummedAddress],
    end_tokens: set[ChecksummedAddress],
) -> dict[tuple[ChecksummedAddress, ChecksummedAddress], Direction]:
    """Prepare a traversal plan that will cover all combinations from the given starting and ending.

    sets.

    Returns:
        The computed value.

    """
    # Assemble an exhaustive plan based on the Cartesian product of all start and end nodes:
    # e.g. P(a|b -> a|b) == P(a->a) + P(a->b) + P(b->a) + P(b->b)
    traversal_plan: dict[
        tuple[ChecksummedAddress, ChecksummedAddress],
        Direction,
    ] = dict.fromkeys(
        itertools.product(start_tokens, end_tokens),
        Direction.FORWARD,
    )

    # Optimize traversal plan by consolidating parallel forward paths for token pairs found in the
    # starting and ending sets. If a forward path is known, the parallel reverse path can be
    # efficiently included without performing another traversal of the graph:
    # e.g. P(a->b) + P(b->a) == P(a<->b)
    tokens_used_for_start_and_end = start_tokens & end_tokens
    if len(tokens_used_for_start_and_end) > 1:
        logger.debug("Optimizing traversal plan.")
        for start_token, end_token in itertools.combinations(tokens_used_for_start_and_end, 2):
            traversal_plan[start_token, end_token] = Direction.FORWARD_AND_REVERSE
            del traversal_plan[end_token, start_token]

    return traversal_plan


def _convert_pool_type_filter(
    pool_type_per_depth: Sequence[set[type] | None] | None,
) -> list[set[int] | None] | None:
    """Convert the Python ``set[type]`` per-depth filter to Rust u8 discriminants.

    Returns:
        A list of ``None`` / ``set[int]`` entries, or ``None`` if no filter.

    """
    if pool_type_per_depth is None:
        return None
    result: list[set[int] | None] = []
    for allowed in pool_type_per_depth:
        if allowed is None:
            result.append(None)
        else:
            result.append({_pool_kind_for_type(pt) for pt in allowed})
    return result


def find_paths(
    *,
    chain_id: int,
    start_tokens: Iterable[ChecksummedAddress | str],
    end_tokens: Iterable[ChecksummedAddress | str],
    min_depth: int = 2,
    max_depth: int | None = None,
    pool_types: Sequence[type] = (LiquidityPoolTable, UniswapV4PoolTable),
    db: DatabaseSessionManager,
    pool_type_per_depth: Sequence[set[type] | None] | None = None,
    allowed_intermediate_tokens: Iterable[ChecksummedAddress | str] | None = None,
) -> Iterator[Sequence[PathStep]]:
    """Find paths from each of the given start tokens to each of the given end tokens.

    Uses a depth-first search strategy. The search will exhaustively discover paths
    from a minimum depth to an optional maximum.

    Paths may be constrained to a subset of pool types. If not specified, all valid
    pool types will be included.

    Args:
        chain_id: The chain ID to restrict pool and token queries.
        start_tokens: Token addresses to use as the start of each path.
        end_tokens: Token addresses to use as the end of each path.
        min_depth: The minimum number of hops in yielded paths.
        max_depth: The optional maximum number of hops in yielded paths.
        pool_types: Database model classes for the pool types to include in the
            graph (default: ``LiquidityPoolTable`` and ``UniswapV4PoolTable``).
        db: The database session manager used to open a read session.
        pool_type_per_depth: If set, a sequence of allowed pool type sets at each
            depth. Depth 0 = first hop, depth 1 = second hop, etc. A ``None`` entry
            allows all pool types at that depth. When provided, edges whose
            pool_type is not in the allowed set are pruned before recursion.
        allowed_intermediate_tokens: If set, restrict the graph to only these token
            addresses as intermediate nodes. Pools connecting any non-whitelisted
            intermediate token are excluded from the graph. Use this to filter out
            tax tokens, fee-on-transfer tokens, and low-quality pairs that would
            waste simulation gas.

    Yields:
        Sequence[PathStep]: A valid arbitrage path from a start token to an end token.

    Raises:
        DegenbotValueError: If no pools are found for the given chain ID or tokens.

    """
    # @dev Liquidity pool lookups using a token ID are implicitly filtered for the chain ID, since
    # token addresses are unique to the chain. WHERE clauses can therefore be omitted from SELECTs.

    start = time.perf_counter()

    with db() as session:
        allowed_token_ids: set[TokenId] | None = None
        if allowed_intermediate_tokens is not None:
            allowed_token_ids = set(
                resolve_token_ids(
                    chain_id,
                    (get_checksum_address(tok) for tok in allowed_intermediate_tokens),
                    session,
                ).values(),
            )

        prepared = _prepare_graph(
            chain_id=chain_id,
            pool_types=pool_types,
            session=session,
            allowed_intermediate_tokens=allowed_token_ids,
        )

        rust_filter = _convert_pool_type_filter(pool_type_per_depth)

        traversal_plan = _prepare_traversal_plan(
            start_tokens={get_checksum_address(token) for token in start_tokens},
            end_tokens={get_checksum_address(token) for token in end_tokens},
        )

        for (start_token, end_token), direction in traversal_plan.items():
            start_token_id = resolve_token_ids(chain_id, [start_token], session).get(start_token)
            if start_token_id is None:
                msg = f"Start token {start_token} was not found in the database."
                raise DegenbotValueError(message=msg)

            end_token_id = resolve_token_ids(chain_id, [end_token], session).get(end_token)
            if end_token_id is None:
                msg = f"End token {end_token} was not found in the database."
                raise DegenbotValueError(message=msg)

            logger.debug(
                f"Finding paths from {start_token} "
                f"(id {start_token_id}) -> {end_token} (id {end_token_id})",
            )

            # A permutation filter implies an exact hop depth: don't yield
            # shorter cycles that merely prefix-match the first N depths
            # (e.g. a 3-depth V3-V3-V2 filter must not leak 2-hop V3-V3).
            effective_min_depth = (
                min_depth
                if pool_type_per_depth is None
                else max(min_depth, len(pool_type_per_depth))
            )

            logger.debug(f"Performing generic {max_depth}-pool path search")

            # The Rust DFS returns a lazy iterator — paths are yielded one at
            # a time, so memory is bounded even for graphs that produce
            # millions of paths.
            path_iter = find_paths_rust(
                prepared.edges,
                start_token_id,
                end_token_id,
                effective_min_depth,
                max_depth,
                direction == Direction.FORWARD_AND_REVERSE,
                rust_filter,
            )

            for raw_path in path_iter:
                yield _build_path_steps(
                    raw_path,
                    prepared.v2v3_addresses,
                    prepared.v4_lookups,
                    prepared.pool_id_to_type,
                )

            logger.debug(
                f"Completed structured generic search (max depth {max_depth}) "
                f"at +{time.perf_counter() - start:.1f}s",
            )


async def find_paths_async(
    *,
    chain_id: int,
    start_tokens: Iterable[ChecksummedAddress | str],
    end_tokens: Iterable[ChecksummedAddress | str],
    min_depth: int = 2,
    max_depth: int | None = None,
    pool_types: Sequence[type] = [LiquidityPoolTable, UniswapV4PoolTable],
    db: DatabaseSessionManager,
    pool_type_per_depth: Sequence[set[type] | None] | None = None,
    allowed_intermediate_tokens: Iterable[ChecksummedAddress | str] | None = None,
) -> AsyncIterator[Sequence[PathStep]]:
    """Async version of ``find_paths``.

    The Rust DFS runs in a single GIL-released call and returns all results at
    once. This async wrapper yields them individually so consumers can iterate
    asynchronously. Periodic ``asyncio.sleep(0)`` calls give other tasks a
    chance to run during long searches.

    Args:
        chain_id: The chain ID to restrict pool and token queries.
        start_tokens: Token addresses to use as the start of each path.
        end_tokens: Token addresses to use as the end of each path.
        min_depth: The minimum number of hops in yielded paths.
        max_depth: The optional maximum number of hops in yielded paths.
        pool_types: Database model classes for the pool types to include in the
            graph (default: ``LiquidityPoolTable`` and ``UniswapV4PoolTable``).
        db: The database session manager used to open a read session.
        pool_type_per_depth: If set, a sequence of allowed pool type sets at each
            depth. Depth 0 = first hop, depth 1 = second hop, etc. A ``None`` entry
            allows all pool types at that depth. When provided, edges whose
            pool_type is not in the allowed set are pruned before recursion.
        allowed_intermediate_tokens: If set, restrict the graph to only these token
            addresses as intermediate nodes. Pools connecting any non-whitelisted
            intermediate token are excluded from the graph. Use this to filter out
            tax tokens, fee-on-transfer tokens, and low-quality pairs that would
            waste simulation gas.

    Yields:
        Sequences of PathStep objects representing arbitrage paths.

    """
    # The sync `find_paths` generator is driven by the async event loop: we
    # iterate it synchronously and yield each result. The Rust DFS call inside
    # `find_paths` releases the GIL via `py.detach()`, so other Python tasks
    # can run during the search.
    #
    # Discovery-phase progress log (NY4EFN): emit a `[pathfinding]` heartbeat
    # every `_DISCOVERY_HEARTBEAT_INTERVAL_S` of yielded paths so a future hang
    # is visible at a glance, not just "78% CPU, no logs". This fires while
    # paths are streaming (the common prior-run shape: ~317k yields). A
    # zero-yield grind blocks the asyncio event loop on the same thread, so
    # this Python-side log cannot fire there — the Rust `OwnedPathFinder`
    # emits a GIL-free stderr heartbeat (`[pathfinding] discovery heartbeat:`)
    # for that case; together they cover both shapes.
    discovery_start = time.perf_counter()
    discovery_yielded = 0
    discovery_last_log = discovery_start

    for path in find_paths(
        chain_id=chain_id,
        start_tokens=start_tokens,
        end_tokens=end_tokens,
        min_depth=min_depth,
        max_depth=max_depth,
        pool_types=pool_types,
        db=db,
        pool_type_per_depth=pool_type_per_depth,
        allowed_intermediate_tokens=allowed_intermediate_tokens,
    ):
        await asyncio.sleep(0)
        yield path
        discovery_yielded += 1
        now = time.perf_counter()
        if now - discovery_last_log >= _DISCOVERY_HEARTBEAT_INTERVAL_S:
            logger.info(
                "[pathfinding] discovery progress: paths_yielded=%d elapsed=%.1fs",
                discovery_yielded,
                now - discovery_start,
            )
            discovery_last_log = now

    logger.info(
        "[pathfinding] discovery complete: paths_yielded=%d elapsed=%.1fs",
        discovery_yielded,
        time.perf_counter() - discovery_start,
    )
