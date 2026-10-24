"""Shared type resolution for pool construction.

Functions extracted from Bot so that pool type resolution
logic (DB lookup, factory fetch, on-chain probing, class dispatch) is
defined once and used by the Bot build path.

Pure functions (no I/O):
- pool_class_for_descriptor()
- _descriptor_from_probing_result()

Sync functions (accept BotIo):
- fetch_factory_from_chain()
- resolve_pool_type_by_probing()
- resolve_pool_type()
"""

from __future__ import annotations

import contextlib
from typing import TYPE_CHECKING, cast

from degenbot.balancer import BalancerFamily
from degenbot.balancer.pools import BalancerV2Pool
from degenbot.balancer.stable_pools import BalancerV2StablePool
from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.exceptions.base import DegenbotValueError
from degenbot.registry.pool_type import pool_type_registry
from degenbot.types.pool_type import (
    BALANCER_VARIANTS,
    PoolFamily,
    PoolProbe,
    PoolTypeDescriptor,
    PoolVariant,
    classify_pool_variant,
    derive_kind,
)
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

if TYPE_CHECKING:
    from degenbot._ffi import BotIo
    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool
    from degenbot.types.aliases import ChainId
    from degenbot.types.chain import ChecksummedAddress


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


def _build_descriptor_from_seam_rows(
    *,
    pool_kind: str,
    exchange_factory: str,
) -> PoolTypeDescriptor:
    """Map Rust-seam rows to a `PoolTypeDescriptor`.

    Takes the two fields the builder reads (`pool.kind` + `pool.exchange.factory`)
    fetched via `BotIo.fetch_pool_row` / `fetch_exchange`.

    Returns:
        The computed value.

    Raises:
        DegenbotValueError: On a kind the registry does not know. A present
            but unrecognized DB kind must not fall through to on-chain
            probing, which would silently re-classify the pool.

    """
    descriptor = pool_type_registry.get_descriptor_by_kind(pool_kind)
    if descriptor is None:
        msg = (
            f"Unrecognized pool kind {pool_kind!r} from the database; "
            f"pool_type_registry has no descriptor for it."
        )
        raise DegenbotValueError(message=msg)
    return PoolTypeDescriptor(
        family=descriptor.family,
        variant=descriptor.variant,
        kind=descriptor.kind,
        factory=get_checksum_address(exchange_factory),
    )


def _descriptor_from_probing_result(
    *,
    succeeded: PoolProbe | None,
    chain_id: ChainId,
    factory: ChecksummedAddress,
) -> PoolTypeDescriptor:
    """Map 'which probe succeeded' to a PoolTypeDescriptor.

    If the factory is registered in pool_type_registry, uses the registry
    descriptor. Otherwise derives a default descriptor from the probe.

    ``None`` (no probe succeeded) raises — an unverified contract is not a
    Curve pool. The Rust probe reports the unknown identity instead of
    returning a Curve fallback.

    Returns:
        The computed value.

    Raises:
        DegenbotValueError: On ``None`` or a probe result outside the
            PoolProbe space — an unverified/unknown probe must never be
            silently classified.

    """
    if succeeded is None:
        msg = (
            "No pool probe succeeded; refusing to classify an unverified "
            "contract as STABLESWAP. The Rust probe raises UnknownPoolIdentity "
            "instead of returning a Curve fallback."
        )
        raise DegenbotValueError(message=msg)

    registry_descriptor = pool_type_registry.get_descriptor(chain_id, factory)
    if registry_descriptor is not None:
        return registry_descriptor

    match succeeded:
        case PoolProbe.V3:
            family = PoolFamily.CONCENTRATED_LIQUIDITY
        case PoolProbe.V2:
            family = PoolFamily.CONSTANT_PRODUCT
        case _:
            raise DegenbotValueError(message=f"Unrecognized probe result: {succeeded!r}")

    return PoolTypeDescriptor(
        family=family,
        variant=None,
        kind=derive_kind(family, None),
        factory=factory,
    )


