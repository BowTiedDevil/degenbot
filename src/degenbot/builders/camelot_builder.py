"""Builder for Camelot V2 pools."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

import eth_abi.abi

from degenbot.camelot.pools import CamelotLiquidityPool
from degenbot.checksum_cache import get_checksum_address
from degenbot.connection.connection_manager import ConnectionManager
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.provider.call_helpers import encode_function_calldata
from degenbot.registry import PoolRegistry, TokenRegistry
from degenbot.registry.pool_type import pool_type_registry
from degenbot.types.aliases import ChainId
from degenbot.uniswap.v2_types import UniswapV2PoolExternalUpdate

from degenbot.builders.erc20_builder import Erc20Builder
from degenbot.builders.v2_builder_base import V2BuilderBase

if TYPE_CHECKING:
    from web3.types import BlockIdentifier


class CamelotBuilder(V2BuilderBase):
    """Builds and updates Camelot V2 pools."""

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
    ) -> CamelotLiquidityPool:
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

        # Camelot-specific: fetch stableSwap, fee denominator, and fee percents
        stable_swap_result = provider.call(
            to=pool_address,
            data=encode_function_calldata("stableSwap()", None),
            block=state_block,
        )
        (stable_swap,) = eth_abi.abi.decode(types=["bool"], data=stable_swap_result)

        fee_denom_result = provider.call(
            to=pool_address,
            data=encode_function_calldata("FEE_DENOMINATOR()", None),
            block=state_block,
        )
        (fee_denominator,) = eth_abi.abi.decode(types=["uint256"], data=fee_denom_result)

        fee0_result = provider.call(
            to=pool_address,
            data=encode_function_calldata("token0FeePercent()", None),
            block=state_block,
        )
        (fee_token0_raw,) = eth_abi.abi.decode(types=["uint16"], data=fee0_result)

        fee1_result = provider.call(
            to=pool_address,
            data=encode_function_calldata("token1FeePercent()", None),
            block=state_block,
        )
        (fee_token1_raw,) = eth_abi.abi.decode(types=["uint16"], data=fee1_result)

        # Determine pool class from registry
        pool_class = pool_type_registry.get_v2_class(chain_id, common.factory)

        pool = pool_class(
            address=pool_address,
            token0=token0,
            token1=token1,
            factory=common.factory,
            fee_token0=fee_token0_raw,
            fee_token1=fee_token1_raw,
            fee_denominator=fee_denominator,
            reserves_token0=common.reserves0,
            reserves_token1=common.reserves1,
            stable_swap=stable_swap,
            chain_id=common.chain_id,
            state_block=common.state_block,
            deployer_address=common.deployer,
        )

        self._register_pool(pool, chain_id=chain_id)
        self._log_pool(pool, silent=silent, token0=token0, token1=token1, reserves0=common.reserves0, reserves1=common.reserves1)
        return pool

    def update(
        self,
        pool: Any,
        *,
        block_number: BlockIdentifier | None = None,
    ) -> bool:
        if not isinstance(pool, CamelotLiquidityPool):
            msg = f"CamelotBuilder cannot update {type(pool).__name__}"
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
