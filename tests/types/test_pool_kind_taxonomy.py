"""degenbot.pathfinding — the pool-family taxonomy is minted, not re-declared.

Pins the S12 derivation: Python's family sets are BUILT from the core's two
exported vocabularies (:func:`pool_family_tags` for the internal kebab tag,
:func:`pool_kind_labels` for the public version label), each list pairs with
exactly one FFI ``PoolKind`` member, and a label without a binding member fails
loudly instead of guessing.
"""

from __future__ import annotations

import pytest

from degenbot.pathfinding import (
    ALL_POOL_KINDS,
    FAMILY_TAG_TO_POOL_KIND,
    POOL_FAMILY_TAGS,
    POOL_KIND_BY_LABEL,
    POOL_KIND_LABEL,
    POOL_KIND_LABELS,
)
from degenbot.pathfinding._kinds import _pool_kind_for_label
from tests.helpers.seam import PoolKind, pool_family_tags, pool_kind_labels


def test_family_tags_are_the_core_internal_kebab_vocabulary() -> None:
    # The minted tuple IS the core's list, in discriminant order — not a
    # Python-side copy that can drift from it. The internal spelling is kebab
    # (`v2`), the one `RegisteredPoolFamily::tag` owns.
    assert POOL_FAMILY_TAGS == tuple(pool_family_tags())
    assert POOL_FAMILY_TAGS == ("v2", "v3", "v4")


def test_kind_labels_are_the_core_public_vocabulary() -> None:
    # The operator wire's vocabulary, aligned index-for-index with the tags.
    assert POOL_KIND_LABELS == tuple(pool_kind_labels())
    assert POOL_KIND_LABELS == ("V2", "V3", "V4")
    assert len(POOL_KIND_LABELS) == len(POOL_FAMILY_TAGS)


def test_every_tag_maps_to_exactly_one_pool_kind_member() -> None:
    assert set(FAMILY_TAG_TO_POOL_KIND) == set(POOL_FAMILY_TAGS)
    assert ALL_POOL_KINDS == tuple(FAMILY_TAG_TO_POOL_KIND[tag] for tag in POOL_FAMILY_TAGS)
    # The members are the pyclass's own — identity, not a re-declaration. The
    # label is what names the member, so the pairing is index-aligned over the
    # two exported lists.
    for tag, label in zip(POOL_FAMILY_TAGS, POOL_KIND_LABELS, strict=True):
        member = FAMILY_TAG_TO_POOL_KIND[tag]
        assert member is getattr(PoolKind, label)
        assert POOL_KIND_BY_LABEL[label] is member
        assert POOL_KIND_LABEL[member] == label


def test_the_two_vocabularies_do_not_share_a_key() -> None:
    # No dual acceptance: an internal tag is not a wire label and vice versa.
    assert set(FAMILY_TAG_TO_POOL_KIND).isdisjoint(POOL_KIND_BY_LABEL)


def test_stale_binding_fails_loudly() -> None:
    # A core label the binding does not know must raise, never guess — the
    # loud-failure half of the derivation contract.
    with pytest.raises(AttributeError, match="V9"):
        _pool_kind_for_label("V9")
