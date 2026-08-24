"""Unit test for the V2/V4 pool-id namespace collision (PUUP62 / NCNJSS).

The graph node key (`pool_id`) is NOT globally unique across pool families:
V2/V3 share the `pools.id` counter, while V4 uses an independent
`managed_pool_id` counter that overlaps it (measured: 116,224 V4 ids collide
with a V2/V3 pools.id on the mainnet DB). Two failures followed from this:

1. **type collapse (original):** `pool_id_to_type` keyed by bare `pool_id`
collapsed a V2 + V4 pair into ONE class -> V2 steps rebuilt without a hash ->
the `v4-no-hash` registration skip (369k observed) that silently dropped
~14.8k buildable V2 pools. Fixed by keying on `(pool_id, pool_kind_u8)` with
the `_POOL_KIND_TO_BASE` family-base fallback.
2. **edge aliasing (follow-on, live-DB discovery):** the DFS edge list carried
both pools under ONE id, so the walker could use both edges as if they were a
single pool and yield paths that cannot close (148,896 broken paths measured).
Fixed by namespacing V4 graph ids above `_V4_POOL_ID_OFFSET` (Rust
`V4_POOL_ID_OFFSET`), with demangling in `_build_path_steps`. In production the
V4 id passed here is `managed_pool_id + 1 << 32`; this legacy-shape test
documents the `(pool_id, pool_kind)`-key disambiguation that remains the last
defense for any pre-namespace input.
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


from degenbot.pathfinding._pathfinding import _V4_POOL_ID_OFFSET


def test_build_path_steps_disambiguates_colliding_v2_v4_pool_id():
    """A V2 pool and a V4 pool sharing the same NUMERIC `pool_id` must
    reconstruct to their OWN pools — the right family, the right address,
    the right hash.

    Under the namespace contract, the V4 pool's graph id is
    ``managed_pool_id + _V4_POOL_ID_OFFSET``, so the two pools' graph ids
    are distinct even though their numeric ids collide; `_build_path_steps`
    demangles the V4 graph id back to the raw ``managed_pool_id`` before the
    `v4_lookups` lookup. The `(pool_id, pool_kind_u8)`-typed entry in
    `pool_id_to_type` uses the NAMESPACED id (that's what the Rust seam
    emits), and the V2 family recovers via the `_POOL_KIND_TO_BASE`
    fallback.
    """
    shared_numeric_id = 7  # V2 pools.id and V4 managed_pool_id collide

    v2_graph_id = shared_numeric_id
    v4_managed_id = shared_numeric_id
    v4_graph_id = v4_managed_id + _V4_POOL_ID_OFFSET

    v2v3_addresses = {v2_graph_id: V2_ADDRESS}
    # v4_lookups is keyed by the RAW managed_pool_id (post-demangle).
    v4_lookups = {v4_managed_id: (V4_MANAGER, V4_POOL_HASH)}

    # The Rust seam's kind maps are keyed by the NAMESPACED graph id.
    pool_id_to_type = {(v4_graph_id, _POOL_KIND_V4): UniswapV4PoolTable}

    # A DFS path with a V2 edge and a V4 edge, through the colliding
    # numeric id (V2 as-is, V4 namespaced).
    path = [
        (v2_graph_id, _POOL_KIND_V2),
        (v4_graph_id, _POOL_KIND_V4),
    ]

    steps = _build_path_steps(path, v2v3_addresses, v4_lookups, pool_id_to_type)

    # Hop 0 is the V2 pool: typed V2 family, address from v2v3_addresses,
    # NO hash (a V2 pool has no pool_hash).
    assert issubclass(steps[0].type, UniswapV2PoolTableBase), (
        f"V2 hop mis-typed as {steps[0].type.__name__}"
    )
    assert steps[0].address == V2_ADDRESS
    assert steps[0].hash is None

    # Hop 1 is the V4 pool: typed V4 family, hash from v4_lookups — and the
    # namespaced graph id demangled to the raw managed_pool_id on lookup.
    assert issubclass(steps[1].type, UniswapV4PoolTableBase), (
        f"V4 hop mis-typed as {steps[1].type.__name__}"
    )
    assert steps[1].hash == V4_POOL_HASH


def test_v4_graph_ids_are_namespaced():
    """The namespace invariant: a V4 graph id never equals a V2/V3 id from
    the same numeric space — demangling round-trips."""
    managed_id = 125_003  # the live V4 pool that triggered the discovery
    graph_id = managed_id + _V4_POOL_ID_OFFSET
    # A `pools.id` (up to ~678k observed live, realistically << 2^32) can
    # never alias a namespaced V4 id.
    assert graph_id > 1 << 20
    assert graph_id - _V4_POOL_ID_OFFSET == managed_id
