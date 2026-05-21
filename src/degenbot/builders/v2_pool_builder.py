"""Builder for base Uniswap V2-style pools."""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.builders.v2_builder_base import V2BuilderBase
from degenbot.checksum_cache import get_checksum_address
from degenbot.logging import logger
from degenbot.registry.pool_type import pool_type_registry
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v2_types import UniswapV2PoolExternalUpdate

if TYPE_CHECKING:
    from web3.types import BlockIdentifier

    from degenbot.builders.context import BuilderContext
    from degenbot.builders.pool_io import PoolIO
    from degenbot.builders.request import BuildPoolRequest
    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool
    from degenbot.types.aliases import ChainId


class V2PoolBuilder(V2BuilderBase):
    """Builds and updates base Uniswap V2-style pools.

    Owns the full I/O choreography: DB lookup → RPC fetch → decode →
    construct pool → register.

    Aerodrome and Camelot pools are handled by their own builders
    (AerodromeV2Builder, CamelotBuilder), registered separately in Bot.
    """

    def __init__(self, ctx: BuilderContext) -> None:
        super().__init__(ctx)

    def build(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        io: PoolIO,
        request: BuildPoolRequest,
    ) -> AbstractLiquidityPool:
        """Fetch pool data from DB/RPC and construct an I/O-free V2-style pool."""

        pool_address = get_checksum_address(address)
        chain_id = chain_id or self._default_chain_id
        assert chain_id is not None, "chain_id must be provided or set as default_chain_id"
        state_block = (
            request.state_block
            if request.state_block is not None
            else io.get_block_number()
        )

        common = self._fetch_v2_common_data(
            pool_address,
            chain_id=chain_id,
            state_block=state_block,
            deployer_address=request.deployer_address,
            init_hash=request.init_hash,
            io=io,
        )

        # Build tokens
        token0 = self._erc20_builder.build(
            common.token0_address, chain_id=chain_id, silent=request.silent, io=io
        )
        token1 = self._erc20_builder.build(
            common.token1_address, chain_id=chain_id, silent=request.silent, io=io
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
            state_cache_depth=request.state_cache_depth,
        )

        # Register pool
        self._register_pool(pool, chain_id=chain_id)

        if not request.silent:
            logger.info(pool.name)
            logger.info(f"• Token 0: {token0} - Reserves: {common.reserves0}")
            logger.info(f"• Token 1: {token1} - Reserves: {common.reserves1}")

        return pool

    def update(
        self,
        pool: AbstractLiquidityPool,
        *,
        block_number: BlockIdentifier | None = None,
        io: PoolIO | None = None,
    ) -> bool:
        """Fetch current state from chain and push update to the pool."""
        if not isinstance(pool, UniswapV2Pool):
            msg = f"V2PoolBuilder cannot update {type(pool).__name__}"
            raise TypeError(msg)

        assert pool.chain_id is not None
        assert io is not None, "io must be provided for update()"
        block_number_ = block_number if block_number is not None else io.get_block_number()
        block_number_ = int(block_number_) if not isinstance(block_number_, int) else block_number_
        reserves0, reserves1 = self._fetch_reserves(
            pool.address, io, block_identifier=block_number_
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
