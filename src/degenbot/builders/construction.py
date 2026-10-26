"""The construction request vocabulary (ADR-085 D2/D3).

`PoolConstructionRequest` is the seam's ONE construction input: the resolved
`(family, variant)` identity plus the family's hints, as a value. It is
re-exported from the Rust core verbatim — the request has no Python twin to
translate to, and a wrapper class would only give the same value two names.

`request_for` is the one place the resolved identity picks a family constructor:
the pyclass exposes a sealed constructor per family (so a family's required hints
are arguments rather than runtime checks), and this module maps the identity the
resolver answered onto the matching one.
"""

from __future__ import annotations

from collections.abc import Iterable

from degenbot._ffi import PoolConstructionRequest
from degenbot.exceptions import DegenbotValueError
from degenbot.types.pool_type import PoolFamily

__all__ = ["PoolConstructionRequest", "request_for"]


def request_for(
    address: str,
    *,
    family: PoolFamily,
    variant: str | None,
    block: int | None,
    db: bool,
    factories: Iterable[str] = (),
    registries: Iterable[str] = (),
    vault: str | None = None,
    invariant_version: int | None = None,
) -> PoolConstructionRequest:
    """Build the family-tagged request for a resolved identity.

    The `(family, variant)` pair is the resolver's answer, never a subclass test:
    this maps it onto the constructor whose signature carries that family's
    required hints.

    Returns:
        The request the one construction verb consumes.

    Raises:
        DegenbotValueError: If the family needs a hint the caller did not supply,
            or the family has no construction arm.

    """
    if family is PoolFamily.UNMODELED:
        # The refusal-only identity: the request reaches the seam so the
        # refusal names this family, never another arm's builder failure.
        return PoolConstructionRequest.unmodeled(address, block=block, db=db)

    if family is PoolFamily.CONSTANT_PRODUCT:
        if variant == "aerodrome":
            return PoolConstructionRequest.aerodrome_v2(address, block=block, db=db)
        return PoolConstructionRequest.v2(address, block=block, db=db)

    if family is PoolFamily.CONCENTRATED_LIQUIDITY:
        return PoolConstructionRequest.concentrated_liquidity(
            address,
            factories=list(factories) or None,
            invariant_version=invariant_version,
            block=block,
            db=db,
        )

    if family is PoolFamily.WEIGHTED:
        return PoolConstructionRequest.weighted(
            address, _required_vault(family, vault), block=block, db=db
        )

    if family is PoolFamily.STABLESWAP:
        if variant == "balancer_stable":
            return PoolConstructionRequest.balancer_stable(
                address, _required_vault(family, vault), block=block, db=db
            )
        return PoolConstructionRequest.curve(address, [*registries], block=block, db=db)

    msg = f"construct_pool: the {family.value!r} family has no construction arm"
    raise DegenbotValueError(message=msg)  # pragma: no cover - exhaustiveness guard


def _required_vault(family: PoolFamily, vault: str | None) -> str:
    """Return the configured Vault address, or refuse the build loudly."""
    if vault is None:
        msg = f"construct_pool: the {family.value!r} family requires a Vault address"
        raise DegenbotValueError(message=msg)
    return vault
