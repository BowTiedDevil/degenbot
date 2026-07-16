"""Pathfinding utilities for discovering arbitrage routes."""

import asyncio
import enum
import itertools
import time
from collections.abc import AsyncIterator, Iterable, Iterator, Sequence
from dataclasses import dataclass
from typing import cast

import sqlalchemy
from eth_typing import ChecksumAddress
from sqlalchemy.orm import Session

from degenbot.checksum_cache import get_checksum_address
from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.database.models.pools import (
    LiquidityPoolTable,
    PoolManagerTable,
    UniswapV2PoolTableBase,
    UniswapV3PoolTableBase,
    UniswapV4PoolTable,
)
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.exceptions.base import DegenbotValueError
from degenbot.logging import logger
from degenbot.pathfinding import build_path_graph, find_paths_rust

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


@dataclass(slots=True, frozen=True)
class PathStep:
    """PathStep class."""

    address: ChecksumAddress
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


def _get_tokens_with_min_degree(
    degree: int,
    session: Session,
    chain_id: int,
    pool_types: Sequence[type],
) -> set[TokenId]:
    """Candidate tokens appearing in ≥ `degree` pools (SQLAlchemy oracle path).

    Retained as the §4.2 parity oracle for the `_prepare_graph_sqlalchemy`
    fallback (used when the bound DB is in-memory `:memory:` — the Rust
    `build_path_graph` seam opens a separate in-memory connection that can't
    share SQLAlchemy's in-memory DB). The primary file-backed path runs the
    fetch in Rust (`degenbot_db::fetch_tokens_with_min_degree` via
    `build_path_graph`).

    Returns:
        The set of candidate token IDs.

    """
    token_count_selects: list[sqlalchemy.Select[tuple[TokenId]]] = []
    for pool_type in pool_types:
        if issubclass(pool_type, LiquidityPoolTable):
            token_count_selects.extend((
                sqlalchemy.select(pool_type.token0_id.label("token_id")).where(
                    pool_type.chain == chain_id,
                ),
                sqlalchemy.select(pool_type.token1_id.label("token_id")).where(
                    pool_type.chain == chain_id,
                ),
            ))
        if issubclass(pool_type, UniswapV4PoolTable):
            token_count_selects.extend((
                sqlalchemy.select(pool_type.currency0_id.label("token_id")).where(
                    pool_type.manager.has(chain=chain_id),
                ),
                sqlalchemy.select(pool_type.currency1_id.label("token_id")).where(
                    pool_type.manager.has(chain=chain_id),
                ),
            ))
    token_count_subq = sqlalchemy.union_all(*token_count_selects).subquery()
    token_counts_greater_than_two_subq = (
        sqlalchemy
        .select(
            token_count_subq.columns["token_id"],
            sqlalchemy.func.count().label("pool_count"),
        )
        .group_by(token_count_subq.columns["token_id"])
        .having(sqlalchemy.func.count() >= degree)
        .subquery()
    )
    return set(
        session.scalars(
            sqlalchemy.select(token_counts_greater_than_two_subq.columns["token_id"]),
        ).all(),
    )


@dataclass(slots=True)
class _PreparedGraph:
    """The flat edge list + address lookups produced by ``_prepare_graph``.

    Attributes:
        edges: Flat ``(token0, token1, pool_id, pool_kind_u8)`` tuples for Rust.
        v2v3_addresses: Maps V2/V3 pool IDs to their on-chain addresses.
        v4_lookups: Maps V4 pool IDs to ``(manager_address, pool_hash)``.
        pool_id_to_type: Maps every pool ID to its concrete table class,
            used to reconstruct ``PathStep.type`` accurately (V2 vs V3).

    """

    edges: list[tuple[TokenId, TokenId, PoolId, int]]
    v2v3_addresses: dict[PoolId, ChecksumAddress]
    v4_lookups: dict[PoolId, tuple[ChecksumAddress, str]]
    pool_id_to_type: dict[PoolId, type[LiquidityPoolTable | UniswapV4PoolTable]]


