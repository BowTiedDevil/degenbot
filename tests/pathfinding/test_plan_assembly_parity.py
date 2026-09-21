"""Parity oracle for the Rust-ported pathfinding plan assembly.

Pins the behavior formerly implemented by the Python ``_prepare_traversal_plan``,
``_convert_pool_type_filter``, and ``_build_path_steps`` helpers, now owned by
the Rust core seams (``prepare_traversal_plan`` / ``convert_pool_type_filter`` /
``PathStepBuilder``). Fixture-driven; no live RPC, no anvil.
"""

from __future__ import annotations

from degenbot.database.models.pools import (
    SushiswapV3PoolTable,
    UniswapV2PoolTable,
    UniswapV2PoolTableBase,
    UniswapV3PoolTable,
    UniswapV4PoolTable,
)
from degenbot.pathfinding import (
    PathStep,
    PathStepBuilder,
    PoolKind,
    convert_pool_type_filter,
    prepare_traversal_plan,
)


def test_product_plan_is_forward_only() -> None:
    """A plan over distinct boundary sets is the plain Cartesian product."""
    assert prepare_traversal_plan([1, 2], [3, 4], 2, None) == [
        (1, 3, False, 2),
        (1, 4, False, 2),
        (2, 3, False, 2),
        (2, 4, False, 2),
    ]


def test_shared_boundaries_consolidate_reverse() -> None:
    """Tokens in both boundary sets merge ``(a, b)`` + ``(b, a)`` into one
    ``FORWARD_AND_REVERSE`` traversal."""
    assert prepare_traversal_plan([1, 2], [1, 2], 2, None) == [
        (1, 1, False, 2),
        (1, 2, True, 2),
        (2, 2, False, 2),
    ]


def test_filter_length_floors_min_depth() -> None:
    """A per-depth filter of length N implies an exactly-N-hop permutation."""
    assert prepare_traversal_plan([1], [1], 2, 2) == [(1, 1, False, 2)]
    assert prepare_traversal_plan([1], [1], 2, 4) == [(1, 1, False, 4)]
    assert prepare_traversal_plan([1], [1], 4, 2) == [(1, 1, False, 4)]


def test_boundary_ids_are_deduplicated() -> None:
    assert prepare_traversal_plan([1, 1, 2], [2, 2], 2, None) == [
        (1, 2, False, 2),
        (2, 2, False, 2),
    ]


def test_convert_filter_maps_classes_to_typed_kinds() -> None:
    assert convert_pool_type_filter(None) is None
    converted = convert_pool_type_filter(
        [{UniswapV2PoolTable}, None, {UniswapV3PoolTable, UniswapV4PoolTable}],
    )
    assert converted == [{PoolKind.V2}, None, {PoolKind.V3, PoolKind.V4}]


def test_builder_recovers_concrete_subclass() -> None:
    """The builder maps a raw DB ``kind`` string to the exact concrete table
    class in ``pool_types`` — not merely the family base."""
    builder = PathStepBuilder(
        pool_types=[SushiswapV3PoolTable, UniswapV2PoolTable],
        pool_id_to_kind_string={5: "sushiswap_v3", 6: "uniswap_v2"},
        v2v3_addresses={5: "0x" + "55" * 20, 6: "0x" + "66" * 20},
        v4_lookups={},
        step_cls=PathStep,
    )
    steps = builder.build([(5, PoolKind.V3), (6, PoolKind.V2)])
    assert steps[0].type is SushiswapV3PoolTable
    assert steps[1].type is UniswapV2PoolTable
    assert steps[0].hash is None


def test_builder_family_fallback_for_absent_concrete_class() -> None:
    """A pool whose concrete ``kind`` string is not in ``pool_types`` recovers
    its family-base class."""
    builder = PathStepBuilder(
        pool_types=[UniswapV2PoolTable],
        pool_id_to_kind_string={9: "some_unlisted_v2"},
        v2v3_addresses={9: "0x" + "99" * 20},
        v4_lookups={},
        step_cls=PathStep,
    )
    (step,) = builder.build([(9, PoolKind.V2)])
    assert step.type is UniswapV2PoolTableBase


def test_builder_v4_uses_manager_address_and_hash() -> None:
    manager = "0x" + "aa" * 20
    pool_hash = "0xdeadbeef"
    builder = PathStepBuilder(
        pool_types=[UniswapV4PoolTable],
        pool_id_to_kind_string={(1 << 32) + 3: "uniswap_v4"},
        v2v3_addresses={},
        v4_lookups={(1 << 32) + 3: (manager, pool_hash)},
        step_cls=PathStep,
    )
    (step,) = builder.build([((1 << 32) + 3, PoolKind.V4)])
    assert step.type is UniswapV4PoolTable
    assert step.address == manager
    assert step.hash == pool_hash
