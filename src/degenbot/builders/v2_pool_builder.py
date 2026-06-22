"""Builder for base Uniswap V2-style pools."""

from __future__ import annotations

from dataclasses import dataclass
from fractions import Fraction
from typing import TYPE_CHECKING

import eth_abi.abi

from degenbot.builders.v2_builder_base import V2BuilderBase
from degenbot.checksum_cache import get_checksum_address
from degenbot.degenbot_rs import dex_identity
from degenbot.logging import logger
from degenbot.provider.call_helpers import encode_function_calldata
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


@dataclass(frozen=True)
class CamelotStateFetch:
    """Camelot-specific on-chain state (ADR-005 slice 7 step 4b fold).

    Camelot pairs expose their fee + stable flag via 4 non-standard calls
    (``stableSwap()``, ``FEE_DENOMINATOR()``, ``token0FeePercent()``,
    ``token1FeePercent()``) that the generic V2 common-fetch path doesn't
    cover. The folded V2 builder fetches these only when the resolved
    ``dex_identity.variant`` is a Camelot preset.
    """

    stable_swap: bool
    fee_token0: int
    fee_token1: int
    fee_denominator: int


class V2PoolBuilder(V2BuilderBase):
    """Builds and updates base Uniswap V2-style pools.

    Owns the full I/O choreography: DB lookup → RPC fetch → decode →
    construct pool → register.

    ADR-005 slice 7 step 4b: the hollow DEX subclasses were deleted — this
    builder handles ALL V2-family DEXes (Uniswap/Sushi/Pancake/Swapbased/
    Camelot). Camelot's 4 non-standard on-chain fetches + the stable-preset
    switch are folded into ``build`` as a branch keyed on
    ``dex_identity.variant``. Aerodrome remains its own builder (deferred per
    TODO-e30504ed).
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

        # Resolve the canonical DexIdentity preset (ADR-005 slice 7 step 3) —
        # carries the variant tag, reserves ABI shape, + default fees. Passed
        # to the companion as metadata; explicit factory/fees (from on-chain
        # fetches) still take precedence in the companion constructor.
        dex = pool_type_registry.get_v2_identity(chain_id, common.factory)

        # ADR-005 slice 7 step 4b: Camelot fold. Camelot pairs expose their fee
        # + stable flag via 4 non-standard calls the common-fetch path doesn't
        # cover. The branch keys on ``dex.variant`` being a Camelot preset;
        # when stable, switch to the ``camelot-v2-stable`` preset.
        camelot: CamelotStateFetch | None = None
        fee_token0 = common.fee_token0
        fee_token1 = common.fee_token1
        gamma_numer0 = common.fee_token0.denominator - common.fee_token0.numerator
        fee_denom0 = common.fee_token0.denominator
        gamma_numer1 = common.fee_token1.denominator - common.fee_token1.numerator
        fee_denom1 = common.fee_token1.denominator
        if dex is not None and dex.variant.startswith("camelot-v2"):
            camelot = self._fetch_camelot_state(common.pool_address, io=io, state_block=state_block)
            fee_token0 = Fraction(camelot.fee_token0, camelot.fee_denominator)
            fee_token1 = Fraction(camelot.fee_token1, camelot.fee_denominator)
            gamma_numer0 = camelot.fee_denominator - camelot.fee_token0
            fee_denom0 = camelot.fee_denominator
            gamma_numer1 = camelot.fee_denominator - camelot.fee_token1
            fee_denom1 = camelot.fee_denominator
            if camelot.stable_swap:
                stable_dex = dex_identity("camelot-v2-stable")
                assert stable_dex is not None, "camelot-v2-stable preset must resolve"
                dex = stable_dex

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
            gamma_numer0=gamma_numer0,
            fee_denom0=fee_denom0,
            gamma_numer1=gamma_numer1,
            fee_denom1=fee_denom1,
            factory=common.factory,
            update_block=common.state_block,
        )
        py_pool = self._py_bot.get_pool(pool_id)
        assert py_pool is not None, "register_v2_pool returned a pool_id with no handle"

        pool = pool_class(
            py_pool,
            address=common.pool_address,
            token0=token0,
            token1=token1,
            dex=dex,
            chain_id=common.chain_id,
            factory=common.factory,
            fee_token0=fee_token0,
            fee_token1=fee_token1,
            deployer_address=common.deployer,
            init_hash=common.init_hash,
        )

        # Camelot stable-strategy wiring (folded in slice 7 step 4a): the
        # ``stable_swap`` flag + ``fee_denominator`` drive the solidly-stable
        # calc + hop branch on the base ``LiquidityPool``.
        if camelot is not None:
            pool.stable_swap = camelot.stable_swap
            pool.fee_denominator = camelot.fee_denominator

        # Register pool
        self._register_pool(pool, chain_id=chain_id)

        if not request.silent:
            logger.info(pool.name)
            logger.info(f"• Token 0: {token0} - Reserves: {common.reserves0}")
            logger.info(f"• Token 1: {token1} - Reserves: {common.reserves1}")

        return pool

    @staticmethod
    def _fetch_camelot_state(
        pool_address: str,
        *,
        io: PoolIO,
        state_block: int,
    ) -> CamelotStateFetch:
        """Fetch Camelot-specific on-chain state (4 non-standard calls).

        Mirrors the deleted ``CamelotBuilder.build`` fetch sequence: the
        stable flag, the integer fee denominator, + the per-direction fee
        percents. The generic V2 common-fetch path doesn't cover these
        (Camelot's ABI differs from Uniswap V2's ``fee()``).

        Returns:
            A ``CamelotStateFetch`` with stable_swap + integer fee fields.

        """
        # ADR-005 slice 14q: when io is a PyBotIo, delegate all 4 RPCs to Rust.
        fetch_camelot_state = getattr(io, "fetch_camelot_state", None)
        if fetch_camelot_state is not None:
            stable_swap, fee_denominator, fee_token0, fee_token1 = fetch_camelot_state(
                pool_address, block=state_block
            )
            return CamelotStateFetch(
                stable_swap=stable_swap,
                fee_token0=int(fee_token0),
                fee_token1=int(fee_token1),
                fee_denominator=int(fee_denominator),
            )
        stable_result = io.call(
            to=pool_address,
            data=encode_function_calldata("stableSwap()", None),
            block=state_block,
        )
        (stable_swap,) = eth_abi.abi.decode(types=["bool"], data=stable_result)

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
        (fee_token0,) = eth_abi.abi.decode(types=["uint16"], data=fee0_result)

        fee1_result = io.call(
            to=pool_address,
            data=encode_function_calldata("token1FeePercent()", None),
            block=state_block,
        )
        (fee_token1,) = eth_abi.abi.decode(types=["uint16"], data=fee1_result)

        return CamelotStateFetch(
            stable_swap=stable_swap,
            fee_token0=fee_token0,
            fee_token1=fee_token1,
            fee_denominator=fee_denominator,
        )

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
