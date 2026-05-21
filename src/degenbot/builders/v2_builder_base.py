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
from degenbot.provider.call_helpers import encode_function_calldata
from degenbot.registry.pool_type import pool_type_registry
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress
    from hexbytes import HexBytes

    from degenbot.builders.context import BuilderContext
    from degenbot.builders.pool_io import PoolIO
    from degenbot.erc20 import Erc20Token
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


class V2BuilderBase:
    """
    Base class for V2-style pool builders.

    Provides shared I/O orchestration (DB lookup, chain fetch,
    token construction, reserve fetch, registry lookup).
    Subclasses implement variant-specific construction and update.
    """

    def __init__(self, ctx: BuilderContext) -> None:
        self._default_chain_id = ctx.default_chain_id
        self._db = ctx.db
        self._pools = ctx.pools
        self._tokens = ctx.tokens
        self._erc20_builder = ctx.erc20_builder

    @staticmethod
    def decode_immutable_data(
        factory_result: HexBytes,
        token0_result: HexBytes,
        token1_result: HexBytes,
    ) -> tuple[ChecksumAddress, ChecksumAddress, ChecksumAddress]:
        """Decode raw call results into typed addresses."""

        (factory_raw,) = eth_abi.abi.decode(types=["address"], data=factory_result)
        (token0_raw,) = eth_abi.abi.decode(types=["address"], data=token0_result)
        (token1_raw,) = eth_abi.abi.decode(types=["address"], data=token1_result)
        return (
            get_checksum_address(factory_raw),
            get_checksum_address(token0_raw),
            get_checksum_address(token1_raw),
        )

    @staticmethod
    def extract_db_values(
        pool_from_db: LiquidityPoolTable,
    ) -> tuple[ChecksumAddress, ChecksumAddress, ChecksumAddress, Fraction, Fraction]:
        """Extract factory, token addresses, and fees from a DB row."""
        factory = get_checksum_address(pool_from_db.exchange.factory)
        token0_address = get_checksum_address(pool_from_db.token0.address)
        token1_address = get_checksum_address(pool_from_db.token1.address)
        if isinstance(pool_from_db, UniswapFeeMixin):
            fee_token0 = Fraction(pool_from_db.fee_token0, pool_from_db.fee_denominator)
            fee_token1 = Fraction(pool_from_db.fee_token1, pool_from_db.fee_denominator)
        else:
            fee_token0 = Fraction(3, 1000)
            fee_token1 = Fraction(3, 1000)
        return factory, token0_address, token1_address, fee_token0, fee_token1

    @staticmethod
    def resolve_deployer_and_init_hash(
        *,
        chain_id: ChainId,
        factory: ChecksumAddress,
        default_init_hash: str,
    ) -> tuple[str, str]:
        """Resolve deployer address and init hash from the pool type registry."""
        deployer = factory
        resolved_init_hash = default_init_hash
        registry_deployment = pool_type_registry.get_deployment(chain_id, factory)
        if registry_deployment is not None:
            if registry_deployment.pool_init_hash is not None:
                resolved_init_hash = registry_deployment.pool_init_hash
            if registry_deployment.deployer is not None:
                deployer = get_checksum_address(registry_deployment.deployer)

        return deployer, resolved_init_hash

    def _fetch_v2_common_data(
        self,
        pool_address: str,
        *,
        chain_id: ChainId,
        state_block: int,
        io: PoolIO,
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
            factory, token0_address, token1_address, fee_token0, fee_token1 = (
                V2BuilderBase.extract_db_values(pool_from_db)
            )
        else:
            # Fetch immutable values from chain
            try:
                factory_result = io.call(
                    to=pool_address,
                    data=encode_function_calldata("factory()", None),
                )
                token0_result = io.call(
                    to=pool_address,
                    data=encode_function_calldata("token0()", None),
                )
                token1_result = io.call(
                    to=pool_address,
                    data=encode_function_calldata("token1()", None),
                )
            except Exception as exc:
                raise LiquidityPoolError(message="Could not decode contract data") from exc

            factory, token0_address, token1_address = V2BuilderBase.decode_immutable_data(
                factory_result=factory_result,
                token0_result=token0_result,
                token1_result=token1_result,
            )

            # Default fee for V2 pools
            fee_token0 = Fraction(3, 1000)
            fee_token1 = Fraction(3, 1000)

        # Fetch reserves
        reserves_result = io.call(
            to=pool_address,
            data=encode_function_calldata("getReserves()", None),
            block=state_block,
        )
        reserves0, reserves1 = eth_abi.abi.decode(
            types=["uint256", "uint256"],
            data=reserves_result,
        )

        # Determine deployer and init_hash from pool type registry
        deployer, resolved_init_hash = V2BuilderBase.resolve_deployer_and_init_hash(
            chain_id=chain_id,
            factory=factory,
            default_init_hash=UniswapV2Pool.UNISWAP_V2_MAINNET_POOL_INIT_HASH,
        )

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
        io: PoolIO,
        *,
        block_identifier: int,
    ) -> tuple[int, int]:
        """Fetch current reserves from chain via PoolIO."""

        pool_address = get_checksum_address(pool_address)

        reserves_result = io.call(
            to=pool_address,
            data=encode_function_calldata("getReserves()", None),
            block=block_identifier,
        )
        reserves0, reserves1 = eth_abi.abi.decode(
            types=["uint256", "uint256"],
            data=reserves_result,
        )
        return reserves0, reserves1
