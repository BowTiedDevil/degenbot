"""Shared type resolution for pool construction.

Functions extracted from Bot so that pool type resolution
logic (DB lookup, factory fetch, on-chain probing, class dispatch) is
defined once and used by the Bot build path.

Pure functions (no I/O):
- pool_class_for_descriptor()
- _build_descriptor_from_db_result()
- _descriptor_from_probing_result()

Sync functions (accept RustBotIo):
- fetch_factory_from_chain()
- resolve_pool_type_by_probing()
- resolve_pool_type()
"""

from __future__ import annotations

import contextlib
from typing import TYPE_CHECKING, cast

from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.exceptions.base import DegenbotValueError
from degenbot.registry.pool_type import pool_type_registry
from degenbot.types.pool_type import PoolFamily, PoolTypeDescriptor, derive_kind
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.bot import RustBotIo
    from degenbot.database.models.pools import LiquidityPoolTable
    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool
    from degenbot.types.aliases import ChainId


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
            # Variant-aware: reject Balancer stable pools without factory registration
            if pool_type.variant is not None and pool_type.variant.startswith("balancer"):
                msg = (
                    f"Balancer stable pool with unregistered factory {pool_type.factory}. "
                    f"Register the factory address in pool_type_registry first."
                )
                raise DegenbotValueError(message=msg)
            return CurveStableswapPool
        case PoolFamily.WEIGHTED:
            # No default Balancer weighted class — require factory registration
            if pool_type.variant is not None and pool_type.variant.startswith("balancer"):
                msg = (
                    f"Balancer weighted pool with unregistered factory {pool_type.factory}. "
                    f"Register the factory address in pool_type_registry first."
                )
                raise DegenbotValueError(message=msg)
            msg = f"No pool class for WEIGHTED family with variant {pool_type.variant!r}"
            raise DegenbotValueError(message=msg)
        case _:
            msg = f"No pool class for family {pool_type.family.value!r}"
            raise DegenbotValueError(message=msg)


def _build_descriptor_from_db_result(
    pool_from_db: LiquidityPoolTable,
) -> PoolTypeDescriptor | None:
    """Map a DB row to a PoolTypeDescriptor.

    Returns None if the kind can't be resolved from the registry.
    Read-only dependency on pool_type_registry.

    Returns:
        The computed value.

    """
    kind = pool_from_db.kind
    descriptor = pool_type_registry.get_descriptor_by_kind(kind)
    if descriptor is not None:
        return PoolTypeDescriptor(
            family=descriptor.family,
            variant=descriptor.variant,
            kind=descriptor.kind,
            factory=get_checksum_address(pool_from_db.exchange.factory),
        )
    return None


def _build_descriptor_from_seam_rows(
    *,
    pool_kind: str,
    exchange_factory: str,
) -> PoolTypeDescriptor | None:
    """Map Rust-seam rows to a `PoolTypeDescriptor` (QVMWQC).

    The seam version of [`_build_descriptor_from_db_result`]: instead of a
    hydrated ORM row, takes the two fields the builder reads (`pool.kind` +
    `pool.exchange.factory`) fetched via `RustBotIo.fetch_pool_row` /
    `fetch_exchange`.

    Returns:
        The computed value.

    """
    descriptor = pool_type_registry.get_descriptor_by_kind(pool_kind)
    if descriptor is not None:
        return PoolTypeDescriptor(
            family=descriptor.family,
            variant=descriptor.variant,
            kind=descriptor.kind,
            factory=get_checksum_address(exchange_factory),
        )
    return None


def _descriptor_from_probing_result(
    *,
    succeeded_method: str | None,
    chain_id: ChainId,
    factory: ChecksumAddress,
) -> PoolTypeDescriptor:
    """Map 'which method succeeded' to a PoolTypeDescriptor.

    If the factory is registered in pool_type_registry, uses the registry
    descriptor. Otherwise derives a default descriptor from the method name.
    If succeeded_method is None (no method succeeded), returns STABLESWAP.

    Returns:
        The computed value.

    """
    if succeeded_method is None:
        return PoolTypeDescriptor(
            family=PoolFamily.STABLESWAP,
            variant=None,
            kind=derive_kind(PoolFamily.STABLESWAP, None),
            factory=factory,
        )

    registry_descriptor = pool_type_registry.get_descriptor(chain_id, factory)
    if registry_descriptor is not None:
        return registry_descriptor

    match succeeded_method:
        case "slot0":
            family = PoolFamily.CONCENTRATED_LIQUIDITY
        case "getReserves":
            family = PoolFamily.CONSTANT_PRODUCT
        case _:
            family = PoolFamily.STABLESWAP

    return PoolTypeDescriptor(
        family=family,
        variant=None,
        kind=derive_kind(family, None),
        factory=factory,
    )


