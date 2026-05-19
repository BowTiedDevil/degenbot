"""Shared type resolution for pool construction.

Functions extracted from Bot and AsyncBot so that pool type resolution
logic (DB lookup, factory fetch, on-chain probing, class dispatch) is
defined once and used by both sync and async paths.

Pure functions (no I/O):
- pool_class_for_descriptor()

Sync functions (accept PoolIO):
- fetch_factory_from_chain()
- resolve_pool_type_by_probing()
- resolve_pool_type()

Async functions (accept AsyncPoolIO):
- fetch_factory_from_chain_async()
- resolve_pool_type_by_probing_async()
- resolve_pool_type_async()
"""

from __future__ import annotations

import contextlib
from typing import TYPE_CHECKING, cast

import eth_abi.abi
from eth_abi.exceptions import DecodingError
from sqlalchemy import select
from web3.exceptions import Web3Exception

from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.database.models.pools import LiquidityPoolTable
from degenbot.exceptions.base import DegenbotValueError
from degenbot.provider.call_helpers import encode_function_calldata
from degenbot.registry.pool_type import pool_type_registry
from degenbot.types.pool_type import PoolFamily, PoolTypeDescriptor, derive_kind
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.builders.pool_io import AsyncPoolIOProtocol, PoolIO
    from degenbot.database.session_manager import DatabaseSessionManager
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
                pool_type_registry.get_v2_class(chain_id, pool_type.factory or "")
                or UniswapV2Pool,
            )
        case PoolFamily.CONCENTRATED_LIQUIDITY:
            return cast(
                "type[AbstractLiquidityPool]",
                pool_type_registry.get_v3_class(chain_id, pool_type.factory or "")
                or UniswapV3Pool,
            )
        case PoolFamily.STABLESWAP:
            return CurveStableswapPool
        case _:
            msg = f"No pool class for family {pool_type.family.value!r}"
            raise DegenbotValueError(message=msg)


def fetch_factory_from_chain(
    address: ChecksumAddress,
    *,
    chain_id: ChainId,  # noqa: ARG001 — kept for API consistency with resolve_pool_type
    io: PoolIO,
) -> ChecksumAddress | None:
    """Fetch the factory address from the pool contract's factory() method."""
    try:
        factory_result = io.call(
            to=address,
            data=encode_function_calldata("factory()", None),
        )
        (factory_raw,) = eth_abi.abi.decode(types=["address"], data=factory_result)
        return get_checksum_address(factory_raw)
    except (Web3Exception, DecodingError):
        return None


async def fetch_factory_from_chain_async(
    address: ChecksumAddress,
    *,
    chain_id: ChainId,  # noqa: ARG001 — kept for API consistency with resolve_pool_type_async
    io: AsyncPoolIOProtocol,
) -> ChecksumAddress | None:
    """Fetch the factory address from the pool contract's factory() method (async)."""
    try:
        factory_result = await io.call(
            to=address,
            data=encode_function_calldata("factory()", None),
        )
        (factory_raw,) = eth_abi.abi.decode(types=["address"], data=factory_result)
        return get_checksum_address(factory_raw)
    except (Web3Exception, DecodingError):
        return None


def resolve_pool_type_by_probing(
    address: ChecksumAddress,
    *,
    chain_id: ChainId,
    factory: ChecksumAddress,
    io: PoolIO,
) -> PoolTypeDescriptor:
    """Determine pool type by probing the contract on-chain.

    Tries V3 methods first (slot0), then V2 methods (getReserves),
    then Curve methods (coins). This is the fallback when neither
    the DB nor the registry identifies the factory.
    """
    # Try V3: slot0() exists → CONCENTRATED_LIQUIDITY
    try:
        io.call(
            to=address,
            data=encode_function_calldata("slot0()", None),
        )
    except Web3Exception:
        pass
    else:
        registry_descriptor = pool_type_registry.get_descriptor(chain_id, factory)
        if registry_descriptor is not None:
            return registry_descriptor
        return PoolTypeDescriptor(
            family=PoolFamily.CONCENTRATED_LIQUIDITY,
            variant=None,
            kind=derive_kind(PoolFamily.CONCENTRATED_LIQUIDITY, None),
            factory=factory,
        )

    # Try V2: getReserves() exists → CONSTANT_PRODUCT
    try:
        io.call(
            to=address,
            data=encode_function_calldata("getReserves()", None),
        )
    except Web3Exception:
        pass
    else:
        registry_descriptor = pool_type_registry.get_descriptor(chain_id, factory)
        if registry_descriptor is not None:
            return registry_descriptor
        return PoolTypeDescriptor(
            family=PoolFamily.CONSTANT_PRODUCT,
            variant=None,
            kind=derive_kind(PoolFamily.CONSTANT_PRODUCT, None),
            factory=factory,
        )

    # Fall through to Curve — assume STABLESWAP if nothing else matched
    return PoolTypeDescriptor(
        family=PoolFamily.STABLESWAP,
        variant=None,
        kind=derive_kind(PoolFamily.STABLESWAP, None),
        factory=factory,
    )