def _prepare_graph(
    chain_id: int,
    pool_types: Sequence[type],
    session: Session,
    allowed_intermediate_tokens: set[TokenId] | None = None,
) -> _PreparedGraph:
    """Build the flat edge list + address lookup dicts for the Rust DFS.

    Delegates the bulk DB read + candidate-token edge filter to the Rust core
    via `build_path_graph` (AF7OEL) for file-backed DBs (production + the
    file-backed pathfinding fixtures). For in-memory `:memory:` DBs the Rust
    seam opens a separate connection that can't share SQLAlchemy's in-memory
    DB, so those fall back to the SQLAlchemy parity oracle
    (`_prepare_graph_sqlalchemy` — the §4.2 oracle kept until the seam can
    bind a shared in-memory handle).

    Returns:
        A ``_PreparedGraph`` with flat edges + address lookup dicts.

    """
    engine = session.bind
    db_path = getattr(getattr(engine, "url", None), "database", None) if engine else None

    # In-memory DBs can't be shared with a separate Rust connection — use the
    # SQLAlchemy parity oracle (§4.2). File-backed DBs take the Rust seam.
    if not db_path or db_path == ":memory:":
        return _prepare_graph_sqlalchemy(
            chain_id=chain_id,
            pool_types=pool_types,
            session=session,
            allowed_intermediate_tokens=allowed_intermediate_tokens,
        )
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
    pool_id_to_type: dict[PoolId, type[LiquidityPoolTable | UniswapV4PoolTable]] = {}
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
            pool_id_to_type[pool_id] = cast("type[LiquidityPoolTable | UniswapV4PoolTable]", cls)

    logger.debug(
        f"Built graph at +{time.perf_counter() - start:.1f}s: {len(raw['edges'])} edges",
    )

    return _PreparedGraph(
        edges=list(raw["edges"]),
        v2v3_addresses={int(pid): addr for pid, addr in raw["v2v3_addresses"].items()},
        v4_lookups={int(pid): (mgr, hsh) for pid, (mgr, hsh) in raw["v4_lookups"].items()},
        pool_id_to_type=pool_id_to_type,
    )


def _prepare_graph_sqlalchemy(
    *,
    chain_id: int,
    pool_types: Sequence[type],
    session: Session,
    allowed_intermediate_tokens: set[TokenId] | None = None,
) -> _PreparedGraph:
    """SQLAlchemy parity oracle (§4.2) for in-memory `:memory:` DBs.

    The Rust `build_path_graph` seam opens a separate connection that can't
    share SQLAlchemy's in-memory DB, so `:memory:` DBs keep this Python
    build path as the parity oracle until the seam can bind a shared
    in-memory handle. File-backed DBs take `_prepare_graph_rust`.

    Returns:
        A ``_PreparedGraph`` with flat edges + address lookup dicts.

    """
    start = time.perf_counter()

    candidate_tokens = _get_tokens_with_min_degree(
        degree=2,
        session=session,
        chain_id=chain_id,
        pool_types=pool_types,
    )
    logger.debug(f"Found {len(candidate_tokens)} candidate tokens held by 2 or more pools")

    if allowed_intermediate_tokens is not None:
        before = len(candidate_tokens)
        candidate_tokens &= allowed_intermediate_tokens
        logger.debug(
            f"Token whitelist applied: {before} → {len(candidate_tokens)} candidate tokens",
        )

    edges: list[tuple[TokenId, TokenId, PoolId, int]] = []
    v2v3_addresses: dict[PoolId, ChecksumAddress] = {}
    v4_lookups: dict[PoolId, tuple[ChecksumAddress, str]] = {}
    pool_id_to_type: dict[PoolId, type[LiquidityPoolTable | UniswapV4PoolTable]] = {}

    for pool_type in pool_types:
        if issubclass(pool_type, UniswapV4PoolTable):
            for pool_id, currency0_id, currency1_id, manager_address, pool_hash in session.execute(
                sqlalchemy
                .select(
                    pool_type.id,
                    pool_type.currency0_id,
                    pool_type.currency1_id,
                    PoolManagerTable.address,
                    pool_type.pool_hash,
                )
                .join(pool_type.manager)
                .where(pool_type.manager.has(chain=chain_id)),
            ).all():
                if currency0_id in candidate_tokens and currency1_id in candidate_tokens:
                    edges.append((currency0_id, currency1_id, pool_id, _POOL_KIND_V4))
                    v4_lookups[pool_id] = (manager_address, pool_hash)
                    pool_id_to_type[pool_id] = pool_type

        elif issubclass(pool_type, LiquidityPoolTable):
            pool_kind = _pool_kind_for_type(pool_type)
            for pool_id, token0_id, token1_id, address in session.execute(
                sqlalchemy.select(
                    pool_type.id,
                    pool_type.token0_id,
                    pool_type.token1_id,
                    pool_type.address,
                ).where(pool_type.chain == chain_id),
            ).all():
                if token0_id in candidate_tokens and token1_id in candidate_tokens:
                    edges.append((token0_id, token1_id, pool_id, pool_kind))
                    v2v3_addresses[pool_id] = address
                    pool_id_to_type[pool_id] = pool_type

        logger.debug(f"Added edges for pool type {pool_type.__name__}")

    logger.debug(
        f"Built graph at +{time.perf_counter() - start:.1f}s: {len(edges)} edges",
    )

    return _PreparedGraph(
        edges=edges,
        v2v3_addresses=v2v3_addresses,
        v4_lookups=v4_lookups,
        pool_id_to_type=pool_id_to_type,
    )


