"""Builder for base Uniswap V2-style pools."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

from degenbot.builders.v2_builder_base import V2BuilderBase
from degenbot.checksum_cache import get_checksum_address
from degenbot.logging import logger
from degenbot.registry.pool_type import pool_type_registry
from degenbot.types.aliases import ChainId
from degenbot.types.pool_protocols import ConstantProductPool
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v2_types import UniswapV2PoolExternalUpdate

if TYPE_CHECKING:
    from web3.types import BlockIdentifier


class V2PoolBuilder(V2BuilderBase):
    """Builds and updates base Uniswap V2-style pools.

    Owns the full I/O choreography: DB lookup → RPC fetch → decode →
    construct pool → register.

    Aerodrome and Camelot pools are handled by their own builders
    (AerodromeV2Builder, CamelotBuilder), registered separately in Bot.
    """

    def build(
        self,
        pool_address: str,
        *,
        chain_id: ChainId | None = None,
        deployer_address: str | None = None,
        init_hash: str | None = None,
        state_block: int | None = None,
        silent: bool = False,
        state_cache_depth: int = 8,
    ) -> ConstantProductPool:
        """Fetch pool data from DB/RPC and construct an I/O-free V2-style pool."""

        pool_address = get_checksum_address(pool_address)
        chain_id = chain_id or self._connections.default_chain_id
        provider = self._connections.get_provider(chain_id)
        state_block = state_block if state_block is not None else provider.get_block_number()

        common = self._fetch_v2_common_data(
            pool_address,
            chain_id=chain_id,
            state_block=state_block,
            deployer_address=deployer_address,
            init_hash=init_hash,
            provider=provider,
        )

        # Build tokens
        token0 = self._erc20_builder.build(common.token0_address, chain_id=chain_id, silent=silent)
        token1 = self._erc20_builder.build(common.token1_address, chain_id=chain_id, silent=silent)

        # Determine pool class from registry
        pool_class = pool_type_registry.get_v2_class(chain_id, common.factory)

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
        )

        # Register pool
        self._register_pool(pool, chain_id=chain_id)

        if not silent:
            logger.info(pool.name)
            logger.info(f"• Token 0: {token0} - Reserves: {common.reserves0}")
            logger.info(f"• Token 1: {token1} - Reserves: {common.reserves1}")

        return pool

    def update(
        self,
        pool: Any,
        *,
        block_number: BlockIdentifier | None = None,
    ) -> bool:
        """Fetch current state from chain and push update to the pool."""
        if not isinstance(pool, UniswapV2Pool):
            msg = f"V2PoolBuilder cannot update {type(pool).__name__}"
            raise TypeError(msg)

        provider = self._connections.get_provider(pool.chain_id)
        _block_number = block_number if block_number is not None else provider.get_block_number()
        reserves0, reserves1 = self._fetch_reserves(pool.address, provider, block_identifier=_block_number)

        if pool.reserves_token0 == reserves0 and pool.reserves_token1 == reserves1:
            return False

        update = UniswapV2PoolExternalUpdate(
            block_number=_block_number,
            reserves_token0=reserves0,
            reserves_token1=reserves1,
        )
        pool.external_update(update)
        return True