def fetch_factory_from_chain(
    address: ChecksumAddress,
    *,
    chain_id: ChainId,  # ruff:ignore[unused-function-argument] — kept for API consistency with resolve_pool_type
    io: RustBotIo,
) -> ChecksumAddress | None:
    """Fetch the factory address from the pool contract's factory() method.

    The encode → call → decode → checksum choreography is Rust-owned
    (``RustBotIo.fetch_factory_address``, ADR-005 slice 14b). RustBotIo is the
    only executor; the Python parity-gate fallback is retired.

    Returns:
        The computed value.

    """
    return io.fetch_factory_address(address)


def resolve_pool_type_by_probing(
    address: ChecksumAddress,
    *,
    chain_id: ChainId,
    factory: ChecksumAddress,
    io: RustBotIo,
) -> PoolTypeDescriptor:
    """Determine pool type by probing the contract on-chain.

    Tries V3 methods first (slot0), then V2 methods (getReserves),
    then falls back to STABLESWAP. Descriptor construction is
    delegated to _descriptor_from_probing_result.

    Returns:
        The computed value.

    """
    # ADR-005 slice 14i: delegate the 4-call probing choreography to Rust
    # (``RustBotIo.probe_pool_type``). RustBotIo is the only executor; the
    # Python slot0/getReserves/getPoolId probing fallback is retired.
    result = io.probe_pool_type(address)
    if result == "slot0":
        return _descriptor_from_probing_result(
            succeeded_method="slot0",
            chain_id=chain_id,
            factory=factory,
        )
    if result == "getReserves":
        return _descriptor_from_probing_result(
            succeeded_method="getReserves",
            chain_id=chain_id,
            factory=factory,
        )
    if result == "balancer_weighted":
        return PoolTypeDescriptor(
            family=PoolFamily.WEIGHTED,
            variant="balancer_weighted",
            kind=derive_kind(PoolFamily.WEIGHTED, "balancer_weighted"),
            factory=factory,
        )
    if result == "balancer_stable":
        return PoolTypeDescriptor(
            family=PoolFamily.STABLESWAP,
            variant="balancer_stable",
            kind=derive_kind(PoolFamily.STABLESWAP, "balancer_stable"),
            factory=factory,
        )
    # "stableswap" fallback.
    return _descriptor_from_probing_result(
        succeeded_method=None,
        chain_id=chain_id,
        factory=factory,
    )


def resolve_pool_type(
    address: ChecksumAddress,
    *,
    chain_id: ChainId,
    io: RustBotIo,
) -> PoolTypeDescriptor:
    """Resolve the pool type for the given address.

    Consults these sources in order:
    1. Database `kind` column (exact polymorphic type)
    2. PoolTypeRegistry registration (factory address → descriptor)
    3. On-chain probing (slot0 vs getReserves) when factory is unknown

    Raises DegenbotValueError if the type cannot be determined.

    Returns:
        The computed value.

    Raises:
        DegenbotValueError: If the operation fails.

    """
    # Step 1: DB lookup — the `kind` column is the most direct signal.
    # Route through the Rust `RustBotIo` seam (QVMWQC): `fetch_pool_row`
    # carries `kind` + `exchange_id`; `fetch_exchange` hydrates the factory.
    # The `contextlib.suppress` makes a missing/empty DB a skip, not an error.
    with contextlib.suppress(Exception):
        pool_row = io.fetch_pool_row(chain_id=chain_id, address=address)
        if pool_row is not None:
            exchange_row = io.fetch_exchange(exchange_id=pool_row.exchange_id)
            if exchange_row is not None:
                descriptor = _build_descriptor_from_seam_rows(
                    pool_kind=pool_row.kind,
                    exchange_factory=exchange_row.factory,
                )
                if descriptor is not None:
                    return descriptor

    # Step 2: Factory address lookup via PoolTypeRegistry
    factory = fetch_factory_from_chain(address, chain_id=chain_id, io=io)
    if factory is not None:
        # Check if the factory is registered in the pool type registry
        registry_descriptor = pool_type_registry.get_descriptor(chain_id, factory)
        if registry_descriptor is not None:
            return registry_descriptor

        # Step 3: No registry match — probe the contract to determine invariant
        return resolve_pool_type_by_probing(address, chain_id=chain_id, factory=factory, io=io)

    raise DegenbotValueError(
        message=(
            f"Cannot resolve pool type for address {address} on chain {chain_id}. "
            f"The factory() call failed and no database entry exists."
        ),
    )
