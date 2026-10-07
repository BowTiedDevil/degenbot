"""degenbot.pathfinding — the pool-family taxonomy is minted, not re-declared.

Pins the S12 derivation: Python's family sets are BUILT from the core's
exported tag list (:func:`pool_family_tags`), the wire → member conversion
covers exactly that list, and a tag without a binding member fails loudly
instead of guessing.
"""

from __future__ import annotations

import pytest

from degenbot._ffi import PoolKind, pool_family_tags
from degenbot.pathfinding import (
    ALL_POOL_KINDS,
    FAMILY_TAG_TO_POOL_KIND,
    POOL_FAMILY_TAGS,
    POOL_KIND_TAG,
)
from degenbot.pathfinding._kinds import _pool_kind_for_tag


def test_family_tags_are_minted_from_the_core_list() -> None:
    # The minted tuple IS the core's list, in discriminant order — not a
    # Python-side copy that can drift from it.
    assert POOL_FAMILY_TAGS == tuple(pool_family_tags())
    assert len(POOL_FAMILY_TAGS) > 0


def test_every_tag_maps_to_exactly_one_pool_kind_member() -> None:
    assert set(FAMILY_TAG_TO_POOL_KIND) == set(POOL_FAMILY_TAGS)
    assert ALL_POOL_KINDS == tuple(FAMILY_TAG_TO_POOL_KIND[tag] for tag in POOL_FAMILY_TAGS)
    # The members are the pyclass's own — identity, not a re-declaration.
    for tag, member in FAMILY_TAG_TO_POOL_KIND.items():
        assert member is getattr(PoolKind, tag)


def test_labels_invert_the_tag_map() -> None:
    assert POOL_KIND_TAG == {kind: tag for tag, kind in FAMILY_TAG_TO_POOL_KIND.items()}


def test_stale_binding_fails_loudly() -> None:
    # A core tag the binding does not know must raise, never guess — the
    # loud-failure half of the derivation contract.
    with pytest.raises(AttributeError, match="V9"):
        _pool_kind_for_tag("V9")
