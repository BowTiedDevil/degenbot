"""Companion-class selection for a resolved pool identity.

Pool identity resolution has ONE core home (`bot_core::pool_builder::
pool_identity`) — the ladder (DB kind, registered species/factory, the
factory read, the positive V3/V2/Balancer/Curve probes, Balancer factory
membership) and the structured answer both consumers share. The identity
crosses the seam as ``BotIo.resolve_pool_identity`` (a ``PoolIdentity``
pyclass: species / family / variant / factory / persisted kind — never a
Python class name).

What remains here is the companion layer's own decision, and nothing else:
which Python pool class a resolved identity selects. Pure functions only
(no I/O):

- ``descriptor_for_identity()`` — project the core's structured identity
  onto the ``PoolTypeDescriptor`` the companion binds.
- ``pool_class_for_descriptor()`` — the class selection.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, cast

from degenbot.balancer.pools import BalancerV2Pool
from degenbot.balancer.stable_pools import BalancerV2StablePool
from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.exceptions.base import DegenbotValueError
from degenbot.registry.pool_type import pool_type_registry
from degenbot.types.pool_type import (
    BALANCER_VARIANTS,
    PoolFamily,
    PoolTypeDescriptor,
    PoolVariant,
    classify_pool_variant,
)
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

if TYPE_CHECKING:
    from degenbot._ffi import PoolIdentity
    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool
    from degenbot.types.aliases import ChainId


def descriptor_for_identity(identity: PoolIdentity) -> PoolTypeDescriptor:
    """Project the core's structured identity onto the Python descriptor.

    A pure projection: the family/variant/kind/factory facts the core
    resolved, re-typed into the companion's identity value. No resolution
    happens here (the core ladder already answered), and no class name
    crosses the seam in either direction.

    Returns:
        The companion's view of the resolved identity.

    """
    factory = identity.factory
    return PoolTypeDescriptor(
        family=PoolFamily(identity.family),
        variant=identity.variant,
        kind=identity.persisted_kind,
        factory=get_checksum_address(factory) if factory is not None else None,
    )


def pool_class_for_descriptor(
    pool_type: PoolTypeDescriptor,
    *,
    chain_id: ChainId,
) -> type[AbstractLiquidityPool]:
    """Resolve a PoolTypeDescriptor to a concrete pool class.

    Consults the pool_type_registry to find the registered class
    for this factory on this chain. Falls back to a default class
    based on the family if no specific registration exists.

    Returns:
        The computed value.

    Raises:
        DegenbotValueError: If the operation fails.

    """
    if pool_type.factory is not None:
        pool_class = pool_type_registry.get_class(chain_id, pool_type.factory)
        if pool_class is not None:
            return pool_class

    # Default classes when no factory-specific registration exists
    match pool_type.family:
        case PoolFamily.CONSTANT_PRODUCT:
            return cast(
                "type[AbstractLiquidityPool]",
                pool_type_registry.get_v2_class(chain_id, pool_type.factory or "") or UniswapV2Pool,
            )
        case PoolFamily.CONCENTRATED_LIQUIDITY:
            return cast(
                "type[AbstractLiquidityPool]",
                pool_type_registry.get_v3_class(chain_id, pool_type.factory or "") or UniswapV3Pool,
            )
        case PoolFamily.STABLESWAP:
            # Variant-aware: a Balancer variant must resolve to the Balancer
            # stable class (never to CurveStableswapPool), and an unrecognized
            # variant must not masquerade as generic Curve (ADR-021 — classify
            # or raise). The family's own class is the answer even when the
            # factory is unregistered: a probed Balancer identity (or a DB kind)
            # is authoritative, the factory only refines it to a deployment.
            variant = classify_pool_variant(pool_type.variant)
            if variant is PoolVariant.BALANCER_STABLE:
                return BalancerV2StablePool
            if variant in BALANCER_VARIANTS:
                msg = (
                    f"Balancer weighted variant {pool_type.variant!r} on a stableswap family "
                    f"descriptor (factory {pool_type.factory})."
                )
                raise DegenbotValueError(message=msg)
            if variant is PoolVariant.UNRECOGNIZED:
                msg = (
                    f"Unrecognized stableswap variant {pool_type.variant!r} with unregistered "
                    f"factory {pool_type.factory}; refusing to default it to CurveStableswapPool."
                )
                raise DegenbotValueError(message=msg)
            return CurveStableswapPool
        case PoolFamily.WEIGHTED:
            if classify_pool_variant(pool_type.variant) is PoolVariant.BALANCER_WEIGHTED:
                return BalancerV2Pool
            msg = f"No pool class for WEIGHTED family with variant {pool_type.variant!r}"
            raise DegenbotValueError(message=msg)
        case _:
            msg = f"No pool class for family {pool_type.family.value!r}"
            raise DegenbotValueError(message=msg)
