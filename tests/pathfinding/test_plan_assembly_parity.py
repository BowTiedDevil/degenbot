"""Parity oracle for the Rust-owned pathfinding plan and step seams.

The public boundary consumes typed ``PoolKind`` values only and preserves V2/V3
address identity plus V4 manager-address and pool-hash identity.
"""

from __future__ import annotations

from degenbot.pathfinding import (
    PathfindingRequest,
    PathStep,
    PathStepBuilder,
    PoolKind,
    convert_pool_type_filter,
    prepare_traversal_plan,
)


def test_pathfinding_request_defaults_to_every_supported_family() -> None:
    default = PathfindingRequest.__dataclass_fields__["pool_types"].default
    assert default == (PoolKind.V2, PoolKind.V3, PoolKind.V4)


def test_product_plan_is_forward_only() -> None:
    """A plan over distinct boundary sets is the plain Cartesian product."""
    assert prepare_traversal_plan([1, 2], [3, 4], 2, None) == [
        (1, 3, False, 2),
        (1, 4, False, 2),
        (2, 3, False, 2),
        (2, 4, False, 2),
    ]


def test_shared_boundaries_consolidate_reverse() -> None:
    """Tokens in both boundary sets merge forward and reverse traversals."""
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


def test_convert_filter_preserves_typed_kinds() -> None:
    assert convert_pool_type_filter(None) is None
    converted = convert_pool_type_filter(
        [{PoolKind.V2}, None, {PoolKind.V3, PoolKind.V4}],
    )
    assert converted == [{PoolKind.V2}, None, {PoolKind.V3, PoolKind.V4}]


def test_builder_preserves_all_family_identities() -> None:
    v2_address = "0x" + "22" * 20
    v3_address = "0x" + "33" * 20
    v4_manager = "0x" + "44" * 20
    v4_hash = "0x" + "ab" * 32
    builder = PathStepBuilder(
        v2v3_addresses={2: v2_address, 3: v3_address},
        v4_lookups={4: (v4_manager, v4_hash)},
        step_cls=PathStep,
    )

    steps = builder.build([(2, PoolKind.V2), (3, PoolKind.V3), (4, PoolKind.V4)])

    assert [(step.type, step.address, step.hash) for step in steps] == [
        (PoolKind.V2, v2_address, None),
        (PoolKind.V3, v3_address, None),
        (PoolKind.V4, v4_manager, v4_hash),
    ]
