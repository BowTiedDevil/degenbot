"""The one construction request: a resolved identity picks the family arm.

``request_for`` maps the resolver's ``(family, variant)`` answer onto the sealed
per-family constructor whose signature carries that family's required hints.
The unmodeled identity (a positively identified pool no construction arm
models) reaches the seam under its OWN name, so its refusal never lands on
another family's builder failure — the measured mislabel this vocabulary
exists to prevent.
"""

from __future__ import annotations

import pytest

from degenbot.builders.construction import request_for
from degenbot.exceptions import DegenbotValueError
from degenbot.types.pool_type import PoolFamily

_ADDRESS = "0x" + "11" * 20
_VAULT = "0x" + "33" * 20


def test_unmodeled_identity_reaches_the_seam_under_its_own_name() -> None:
    request = request_for(
        _ADDRESS,
        family=PoolFamily.UNMODELED,
        variant="linear",
        block=1,
        db=True,
        vault=_VAULT,
    )
    assert request.family == "unmodeled"


def test_resolved_identity_picks_the_family_constructor() -> None:
    v2 = request_for(
        _ADDRESS, family=PoolFamily.CONSTANT_PRODUCT, variant="uniswap", block=1, db=True
    )
    assert (v2.family, v2.variant) == ("constant_product", None)

    aerodrome = request_for(
        _ADDRESS, family=PoolFamily.CONSTANT_PRODUCT, variant="aerodrome", block=1, db=True
    )
    assert (aerodrome.family, aerodrome.variant) == ("constant_product", "aerodrome")

    cl = request_for(
        _ADDRESS,
        family=PoolFamily.CONCENTRATED_LIQUIDITY,
        variant=None,
        block=1,
        db=True,
        factories=["0x" + "22" * 20],
        invariant_version=2,
    )
    assert (cl.family, cl.factories, cl.invariant_version) == (
        "concentrated_liquidity",
        ["0x" + "22" * 20],
        2,
    )

    weighted = request_for(
        _ADDRESS, family=PoolFamily.WEIGHTED, variant=None, block=1, db=True, vault=_VAULT
    )
    assert (weighted.family, weighted.variant, weighted.vault) == (
        "weighted",
        "balancer_weighted",
        _VAULT,
    )

    stable = request_for(
        _ADDRESS,
        family=PoolFamily.STABLESWAP,
        variant="balancer_stable",
        block=1,
        db=True,
        vault=_VAULT,
    )
    assert (stable.family, stable.variant, stable.vault) == (
        "stableswap",
        "balancer_stable",
        _VAULT,
    )

    curve = request_for(
        _ADDRESS,
        family=PoolFamily.STABLESWAP,
        variant=None,
        block=1,
        db=True,
        registries=["0x" + "44" * 20],
    )
    assert (curve.family, curve.registries) == ("stableswap", ["0x" + "44" * 20])


def test_a_required_family_hint_is_checked_where_the_family_is_chosen() -> None:
    with pytest.raises(DegenbotValueError):
        request_for(_ADDRESS, family=PoolFamily.WEIGHTED, variant=None, block=1, db=True)
