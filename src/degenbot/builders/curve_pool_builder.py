from __future__ import annotations

import contextlib
from fractions import Fraction
from typing import TYPE_CHECKING, Any, cast

import eth_abi.abi
import sqlalchemy.exc
from sqlalchemy import select

from degenbot.builders.erc20_builder import Erc20Builder
from degenbot.checksum_cache import get_checksum_address
from degenbot.connection.connection_manager import ConnectionManager
from degenbot.constants import ZERO_ADDRESS as _ZERO_ADDRESS
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.curve.deployments import CURVE_V1_FACTORY_ADDRESS, CURVE_V1_REGISTRY_ADDRESS
from degenbot.curve.fetcher_factory import CurveFetcherFactory
from degenbot.curve.types import CurveStableswapPoolExternalUpdate
from degenbot.database.models.pools import LiquidityPoolTable
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.exceptions.liquidity_pool import BrokenPool, LiquidityPoolError
from degenbot.functions import encode_function_calldata, raw_call
from degenbot.logging import logger
from degenbot.registry import PoolRegistry, TokenRegistry
from degenbot.types.aliases import ChainId

if TYPE_CHECKING:
    from web3.types import BlockIdentifier


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

        """Fetch pool data from RPC and construct an I/O-free CurveStableswapPool."""

        pool_address = get_checksum_address(address)
        chain_id = chain_id or self._connections.default_chain_id
        provider = self._connections.get_provider(chain_id)

        state_block = state_block if state_block is not None else provider.get_block_number()

        # Fetch pool parameters - Curve pools don't have a standard N_COINS
        # function, so we iterate until we hit a revert
        w3 = self._connections.get_web3(chain_id)

        # Create fetcher factory for I/O-free pool operation
        fetchers = CurveFetcherFactory(connections=self._connections, chain_id=chain_id)

        token_addresses: list[str] = []
        balances: list[int] = []

        # Iterate to find all coins (max 8 for Curve V1)
        # Some pools use coins(uint256), others use coins(int128)
        coin_prototype = None
        balance_prototype = None

        for i in range(8):
            if coin_prototype is None:
                # Try uint256 first
                try:
                    coin_addr = w3.eth.call(
                        {
                            "to": pool_address,
                            "data": encode_function_calldata(
                                function_prototype="coins(uint256)",
                                function_arguments=[i],
                            ),
                        },
                        block_identifier=state_block,
                    )
                    (token_address,) = eth_abi.abi.decode(types=["address"], data=coin_addr)
                    if int(token_address, 16) != 0:
                        coin_prototype = "coins(uint256)"
                        balance_prototype = "balances(uint256)"
                except Exception:
                    pass

                # Try int128 if uint256 failed
                if coin_prototype is None:
                    try:
                        coin_addr = w3.eth.call(
                            {
                                "to": pool_address,
                                "data": encode_function_calldata(
                                    function_prototype="coins(int128)",
                                    function_arguments=[i],
                                ),
                            },
                            block_identifier=state_block,
                        )
                        (token_address,) = eth_abi.abi.decode(types=["address"], data=coin_addr)
                        if int(token_address, 16) != 0:
                            coin_prototype = "coins(int128)"
                            balance_prototype = "balances(int128)"
                    except Exception:
                        pass

                if coin_prototype is None:
                    # Neither worked, bail out
                    break

                # We found the prototype, now decode the address we already fetched
                if int(token_address, 16) == 0:
                    break
                token_addresses.append(token_address)
            else:
                # Use the known prototype
                try:
                    coin_addr = w3.eth.call(
                        {
                            "to": pool_address,
                            "data": encode_function_calldata(
                                function_prototype=coin_prototype,
                                function_arguments=[i],
                            ),
                        },
                        block_identifier=state_block,
                    )
                    (token_address,) = eth_abi.abi.decode(types=["address"], data=coin_addr)
                    if int(token_address, 16) == 0:
                        break
                    token_addresses.append(token_address)
                except Exception:
                    break

            # Fetch balance
            try:
                balance_result = w3.eth.call(
                    {
                        "to": pool_address,
                        "data": encode_function_calldata(
                            function_prototype=balance_prototype,
                            function_arguments=[i],
                        ),
                    },
                    block_identifier=state_block,
                )
                (balance,) = eth_abi.abi.decode(types=["uint256"], data=balance_result)
                balances.append(balance)
            except Exception:
                break

        # Fetch A, fee, admin_fee
        a_result = w3.eth.call(
            {
                "to": pool_address,
                "data": encode_function_calldata(function_prototype="A()", function_arguments=[]),
            },
            block_identifier=state_block,
        )
        (a_coefficient,) = eth_abi.abi.decode(types=["uint256"], data=a_result)

        fee_result = w3.eth.call(
            {
                "to": pool_address,
                "data": encode_function_calldata(function_prototype="fee()", function_arguments=[]),
            },
            block_identifier=state_block,
        )
        (fee,) = eth_abi.abi.decode(types=["uint256"], data=fee_result)

        admin_fee_result = w3.eth.call(
            {
                "to": pool_address,
                "data": encode_function_calldata(
                    function_prototype="admin_fee()",
                    function_arguments=[],
                ),
            },
            block_identifier=state_block,
        )
        (admin_fee,) = eth_abi.abi.decode(types=["uint256"], data=admin_fee_result)

        # Fetch A ramping parameters (optional - may not exist for all pools)
        initial_a: int | None = None
        initial_a_time: int | None = None
        future_a: int | None = None
        future_a_time: int | None = None
        try:
            initial_a_result = w3.eth.call(
                {
                    "to": pool_address,
                    "data": encode_function_calldata(
                        function_prototype="initial_A()",
                        function_arguments=[],
                    ),
                },
                block_identifier=state_block,
            )
            (initial_a,) = eth_abi.abi.decode(types=["uint256"], data=initial_a_result)

            initial_a_time_result = w3.eth.call(
                {
                    "to": pool_address,
                    "data": encode_function_calldata(
                        function_prototype="initial_A_time()",
                        function_arguments=[],
                    ),
                },
                block_identifier=state_block,
            )
            (initial_a_time,) = eth_abi.abi.decode(types=["uint256"], data=initial_a_time_result)

            future_a_result = w3.eth.call(
                {
                    "to": pool_address,
                    "data": encode_function_calldata(
                        function_prototype="future_A()",
                        function_arguments=[],
                    ),
                },
                block_identifier=state_block,
            )
            (future_a,) = eth_abi.abi.decode(types=["uint256"], data=future_a_result)

            future_a_time_result = w3.eth.call(
                {
                    "to": pool_address,
                    "data": encode_function_calldata(
                        function_prototype="future_A_time()",
                        function_arguments=[],
                    ),
                },
                block_identifier=state_block,
            )
            (future_a_time,) = eth_abi.abi.decode(types=["uint256"], data=future_a_time_result)
        except Exception:
            # Pool doesn't support A ramping functions
            pass

        # Get block timestamp for _create_timestamp
        block = provider.get_block(state_block)
        create_timestamp = block["timestamp"]

        admin_fee_result = w3.eth.call(
            {
                "to": pool_address,
                "data": encode_function_calldata(
                    function_prototype="admin_fee()",
                    function_arguments=[],
                ),
            },
            block_identifier=state_block,
        )
        (admin_fee,) = eth_abi.abi.decode(types=["uint256"], data=admin_fee_result)

        # Build tokens
        tokens = tuple(
            self._erc20_builder.build(addr, chain_id=chain_id, silent=silent)
            for addr in token_addresses
        )

        # Detect lending tokens (cTokens, yTokens)
        # For lending tokens, precision_multipliers must be based on
        # the UNDERLYING token decimals, not the wrapped token decimals.
        # e.g., cDAI has 8 decimals, but DAI has 18, so precision_multiplier = 10^0 = 1
        # cUSDC has 8 decimals, but USDC has 6, so precision_multiplier = 10^12
        use_lending: list[bool] = []
        precision_multiplier_overrides: dict[int, int] = {}  # index -> override value
        for idx, token_addr in enumerate(token_addresses):
            is_lending = False
            checksummed_addr = get_checksum_address(token_addr)
            # Check if token is a cToken using isCToken()
            try:
                is_ctoken_result = w3.eth.call(
                    {
                        "to": checksummed_addr,
                        "data": Web3.keccak(text="isCToken()")[:4],
                    },
                    block_identifier=state_block,
                )
                (is_c,) = eth_abi.abi.decode(types=["bool"], data=is_ctoken_result)
                if is_c:
                    is_lending = True
                    # cToken: get underlying token decimals via underlying() method
                    try:
                        underlying_result = w3.eth.call(
                            {
                                "to": checksummed_addr,
                                "data": Web3.keccak(text="underlying()")[:4],
                            },
                            block_identifier=state_block,
                        )
                        (underlying_addr,) = eth_abi.abi.decode(
                            types=["address"], data=underlying_result
                        )
                        underlying_addr = get_checksum_address(underlying_addr)
                        # Fetch underlying decimals
                        try:
                            underlying_dec_result = w3.eth.call(
                                {
                                    "to": underlying_addr,
                                    "data": encode_function_calldata(
                                        function_prototype="decimals()",
                                        function_arguments=[],
                                    ),
                                },
                                block_identifier=state_block,
                            )
                            (underlying_dec,) = eth_abi.abi.decode(
                                types=["uint8"], data=underlying_dec_result
                            )
                            # Override precision_multiplier to use underlying decimals
                            precision_multiplier_overrides[idx] = 10 ** (18 - underlying_dec)
                        except Exception:  # noqa: BLE001
                            pass
                    except Exception:  # noqa: BLE001
                        pass
            except Exception:  # noqa: BLE001
                pass
            # Check if token is a yToken (has token() method returning underlying)
            if not is_lending:
                try:
                    ytoken_result = w3.eth.call(
                        {
                            "to": checksummed_addr,
                            "data": Web3.keccak(text="token()")[:4],
                        },
                        block_identifier=state_block,
                    )
                    (underlying_addr,) = eth_abi.abi.decode(types=["address"], data=ytoken_result)
                    # Verify the underlying is a valid address (not zero)
                    if int(underlying_addr, 16) != 0:
                        is_lending = True
                        # yToken: typically has same decimals as underlying,
                        # no override needed
                except Exception:  # noqa: BLE001
                    pass
            use_lending.append(is_lending)

        # Detect crypto pool parameters (fee_gamma, mid_fee, out_fee, gamma)
        pool_fee_gamma: int | None = None
        pool_mid_fee: int | None = None
        pool_out_fee: int | None = None
        pool_gamma: int | None = None
        try:
            fee_gamma_result = w3.eth.call(
                {
                    "to": pool_address,
                    "data": encode_function_calldata(
                        function_prototype="fee_gamma()",
                        function_arguments=[],
                    ),
                },
                block_identifier=state_block,
            )
            (fee_gamma_val,) = eth_abi.abi.decode(types=["uint256"], data=fee_gamma_result)
            if fee_gamma_val > 0:
                pool_fee_gamma = fee_gamma_val
                # Fetch related crypto pool parameters
                try:
                    (mid_fee_val,) = eth_abi.abi.decode(
                        types=["uint256"],
                        data=w3.eth.call(
                            {
                                "to": pool_address,
                                "data": encode_function_calldata(
                                    function_prototype="mid_fee()",
                                    function_arguments=[],
                                ),
                            },
                            block_identifier=state_block,
                        ),
                    )
                    pool_mid_fee = mid_fee_val
                except Exception:  # noqa: BLE001
                    pass
                try:
                    (out_fee_val,) = eth_abi.abi.decode(
                        types=["uint256"],
                        data=w3.eth.call(
                            {
                                "to": pool_address,
                                "data": encode_function_calldata(
                                    function_prototype="out_fee()",
                                    function_arguments=[],
                                ),
                            },
                            block_identifier=state_block,
                        ),
                    )
                    pool_out_fee = out_fee_val
                except Exception:  # noqa: BLE001
                    pass
                try:
                    (gamma_val,) = eth_abi.abi.decode(
                        types=["uint256"],
                        data=w3.eth.call(
                            {
                                "to": pool_address,
                                "data": encode_function_calldata(
                                    function_prototype="gamma()",
                                    function_arguments=[],
                                ),
                            },
                            block_identifier=state_block,
                        ),
                    )
                    pool_gamma = gamma_val
                except Exception:  # noqa: BLE001
                    pass
        except Exception:  # noqa: BLE001
            pass

        # Fetch offpeg_fee_multiplier (used by some lending/crypto pools)
        pool_offpeg_fee_multiplier: int | None = None
        try:
            (offpeg_fee_val,) = eth_abi.abi.decode(
                types=["uint256"],
                data=w3.eth.call(
                    {
                        "to": pool_address,
                        "data": encode_function_calldata(
                            function_prototype="offpeg_fee_multiplier()",
                            function_arguments=[],
                        ),
                    },
                    block_identifier=state_block,
                ),
            )
            pool_offpeg_fee_multiplier = offpeg_fee_val
        except Exception:  # noqa: BLE001
            pass

        # Fetch LP token from Curve registry
        lp_token_address: str | None = None
        for registry_address in [
            CURVE_V1_REGISTRY_ADDRESS,
            CURVE_V1_FACTORY_ADDRESS,
        ]:
            try:
                lp_token_result = w3.eth.call(
                    {
                        "to": registry_address,
                        "data": encode_function_calldata(
                            function_prototype="get_lp_token(address)",
                            function_arguments=[pool_address],
                        ),
                    },
                    block_identifier=state_block,
                )
                (lp_token_addr,) = eth_abi.abi.decode(types=["address"], data=lp_token_result)
                if lp_token_addr != _ZERO_ADDRESS:
                    lp_token_address = lp_token_addr
                    break
            except Exception:
                continue

        # Build LP token if found
        lp_token: Erc20Token | None = None
        if lp_token_address is not None:
            lp_token = self._erc20_builder.build(lp_token_address, chain_id=chain_id, silent=silent)

        # Check if this is a metapool and fetch base pool info
        base_pool: CurveStableswapPool | None = None
        tokens_underlying: tuple[Erc20Token, ...] | None = None

        for registry_address in [
            CURVE_V1_REGISTRY_ADDRESS,
            CURVE_V1_FACTORY_ADDRESS,
        ]:
            try:
                is_meta_result = w3.eth.call(
                    {
                        "to": registry_address,
                        "data": encode_function_calldata(
                            function_prototype="is_meta(address)",
                            function_arguments=[pool_address],
                        ),
                    },
                    block_identifier=state_block,
                )
                (is_meta,) = eth_abi.abi.decode(types=["bool"], data=is_meta_result)
                if not is_meta:
                    # Try next registry
                    continue

                # Get base pool address from the pool contract itself
                try:
                    base_pool_result = w3.eth.call(
                        {
                            "to": pool_address,
                            "data": encode_function_calldata(
                                function_prototype="base_pool()",
                                function_arguments=[],
                            ),
                        },
                        block_identifier=state_block,
                    )
                    (base_pool_address,) = eth_abi.abi.decode(
                        types=["address"], data=base_pool_result
                    )
                    base_pool_address = get_checksum_address(base_pool_address)
                except Exception:
                    # If base_pool() doesn't exist, try registry
                    try:
                        base_pool_result = w3.eth.call(
                            {
                                "to": registry_address,
                                "data": encode_function_calldata(
                                    function_prototype="get_base_pool(address)",
                                    function_arguments=[pool_address],
                                ),
                            },
                            block_identifier=state_block,
                        )
                        (base_pool_address,) = eth_abi.abi.decode(
                            types=["address"], data=base_pool_result
                        )
                        base_pool_address = get_checksum_address(base_pool_address)
                    except Exception:
                        # Last resort: if the pool's second token is a known
                        # base pool LP token, use the corresponding base pool
                        base_pool_address = _ZERO_ADDRESS
                        if (
                            len(token_addresses) >= 2
                            and token_addresses[1].lower()
                            == "0x6c3F90f043a72FA612Cbac8115ee7e52bDE6E490".lower()
                        ):
                            # 3Crv LP token → base pool is the tripool
                            base_pool_address = "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7"

                # Get underlying coins from registry
                underlying_coins_result = w3.eth.call(
                    {
                        "to": registry_address,
                        "data": encode_function_calldata(
                            function_prototype="get_underlying_coins(address)",
                            function_arguments=[pool_address],
                        ),
                    },
                    block_identifier=state_block,
                )
                underlying_addresses = eth_abi.abi.decode(
                    types=["address[8]"], data=underlying_coins_result
                )[0]

                # Build base pool (recursive call)
                base_pool = self.build(
                    base_pool_address,
                    chain_id=chain_id,
                    state_block=state_block,
                    silent=silent,
                    state_cache_depth=state_cache_depth,
                )

                # Build underlying tokens
                underlying_tokens: list[Erc20Token] = []
                for underlying_addr in underlying_addresses:
                    if int(underlying_addr, 16) == 0:
                        break
                    underlying_tokens.append(
                        self._erc20_builder.build(underlying_addr, chain_id=chain_id, silent=silent)
                    )
                tokens_underlying = tuple(underlying_tokens)
                # Found metapool info, stop checking other registries
                break
            except Exception:
                continue

        # Skip pools with fewer than 2 tokens
        if len(tokens) < 2:
            raise BrokenPool()

        # Construct pool
        pool = CurveStableswapPool(
            address=pool_address,
            tokens=tokens,
            a_coefficient=a_coefficient,
            fee=fee,
            admin_fee=admin_fee,
            balances=balances,
            chain_id=chain_id,
            state_block=state_block,
            state_cache_depth=state_cache_depth,
            # A ramping parameters
            initial_a_coefficient=initial_a,
            future_a_coefficient=future_a,
            initial_a_coefficient_time=initial_a_time,
            future_a_coefficient_time=future_a_time,
            create_timestamp=create_timestamp,
            # LP token
            lp_token=lp_token,
            # Metapool parameters
            base_pool=base_pool,
            tokens_underlying=tokens_underlying,
            # Lending parameters
            use_lending=use_lending,
            # Precision multipliers override for lending pools
            precision_multipliers=tuple(
                precision_multiplier_overrides.get(i, 10 ** (18 - tokens[i].decimals))
                for i in range(len(tokens))
            )
            if precision_multiplier_overrides
            else None,
            # Crypto pool parameters
            fee_gamma=pool_fee_gamma,
            mid_fee=pool_mid_fee,
            out_fee=pool_out_fee,
            gamma=pool_gamma,
            offpeg_fee_multiplier=pool_offpeg_fee_multiplier,
            # Fetcher callbacks for I/O-free operation
            virtual_price_fetcher=fetchers.virtual_price_fetcher(
                pool_address, base_pool_address=base_pool_address if base_pool else None
            ),
            base_virtual_price_fetcher=fetchers.base_virtual_price_fetcher(
                pool_address
            ),
            timestamp_fetcher=fetchers.timestamp_fetcher(),
            redemption_price_fetcher=fetchers.redemption_price_fetcher(
                pool_address
            ),
            admin_balances_fetcher=fetchers.admin_balances_fetcher(pool_address),
            block_number_fetcher=fetchers.block_number_fetcher(),
            total_supply_fetcher=fetchers.total_supply_fetcher(),
            token_balance_fetcher=fetchers.token_balance_fetcher(),
            provider_call=fetchers.provider_call(),
            # Crypto pool fetchers (only useful for crypto pools like Tricrypto)
            D_fetcher=fetchers.D_fetcher(pool_address)
            if pool_fee_gamma
            else None,
            gamma_fetcher=fetchers.gamma_fetcher(pool_address)
            if pool_fee_gamma
            else None,
            price_scale_fetcher=fetchers.price_scale_fetcher(
                pool_address, len(tokens)
            )
            if pool_fee_gamma
            else None,
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

    def update(
        self,
        pool: Any,
        *,
        block_number: BlockIdentifier | None = None,
    ) -> bool:
        """Fetch current state from chain and push update to the pool."""
        if not isinstance(pool, CurveStableswapPool):
            raise TypeError(f"CurvePoolBuilder cannot update {type(pool).__name__}")

        provider = self._connections.get_provider(pool.chain_id)
        _block_number = block_number if block_number is not None else provider.get_block_number()

        # Fetch balances for each token in the pool
        w3 = self._connections.get_web3(pool.chain_id)
        new_balances: list[int] = []
        for i, token in enumerate(pool.tokens):
            (balance,) = cast(
                "tuple[int]",
                eth_abi.abi.decode(
                    types=["uint256"],
                    data=w3.eth.call(
                        {
                            "to": pool.address,
                            "data": encode_function_calldata(
                                function_prototype="balances(uint256)",
                                function_arguments=[i],
                            ),
                        },
                        block_identifier=_block_number,
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
