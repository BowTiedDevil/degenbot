"""AsyncBot — the async counterpart to Bot.

Owns an AsyncConnectionManager and provides async factory/I-O methods.
Returns the same I/O-free domain objects as Bot.
"""

from __future__ import annotations

import contextlib
from fractions import Fraction
from typing import TYPE_CHECKING, Any, Sequence

import eth_abi.abi
import sqlalchemy.exc
from eth_abi.exceptions import DecodingError
from hexbytes import HexBytes
from web3 import Web3
from web3.exceptions import Web3Exception
from web3.types import BlockIdentifier

from degenbot.checksum_cache import get_checksum_address
from degenbot.config import DegenbotConfig, _init_config
from degenbot.connection.async_connection_manager import AsyncConnectionManager
from degenbot.database.operations import get_scoped_sqlite_session
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.erc20.erc20 import Erc20Token
from degenbot.erc20.ether_placeholder import EtherPlaceholder
from degenbot.exceptions.manager import ManagerAlreadyInitialized
from degenbot.exceptions.base import DegenbotValueError
from degenbot.functions import encode_function_calldata, async_raw_call
from degenbot.logging import logger
from degenbot.registry import ManagedPoolRegistry, PoolRegistry, TokenRegistry
from degenbot.types.abstract import AbstractPoolManager
from degenbot.types.aliases import ChainId

if TYPE_CHECKING:
    pass


