"""Builder for Aerodrome V2 pools."""

from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING, Any

import eth_abi.abi

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.aerodrome.types import AerodromeV2PoolExternalUpdate
from degenbot.builders.v2_builder_base import V2BuilderBase
from degenbot.checksum_cache import get_checksum_address
from degenbot.provider.call_helpers import encode_function_calldata
from degenbot.registry.pool_type import pool_type_registry

if TYPE_CHECKING:
    from web3.types import BlockIdentifier

    from degenbot.builders.context import BuilderContext
    from degenbot.builders.pool_io import PoolIO
    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool
    from degenbot.types.aliases import ChainId


class AerodromeV2Builder(V2BuilderBase):
    """Builds and updates Aerodrome V2 pools."""

    def __init__(self, ctx: BuilderContext) -> None:
        super().__init__(ctx)

    def build(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        deployer_address: str | None = None,
        init_hash: str | None = None,
        state_block: int | None = None,
        silent: bool = False,
        state_cache_depth: int = 8,
        io: PoolIO,
        **kwargs: Any,  # noqa: ARG002
    ) -> AbstractLiquidityPool:
        pool_address = get_checksum_address(address)
        chain_id = chain_id or self._default_chain_id
        assert chain_id is not None, "chain_id must be provided or set as default_chain_id"
        state_block = state_block if state_block is not None else io.get_block_number()

        common = self._fetch_v2_common_data(
            pool_address,
            chain_id=chain_id,
            state_block=state_block,
            deployer_address=deployer_address,
            init_hash=init_hash,
            io=io,
        )

        # Build tokens
        token0 = self._erc20_builder.build(
            common.token0_address, chain_id=chain_id, silent=silent, io=io
        )
        token1 = self._erc20_builder.build(
            common.token1_address, chain_id=chain_id, silent=silent, io=io
        )

        # Aerodrome-specific: fetch stable flag and fee
        stable_result = io.call(
            to=pool_address,
            data=encode_function_calldata("stable()", None),
            block=state_block,
        )
        (stable,) = eth_abi.abi.decode(types=["bool"], data=stable_result)

        fee_result = io.call(
            to=common.factory,
            data=encode_function_calldata(
                "getFee(address,bool)",
                [pool_address, stable],
            ),
            block=state_block,
        )
        (fee_raw,) = eth_abi.abi.decode(types=["uint256"], data=fee_result)
        fee = Fraction(fee_raw, AerodromeV2Pool.FEE_DENOMINATOR)

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
            fee=fee,
            stable=stable,
            reserves_token0=common.reserves0,
            reserves_token1=common.reserves1,
            chain_id=common.chain_id,
            deployer_address=common.deployer,
            state_block=common.state_block,
            state_cache_depth=state_cache_depth,
        )
        assert isinstance(pool, AerodromeV2Pool)

        self._register_pool(pool, chain_id=chain_id)
        self._log_pool(
            pool,
            silent=silent,
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
        if not isinstance(pool, AerodromeV2Pool):
            msg = f"AerodromeV2Builder cannot update {type(pool).__name__}"
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

        update = AerodromeV2PoolExternalUpdate(
            block_number=block_number_,
            reserves_token0=reserves0,
            reserves_token1=reserves1,
        )
        pool.external_update(update)
        return True
