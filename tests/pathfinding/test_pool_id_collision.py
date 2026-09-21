"""V2/V4 pool-id namespace collision coverage for the Rust ``PathStepBuilder``.

The graph node key (`pool_id`) is NOT globally unique across pool families:
V2/V3 share the `pools.id` counter, while V4 uses an independent
`managed_pool_id` counter that overlaps it (measured: 116,224 V4 ids collide
with a V2/V3 pools.id on the mainnet DB). The Rust `fetch_path_graph_edges`
seam namespaces every V4 graph id above `1 << 32`, so the builder's
`pool_id_to_kind_string` / `v4_lookups` lookups are keyed by the SAME
namespaced id the DFS yields. This test pins that contract through the Rust
builder now that the Python `_build_path_steps` helper is gone.
"""

from degenbot.database.models.pools import (
    UniswapV2PoolTable,
    UniswapV2PoolTableBase,
    UniswapV4PoolTable,
    UniswapV4PoolTableBase,
)
from degenbot.pathfinding import PathStep, PathStepBuilder, PoolKind

V2_ADDRESS = "0x" + "22" * 20
V4_MANAGER = "0x" + "33" * 20
V4_POOL_HASH = "0xabcdef1234567890"

# Mirrors `degenbot_db::pathfinding::V4_POOL_ID_OFFSET`.
V4_POOL_ID_OFFSET = 1 << 32


def test_path_step_builder_disambiguates_colliding_v2_v4_pool_id() -> None:
    """A V2 pool and a V4 pool sharing the same NUMERIC `pool_id` must
    reconstruct to their OWN pools — right family, right address, right hash.

    The V4 pool's graph id is `managed_pool_id + V4_POOL_ID_OFFSET`, so its
    namespaced id differs from the V2 `pools.id` even though the counters
    collide.
    """
    shared_numeric_id = 7
    v2_graph_id = shared_numeric_id
    v4_graph_id = shared_numeric_id + V4_POOL_ID_OFFSET

    builder = PathStepBuilder(
        pool_types=[UniswapV2PoolTable, UniswapV4PoolTable],
        pool_id_to_kind_string={
            v2_graph_id: "uniswap_v2",
            v4_graph_id: "uniswap_v4",
        },
        v2v3_addresses={v2_graph_id: V2_ADDRESS},
        v4_lookups={v4_graph_id: (V4_MANAGER, V4_POOL_HASH)},
        step_cls=PathStep,
    )

    steps = builder.build([(v2_graph_id, PoolKind.V2), (v4_graph_id, PoolKind.V4)])

    assert issubclass(steps[0].type, UniswapV2PoolTableBase)
    assert steps[0].address == V2_ADDRESS
    assert steps[0].hash is None

    assert issubclass(steps[1].type, UniswapV4PoolTableBase)
    assert steps[1].address == V4_MANAGER
    assert steps[1].hash == V4_POOL_HASH


def test_path_step_builder_falls_back_to_family_base() -> None:
    """A pool whose concrete `kind` string is absent from `pool_types`
    recovers its family-base class, not a None/KeyError."""
    graph_id = 11
    builder = PathStepBuilder(
        pool_types=[UniswapV2PoolTable],
        pool_id_to_kind_string={graph_id: "sushiswap_v2"},
        v2v3_addresses={graph_id: V2_ADDRESS},
        v4_lookups={},
        step_cls=PathStep,
    )
    (step,) = builder.build([(graph_id, PoolKind.V2)])
    assert issubclass(step.type, UniswapV2PoolTableBase)
    assert step.address == V2_ADDRESS
