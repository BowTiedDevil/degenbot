"""Async builder for base Uniswap V2-style pools."""

from __future__ import annotations

import contextlib
from fractions import Fraction
from typing import TYPE_CHECKING, Any

import eth_abi.abi
from sqlalchemy import select

from degenbot.builders.v2_builder_base import V2BuilderBase, V2CommonData
from degenbot.checksum_cache import get_checksum_address
from degenbot.database.models.pools import LiquidityPoolTable
from degenbot.exceptions.pool import LiquidityPoolError
from degenbot.logging import logger
from degenbot.provider.call_helpers import encode_function_calldata
from degenbot.registry.pool_type import pool_type_registry
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v2_types import UniswapV2PoolExternalUpdate

if TYPE_CHECKING:
    from web3.types import BlockIdentifier

    from degenbot.builders.async_context import AsyncBuilderContext
    from degenbot.builders.pool_io import AsyncPoolIO
    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool
    from degenbot.types.aliases import ChainId


class AsyncV2PoolBuilder:
    """Async counterpart of V2PoolBuilder.

    Builds UniswapV2Pool instances using AsyncPoolIO for I/O.
    Shares pure decode/resolve logic with V2BuilderBase via static methods.
    """

    def __init__(self, ctx: AsyncBuilderContext) -> None:
        self._default_chain_id = ctx.default_chain_id
        self._db = ctx.db
        self._pools = ctx.pools
        self._tokens = ctx.tokens
        self._erc20_builder = ctx.erc20_builder

    async def _fetch_v2_common_data(
        self,
        pool_address: str,
        *,
        chain_id: ChainId,
        state_block: int,
        deployer_address: str | None,
        init_hash: str | None,
        io: AsyncPoolIO,
    ) -> V2CommonData:
        """Fetch data shared by all V2 variants using async I/O."""

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
            # Fetch immutable values from chain (async)
            try:
                factory_result = await io.call(
                    to=pool_address,
                    data=encode_function_calldata("factory()", None),
                )
                token0_result = await io.call(
                    to=pool_address,
                    data=encode_function_calldata("token0()", None),
                )
                token1_result = await io.call(
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

        # Fetch reserves (async)
        reserves_result = await io.call(
            to=pool_address,
            data=encode_function_calldata("getReserves()", None),
            block=state_block,
        )
        reserves0, reserves1 = eth_abi.abi.decode(
            types=["uint256", "uint256"],
            data=reserves_result,
        )

        # Resolve deployer and init_hash (pure — shared with sync builder)
        deployer, resolved_init_hash = V2BuilderBase.resolve_deployer_and_init_hash(
            chain_id=chain_id,
            factory=factory,
            default_init_hash=UniswapV2Pool.UNISWAP_V2_MAINNET_POOL_INIT_HASH,
            deployer_override=deployer_address,
            init_hash_override=init_hash,
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

    async def build(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        deployer_address: str | None = None,
        init_hash: str | None = None,
        state_block: int | None = None,
        silent: bool = False,
        state_cache_depth: int = 8,
        io: AsyncPoolIO,
        **kwargs: Any,  # noqa: ARG002
    ) -> AbstractLiquidityPool:
        """Fetch pool data from DB/RPC and construct an I/O-free V2-style pool."""

        pool_address = get_checksum_address(address)
        chain_id = chain_id or self._default_chain_id
        assert chain_id is not None, "chain_id must be provided or set as default_chain_id"

        state_block = state_block if state_block is not None else await io.get_block_number()

        common = await self._fetch_v2_common_data(
            pool_address,
            chain_id=chain_id,
            state_block=state_block,
            deployer_address=deployer_address,
            init_hash=init_hash,
            io=io,
        )

        # Build tokens (async)
        token0 = await self._erc20_builder.build(
            common.token0_address, chain_id=chain_id, silent=silent, io=io
        )
        token1 = await self._erc20_builder.build(
            common.token1_address, chain_id=chain_id, silent=silent, io=io
        )

        # Determine pool class from registry
        pool_class = pool_type_registry.get_v2_class(chain_id, common.factory)
        if pool_class is None:
            msg = f"No V2 pool class registered for chain {chain_id}, factory {common.factory}"
            raise ValueError(msg)

        pool = pool_class(
            address=pool_address,
            chain_id=common.chain_id,
            token0=token0,
            token1=token1,
            factory=common.factory,
            fee_token0=common.fee_token0,
            fee_token1=common.fee_token1,
            reserves_token0=common.reserves0,
            reserves_token1=common.reserves1,
            state_block=common.state_block,
            deployer_address=common.deployer,
            init_hash=common.init_hash,
            state_cache_depth=state_cache_depth,
        )

        # Register pool
        self._pools.add(pool_address=pool.address, chain_id=chain_id, pool=pool)

        if not silent:
            logger.info(pool.name)
            logger.info(f"• Token 0: {token0} - Reserves: {common.reserves0}")
            logger.info(f"• Token 1: {token1} - Reserves: {common.reserves1}")

        return pool

    async def update(  # noqa: PLR6301
        self,
        pool: AbstractLiquidityPool,
        *,
        block_number: BlockIdentifier | None = None,
        io: AsyncPoolIO | None = None,
    ) -> bool:
        """Fetch current state from chain and push update to the pool."""
        assert io is not None, "io must be provided for update()"
        if not isinstance(pool, UniswapV2Pool):
            msg = f"AsyncV2PoolBuilder cannot update {type(pool).__name__}"
            raise TypeError(msg)

        assert pool.chain_id is not None

        raw_block = block_number if block_number is not None else await io.get_block_number()
        block_number_ = int(raw_block) if not isinstance(raw_block, int) else raw_block

        reserves_result = await io.call(
            to=pool.address,
            data=encode_function_calldata("getReserves()", None),
            block=block_number_,
        )
        reserves0, reserves1 = eth_abi.abi.decode(
            types=["uint256", "uint256"],
            data=reserves_result,
        )

        if pool.reserves_token0 == reserves0 and pool.reserves_token1 == reserves1:
            return False

        update = UniswapV2PoolExternalUpdate(
            block_number=block_number_,
            reserves_token0=reserves0,
            reserves_token1=reserves1,
        )
        pool.external_update(update)
        return True
