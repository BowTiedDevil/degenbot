"""Builder for Camelot V2 pools."""

from __future__ import annotations

from typing import TYPE_CHECKING

import eth_abi.abi

from degenbot.builders.v2_builder_base import V2BuilderBase
from degenbot.camelot.pools import CamelotLiquidityPool
from degenbot.checksum_cache import get_checksum_address
from degenbot.provider.call_helpers import encode_function_calldata
from degenbot.registry.pool_type import pool_type_registry
from degenbot.uniswap.v2_types import UniswapV2PoolExternalUpdate

if TYPE_CHECKING:
    from web3.types import BlockIdentifier

    from degenbot.builders.context import BuilderContext
    from degenbot.builders.pool_io import PoolIO
    from degenbot.builders.request import BuildRequest
    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool
    from degenbot.types.aliases import ChainId


class CamelotBuilder(V2BuilderBase):
    """Builds and updates Camelot V2 pools."""

    def __init__(self, ctx: BuilderContext) -> None:
        super().__init__(ctx)

    def build(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        io: PoolIO,
        request: BuildRequest,
    ) -> AbstractLiquidityPool:
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
            io=io,
        )

        # Build tokens
        token0 = self._erc20_builder.build(
            common.token0_address, chain_id=chain_id, silent=request.silent, io=io
        )
        token1 = self._erc20_builder.build(
            common.token1_address, chain_id=chain_id, silent=request.silent, io=io
        )

        # Camelot-specific: fetch stableSwap, fee denominator, and fee percents
        stable_swap_result = io.call(
            to=pool_address,
            data=encode_function_calldata("stableSwap()", None),
            block=state_block,
        )
        (stable_swap,) = eth_abi.abi.decode(types=["bool"], data=stable_swap_result)

        fee_denom_result = io.call(
            to=pool_address,
            data=encode_function_calldata("FEE_DENOMINATOR()", None),
            block=state_block,
        )
        (fee_denominator,) = eth_abi.abi.decode(types=["uint256"], data=fee_denom_result)

        fee0_result = io.call(
            to=pool_address,
            data=encode_function_calldata("token0FeePercent()", None),
            block=state_block,
        )
        (fee_token0_raw,) = eth_abi.abi.decode(types=["uint16"], data=fee0_result)

        fee1_result = io.call(
            to=pool_address,
            data=encode_function_calldata("token1FeePercent()", None),
            block=state_block,
        )
        (fee_token1_raw,) = eth_abi.abi.decode(types=["uint16"], data=fee1_result)

        # Determine pool class from registry
        pool_class = pool_type_registry.get_v2_class(chain_id, common.factory)
        if pool_class is None:
            msg = f"No V2 pool class registered for chain {chain_id}, factory {common.factory}"
            raise ValueError(msg)

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
        assert isinstance(pool, CamelotLiquidityPool)

        self._register_pool(pool, chain_id=chain_id)
        self._log_pool(
            pool,
            silent=request.silent,
            token0=token0,
            token1=token1,
            reserves0=common.reserves0,
            reserves1=common.reserves1,
        )
        return pool

    def update(
        self,
        pool: AbstractLiquidityPool,
        *,
        block_number: BlockIdentifier | None = None,
        io: PoolIO | None = None,
    ) -> bool:
        if not isinstance(pool, CamelotLiquidityPool):
            msg = f"CamelotBuilder cannot update {type(pool).__name__}"
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
