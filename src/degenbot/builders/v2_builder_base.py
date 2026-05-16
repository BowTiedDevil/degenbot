"""V2 builder base class and shared data types."""

from __future__ import annotations

import contextlib
from dataclasses import dataclass
from fractions import Fraction
from typing import TYPE_CHECKING

import eth_abi.abi
from sqlalchemy import select

from degenbot.checksum_cache import get_checksum_address
from degenbot.database.models.pools import LiquidityPoolTable, UniswapFeeMixin
from degenbot.exceptions.pool import LiquidityPoolError
from degenbot.logging import logger
from degenbot.provider.call_helpers import encode_function_calldata, raw_call
from degenbot.registry.pool_type import pool_type_registry
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.builders.erc20_builder import Erc20Builder
    from degenbot.connection.connection_manager import ConnectionManager
    from degenbot.database.session_manager import DatabaseSessionManager
    from degenbot.erc20 import Erc20Token
    from degenbot.provider.interface import ProviderAdapter
    from degenbot.registry import PoolRegistry, TokenRegistry
    from degenbot.types.abstract import AbstractLiquidityPool
    from degenbot.types.aliases import ChainId


@dataclass(frozen=True)
class V2CommonData:
    """Data fetched from DB/chain that all V2 variants need.

    Produced by V2BuilderBase._fetch_v2_common_data().
    Consumed by variant-specific build() methods.
    """

    pool_address: ChecksumAddress
    chain_id: ChainId
    factory: ChecksumAddress
    token0_address: ChecksumAddress
    token1_address: ChecksumAddress
    fee_token0: Fraction
    fee_token1: Fraction
    reserves0: int
    reserves1: int
    deployer: str
    init_hash: str
    state_block: int


class V2BuilderBase:  # noqa: B903
    """
    Base class for V2-style pool builders.

    Provides shared I/O orchestration (DB lookup, chain fetch,
    token construction, reserve fetch, registry lookup).
    Subclasses implement variant-specific construction and update.
    """

    def __init__(
        self,
        *,
        connections: ConnectionManager,
        db: DatabaseSessionManager,
        pools: PoolRegistry,
        tokens: TokenRegistry,
        erc20_builder: Erc20Builder,
    ) -> None:
        self._connections = connections
        self._db = db
        self._pools = pools
        self._tokens = tokens
        self._erc20_builder = erc20_builder

    def _fetch_v2_common_data(
        self,
        pool_address: str,
        *,
        chain_id: ChainId,
        state_block: int,
        deployer_address: str | None,
        init_hash: str | None,
        provider: ProviderAdapter,
    ) -> V2CommonData:
        """Fetch data shared by all V2 variants.

        Returns a frozen dataclass with all values needed
        for variant-specific construction.
        """

        pool_address = get_checksum_address(pool_address)

        # Try DB first
        pool_from_db = None
        with contextlib.suppress(Exception), self._db() as session:
            pool_from_db = session.scalar(
                select(LiquidityPoolTable).where(
                    LiquidityPoolTable.address == pool_address,
                    LiquidityPoolTable.chain == chain_id,
                )
            )

        # Get factory and token addresses
        if pool_from_db is not None:
            factory = get_checksum_address(pool_from_db.exchange.factory)
            token0_address = get_checksum_address(pool_from_db.token0.address)
            token1_address = get_checksum_address(pool_from_db.token1.address)
            if isinstance(pool_from_db, UniswapFeeMixin):
                fee_token0 = Fraction(pool_from_db.fee_token0, pool_from_db.fee_denominator)
                fee_token1 = Fraction(pool_from_db.fee_token1, pool_from_db.fee_denominator)
            else:
                fee_token0 = Fraction(3, 1000)
                fee_token1 = Fraction(3, 1000)
        else:
            # Fetch immutable values from chain
            try:
                factory_result = provider.call(
                    to=pool_address,
                    data=encode_function_calldata("factory()", None),
                )
                token0_result = provider.call(
                    to=pool_address,
                    data=encode_function_calldata("token0()", None),
                )
                token1_result = provider.call(
                    to=pool_address,
                    data=encode_function_calldata("token1()", None),
                )
            except Exception as exc:
                raise LiquidityPoolError(message="Could not decode contract data") from exc

            (factory_raw,) = eth_abi.abi.decode(types=["address"], data=factory_result)
            (token0_raw,) = eth_abi.abi.decode(types=["address"], data=token0_result)
            (token1_raw,) = eth_abi.abi.decode(types=["address"], data=token1_result)

            factory = get_checksum_address(factory_raw)
            token0_address = get_checksum_address(token0_raw)
            token1_address = get_checksum_address(token1_raw)

            # Default fee for V2 pools
            fee_token0 = Fraction(3, 1000)
            fee_token1 = Fraction(3, 1000)

        # Fetch reserves

        reserves0, reserves1 = raw_call(
            provider,
            address=pool_address,
            calldata=encode_function_calldata("getReserves()", None),
            return_types=["uint256", "uint256"],
            block_identifier=state_block,
        )

        # Determine deployer and init_hash from pool type registry
        deployer = factory
        resolved_init_hash = UniswapV2Pool.UNISWAP_V2_MAINNET_POOL_INIT_HASH
        registry_deployment = pool_type_registry.get_deployment(chain_id, factory)
        if registry_deployment is not None:
            if registry_deployment.pool_init_hash is not None:
                resolved_init_hash = registry_deployment.pool_init_hash
            if registry_deployment.deployer is not None:
                deployer = get_checksum_address(registry_deployment.deployer)

        deployer = get_checksum_address(deployer_address) if deployer_address else deployer
        resolved_init_hash = init_hash or resolved_init_hash

        return V2CommonData(
            pool_address=pool_address,
            chain_id=chain_id,
            factory=factory,
            token0_address=token0_address,
            token1_address=token1_address,
            fee_token0=fee_token0,
            fee_token1=fee_token1,
            reserves0=reserves0,
            reserves1=reserves1,
            deployer=deployer,
            init_hash=resolved_init_hash,
            state_block=state_block,
        )

    def _register_pool(
        self,
        pool: AbstractLiquidityPool,
        *,
        chain_id: ChainId,
    ) -> None:
        self._pools.add(pool_address=pool.address, chain_id=chain_id, pool=pool)

    @staticmethod
    def _log_pool(
        pool: AbstractLiquidityPool,
        *,
        silent: bool,
        token0: Erc20Token,
        token1: Erc20Token,
        reserves0: int,
        reserves1: int,
    ) -> None:
        if not silent:
            logger.info(pool.name)
            logger.info(f"• Token 0: {token0} - Reserves: {reserves0}")
            logger.info(f"• Token 1: {token1} - Reserves: {reserves1}")

    @staticmethod
    def _fetch_reserves(
        pool_address: str,
        provider: ProviderAdapter,
        *,
        block_identifier: int,
    ) -> tuple[int, int]:
        """Fetch current reserves from chain."""

        pool_address = get_checksum_address(pool_address)

        return raw_call(
            provider,
            address=pool_address,
            calldata=encode_function_calldata("getReserves()", None),
            return_types=["uint256", "uint256"],
            block_identifier=block_identifier,
        )
