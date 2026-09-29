"""degenbot.types.pool_type — the pool-variant taxonomy conversion.

Pins :func:`classify_pool_variant`, the ONE raw-string → :class:`PoolVariant`
conversion point: ``None`` passes through (the canonical Uniswap identity),
every shipped spelling maps to its member, and the open-set contract surfaces
an unknown spelling (e.g. a third-party overlay registration) as
``UNRECOGNIZED`` rather than guessing.
"""

from __future__ import annotations

import pytest

from degenbot.types.pool_type import (
    BALANCER_VARIANTS,
    PoolVariant,
    classify_pool_variant,
)


@pytest.mark.parametrize(
    ("raw", "expected"),
    [
        (None, None),
        ("balancer_stable", PoolVariant.BALANCER_STABLE),
        ("balancer_weighted", PoolVariant.BALANCER_WEIGHTED),
        ("aerodrome", PoolVariant.AERODROME),
        ("camelot", PoolVariant.CAMELOT),
        ("pancakeswap", PoolVariant.PANCAKESWAP),
        ("sushiswap", PoolVariant.SUSHISWAP),
        ("swapbased", PoolVariant.SWAPBASED),
    ],
)
def test_classify_passes_none_through_and_maps_known_spellings(
    raw: str | None, expected: PoolVariant | None
) -> None:
    assert classify_pool_variant(raw) is expected


@pytest.mark.parametrize("raw", ["mycustom", "unknown-dex", "SushiSwap", ""])
def test_classify_surfaces_unknown_spellings_as_unrecognized(raw: str) -> None:
    # The set is intentionally OPEN: an unregistered spelling (a third-party
    # overlay variant, a case drift) is UNRECOGNIZED, not an error and not a
    # guess. Decision points that must act on variant semantics raise on it.
    assert classify_pool_variant(raw) is PoolVariant.UNRECOGNIZED


def test_balancer_variants_are_exactly_the_vault_dependent_pair() -> None:
    # The variants that REQUIRE a factory registration (no default class).
    assert BALANCER_VARIANTS == frozenset({
        PoolVariant.BALANCER_STABLE,
        PoolVariant.BALANCER_WEIGHTED,
    })


def test_unrecognized_is_not_vault_dependent() -> None:
    # UNRECOGNIZED must fall into the loud unknown-variant arm at the decision
    # points, never the Balancer-registration arm.
    assert PoolVariant.UNRECOGNIZED not in BALANCER_VARIANTS
