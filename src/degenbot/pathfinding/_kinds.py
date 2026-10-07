"""The pool-family taxonomy — minted from the core's exported tag list.

The canonical vocabulary is the Rust core's graph discriminant
(`degenbot_pathfinding`'s ``PoolKind``, crossing the FFI as
:class:`~degenbot.pathfinding.PoolKind`). This module derives Python's
family sets from the core's exported tag list (:func:`pool_family_tags`)
instead of re-declaring them — the S12 pattern: a family added in the core
surfaces here on the next build, and a stale binding fails loudly at the
mint rather than mistranslating.
"""

from __future__ import annotations

# ADR-013: Rust symbols enter Python through a `degenbot.<domain>` barrier,
# and this module sits inside one. Resolve the core vocabulary from the
# partially-initialized package — the ordering contract lives in
# `degenbot/pathfinding/__init__.py`.
from degenbot.pathfinding import PoolKind, pool_family_tags


def _pool_kind_for_tag(tag: str) -> PoolKind:
    """Resolve one core tag to its FFI :class:`PoolKind` member — loudly.

    Returns:
        The FFI enum member whose name equals the core tag.

    Raises:
        AttributeError: when the binding has no member for a tag the core
            exports (a stale FFI binding meeting a newer core). Never
            guesses.

    """
    member = getattr(PoolKind, tag, None)
    if member is None:
        msg = f"core pool-family tag {tag!r} has no PoolKind member (stale FFI binding?)"
        raise AttributeError(msg)
    return member


#: The canonical family tags, in discriminant order — BUILT from the core's
#: exported list, never re-declared.
POOL_FAMILY_TAGS: tuple[str, ...] = tuple(pool_family_tags())

#: The wire tag → typed :class:`PoolKind` map, minted from the tag list: the
#: ONE conversion for the operator/render seams.
FAMILY_TAG_TO_POOL_KIND: dict[str, PoolKind] = {
    tag: _pool_kind_for_tag(tag) for tag in POOL_FAMILY_TAGS
}

#: Every taxonomy member, in discriminant order — the default family set the
#: graph traverses.
ALL_POOL_KINDS: tuple[PoolKind, ...] = tuple(FAMILY_TAG_TO_POOL_KIND.values())

#: The typed member → bare tag label. The pyclass ``str()`` spells
#: ``PoolKind.V2``; operator-facing lines keep the bare spelling.
POOL_KIND_TAG: dict[PoolKind, str] = {kind: tag for tag, kind in FAMILY_TAG_TO_POOL_KIND.items()}