class AsyncBot:
    """
    Async session object that owns the runtime state for a degenbot run.

    Mirrors Bot with AsyncConnectionManager and async factory/I-O methods.
    Returns the same I/O-free domain objects as Bot.
    """

    def __init__(self, config: DegenbotConfig) -> None:
        self.config = config
        self.connections = AsyncConnectionManager()
        self.db = DatabaseSessionManager(
            get_scoped_sqlite_session(database_path=config.database.path)
        )
        self.pools = PoolRegistry()
        self.tokens = TokenRegistry()
        self.managed_pools = ManagedPoolRegistry()
        self._managers: dict[tuple[ChainId, str], Any] = {}

    @classmethod
    def from_config_file(cls) -> AsyncBot:
        return cls(config=_init_config())

    def add_manager[M: AbstractPoolManager](
        self,
        manager_cls: type[M],
        *args: Any,
        **kwargs: Any,
    ) -> M:
        """Add a pool manager to this bot session. Same as Bot.add_manager."""
        # Inject bot reference
        kwargs["bot"] = self

        # Enforce one manager per (chain_id, factory) within this Bot
        chain_id = kwargs.get("chain_id")
        factory_address = kwargs.get("factory_address")
        if chain_id is not None and factory_address is not None:
            key = (chain_id, get_checksum_address(factory_address))
            if key in self._managers:
                raise ManagerAlreadyInitialized(
                    message=f"A {manager_cls.__name__} is already registered for chain {chain_id}, factory {factory_address}"
                )
            manager = manager_cls(*args, **kwargs)
            self._managers[key] = manager
            return manager

        manager = manager_cls(*args, **kwargs)
        return manager

    # ------------------------------------------------------------------
    # ERC-20 token factory
    # ------------------------------------------------------------------

    async def build_erc20token(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        silent: bool = False,
    ) -> Erc20Token:
        """Fetch token metadata from DB/RPC and construct an I/O-free Erc20Token."""
        from degenbot.database.models.erc20 import Erc20TokenTable
        from degenbot.erc20.erc20 import get_token_from_database

        address = get_checksum_address(address)
        chain_id = chain_id or self.connections.default_chain_id

        # Check registry first
        if (existing := self.tokens.get(token_address=address, chain_id=chain_id)) is not None:
            return existing

        # Check for Ether placeholder
        if address in EtherPlaceholder.addresses:
            token = EtherPlaceholder(address, chain_id=chain_id)
            self.tokens.add(token_address=token.address, chain_id=chain_id, token=token)
            if not silent:
                logger.info(f"• {token.symbol} ({token.name})")
            return token

        # Try DB first
        token_from_db = None
        with contextlib.suppress(Exception), self.db() as session:
            token_from_db = get_token_from_database(
                token=address, chain_id=chain_id, session=session,
            )

        name: str | None = None
        symbol: str | None = None
        decimals: int | None = None

        if token_from_db is not None:
            if token_from_db.name is not None:
                name = token_from_db.name
            if token_from_db.symbol is not None:
                symbol = token_from_db.symbol
            if token_from_db.decimals is not None:
                decimals = token_from_db.decimals

        # Fetch missing values from chain
        if name is None or symbol is None or decimals is None:
            provider = self.connections.get_provider(chain_id)

            try:
                fetched_name, fetched_symbol, fetched_decimals = (
                    await self._fetch_name_symbol_decimals_batched(
                        address=address, provider=provider
                    )
                )
            except (Web3Exception, DecodingError):
                fetched_name = Erc20Token.UNKNOWN_NAME
                fetched_symbol = Erc20Token.UNKNOWN_SYMBOL
                fetched_decimals = Erc20Token.UNKNOWN_DECIMALS

            name = name or fetched_name
            symbol = symbol or fetched_symbol
            decimals = decimals or fetched_decimals

        token = Erc20Token(
            address=address,
            chain_id=chain_id,
            name=name,
            symbol=symbol,
            decimals=decimals,
        )

        # Register (no self-registration)
        self.tokens.add(token_address=token.address, chain_id=chain_id, token=token)

        if not silent:
            logger.info(f"• {token.symbol} ({token.name})")

        return token

    async def _fetch_name_symbol_decimals_batched(
        self,
        address: str,
        provider: Any,
    ) -> tuple[str, str, int]:
        """Fetch name, symbol, decimals from chain in three async calls."""
        name_result = await provider.call(
            to=address,
            data=encode_function_calldata("name()", None),
        )
        symbol_result = await provider.call(
            to=address,
            data=encode_function_calldata("symbol()", None),
        )
        decimals_result = await provider.call(
            to=address,
            data=encode_function_calldata("decimals()", None),
        )

        (name,) = eth_abi.abi.decode(types=["string"], data=name_result)
        (symbol,) = eth_abi.abi.decode(types=["string"], data=symbol_result)
        (decimals,) = eth_abi.abi.decode(types=["uint8"], data=decimals_result)

        return name, symbol, decimals

    def get_token(self, address: str, *, chain_id: ChainId | None = None) -> Erc20Token:
        """Get a token from the registry (sync — no async I/O)."""
        chain_id = chain_id or self.connections.default_chain_id
        return self.tokens.get(token_address=address, chain_id=chain_id)

    # ------------------------------------------------------------------
    # V2 pool factory
    # ------------------------------------------------------------------

    async def build_v2_pool(
        self,
        pool_address: str,
        *,
        chain_id: ChainId | None = None,
        deployer_address: str | None = None,
        init_hash: str | None = None,
        state_block: int | None = None,
        silent: bool = False,
    ) -> Any:
        """Fetch pool data from DB/RPC and construct an I/O-free UniswapV2Pool."""
        from degenbot.database.models.pools import LiquidityPoolTable
        from degenbot.exceptions.liquidity_pool import LiquidityPoolError
        from degenbot.uniswap.deployments import FACTORY_DEPLOYMENTS as _FACTORY_DEPLOYMENTS
        from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool

        pool_address = get_checksum_address(pool_address)
        chain_id = chain_id or self.connections.default_chain_id
        provider = self.connections.get_provider(chain_id)

        _state_block = state_block if state_block is not None else await provider.get_block_number()

        # Try DB first
        pool_from_db = None
        with contextlib.suppress(Exception), self.db() as session:
            from sqlalchemy import select

            pool_from_db = session.scalar(
                select(LiquidityPoolTable).where(
                    LiquidityPoolTable.address == pool_address,
                    LiquidityPoolTable.chain == chain_id,
                )
            )

        # Get immutable values
        if pool_from_db is not None:
            factory = get_checksum_address(pool_from_db.exchange.factory)
            token0_address = pool_from_db.token0.address
            token1_address = pool_from_db.token1.address
            fee_token0 = Fraction(pool_from_db.fee_token0, pool_from_db.fee_denominator)
            fee_token1 = Fraction(pool_from_db.fee_token1, pool_from_db.fee_denominator)
        else:
            try:
                factory_result = await provider.call(
                    to=pool_address, data=encode_function_calldata("factory()", None),
                )
                token0_result = await provider.call(
                    to=pool_address, data=encode_function_calldata("token0()", None),
                )
                token1_result = await provider.call(
                    to=pool_address, data=encode_function_calldata("token1()", None),
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
        token0 = await self.build_erc20token(token0_address, chain_id=chain_id, silent=silent)
        token1 = await self.build_erc20token(token1_address, chain_id=chain_id, silent=silent)

        # Fetch reserves
        try:
            reserves_result = await provider.call(
                to=pool_address,
                data=encode_function_calldata("getReserves()", None),
                block=_state_block,
            )
        except Exception as exc:
            raise LiquidityPoolError(message="Could not decode contract data") from exc

        reserves0, reserves1, _ = eth_abi.abi.decode(
            types=["uint256", "uint256", "uint256"], data=reserves_result,
        )

        # Determine deployer/init_hash
        _deployer = factory
        _init_hash = UniswapV2Pool.UNISWAP_V2_MAINNET_POOL_INIT_HASH
        with contextlib.suppress(KeyError):
            factory_deployment = _FACTORY_DEPLOYMENTS[chain_id][factory]
            _init_hash = factory_deployment.pool_init_hash
            if factory_deployment.deployer is not None:
                _deployer = factory_deployment.deployer

        _deployer = deployer_address or _deployer
        _init_hash = init_hash or _init_hash

        pool = UniswapV2Pool(
            address=pool_address,
            chain_id=chain_id,
            token0=token0,
            token1=token1,
            factory=factory,
            fee_token0=fee_token0,
            fee_token1=fee_token1,
            reserves_token0=reserves0,
            reserves_token1=reserves1,
            deployer_address=_deployer,
            init_hash=_init_hash,
            state_block=_state_block,
        )

        self.pools.add(pool_address=pool.address, chain_id=chain_id, pool=pool)

        if not silent:
            logger.info(pool.name)
            logger.info(f"• Address: {pool.address}")
            logger.info(f"• Token 0: {token0}")
            logger.info(f"• Token 1: {token1}")

        return pool

    # ------------------------------------------------------------------
    # V3 pool factory
    # ------------------------------------------------------------------

    async def build_v3_pool(
        self,
        pool_address: str,
        *,
        chain_id: ChainId | None = None,
        deployer_address: str | None = None,
        init_hash: str | None = None,
        state_block: int | None = None,
        silent: bool = False,
    ) -> Any:
        """Fetch pool data from DB/RPC and construct an I/O-free UniswapV3Pool."""
        from degenbot.database.models.pools import LiquidityPoolTable
        from degenbot.exceptions.liquidity_pool import LiquidityPoolError
        from degenbot.uniswap.deployments import FACTORY_DEPLOYMENTS as _FACTORY_DEPLOYMENTS
        from degenbot.uniswap.v3_functions import get_tick_word_and_bit_position
        from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
        from degenbot.uniswap.v3_types import (
            UniswapV3BitmapAtWord,
            UniswapV3LiquidityAtTick,
        )

        pool_address = get_checksum_address(pool_address)
        chain_id = chain_id or self.connections.default_chain_id
        provider = self.connections.get_provider(chain_id)

        _state_block = state_block if state_block is not None else await provider.get_block_number()

        # Try DB first
        pool_from_db = None
        with contextlib.suppress(Exception), self.db() as session:
            from sqlalchemy import select

            pool_from_db = session.scalar(
                select(LiquidityPoolTable).where(
                    LiquidityPoolTable.address == pool_address,
                    LiquidityPoolTable.chain == chain_id,
                )
            )

        # Get immutable values
        if pool_from_db is not None:
            factory = get_checksum_address(pool_from_db.exchange.factory)
            token0_address = pool_from_db.token0.address
            token1_address = pool_from_db.token1.address
            fee = pool_from_db.fee_token0
            tick_spacing_for_pool = pool_from_db.tick_spacing
        else:
            try:
                factory_result = await provider.call(
                    to=pool_address, data=encode_function_calldata("factory()", None),
                )
                token0_result = await provider.call(
                    to=pool_address, data=encode_function_calldata("token0()", None),
                )
                token1_result = await provider.call(
                    to=pool_address, data=encode_function_calldata("token1()", None),
                )
                fee_result = await provider.call(
                    to=pool_address, data=encode_function_calldata("fee()", None),
                )
                tick_spacing_result = await provider.call(
                    to=pool_address, data=encode_function_calldata("tickSpacing()", None),
                )
            except Exception as exc:
                raise LiquidityPoolError(message="Could not decode contract data") from exc

            (factory_raw,) = eth_abi.abi.decode(types=["address"], data=factory_result)
            (token0_raw,) = eth_abi.abi.decode(types=["address"], data=token0_result)
            (token1_raw,) = eth_abi.abi.decode(types=["address"], data=token1_result)
            (fee,) = eth_abi.abi.decode(types=["uint24"], data=fee_result)
            (tick_spacing_for_pool,) = eth_abi.abi.decode(types=["int24"], data=tick_spacing_result)

            factory = get_checksum_address(factory_raw)
            token0_address = get_checksum_address(token0_raw)
            token1_address = get_checksum_address(token1_raw)
            fee = int(fee)
            tick_spacing_for_pool = int(tick_spacing_for_pool)

        # Build tokens
        token0 = await self.build_erc20token(token0_address, chain_id=chain_id, silent=silent)
        token1 = await self.build_erc20token(token1_address, chain_id=chain_id, silent=silent)

        # Fetch slot0 + liquidity
        try:
            slot0_result = await provider.call(
                to=pool_address,
                data=encode_function_calldata("slot0()", None),
                block=_state_block,
            )
            liquidity_result = await provider.call(
                to=pool_address,
                data=encode_function_calldata("liquidity()", None),
                block=_state_block,
            )
        except Exception as exc:
            raise LiquidityPoolError(message="Could not decode contract data") from exc

        sqrt_price_x96, tick, *_ = eth_abi.abi.decode(
            types=["uint160", "int24", "uint16", "uint16", "uint16", "uint8", "bool"],
            data=slot0_result,
        )
        (liquidity,) = eth_abi.abi.decode(types=["uint128"], data=liquidity_result)

        # Fetch initial tick bitmap and tick data (sparse)
        working_tick_bitmap: dict[int, Any] = {}
        working_tick_data: dict[int, Any] = {}

        word, _ = get_tick_word_and_bit_position(
            tick=int(tick), tick_spacing=tick_spacing_for_pool,
        )

        (bitmap_at_word,) = await async_raw_call(
            provider,
            address=pool_address,
            calldata=encode_function_calldata("tickBitmap(int16)", [word]),
            return_types=["uint256"],
            block_identifier=_state_block,
        )

        if bitmap_at_word != 0:
            active_ticks = [
                ((word << 8) + i) * tick_spacing_for_pool
                for i in range(256)
                if bitmap_at_word & (1 << i) > 0
            ]
            for active_tick in active_ticks:
                result = await provider.call(
                    to=pool_address,
                    data=encode_function_calldata("ticks(int24)", [active_tick]),
                    block=_state_block,
                )
                liquidity_gross, liquidity_net, *_ = eth_abi.abi.decode(
                    types=["uint128", "int128", "uint256", "uint256", "int56", "uint160", "uint32", "bool"],
                    data=result,
                )
                working_tick_data[active_tick] = UniswapV3LiquidityAtTick(
                    liquidity_net=int(liquidity_net),
                    liquidity_gross=int(liquidity_gross),
                    block=_state_block,
                )

        working_tick_bitmap[word] = UniswapV3BitmapAtWord(
            bitmap=bitmap_at_word, block=_state_block,
        )

        _tick_bitmap_arg = working_tick_bitmap if working_tick_data else None
        _tick_data_arg = working_tick_data if working_tick_data else None

        # Determine deployer/init_hash
        _deployer = factory
        _init_hash = UniswapV3Pool.UNISWAP_V3_MAINNET_POOL_INIT_HASH
        with contextlib.suppress(KeyError):
            factory_deployment = _FACTORY_DEPLOYMENTS[chain_id][factory]
            _init_hash = factory_deployment.pool_init_hash
            if factory_deployment.deployer is not None:
                _deployer = factory_deployment.deployer

        _deployer = deployer_address or _deployer
        _init_hash = init_hash or _init_hash

        pool = UniswapV3Pool(
            address=pool_address,
            chain_id=chain_id,
            token0=token0,
            token1=token1,
            factory=factory,
            fee=fee,
            tick_spacing=tick_spacing_for_pool,
            sqrt_price_x96=int(sqrt_price_x96),
            tick=int(tick),
            liquidity=int(liquidity),
            state_block=_state_block,
            tick_bitmap=_tick_bitmap_arg,
            tick_data=_tick_data_arg,
            deployer_address=_deployer,
            init_hash=_init_hash,
        )

        self.pools.add(pool_address=pool.address, chain_id=chain_id, pool=pool)

        if not silent:
            logger.info(pool.name)

        return pool

    # ------------------------------------------------------------------
    # V4 pool factory
    # ------------------------------------------------------------------

    async def build_v4_pool(
        self,
        *,
        pool_id: str | bytes,
        pool_manager_address: str,
        state_view_address: str | None = None,
        tokens: Sequence[str] | None = None,
        fee: int | None = None,
        tick_spacing: int | None = None,
        hook_address: str | None = None,
        chain_id: ChainId | None = None,
        state_block: int | None = None,
        silent: bool = False,
    ) -> Any:
        """Fetch pool data from DB/RPC and construct an I/O-free UniswapV4Pool."""
        from degenbot.constants import ZERO_ADDRESS as _ZERO_ADDRESS
        from degenbot.database.models.pools import PoolManagerTable, UniswapV4PoolTable
        from degenbot.exceptions.liquidity_pool import LiquidityPoolError
        from degenbot.uniswap.v3_functions import get_tick_word_and_bit_position
        from degenbot.uniswap.v4_liquidity_pool import ProtocolFee, Slot0, UniswapV4Pool
        from degenbot.uniswap.v4_types import UniswapV4BitmapAtWord, UniswapV4LiquidityAtTick

        pool_manager_address = get_checksum_address(pool_manager_address)
        pool_id_bytes = HexBytes(pool_id)
        chain_id = chain_id or self.connections.default_chain_id
        provider = self.connections.get_provider(chain_id)

        _state_block = state_block if state_block is not None else await provider.get_block_number()

        # Try DB first
        pool_from_db = None
        with contextlib.suppress(Exception), self.db() as session:
            from sqlalchemy import select as _select

            pool_manager_in_db = session.scalar(
                _select(PoolManagerTable).where(
                    PoolManagerTable.address == pool_manager_address,
                    PoolManagerTable.chain == chain_id,
                )
            )
            if pool_manager_in_db is not None:
                pool_from_db = session.scalar(
                    _select(UniswapV4PoolTable).where(
                        UniswapV4PoolTable.pool_hash == pool_id_bytes.to_0x_hex(),
                        UniswapV4PoolTable.manager.has(id=pool_manager_in_db.id),
                    )
                )

        if pool_from_db is not None:
            currency0_address = pool_from_db.currency0.address
            currency1_address = pool_from_db.currency1.address
            _hook_address = get_checksum_address(pool_from_db.hooks)
            tick_spacing_for_pool = pool_from_db.tick_spacing
            fee_for_pool = pool_from_db.fee_currency0
            _state_view_address = pool_from_db.manager.state_view
        else:
            if state_view_address is None:
                raise DegenbotValueError(
                    message="A state view contract address must be provided for a pool not in the database."
                )
            if fee is None:
                raise DegenbotValueError(
                    message="A fee must be provided for a pool not in the database."
                )
            if tick_spacing is None:
                raise DegenbotValueError(
                    message="A tick spacing must be provided for a pool not in the database."
                )
            if tokens is None:
                raise DegenbotValueError(
                    message="Token addresses must be provided for a pool not in the database."
                )

            _state_view_address = get_checksum_address(state_view_address)
            currency0_address, currency1_address = sorted(
                [get_checksum_address(t) for t in tokens],
                key=lambda t: t.lower(),
            )
            _hook_address = (
                get_checksum_address(hook_address) if hook_address is not None else _ZERO_ADDRESS
            )
            fee_for_pool = fee
            tick_spacing_for_pool = tick_spacing

        token0 = await self.build_erc20token(currency0_address, chain_id=chain_id, silent=silent)
        token1 = await self.build_erc20token(currency1_address, chain_id=chain_id, silent=silent)

        # Fetch slot0 + liquidity
        try:
            slot0_result = await provider.call(
                to=_state_view_address,
                data=encode_function_calldata("getSlot0(bytes32)", [pool_id_bytes]),
                block=_state_block,
            )
            liquidity_result = await provider.call(
                to=_state_view_address,
                data=encode_function_calldata("getLiquidity(bytes32)", [pool_id_bytes]),
                block=_state_block,
            )
        except Exception as exc:
            raise LiquidityPoolError(message="Could not decode contract data") from exc

        price, tick_val, protocol_fee_val, lp_fee_val = eth_abi.abi.decode(
            types=["uint160", "int24", "uint24", "uint24"],
            data=slot0_result,
        )
        (liquidity_val,) = eth_abi.abi.decode(types=["uint256"], data=liquidity_result)

        protocol_fee_one_to_zero = protocol_fee_val >> 12
        protocol_fee_zero_to_one = protocol_fee_val & 0xFFF

        # Fetch tick bitmap
        working_tick_bitmap: dict[int, Any] = {}
        working_tick_data: dict[int, Any] = {}

        word, _ = get_tick_word_and_bit_position(
            tick=int(tick_val), tick_spacing=tick_spacing_for_pool,
        )

        (bitmap_at_word,) = await async_raw_call(
            provider,
            address=_state_view_address,
            calldata=encode_function_calldata(
                "getTickBitmap(bytes32,int16)", [pool_id_bytes, word],
            ),
            return_types=["uint256"],
            block_identifier=_state_block,
        )

        if bitmap_at_word != 0:
            active_ticks = [
                ((word << 8) + i) * tick_spacing_for_pool
                for i in range(256)
                if bitmap_at_word & (1 << i) > 0
            ]
            for active_tick in active_ticks:
                result = await provider.call(
                    to=_state_view_address,
                    data=encode_function_calldata(
                        "getTickLiquidity(bytes32,int24)", [pool_id_bytes, active_tick],
                    ),
                    block=_state_block,
                )
                liquidity_gross, liquidity_net = eth_abi.abi.decode(
                    types=["uint128", "int128"], data=result,
                )
                working_tick_data[active_tick] = UniswapV4LiquidityAtTick(
                    liquidity_net=int(liquidity_net),
                    liquidity_gross=int(liquidity_gross),
                    block=_state_block,
                )

        working_tick_bitmap[word] = UniswapV4BitmapAtWord(
            bitmap=bitmap_at_word, block=_state_block,
        )

        _tick_bitmap_arg = working_tick_bitmap if working_tick_data else None
        _tick_data_arg = working_tick_data if working_tick_data else None

        pool = UniswapV4Pool(
            pool_id=pool_id_bytes,
            pool_manager_address=pool_manager_address,
            token0=token0,
            token1=token1,
            fee=fee_for_pool,
            tick_spacing=tick_spacing_for_pool,
            hook_address=_hook_address,
            state_view_address=_state_view_address,
            sqrt_price_x96=int(price),
            tick=int(tick_val),
            liquidity=int(liquidity_val),
            protocol_fee_zero_for_one=protocol_fee_zero_to_one,
            protocol_fee_one_for_zero=protocol_fee_one_to_zero,
            lp_fee=int(lp_fee_val),
            state_block=_state_block,
            tick_bitmap=_tick_bitmap_arg,
            tick_data=_tick_data_arg,
            silent=silent,
        )

        self.managed_pools.add(
            pool=pool, chain_id=chain_id,
            pool_manager_address=pool.address, pool_id=pool.pool_id,
        )

        if not silent:
            logger.info(pool.name)

        return pool

    # ------------------------------------------------------------------
    # I/O methods
    # ------------------------------------------------------------------

    async def get_token_balance(
        self,
        token_address: str,
        holder_address: str,
        *,
        chain_id: ChainId | None = None,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the ERC-20 balance for the given address."""
        token_address = get_checksum_address(token_address)
        holder_address = get_checksum_address(holder_address)
        chain_id = chain_id or self.connections.default_chain_id
        provider = self.connections.get_provider(chain_id)

        token = self.tokens.get(token_address=token_address, chain_id=chain_id)
        if token is None:
            token = await self.build_erc20token(token_address, chain_id=chain_id)

        block_number = await self._resolve_block_number(provider, block_identifier)

        # Check cache
        if (balance := token.get_cached_balance(holder_address, block_number)) is not None:
            return balance

        (balance,) = eth_abi.abi.decode(
            types=["uint256"],
            data=await provider.call(
                to=token.address,
                data=Web3.keccak(text="balanceOf(address)")[:4]
                + eth_abi.abi.encode(types=["address"], args=[holder_address]),
                block=block_number,
            ),
        )

        token.set_cached_balance(holder_address, block_number, balance)
        return balance

    async def get_token_approval(
        self,
        token_address: str,
        owner: str,
        spender: str,
        *,
        chain_id: ChainId | None = None,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the amount that can be spent by `spender` on behalf of `owner`."""
        token_address = get_checksum_address(token_address)
        owner = get_checksum_address(owner)
        spender = get_checksum_address(spender)
        chain_id = chain_id or self.connections.default_chain_id
        provider = self.connections.get_provider(chain_id)

        token = self.tokens.get(token_address=token_address, chain_id=chain_id)
        if token is None:
            token = await self.build_erc20token(token_address, chain_id=chain_id)

        block_number = await self._resolve_block_number(provider, block_identifier)

        # Check cache
        if (approval := token.get_cached_approval(block_number, owner, spender)) is not None:
            return approval

        (approval,) = eth_abi.abi.decode(
            types=["uint256"],
            data=await provider.call(
                to=token.address,
                data=Web3.keccak(text="allowance(address,address)")[:4]
                + eth_abi.abi.encode(types=["address", "address"], args=[owner, spender]),
                block=block_number,
            ),
        )

        token.set_cached_approval(block_number, owner, spender, approval)
        return approval

    async def get_token_total_supply(
        self,
        token_address: str,
        *,
        chain_id: ChainId | None = None,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the total supply for this token."""
        token_address = get_checksum_address(token_address)
        chain_id = chain_id or self.connections.default_chain_id
        provider = self.connections.get_provider(chain_id)

        token = self.tokens.get(token_address=token_address, chain_id=chain_id)
        if token is None:
            token = await self.build_erc20token(token_address, chain_id=chain_id)

        block_number = await self._resolve_block_number(provider, block_identifier)

        # Check cache
        if (total_supply := token.get_cached_total_supply(block_number)) is not None:
            return total_supply

        (total_supply,) = eth_abi.abi.decode(
            types=["uint256"],
            data=await provider.call(
                to=token.address,
                data=Web3.keccak(text="totalSupply()")[:4],
                block=block_number,
            ),
        )

        token.set_cached_total_supply(block_number, total_supply)
        return total_supply

    async def get_ether_balance(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the native ETH balance for the given address."""
        address = get_checksum_address(address)
        chain_id = chain_id or self.connections.default_chain_id
        provider = self.connections.get_provider(chain_id)
        block = block_identifier if isinstance(block_identifier, int) else None
        return await provider.get_balance(address, block=block)

    @staticmethod
    async def _resolve_block_number(provider: Any, block_identifier: BlockIdentifier | None) -> int:
        """Resolve a block identifier to a block number."""
        if block_identifier is None:
            return await provider.get_block_number()
        if isinstance(block_identifier, int):
            return block_identifier
        # For string identifiers like 'latest', 'earliest', 'pending'
        return await provider.get_block_number()

    def get_provider(self, *, chain_id: ChainId) -> Any:
        return self.connections.get_provider(chain_id)

    def get_web3(self, *, chain_id: ChainId) -> Any:
        return self.connections.get_web3(chain_id)
