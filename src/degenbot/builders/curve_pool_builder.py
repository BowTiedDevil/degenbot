from __future__ import annotations

from typing import TYPE_CHECKING, Any, cast

import eth_abi.abi

from degenbot.builders.erc20_builder import Erc20Builder
from degenbot.checksum_cache import get_checksum_address
from degenbot.connection.connection_manager import ConnectionManager
from degenbot.curve._pool_strategies import resolve_pool_strategies
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.curve.deployments import CURVE_V1_FACTORY_ADDRESS, CURVE_V1_REGISTRY_ADDRESS
from degenbot.curve.detection.a_ramping import detect_a_ramping
from degenbot.curve.detection.coin_discovery import discover_coins
from degenbot.curve.detection.crypto_detector import detect_crypto_params
from degenbot.curve.detection.lending_detector import detect_lending_tokens
from degenbot.curve.detection.lp_token import find_lp_token
from degenbot.curve.detection.metapool_detector import detect_metapool
from degenbot.curve.fetcher_factory import CurveFetcherFactory
from degenbot.curve.types import CurveStableswapPoolExternalUpdate
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.exceptions.pool import BrokenPool
from degenbot.logging import logger
from degenbot.provider.call_helpers import encode_function_calldata
from degenbot.provider.interface import ProviderAdapter
from degenbot.registry import PoolRegistry, TokenRegistry
from degenbot.types.aliases import ChainId

if TYPE_CHECKING:
    from web3.types import BlockIdentifier


_REGISTRY_ADDRESSES = (CURVE_V1_REGISTRY_ADDRESS, CURVE_V1_FACTORY_ADDRESS)


