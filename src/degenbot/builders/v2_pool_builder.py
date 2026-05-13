from __future__ import annotations

import contextlib
from fractions import Fraction
from typing import TYPE_CHECKING, Any

import eth_abi.abi
from sqlalchemy import select

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.aerodrome.types import AerodromeV2PoolExternalUpdate
from degenbot.builders.erc20_builder import Erc20Builder
from degenbot.camelot.pools import CamelotLiquidityPool
from degenbot.checksum_cache import get_checksum_address
from degenbot.connection.connection_manager import ConnectionManager
from degenbot.database.models.pools import LiquidityPoolTable
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.exceptions.liquidity_pool import LiquidityPoolError
from degenbot.functions import encode_function_calldata, raw_call
from degenbot.logging import logger
from degenbot.registry import PoolRegistry, TokenRegistry
from degenbot.registry.pool_type import pool_type_registry
from degenbot.types.aliases import ChainId
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v2_types import UniswapV2PoolExternalUpdate

if TYPE_CHECKING:
    from web3.types import BlockIdentifier


class V2PoolBuilder:
    """
    Builds and updates V2-style constant-product liquidity pools.

    Owns the full I/O choreography: DB lookup → RPC fetch → decode →
    construct pool → register.
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

    def build(
        self,
        pool_address: str,
        *,
        chain_id: ChainId | None = None,
        deployer_address: str | None = None,
        init_hash: str | None = None,
        state_block: int | None = None,
        silent: bool = False,
    ) -> UniswapV2Pool:  # type: ignore[name-defined]
        """Fetch pool data from DB/RPC and construct an I/O-free UniswapV2Pool."""

        pool_address = get_checksum_address(pool_address)
        chain_id = chain_id or self._connections.default_chain_id
        provider = self._connections.get_provider(chain_id)

        state_block = state_block if state_block is not None else provider.get_block_number()

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
            token0_address = pool_from_db.token0.address
            token1_address = pool_from_db.token1.address
            fee_token0 = Fraction(pool_from_db.fee_token0, pool_from_db.fee_denominator)
            fee_token1 = Fraction(pool_from_db.fee_token1, pool_from_db.fee_denominator)
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

        # Build tokens
        token0 = self._erc20_builder.build(token0_address, chain_id=chain_id, silent=silent)
        token1 = self._erc20_builder.build(token1_address, chain_id=chain_id, silent=silent)

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
        init_hash = UniswapV2Pool.UNISWAP_V2_MAINNET_POOL_INIT_HASH
        registry_deployment = pool_type_registry.get_deployment(chain_id, factory)
        if registry_deployment is not None:
            if registry_deployment.pool_init_hash is not None:
                init_hash = registry_deployment.pool_init_hash
            if registry_deployment.deployer is not None:
                deployer = registry_deployment.deployer

        deployer = deployer_address or deployer
        init_hash = init_hash or init_hash

        # Determine pool class from registry
        pool_class = pool_type_registry.get_v2_class(chain_id, factory)

        # Construct the pool — handle variant-specific I/O here instead of
        # delegating to pool classmethods
        if issubclass(pool_class, AerodromeV2Pool):
            pool = self._build_aerodrome_v2(
                pool_address=pool_address,
                pool_class=pool_class,
                token0=token0,
                token1=token1,
                factory=factory,
                reserves0=reserves0,
                reserves1=reserves1,
                provider=provider,
                state_block=state_block,
                deployer=deployer,
            )
        elif issubclass(pool_class, CamelotLiquidityPool):
            pool = self._build_camelot(
                pool_address=pool_address,
                pool_class=pool_class,
                token0=token0,
                token1=token1,
                factory=factory,
                reserves0=reserves0,
                reserves1=reserves1,
                provider=provider,
                state_block=state_block,
                deployer=deployer,
            )
        else:
            pool = pool_class(
                address=pool_address,
                chain_id=chain_id,
                token0=token0,
                token1=token1,
                factory=factory,
                fee_token0=fee_token0,
                fee_token1=fee_token1,
                reserves_token0=reserves0,
                reserves_token1=reserves1,
                state_block=state_block,
                deployer_address=deployer,
                init_hash=init_hash,
            )

        # Register pool
        self._pools.add(pool_address=pool.address, chain_id=chain_id, pool=pool)

        if not silent:
            logger.info(pool.name)
            logger.info(f"• Token 0: {token0} - Reserves: {reserves0}")
            logger.info(f"• Token 1: {token1} - Reserves: {reserves1}")

        return pool

    def _build_aerodrome_v2(
        self,
        *,
        pool_address: str,
        pool_class: type[AerodromeV2Pool],
        token0: Any,
        token1: Any,
        factory: str,
        reserves0: int,
        reserves1: int,
        provider: Any,
        state_block: int,
        deployer: str,
    ) -> AerodromeV2Pool:
        """Build an Aerodrome V2 pool by fetching stable flag and fee from chain."""
        # Fetch stable flag from pool contract
        stable_result = provider.call(
            to=pool_address,
            data=encode_function_calldata("stable()", None),
            block=state_block,
        )
        (stable,) = eth_abi.abi.decode(types=["bool"], data=stable_result)

        # Fetch fee from factory contract: getFee(pool, stable)
        fee_result = provider.call(
            to=factory,
            data=encode_function_calldata(
                "getFee(address,bool)",
                [pool_address, stable],
            ),
            block=state_block,
        )
        (fee_raw,) = eth_abi.abi.decode(types=["uint256"], data=fee_result)
        fee = Fraction(fee_raw, AerodromeV2Pool.FEE_DENOMINATOR)

        return pool_class(
            address=pool_address,
            token0=token0,
            token1=token1,
            factory=factory,
            fee=fee,
            stable=stable,
            reserves_token0=reserves0,
            reserves_token1=reserves1,
            chain_id=token0.chain_id,
            deployer_address=deployer,
            state_block=state_block,
        )

    def _build_camelot(
        self,
        *,
        pool_address: str,
        pool_class: type[CamelotLiquidityPool],
        token0: Any,
        token1: Any,
        factory: str,
        reserves0: int,
        reserves1: int,
        provider: Any,
        state_block: int,
        deployer: str,
    ) -> CamelotLiquidityPool:
        """Build a Camelot pool by fetching stableSwap, fee denominator, and fee percents."""
        # Fetch stableSwap flag
        stable_swap_result = provider.call(
            to=pool_address,
            data=encode_function_calldata("stableSwap()", None),
            block=state_block,
        )
        (stable_swap,) = eth_abi.abi.decode(types=["bool"], data=stable_swap_result)

        # Fetch fee denominator
        fee_denom_result = provider.call(
            to=pool_address,
            data=encode_function_calldata("FEE_DENOMINATOR()", None),
            block=state_block,
        )
        (fee_denominator,) = eth_abi.abi.decode(types=["uint256"], data=fee_denom_result)

        # Fetch fee percents
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

        return pool_class(
            address=pool_address,
            token0=token0,
            token1=token1,
            factory=factory,
            fee_token0=fee_token0_raw,
            fee_token1=fee_token1_raw,
            fee_denominator=fee_denominator,
            reserves_token0=reserves0,
            reserves_token1=reserves1,
            stable_swap=stable_swap,
            chain_id=token0.chain_id,
            state_block=state_block,
            deployer_address=deployer,
        )

    def update(
        self,
        pool: Any,
        *,
        block_number: BlockIdentifier | None = None,
    ) -> bool:
        """Fetch current state from chain and push update to the pool."""
        if isinstance(pool, AerodromeV2Pool):
            return self._update_aerodrome_v2(pool, block_number=block_number)
        if isinstance(pool, UniswapV2Pool):
            return self._update_uniswap_v2(pool, block_number=block_number)
        msg = f"V2PoolBuilder cannot update {type(pool).__name__}"
        raise TypeError(msg)

    def _update_uniswap_v2(
        self, pool: UniswapV2Pool, *, block_number: BlockIdentifier | None
    ) -> bool:
        provider = self._connections.get_provider(pool.chain_id)
        _block_number = block_number if block_number is not None else provider.get_block_number()
        reserves0, reserves1 = raw_call(
            provider,
            address=pool.address,
            calldata=encode_function_calldata("getReserves()", None),
            return_types=["uint256", "uint256"],
            block_identifier=_block_number,
        )

        if pool.reserves_token0 == reserves0 and pool.reserves_token1 == reserves1:
            return False

        update = UniswapV2PoolExternalUpdate(
            block_number=_block_number,
            reserves_token0=reserves0,
            reserves_token1=reserves1,
        )
        pool.external_update(update)
        return True

    def _update_aerodrome_v2(
        self, pool: AerodromeV2Pool, *, block_number: BlockIdentifier | None
    ) -> bool:
        provider = self._connections.get_provider(pool.chain_id)
        _block_number = block_number if block_number is not None else provider.get_block_number()
        reserves0, reserves1 = raw_call(
            provider,
            address=pool.address,
            calldata=encode_function_calldata("getReserves()", None),
            return_types=["uint256", "uint256"],
            block_identifier=_block_number,
        )

        if pool.reserves_token0 == reserves0 and pool.reserves_token1 == reserves1:
            return False

        update = AerodromeV2PoolExternalUpdate(
            block_number=_block_number,
            reserves_token0=reserves0,
            reserves_token1=reserves1,
        )
        pool.external_update(update)
        return True
