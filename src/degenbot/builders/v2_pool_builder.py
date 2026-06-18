"""Builder for base Uniswap V2-style pools."""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.builders.v2_builder_base import V2BuilderBase
from degenbot.checksum_cache import get_checksum_address
from degenbot.logging import logger
from degenbot.registry.pool_type import pool_type_registry
from degenbot.uniswap.liquidity_pool import LiquidityPool
from degenbot.uniswap.v2_types import UniswapV2PoolExternalUpdate

if TYPE_CHECKING:
    from web3.types import BlockIdentifier

    from degenbot.builders.context import BuilderContext
    from degenbot.builders.pool_io import PoolIO
    from degenbot.builders.request import BuildRequest
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
        """Initialize the instance."""
        super().__init__(ctx)

    def build(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        io: PoolIO,
        request: BuildRequest,
    ) -> AbstractLiquidityPool:
        """Fetch pool data from DB/RPC and construct an I/O-free V2-style pool.

        Returns:
            The computed value.

        Raises:
            ValueError: If the operation fails.

        """
        pool_address = get_checksum_address(address)
        chain_id = chain_id or self._default_chain_id
        assert chain_id is not None, "chain_id must be provided or set as default_chain_id"
        state_block = (
            request.state_block if request.state_block is not None else io.get_block_number()
        )

        common = self._fetch_v2_common_data(
            pool_address,
            chain_id=chain_id,
            state_block=state_block,
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

        # Register the pool in the shared Rust Bot and wrap the handle with
        # the Python companion (ADR-005 slice 4). The builder's update_block is
        # the fetched state block; reserves are the genesis delta's after-values.
        # ``gamma_numer`` is the retained POST-FEE fraction (Rust convention per
        # the ``IntHopState`` docs: ``gamma_numer=997`` for 0.3%); the source
        # ``fee_tokenN`` Fraction is the FEE — convert by subtraction.
        pool_id = self._py_bot.register_v2_pool(
            address=common.pool_address,
            token0=common.token0_address,
            token1=common.token1_address,
            reserve0=common.reserves0,
            reserve1=common.reserves1,
            gamma_numer0=common.fee_token0.denominator - common.fee_token0.numerator,
            fee_denom0=common.fee_token0.denominator,
            gamma_numer1=common.fee_token1.denominator - common.fee_token1.numerator,
            fee_denom1=common.fee_token1.denominator,
            factory=common.factory,
            update_block=common.state_block,
        )
        py_pool = self._py_bot.get_pool(pool_id)
        assert py_pool is not None, "register_v2_pool returned a pool_id with no handle"

        pool = pool_class(
            py_pool,
            address=common.pool_address,
            chain_id=common.chain_id,
            token0=token0,
            token1=token1,
            factory=common.factory,
            fee_token0=common.fee_token0,
            fee_token1=common.fee_token1,
            deployer_address=common.deployer,
            init_hash=common.init_hash,
        )

        # Register pool
        self._register_pool(pool, chain_id=chain_id)

        if not request.silent:
            logger.info(pool.name)
            logger.info(f"• Token 0: {token0} - Reserves: {common.reserves0}")
            logger.info(f"• Token 1: {token1} - Reserves: {common.reserves1}")

        return pool

    @staticmethod
    def update(
        pool: AbstractLiquidityPool,
        *,
        block_number: BlockIdentifier | None = None,
        io: PoolIO | None = None,
    ) -> bool:
        """Fetch current state from chain and push update to the pool.

        Returns:
            The computed value.

        Raises:
            TypeError: If the operation fails.

        """
        if not isinstance(pool, LiquidityPool):
            msg = f"V2PoolBuilder cannot update {type(pool).__name__}"
            raise TypeError(msg)

        assert pool.chain_id is not None
        assert io is not None, "io must be provided for update()"
        block_number_ = block_number if block_number is not None else io.get_block_number()
        block_number_ = int(block_number_) if not isinstance(block_number_, int) else block_number_
        reserves0, reserves1 = V2BuilderBase._fetch_reserves(  # noqa: SLF001
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