def _build_path_steps(
    path: list[tuple[PoolId, int]],
    v2v3_addresses: dict[PoolId, ChecksumAddress],
    v4_lookups: dict[PoolId, tuple[ChecksumAddress, str]],
    pool_id_to_type: dict[PoolId, type[LiquidityPoolTable | UniswapV4PoolTable]],
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
        pool_type = pool_id_to_type[pool_id]
        if pool_kind_u8 == _POOL_KIND_V4:
            manager_address, pool_hash = v4_lookups[pool_id]
            steps.append(PathStep(address=manager_address, hash=pool_hash, type=pool_type))
        else:
            steps.append(PathStep(address=v2v3_addresses[pool_id], type=pool_type))

    return steps


def _prepare_traversal_plan(
    start_tokens: set[ChecksumAddress],
    end_tokens: set[ChecksumAddress],
) -> dict[tuple[ChecksumAddress, ChecksumAddress], Direction]:
    """Prepare a traversal plan that will cover all combinations from the given starting and ending.

    sets.

    Returns:
        The computed value.

    """
    # Assemble an exhaustive plan based on the Cartesian product of all start and end nodes:
    # e.g. P(a|b -> a|b) == P(a->a) + P(a->b) + P(b->a) + P(b->b)
    traversal_plan: dict[
        tuple[ChecksumAddress, ChecksumAddress],
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
    start_tokens: Iterable[ChecksumAddress | str],
    end_tokens: Iterable[ChecksumAddress | str],
    min_depth: int = 2,
    max_depth: int | None = None,
    pool_types: Sequence[type] = (LiquidityPoolTable, UniswapV4PoolTable),
    db: DatabaseSessionManager,
    pool_type_per_depth: Sequence[set[type] | None] | None = None,
    allowed_intermediate_tokens: Iterable[ChecksumAddress | str] | None = None,
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
                session.scalars(
                    sqlalchemy.select(Erc20TokenTable.id).where(
                        Erc20TokenTable.address.in_({
                            get_checksum_address(t) for t in allowed_intermediate_tokens
                        }),
                        Erc20TokenTable.chain == chain_id,
                    ),
                ).all(),
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
            start_token_id = session.scalar(
                sqlalchemy.select(Erc20TokenTable.id).where(
                    Erc20TokenTable.address == start_token,
                    Erc20TokenTable.chain == chain_id,
                ),
            )
            if start_token_id is None:
                msg = f"Start token {start_token} was not found in the database."
                raise DegenbotValueError(message=msg)

            end_token_id = session.scalar(
                sqlalchemy.select(Erc20TokenTable.id).where(
                    Erc20TokenTable.address == end_token,
                    Erc20TokenTable.chain == chain_id,
                ),
            )
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
    start_tokens: Iterable[ChecksumAddress | str],
    end_tokens: Iterable[ChecksumAddress | str],
    min_depth: int = 2,
    max_depth: int | None = None,
    pool_types: Sequence[type] = [LiquidityPoolTable, UniswapV4PoolTable],
    db: DatabaseSessionManager,
    pool_type_per_depth: Sequence[set[type] | None] | None = None,
    allowed_intermediate_tokens: Iterable[ChecksumAddress | str] | None = None,
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
