"""The pool-family taxonomy — minted from the core's exported tag lists.

The core exports two aligned vocabularies, each with exactly one owner:

- :func:`pool_family_tags` — the INTERNAL kebab family tag
  (``RegisteredPoolFamily::tag``: ``v2`` … ``curve``), the spelling every
  internal ``family`` reader reports and parses.
- :func:`pool_kind_labels` — the public version label
  (``PoolKind::version_label``: ``V2`` … ``V4``), the operator wire's
  ``family`` field and the render seam's hop labels.

This module pairs the two lists with the FFI :class:`PoolKind` members at one
place (:data:`_POOL_FAMILY_AXIS`), so a family added in the core surfaces here
on the next build, and a stale binding fails loudly at the mint rather than
mistranslating.

"""

from __future__ import annotations

from typing import cast

# ADR-013: Rust symbols enter Python through a `degenbot.<domain>` barrier,
# and this module sits inside one. Resolve the core vocabulary from the
# partially-initialized package — the ordering contract lives in
# `degenbot/pathfinding/__init__.py`.
from degenbot.pathfinding import PoolKind, pool_family_tags, pool_kind_labels


def _pool_kind_for_label(label: str) -> PoolKind:
    """Resolve one public version label to its FFI :class:`PoolKind` member — loudly.

    Returns:
        The FFI enum member whose name equals the core label.

    Raises:
        AttributeError: when the binding has no member for a label the core
            exports (a stale FFI binding meeting a newer core). Never
            guesses.

    """
    member = getattr(PoolKind, label, None)
    if member is None:
        msg = f"core pool-kind label {label!r} has no PoolKind member (stale FFI binding?)"
        raise AttributeError(msg)
    return cast("PoolKind", member)


#: The family axis, in the core's discriminant order: internal kebab tag,
#: public version label, FFI member. Pairing the two exported lists here is
#: what keeps the vocabularies from drifting apart.
_POOL_FAMILY_AXIS: tuple[tuple[str, str, PoolKind], ...] = tuple(
    (tag, label, _pool_kind_for_label(label))
    for tag, label in zip(pool_family_tags(), pool_kind_labels(), strict=True)
)


#: The canonical INTERNAL family tags, in discriminant order — BUILT from the
#: core's exported list, never re-declared.
POOL_FAMILY_TAGS: tuple[str, ...] = tuple(axis[0] for axis in _POOL_FAMILY_AXIS)


#: The public version labels (``V2``/``V3``/``V4``), in discriminant order —
#: the operator wire's spelled ``family`` values and the render labels.
POOL_KIND_LABELS: tuple[str, ...] = tuple(axis[1] for axis in _POOL_FAMILY_AXIS)


#: The internal tag → typed :class:`PoolKind` map: the ONE conversion for an
#: internal ``family`` string (a discovery row, a capture/render hop dict).
FAMILY_TAG_TO_POOL_KIND: dict[str, PoolKind] = {
    tag: kind for tag, _label, kind in _POOL_FAMILY_AXIS
}


#: The public label → typed :class:`PoolKind` map: the ONE conversion for the
#: operator wire's ``family`` field and the CLI's version-permutation tokens.
#: Deliberately separate from :data:`FAMILY_TAG_TO_POOL_KIND` — the operator
#: protocol is versioned, so its spelling does not follow internal casing.
POOL_KIND_BY_LABEL: dict[str, PoolKind] = {label: kind for _tag, label, kind in _POOL_FAMILY_AXIS}


#: Every taxonomy member, in discriminant order — the default family set the
#: graph traverses.
ALL_POOL_KINDS: tuple[PoolKind, ...] = tuple(FAMILY_TAG_TO_POOL_KIND.values())


#: The typed member → public version label. The pyclass ``str()`` spells
#: ``PoolKind.V2``; operator-facing lines keep the bare version spelling.
POOL_KIND_LABEL: dict[PoolKind, str] = {kind: label for _tag, label, kind in _POOL_FAMILY_AXIS}
