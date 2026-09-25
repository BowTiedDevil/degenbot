"""Typed pool-family boundary for graph construction.

``classify_pool_kind(s)`` accepts only the Rust-backed ``PoolKind`` values used by
pathfinding. Unsupported family tokens fail at the typed FFI boundary instead of
consulting legacy model classes.
"""

import pytest

from degenbot.exceptions.base import DegenbotValueError
from degenbot.pathfinding import (
    PoolKind,
    classify_pool_kind,
    classify_pool_kinds,
    convert_pool_type_filter,
)


def test_every_supported_pool_kind_round_trips() -> None:
    assert classify_pool_kind(PoolKind.V2) is PoolKind.V2
    assert classify_pool_kind(PoolKind.V3) is PoolKind.V3
    assert classify_pool_kind(PoolKind.V4) is PoolKind.V4


def test_pool_kinds_are_deduplicated() -> None:
    assert classify_pool_kinds([PoolKind.V3, PoolKind.V2, PoolKind.V3]) == {
        PoolKind.V2,
        PoolKind.V3,
    }


def test_unsupported_family_token_aborts_loudly() -> None:
    with pytest.raises(ValueError, match="Unsupported pool kind"):
        classify_pool_kinds(["V5"])
    with pytest.raises(DegenbotValueError, match="Unsupported pool kind"):
        convert_pool_type_filter([{"V5"}])
