"""Unit test for the V2/V4 pool-id namespace collision (PUUP62 / NCNJSS).

The graph node key (`pool_id`) is NOT globally unique across pool families:
V2/V3 share the `pools.id` counter, while V4 uses an independent
`managed_pool_id` counter that overlaps it (measured: 107,966 V2∩V4 collisions
on the mainnet DB). `_prepare_graph` previously keyed `pool_id_to_type` by the
bare `pool_id`, so a V2 pool and a V4 pool sharing an id collapsed to ONE
class. A V2 DFS step whose id collides with a V4 pool then rebuilt as a
V4-typed step with `hash=None` -> the `v4-no-hash` registration skip (369k
observed) that silently dropped ~14.8k buildable V2 pools.

The fix: key `pool_id_to_type` by `(pool_id, pool_kind_u8)` and let
`_build_path_steps` fall back to the family base (`_POOL_KIND_TO_BASE`) when
the Rust seam's `pool_id_to_kind*` maps collapsed the concrete entry. Every
consumer only checks `issubclass(step.type, {V2,V3,V4}PoolTableBase)`, so the
family base is functionally equivalent (see the `_POOL_KIND_TO_BASE` doc).
"""

from degenbot.database.models.pools import (
    UniswapV2PoolTableBase,
    UniswapV4PoolTable,
    UniswapV4PoolTableBase,
)
from degenbot.pathfinding._pathfinding import (
    _POOL_KIND_V2,
    _POOL_KIND_V4,
    _build_path_steps,
)

V2_ADDRESS = "0x" + "22" * 20
V4_MANAGER = "0x" + "33" * 20
V4_POOL_HASH = "0xabcdef1234567890"


def test_build_path_steps_disambiguates_colliding_v2_v4_pool_id():
    """A V2 pool and a V4 pool sharing the same `pool_id` must reconstruct to
    their OWN families, never collapse to one (the `v4-no-hash` bug)."""
    shared_id = 7  # a V2 pool id and a V4 pool id collide (independent counters)

    v2v3_addresses = {shared_id: V2_ADDRESS}
    v4_lookups = {shared_id: (V4_MANAGER, V4_POOL_HASH)}

    # `pool_id_to_type` is keyed by `(pool_id, pool_kind_u8)`. Only the V4
    # entry survived the Rust seam's `pool_id_to_kind` collapse (the seam
    # writes V2/V3 first, then V4 overwrites the shared id). The V2 family
    # must be recovered by the `_POOL_KIND_TO_BASE` fallback.
    pool_id_to_type = {(shared_id, _POOL_KIND_V4): UniswapV4PoolTable}

    # A DFS path with a V2 edge and a V4 edge, both through `shared_id`.
    path = [
        (shared_id, _POOL_KIND_V2),
        (shared_id, _POOL_KIND_V4),
    ]

    steps = _build_path_steps(path, v2v3_addresses, v4_lookups, pool_id_to_type)

    # Hop 0 is the V2 pool: typed V2 family, address from v2v3_addresses,
    # NO hash (a V2 pool has no pool_hash).
    assert issubclass(steps[0].type, UniswapV2PoolTableBase), (
        f"V2 hop mis-typed as {steps[0].type.__name__}"
    )
    assert steps[0].address == V2_ADDRESS
    assert steps[0].hash is None

    # Hop 1 is the V4 pool: typed V4 family, hash from v4_lookups.
    assert issubclass(steps[1].type, UniswapV4PoolTableBase), (
        f"V4 hop mis-typed as {steps[1].type.__name__}"
    )
    assert steps[1].hash == V4_POOL_HASH