def resolve_balancer_descriptor(
    address: ChecksummedAddress,
    *,
    chain_id: ChainId,
    io: BotIo,
) -> PoolTypeDescriptor | None:
    """Resolve a Balancer V2 pool's identity from on-chain factory membership.

    A deployed Balancer pool carries no ``factory()`` getter (the probe
    :func:`resolve_pool_type` tries first), and Balancer V2 exposes no
    enumerable pool registry (``getPoolCount`` / ``getPools`` / ``getPoolAt``
    revert on every factory revision). Two reads do answer universally:

    1. ``getPoolId()`` — every Balancer pool (and no other family we build)
       defines it. A revert means "not a Balancer pool": return ``None`` so the
       caller's non-Balancer path is untouched.
    2. ``isPoolFromFactory(address)`` — every factory revision answers it, so
       membership across the registry's rows for the probed family names the
       owning factory, and that row carries the identity (family, variant,
       kind) plus the class the registry registered for it.

    A pool whose owning factory is not in the registry (the MetaStablePool
    factory is one) still has a positively probed Balancer identity; its
    descriptor keeps the variant with ``factory=None`` and
    :func:`pool_class_for_descriptor` supplies the family's default class.

    A pool whose family probe answers nothing (neither
    ``getNormalizedWeights()`` nor ``getAmplificationParameter()``) is a
    Balancer pool of a shape no construction arm models — a linear pool, a
    boosted pool's linear constituent, a gyro/quant-amm shape. Its identity is
    still positive, so it resolves to :attr:`PoolFamily.UNMODELED` and is
    refused under that name; returning ``None`` here is what sent these
    addresses to the Curve arm, whose builder refused them as a Curve pool.

    Identity is a deployment-time property, so the probes run at the head block
    like :func:`fetch_factory_from_chain` — not at the construction block.

    Returns:
        The descriptor, or ``None`` when the contract is not a Balancer pool.

    """
    try:
        io.fetch_balancer_pool_id(address)
    except Exception:  # ruff: ignore[blind-except] — a revert is "not Balancer"
        return None

    try:
        balancer_family = io.probe_balancer_pool_type(address)
    except Exception:  # ruff: ignore[blind-except]
        # `getPoolId()` answered but neither weights nor amplification did: a
        # Balancer pool outside the weighted/stable families (linear pools,
        # boosted-pool constituents, gyro shapes), which no construction arm
        # models. The identity is positive and keeps its own name — the
        # construction switchboard refuses it as an unmodeled Balancer family.
        return PoolTypeDescriptor(
            family=PoolFamily.UNMODELED,
            variant="balancer_unmodeled",
            kind=derive_kind(PoolFamily.UNMODELED, "balancer_unmodeled"),
            factory=None,
        )

    variant = (
        PoolVariant.BALANCER_WEIGHTED
        if balancer_family is BalancerFamily.Weighted
        else PoolVariant.BALANCER_STABLE
    )
    family = (
        PoolFamily.WEIGHTED if variant is PoolVariant.BALANCER_WEIGHTED else PoolFamily.STABLESWAP
    )

    candidates = [
        descriptor.factory
        for (row_chain_id, _factory), (_cls, descriptor, _deployment) in (
            pool_type_registry.registrations.items()
        )
        if row_chain_id == chain_id
        and descriptor.variant == variant
        and descriptor.factory is not None
    ]
    owning_factory = io.fetch_balancer_factory(address, candidates) if candidates else None
    if owning_factory is not None:
        registered = pool_type_registry.get_descriptor(chain_id, owning_factory)
        if registered is not None:
            return registered

    return PoolTypeDescriptor(
        family=family,
        variant=variant,
        kind=derive_kind(family, variant),
        factory=None,
    )


def fetch_factory_from_chain(
    address: ChecksummedAddress,
    *,
    chain_id: ChainId,  # ruff:ignore[unused-function-argument] — kept for API consistency with resolve_pool_type
    io: BotIo,
) -> ChecksummedAddress | None:
    """Fetch the factory address from the pool contract's factory() method.

    The encode → call → decode → checksum choreography is Rust-owned
    (``BotIo.fetch_factory_address``). BotIo is the only executor; the Python
    parity-gate fallback is retired.

    Returns:
        The computed value.

    """
    return io.fetch_factory_address(address)


