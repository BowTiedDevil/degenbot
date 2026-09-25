"""V2/V4 pool-id namespace collision coverage for the Rust ``PathStepBuilder``.

The graph node key is namespaced per family by the Rust DB seam. This test
pins that the builder keeps the typed family and byte-stable identity aligned
when V2 and V4 numeric counters collide.
"""

from degenbot.pathfinding import PathStep, PathStepBuilder, PoolKind

V2_ADDRESS = "0x" + "22" * 20
V4_MANAGER = "0x" + "33" * 20
V4_POOL_HASH = "0xabcdef1234567890"

# Mirrors `degenbot_db::pathfinding::V4_POOL_ID_OFFSET`.
V4_POOL_ID_OFFSET = 1 << 32


def test_path_step_builder_disambiguates_colliding_v2_v4_pool_id() -> None:
    shared_numeric_id = 7
    v2_graph_id = shared_numeric_id
    v4_graph_id = shared_numeric_id + V4_POOL_ID_OFFSET

    builder = PathStepBuilder(
        v2v3_addresses={v2_graph_id: V2_ADDRESS},
        v4_lookups={v4_graph_id: (V4_MANAGER, V4_POOL_HASH)},
        step_cls=PathStep,
    )

    steps = builder.build([(v2_graph_id, PoolKind.V2), (v4_graph_id, PoolKind.V4)])

    assert steps[0].type is PoolKind.V2
    assert steps[0].address == V2_ADDRESS
    assert steps[0].hash is None
    assert steps[1].type is PoolKind.V4
    assert steps[1].address == V4_MANAGER
    assert steps[1].hash == V4_POOL_HASH