async def resolve_pool_type_by_probing_async(
    address: ChecksumAddress,
    *,
    chain_id: ChainId,
    factory: ChecksumAddress,
    io: AsyncPoolIOProtocol,
) -> PoolTypeDescriptor:
    """Determine pool type by probing the contract on-chain (async)."""
    # Try V3: slot0() exists → CONCENTRATED_LIQUIDITY
    try:
        await io.call(
            to=address,
            data=encode_function_calldata("slot0()", None),
        )
    except Web3Exception:
        pass
    else:
        registry_descriptor = pool_type_registry.get_descriptor(chain_id, factory)
        if registry_descriptor is not None:
            return registry_descriptor
        return PoolTypeDescriptor(
            family=PoolFamily.CONCENTRATED_LIQUIDITY,
            variant=None,
            kind=derive_kind(PoolFamily.CONCENTRATED_LIQUIDITY, None),
            factory=factory,
        )

    # Try V2: getReserves() exists → CONSTANT_PRODUCT
    try:
        await io.call(
            to=address,
            data=encode_function_calldata("getReserves()", None),
        )
    except Web3Exception:
        pass
    else:
        registry_descriptor = pool_type_registry.get_descriptor(chain_id, factory)
        if registry_descriptor is not None:
            return registry_descriptor
        return PoolTypeDescriptor(
            family=PoolFamily.CONSTANT_PRODUCT,
            variant=None,
            kind=derive_kind(PoolFamily.CONSTANT_PRODUCT, None),
            factory=factory,
        )

    # Fall through to Curve — assume STABLESWAP
    return PoolTypeDescriptor(
        family=PoolFamily.STABLESWAP,
        variant=None,
        kind=derive_kind(PoolFamily.STABLESWAP, None),
        factory=factory,
    )


def resolve_pool_type(
    address: ChecksumAddress,
    *,
    chain_id: ChainId,
    io: PoolIO,
    db: DatabaseSessionManager,
) -> PoolTypeDescriptor:
    """Resolve the pool type for the given address.

    Consults these sources in order:
    1. Database `kind` column (exact polymorphic type)
    2. PoolTypeRegistry registration (factory address → descriptor)
    3. On-chain probing (slot0 vs getReserves) when factory is unknown

    Raises DegenbotValueError if the type cannot be determined.
    """
    # Step 1: DB lookup — the `kind` column is the most direct signal
    with contextlib.suppress(Exception), db() as session:
        pool_from_db = session.scalar(
            select(LiquidityPoolTable).where(
                LiquidityPoolTable.address == address,
                LiquidityPoolTable.chain == chain_id,
            )
        )
        if pool_from_db is not None:
            kind = pool_from_db.kind
            descriptor = pool_type_registry.get_descriptor_by_kind(kind)
            if descriptor is not None:
                return PoolTypeDescriptor(
                    family=descriptor.family,
                    variant=descriptor.variant,
                    kind=descriptor.kind,
                    factory=get_checksum_address(pool_from_db.exchange.factory),
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

    raise DegenbotValueError(
        message=(
            f"Cannot resolve pool type for address {address} on chain {chain_id}. "
            f"The factory() call failed and no database entry exists."
        )
    )


async def resolve_pool_type_async(
    address: ChecksumAddress,
    *,
    chain_id: ChainId,
    io: AsyncPoolIOProtocol,
    db: DatabaseSessionManager,
) -> PoolTypeDescriptor:
    """Resolve the pool type for the given address (async).

    Same logic as resolve_pool_type but uses await for I/O calls.
    """
    # Step 1: DB lookup — the `kind` column is the most direct signal
    with contextlib.suppress(Exception), db() as session:
        pool_from_db = session.scalar(
            select(LiquidityPoolTable).where(
                LiquidityPoolTable.address == address,
                LiquidityPoolTable.chain == chain_id,
            )
        )
        if pool_from_db is not None:
            kind = pool_from_db.kind
            descriptor = pool_type_registry.get_descriptor_by_kind(kind)
            if descriptor is not None:
                return PoolTypeDescriptor(
                    family=descriptor.family,
                    variant=descriptor.variant,
                    kind=descriptor.kind,
                    factory=get_checksum_address(pool_from_db.exchange.factory),
                )

    # Step 2: Factory address lookup via PoolTypeRegistry
    factory = await fetch_factory_from_chain_async(address, chain_id=chain_id, io=io)
    if factory is not None:
        registry_descriptor = pool_type_registry.get_descriptor(chain_id, factory)
        if registry_descriptor is not None:
            return registry_descriptor

        # Step 3: No registry match — probe the contract
        return await resolve_pool_type_by_probing_async(
            address, chain_id=chain_id, factory=factory, io=io
        )

    raise DegenbotValueError(
        message=(
            f"Cannot resolve pool type for address {address} on chain {chain_id}. "
            f"The factory() call failed and no database entry exists."
        )
    )
