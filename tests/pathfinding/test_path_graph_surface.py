"""The typed `build_path_graph` surface — the path-graph wire contract.

`build_path_graph` returns ONE typed `PathGraph` value, not a flattened dict of
raw containers: the edge list, the address lookups, the candidate-token set,
and the step builder are attributes of one pyclass, and the Python
`_prepare_graph` passes that value straight through to the DFS instead of
un-packing dict keys. This module pins that surface by behavior — the attribute
names, the value shapes crossing the FFI, and the absence of any dict-key
access — so a reintroduced `raw["edges"]`-style wire contract (or a Python-side
re-flattening shim between the seam and the DFS) fails here.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest

from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import WRAPPED_NATIVE_TOKENS, ZERO_ADDRESS
from degenbot.pathfinding import (
    PathGraph,
    PathStep,
    PoolKind,
    build_path_graph,
    classify_pool_kinds,
)
from degenbot.pathfinding._pathfinding import _prepare_graph
from degenbot.types.chain import ChainId
from tests.helpers.database import seed_v2_topology, seed_v3_topology, seed_v4_topology

if TYPE_CHECKING:
    import pathlib

CHAIN_ID = ChainId.ETH
WETH_ADDRESS = WRAPPED_NATIVE_TOKENS[CHAIN_ID]
NATIVE_ADDRESS = ZERO_ADDRESS
POOL_MANAGER = get_checksum_address("0x" + "99" * 20)
ALL_KINDS = (PoolKind.V2, PoolKind.V3, PoolKind.V4)

# A V4 graph id is namespaced above every `pools.id` (the core's offset).
V4_POOL_ID_OFFSET = 1 << 32


@pytest.fixture
def db(tmp_path: pathlib.Path) -> pathlib.Path:
    """A seeded V2 + V3 + V4 topology (one pool per family at minimum)."""
    db_path = tmp_path / "graph_surface.db"
    seed_v2_topology(
        db_path,
        [
            (
                get_checksum_address("0x" + "21" * 20),
                WETH_ADDRESS,
                get_checksum_address("0x" + "aa" * 20),
            ),
            (
                get_checksum_address("0x" + "22" * 20),
                get_checksum_address("0x" + "aa" * 20),
                WETH_ADDRESS,
            ),
        ],
        chain_id=CHAIN_ID,
    )
    seed_v3_topology(
        db_path,
        [
            (
                get_checksum_address("0x" + "31" * 20),
                WETH_ADDRESS,
                get_checksum_address("0x" + "bb" * 20),
                500,
                10,
            ),
            (
                get_checksum_address("0x" + "32" * 20),
                get_checksum_address("0x" + "bb" * 20),
                WETH_ADDRESS,
                500,
                10,
            ),
        ],
        chain_id=CHAIN_ID,
    )
    seed_v4_topology(
        db_path,
        [
            ("0x" + "0" * 63 + "1", NATIVE_ADDRESS, WETH_ADDRESS, ZERO_ADDRESS, 3000),
            ("0x" + "0" * 63 + "2", WETH_ADDRESS, NATIVE_ADDRESS, ZERO_ADDRESS, 3000),
        ],
        pool_manager_address=POOL_MANAGER,
        chain_id=CHAIN_ID,
    )
    return db_path


def _build(db: pathlib.Path, *, step_cls: type[PathStep] | None = None) -> PathGraph:
    return build_path_graph(
        database_path=str(db),
        chain_id=CHAIN_ID,
        pool_kinds=classify_pool_kinds(ALL_KINDS),
        step_cls=step_cls,
    )


def test_build_path_graph_returns_a_typed_graph_value(db: pathlib.Path) -> None:
    """The seam returns the typed value, and its containers carry FFI types."""
    graph = _build(db)

    assert isinstance(graph, PathGraph)
    assert not isinstance(graph, dict)

    # Edges are `(token0_id, token1_id, pool_id, PoolKind)` tuples — the kind is
    # the typed pyclass, never a bare int or a string tag.
    assert graph.edges, "the seeded topology must yield edges"
    for token0_id, token1_id, pool_id, pool_kind in graph.edges:
        assert isinstance(token0_id, int)
        assert isinstance(token1_id, int)
        assert isinstance(pool_id, int)
        assert isinstance(pool_kind, PoolKind)

    # Candidate tokens: a set of token ids, and every edge endpoint is one.
    assert isinstance(graph.candidate_tokens, set)
    assert all(isinstance(token_id, int) for token_id in graph.candidate_tokens)
    edge_tokens = {token for edge in graph.edges for token in edge[:2]}
    assert edge_tokens <= graph.candidate_tokens

    # `pool_id_to_kind` covers every pool read from the DB (a superset of the
    # edge pool ids, which the candidate-token filter may drop).
    edge_pool_ids = {edge[2] for edge in graph.edges}
    assert edge_pool_ids <= set(graph.pool_id_to_kind)
    assert all(isinstance(pool_kind, PoolKind) for pool_kind in graph.pool_id_to_kind.values())


def test_v2v3_address_lookup_is_checksummed_strings(db: pathlib.Path) -> None:
    """The V2/V3 lookup maps pool ids to checksummed addresses, kind-tagged."""
    graph = _build(db)

    assert graph.v2v3_addresses, "the seeded V2/V3 pools must be looked up"
    for pool_id, address in graph.v2v3_addresses.items():
        assert isinstance(pool_id, int)
        assert isinstance(address, str)
        assert get_checksum_address(address) == address
        assert graph.pool_id_to_kind[pool_id] in {PoolKind.V2, PoolKind.V3}


def test_v4_lookup_carries_the_namespaced_id_and_pool_hash(db: pathlib.Path) -> None:
    """The V4 lookup maps the offset graph id to `(manager address, pool_hash)`."""
    graph = _build(db)

    assert graph.v4_lookups, "the seeded V4 pools must be looked up"
    for pool_id, lookup in graph.v4_lookups.items():
        assert pool_id >= V4_POOL_ID_OFFSET, "V4 graph ids are namespaced above `pools.id`"
        manager_address, pool_hash = lookup
        assert get_checksum_address(manager_address) == manager_address
        assert pool_hash.startswith("0x")
        assert len(pool_hash) == 66
        assert graph.pool_id_to_kind[pool_id] is PoolKind.V4


def test_graph_value_has_no_dict_key_access(db: pathlib.Path) -> None:
    """The wire contract is attributes; subscripting the value is a TypeError."""
    graph = _build(db)

    for key in ("edges", "v2v3_addresses", "v4_lookups", "candidate_tokens"):
        with pytest.raises(TypeError):
            graph[key]  # ty: ignore[not-subscriptable]  # pyright: ignore[reportIndexIssue]


def test_prepare_graph_passes_the_typed_value_through(db: pathlib.Path) -> None:
    """`_prepare_graph` hands the typed value on (no un-flattening shim)."""
    graph = _prepare_graph(
        chain_id=CHAIN_ID,
        pool_types=ALL_KINDS,
        database_path=db,
    )

    assert isinstance(graph, PathGraph)
    assert graph.edges
    assert graph.candidate_tokens
    # The step builder is part of the same value: paths from these edges resolve
    # through this graph's own lookups.
    assert graph.v2v3_addresses
    v2v3_pool_id = next(iter(graph.v2v3_addresses))
    steps = graph.build_steps([(v2v3_pool_id, graph.pool_id_to_kind[v2v3_pool_id])])
    assert steps
    assert isinstance(steps[0], PathStep)
    assert steps[0].address == graph.v2v3_addresses[v2v3_pool_id]


def test_probe_build_without_step_cls_refuses_to_build_steps(db: pathlib.Path) -> None:
    """A probe-style build (no `step_cls`) yields a graph without a builder."""
    graph = _build(db)

    pool_id = next(iter(graph.v2v3_addresses))
    with pytest.raises(ValueError, match="step_cls"):
        graph.build_steps([(pool_id, graph.pool_id_to_kind[pool_id])])