class CurvePoolBuilder:
    """
    Builds and updates Curve StableSwap pools.

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
        address: str,
        *,
        chain_id: ChainId | None = None,
        state_block: int | None = None,
        silent: bool = False,
        state_cache_depth: int = 8,
    ) -> CurveStableswapPool:
        """Fetch pool data from RPC and construct an I/O-free CurveStableswapPool."""

        pool_address = get_checksum_address(address)
        chain_id = chain_id or self._connections.default_chain_id
        provider = self._connections.get_provider(chain_id)
        state_block = state_block if state_block is not None else provider.get_block_number()

        # 1. Discover coins and balances
        coins = discover_coins(provider, pool_address, block_identifier=state_block)

        # 2. Fetch A, fee, admin_fee
        a_coefficient, fee, admin_fee = _fetch_pool_params(
            provider, pool_address, block_identifier=state_block
        )

        # 3. Detect A ramping
        a_ramping = detect_a_ramping(provider, pool_address, block_identifier=state_block)

        # 4. Get block timestamp
        create_timestamp = provider.get_block_timestamp(block=state_block)

        # 5. Build tokens
        tokens = tuple(
            self._erc20_builder.build(addr, chain_id=chain_id, silent=silent)
            for addr in coins.token_addresses
        )

        # 6. Detect lending tokens
        lending = detect_lending_tokens(
            provider, pool_address, coins.token_addresses, tokens, block_identifier=state_block
        )

        # 7. Detect crypto pool parameters
        crypto = detect_crypto_params(provider, pool_address, block_identifier=state_block)

        # 8. Find LP token
        lp_token_address = find_lp_token(
            provider,
            pool_address,
            registry_addresses=_REGISTRY_ADDRESSES,
            block_identifier=state_block,
        )

        # 9. Detect metapool
        metapool = detect_metapool(
            provider,
            pool_address,
            coins.token_addresses,
            registry_addresses=_REGISTRY_ADDRESSES,
            block_identifier=state_block,
        )

        # 10. Build base pool and underlying tokens (if metapool)
        base_pool, tokens_underlying = self._resolve_metapool(
            metapool,
            chain_id,
            state_block,
            silent,
            state_cache_depth,
        )

        # 11. Build LP token
        lp_token = (
            self._erc20_builder.build(lp_token_address, chain_id=chain_id, silent=silent)
            if lp_token_address is not None
            else None
        )

        # 12. Skip broken pools
        if len(tokens) < 2:
            raise BrokenPool()

        # 13. Resolve strategies from pool address
        strategies = resolve_pool_strategies(pool_address)

        # 14. Create data provider and construct pool
        fetchers = CurveFetcherFactory(connections=self._connections, chain_id=chain_id)
        data_provider = fetchers.create_provider(
            pool_address,
            base_pool_address=metapool.base_pool_address if metapool.is_meta else None,
            tokens=list(tokens),
            use_lending=list(lending.use_lending) if lending.use_lending else [False] * len(tokens),
            precision_multipliers=list(lending.precision_multipliers)
            if lending.precision_multipliers
            else [1] * len(tokens),
            rate_multipliers=tuple(
                pm * 10**18 for pm in (lending.precision_multipliers or [1] * len(tokens))
            ),
            lending_rate_style=strategies.lending_rate_style,
            is_crypto=crypto.is_crypto,
            n_coins=len(tokens),
        )
        pool = CurveStableswapPool(
            address=pool_address,
            tokens=tokens,
            a_coefficient=a_coefficient,
            fee=fee,
            admin_fee=admin_fee,
            balances=coins.balances,
            chain_id=chain_id,
            state_block=state_block,
            state_cache_depth=state_cache_depth,
            initial_a_coefficient=a_ramping.initial_a,
            future_a_coefficient=a_ramping.future_a,
            initial_a_coefficient_time=a_ramping.initial_a_time,
            future_a_coefficient_time=a_ramping.future_a_time,
            create_timestamp=create_timestamp,
            lp_token=lp_token,
            base_pool=base_pool,
            tokens_underlying=tokens_underlying,
            use_lending=lending.use_lending,
            precision_multipliers=lending.precision_multipliers,
            fee_gamma=crypto.fee_gamma,
            mid_fee=crypto.mid_fee,
            out_fee=crypto.out_fee,
            gamma=crypto.gamma,
            offpeg_fee_multiplier=crypto.offpeg_fee_multiplier,
            data_provider=data_provider,
            strategies=strategies,
        )

        # Register pool
        self._pools.add(pool, chain_id=chain_id, pool_address=pool.address)

        if not silent:
            logger.info(pool.name)
            logger.info(f"• Address: {pool.address}")
            logger.info(f"• Tokens: {[t.symbol for t in pool.tokens]}")
            logger.info(f"• A: {pool.a_coefficient}")
            logger.info(f"• Fee: {100 * pool.fee / pool.FEE_DENOMINATOR:.4f}%")

        return pool

    def _resolve_metapool(
        self,
        metapool: Any,
        chain_id: ChainId,
        state_block: int,
        silent: bool,
        state_cache_depth: int,
    ) -> tuple[CurveStableswapPool | None, tuple[Any, ...] | None]:
        """Build base pool and underlying tokens for a metapool."""
        if not metapool.is_meta:
            return None, None

        if metapool.base_pool_address is None:
            return None, None

        base_pool = self.build(
            metapool.base_pool_address,
            chain_id=chain_id,
            state_block=state_block,
            silent=silent,
            state_cache_depth=state_cache_depth,
        )

        if metapool.tokens_underlying is None:
            return base_pool, None

        tokens_underlying = tuple(
            self._erc20_builder.build(addr, chain_id=chain_id, silent=silent)
            for addr in metapool.tokens_underlying
        )

        return base_pool, tokens_underlying

    def update(
        self,
        pool: Any,
        *,
        block_number: BlockIdentifier | None = None,
    ) -> bool:
        """Fetch current state from chain and push update to the pool."""
        if not isinstance(pool, CurveStableswapPool):
            msg = f"CurvePoolBuilder cannot update {type(pool).__name__}"
            raise TypeError(msg)

        assert pool.chain_id is not None
        provider = self._connections.get_provider(pool.chain_id)
        _raw_block_number = block_number if block_number is not None else provider.get_block_number()
        _block_number: int = _raw_block_number if isinstance(_raw_block_number, int) else int(_raw_block_number)

        # Fetch balances for each token in the pool
        new_balances: list[int] = []
        for i, _ in enumerate(pool.tokens):
            (balance,) = cast(
                "tuple[int]",
                eth_abi.abi.decode(
                    types=["uint256"],
                    data=provider.call_raw(
                        {
                            "to": pool.address,
                            "data": encode_function_calldata(
                                function_prototype="balances(uint256)",
                                function_arguments=[i],
                            ),
                        },
                        block=_block_number,
                    ),
                ),
            )
            new_balances.append(balance)

        if pool.balances == tuple(new_balances):
            return False

        update = CurveStableswapPoolExternalUpdate(
            block_number=_block_number,
            balances=tuple(new_balances),
        )
        pool.external_update(update)
        return True


def _fetch_pool_params(
    provider: ProviderAdapter,
    pool_address: str,
    *,
    block_identifier: int,
) -> tuple[int, int, int]:
    """Fetch A, fee, and admin_fee from a Curve pool contract."""
    a_result = provider.call_raw(
        {
            "to": pool_address,
            "data": encode_function_calldata(function_prototype="A()", function_arguments=[]),
        },
        block=block_identifier,
    )
    (a_coefficient,) = eth_abi.abi.decode(types=["uint256"], data=a_result)

    fee_result = provider.call_raw(
        {
            "to": pool_address,
            "data": encode_function_calldata(function_prototype="fee()", function_arguments=[]),
        },
        block=block_identifier,
    )
    (fee,) = eth_abi.abi.decode(types=["uint256"], data=fee_result)

    admin_fee_result = provider.call_raw(
        {
            "to": pool_address,
            "data": encode_function_calldata(
                function_prototype="admin_fee()",
                function_arguments=[],
            ),
        },
        block=block_identifier,
    )
    (admin_fee,) = eth_abi.abi.decode(types=["uint256"], data=admin_fee_result)

    return a_coefficient, fee, admin_fee