def resolve_pool_type_by_probing(
    address: ChecksummedAddress,
    *,
    chain_id: ChainId,
    factory: ChecksummedAddress,
    io: BotIo,
) -> PoolTypeDescriptor:
    """Determine pool type by probing the contract on-chain.

    Tries V3 first (slot0), then V2 (getReserves), then Balancer
    (getPoolId + getNormalizedWeights), then Curve (coins(uint256)). When
    no probe answers, the Rust probe raises UnknownPoolIdentity, which this
    function surfaces as DegenbotValueError — an unverified contract is not
    silently classified as Curve.

    Returns:
        The computed value.

    Raises:
        DegenbotValueError: When the seam reports no identity selector
            answered, or on a probe code outside the PoolProbe space —
            Rust/Python wire-order drift, never silently classified.

    """
    # Delegate the probing choreography to Rust (``BotIo.probe_pool_type``).
    # BotIo is the only executor; the Python slot0/getReserves/getPoolId probing
    # fallback is retired.
    try:
        result = io.probe_pool_type(address)
    except ValueError as e:
        # Rust signals "no identity selector answered" as PyValueError (the
        # DegenbotValueError parent class); re-raise as the package exception.
        msg = (
            f"Cannot determine pool type for address {address} on chain "
            f"{chain_id}: no identity selector answered."
        )
        raise DegenbotValueError(message=msg) from e
    try:
        probe = PoolProbe(result)
    except ValueError as e:
        raise DegenbotValueError(
            message=(
                f"Unrecognized probe result from BotIo: {result!r} — "
                "the Rust PoolFamily match in py_bot_io.rs::probe_pool_type "
                "and degenbot.types.pool_type.PoolProbe are out of lock-step"
            ),
        ) from e

    match probe:
        case PoolProbe.V3 | PoolProbe.V2:
            return _descriptor_from_probing_result(
                succeeded=probe,
                chain_id=chain_id,
                factory=factory,
            )
        case PoolProbe.BALANCER_WEIGHTED:
            return PoolTypeDescriptor(
                family=PoolFamily.WEIGHTED,
                variant="balancer_weighted",
                kind=derive_kind(PoolFamily.WEIGHTED, "balancer_weighted"),
                factory=factory,
            )
        case PoolProbe.BALANCER_STABLE:
            return PoolTypeDescriptor(
                family=PoolFamily.STABLESWAP,
                variant="balancer_stable",
                kind=derive_kind(PoolFamily.STABLESWAP, "balancer_stable"),
                factory=factory,
            )
        case PoolProbe.STABLESWAP:
            # Positive Curve identity: coins(uint256) answered. This is the
            # generic Curve descriptor the old ``succeeded=None`` fallback
            # produced; it deliberately bypasses the registry.
            return PoolTypeDescriptor(
                family=PoolFamily.STABLESWAP,
                variant=None,
                kind=derive_kind(PoolFamily.STABLESWAP, None),
                factory=factory,
            )


def resolve_pool_type(
    address: ChecksummedAddress,
    *,
    chain_id: ChainId,
    io: BotIo,
) -> PoolTypeDescriptor:
    """Resolve the pool type for the given address.

    Consults these sources in order:
    1. Database `kind` column (exact polymorphic type)
    2. PoolTypeRegistry registration (factory address → descriptor)
    3. On-chain probing (slot0 vs getReserves) when the factory is unknown to
       the registry
    4. Balancer identity — `getPoolId()` plus `isPoolFromFactory()` membership
       across the registry's Balancer factory rows — when `factory()` does not
       answer at all (every deployed Balancer pool)

    Raises DegenbotValueError if the type cannot be determined.

    Returns:
        The computed value.

    Raises:
        DegenbotValueError: If the operation fails.

    """
    # Step 1: DB lookup — the `kind` column is the most direct signal.
    # Route through the Rust `BotIo` seam: `fetch_pool_row`
    # carries `kind` + `exchange_id`; `fetch_exchange` hydrates the factory.
    # The `contextlib.suppress` makes a missing/empty DB a skip, not an error.
    # Only the fetches are suppressed: a present-but-unrecognized `kind`
    # raises from `_build_descriptor_from_seam_rows` (outside the suppress)
    # rather than falling through to on-chain re-classification.
    pool_row = None
    exchange_row = None
    with contextlib.suppress(Exception):
        pool_row = io.fetch_pool_row(chain_id=chain_id, address=address)
        if pool_row is not None:
            exchange_row = io.fetch_exchange(exchange_id=pool_row.exchange_id)
    if pool_row is not None and exchange_row is not None:
        return _build_descriptor_from_seam_rows(
            pool_kind=pool_row.kind,
            exchange_factory=exchange_row.factory,
        )

    # Step 2: Factory address lookup via PoolTypeRegistry
    factory = fetch_factory_from_chain(address, chain_id=chain_id, io=io)
    if factory is not None:
        # Check if the factory is registered in the pool type registry
        registry_descriptor = pool_type_registry.get_descriptor(chain_id, factory)
        if registry_descriptor is not None:
            return registry_descriptor

        # Step 3: No registry match — probe the contract to determine invariant
        return resolve_pool_type_by_probing(address, chain_id=chain_id, factory=factory, io=io)

    # Step 4: Balancer identity. A Balancer pool has no `factory()` getter and
    # its factory rows are the only place its identity lives, so the membership
    # probe is what keeps the caller from falling through to the Curve arm (the
    # wrong family's refusal).
    balancer_descriptor = resolve_balancer_descriptor(address, chain_id=chain_id, io=io)
    if balancer_descriptor is not None:
        return balancer_descriptor

    raise DegenbotValueError(
        message=(
            f"Cannot resolve pool type for address {address} on chain {chain_id}. "
            f"The factory() call failed, no database entry exists, and no Balancer "
            f"factory claims the pool."
        ),
    )
