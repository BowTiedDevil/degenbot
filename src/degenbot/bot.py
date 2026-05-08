from __future__ import annotations

import contextlib
import dataclasses
from fractions import Fraction
from typing import TYPE_CHECKING, Any, cast

import eth_abi.abi
import sqlalchemy.exc
from eth_abi.exceptions import DecodingError
from hexbytes import HexBytes
from sqlalchemy import select
from web3 import Web3
from web3.exceptions import Web3Exception

from degenbot.aerodrome.pools import AerodromeV2Pool, AerodromeV3Pool
from degenbot.aerodrome.types import AerodromeV2PoolExternalUpdate
from degenbot.camelot.pools import CamelotLiquidityPool
from degenbot.checksum_cache import get_checksum_address
from degenbot.config import DegenbotConfig, _init_config
from degenbot.connection.connection_manager import ConnectionManager
from degenbot.constants import ZERO_ADDRESS as _ZERO_ADDRESS
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.database.models.pools import LiquidityPoolTable, PoolManagerTable, UniswapV4PoolTable
from degenbot.database.operations import get_scoped_sqlite_session
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.erc20 import Erc20Token, EtherPlaceholder
from degenbot.erc20.erc20 import get_token_from_database
from degenbot.exceptions.base import DegenbotValueError
from degenbot.exceptions.liquidity_pool import LiquidityPoolError
from degenbot.exceptions.manager import ManagerAlreadyInitialized
from degenbot.functions import encode_function_calldata, raw_call
from degenbot.logging import logger
from degenbot.pancakeswap.pools import PancakeswapV3Pool
from degenbot.registry import ManagedPoolRegistry, PoolRegistry, TokenRegistry
from degenbot.sushiswap.pools import SushiswapV3Pool
from degenbot.uniswap.deployments import FACTORY_DEPLOYMENTS
from degenbot.uniswap.deployments import FACTORY_DEPLOYMENTS as _FACTORY_DEPLOYMENTS
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v2_types import UniswapV2PoolExternalUpdate
from degenbot.uniswap.v3_functions import get_tick_word_and_bit_position
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v3_types import (
    UniswapV3BitmapAtWord,
    UniswapV3LiquidityAtTick,
    UniswapV3PoolExternalUpdate,
)
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool
from degenbot.uniswap.v4_types import (
    UniswapV4BitmapAtWord,
    UniswapV4LiquidityAtTick,
    UniswapV4PoolExternalUpdate,
)

if TYPE_CHECKING:
    from collections.abc import Callable, Sequence

    from eth_typing import ChecksumAddress
    from web3.types import BlockIdentifier

    from degenbot.types.abstract.pool_manager import AbstractPoolManager
    from degenbot.types.aliases import ChainId


class Bot:
    """
    Explicit session object that owns the runtime state for a degenbot run.

    Replaces the four module-level singletons (`config`, `db_session`,
    `connection_manager`, `pool_registry`/`token_registry`/`managed_pool_registry`)
    with per-session instances owned by this class.

    Bot is:
    - **Factory** — creates pools/tokens via managers, doing all I/O to fetch data
    - **Registry** — tracks what it's created
    - **I/O boundary** — all RPC calls and database access flow through Bot
    - **Session** — the lifetime scope for the entire run
    """

    def __init__(self, config: DegenbotConfig) -> None:
        self.config = config
        self.connections = ConnectionManager()
        self.db = DatabaseSessionManager(
            get_scoped_sqlite_session(database_path=config.database.path)
        )
        self.pools = PoolRegistry()
        self.tokens = TokenRegistry()
        self.managed_pools = ManagedPoolRegistry()
        self._managers: dict[tuple[ChainId, str], AbstractPoolManager] = {}

    @property
    def chain_id(self) -> ChainId:
        """Return the default chain ID from the connection manager."""
        return self.connections.default_chain_id

    @classmethod
    def from_config_file(cls) -> Bot:
        return cls(config=_init_config())

    def add_manager[M: AbstractPoolManager](
        self,
        manager_cls: type[M],
        *,
        factory_address: str,
        chain_id: ChainId | None = None,
        **kwargs: Any,
    ) -> M:
        """Create a pool manager within this bot's session."""
        factory_address = get_checksum_address(factory_address)
        chain_id = chain_id or self.connections.default_chain_id

        key = (chain_id, factory_address)
        if key in self._managers:
            raise ManagerAlreadyInitialized(
                message="A manager has already been initialized for this address. "
                "Access it using the bot's manager registry."
            )

        manager = manager_cls(
            factory_address=factory_address,
            chain_id=chain_id,
            bot=self,
            **kwargs,
        )
        self._managers[key] = manager
        return manager

    def build_erc20token(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        silent: bool = False,
    ) -> Erc20Token:
        """Fetch token metadata from DB/RPC and construct an I/O-free Erc20Token."""

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
                token=address,
                chain_id=chain_id,
                session=session,
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

            if not provider.get_code(address):
                raise DegenbotValueError(message="No contract deployed at this address")

            try:
                fetched_name, fetched_symbol, fetched_decimals = (
                    Erc20Token.fetch_name_symbol_decimals_batched(
                        address=address, provider=provider
                    )
                )
            except (Web3Exception, DecodingError):
                # Fallback: try individual calls with alternate prototypes
                for func_prototype in ("name()", "NAME()"):
                    try:
                        fetched_name = Erc20Token.fetch_name(
                            address=address, provider=provider, func_prototype=func_prototype
                        )
                        break
                    except (Web3Exception, DecodingError):
                        continue
                else:
                    fetched_name = Erc20Token.UNKNOWN_NAME

                for func_prototype in ("symbol()", "SYMBOL()"):
                    try:
                        fetched_symbol = Erc20Token.fetch_symbol(
                            address=address, provider=provider, func_prototype=func_prototype
                        )
                        break
                    except (Web3Exception, DecodingError):
                        continue
                else:
                    fetched_symbol = Erc20Token.UNKNOWN_SYMBOL

                for func_prototype in ("decimals()", "DECIMALS()"):
                    try:
                        fetched_decimals = Erc20Token.fetch_decimals(
                            address=address, provider=provider, func_prototype=func_prototype
                        )
                        break
                    except (Web3Exception, DecodingError):
                        continue
                else:
                    fetched_decimals = Erc20Token.UNKNOWN_DECIMALS

            name = name or fetched_name
            symbol = symbol or fetched_symbol
            decimals = decimals or fetched_decimals

            # Write back to DB if the record exists but was missing data
            if (
                token_from_db is not None
                and token_from_db.name is None
                and token_from_db.symbol is None
                and token_from_db.decimals is None
            ):
                with contextlib.suppress(sqlalchemy.exc.SQLAlchemyError), self.db() as session:
                    token_from_db.decimals = decimals
                    token_from_db.name = name
                    token_from_db.symbol = symbol
                    session.commit()

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

    def get_token(self, address: str, *, chain_id: ChainId | None = None) -> Erc20Token:
        """Get or create a token. Bot handles DB lookup, RPC calls, and registration."""
        return self.build_erc20token(address, chain_id=chain_id)

    def build_v2_pool(
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
        chain_id = chain_id or self.connections.default_chain_id
        provider = self.connections.get_provider(chain_id)

        state_block = state_block if state_block is not None else provider.get_block_number()

        # Try DB first
        pool_from_db = None
        with contextlib.suppress(Exception), self.db() as session:
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
        token0 = self.build_erc20token(token0_address, chain_id=chain_id, silent=silent)
        token1 = self.build_erc20token(token1_address, chain_id=chain_id, silent=silent)

        # Fetch reserves

        reserves0, reserves1 = raw_call(
            provider,
            address=pool_address,
            calldata=encode_function_calldata("getReserves()", None),
            return_types=["uint256", "uint256"],
            block_identifier=state_block,
        )

        # Determine deployer and init_hash from factory deployments
        deployer = factory
        init_hash = UniswapV2Pool.UNISWAP_V2_MAINNET_POOL_INIT_HASH
        with contextlib.suppress(KeyError):
            factory_deployment = FACTORY_DEPLOYMENTS[chain_id][factory]
            init_hash = factory_deployment.pool_init_hash
            if factory_deployment.deployer is not None:
                deployer = factory_deployment.deployer

        deployer = deployer_address or deployer
        init_hash = init_hash or init_hash

        # Detect Camelot pools by checking for stableSwap() function
        # Camelot pools have unique functions like stableSwap() and FEE_DENOMINATOR
        try:
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

            # If we got here, it's a Camelot pool - fetch fee token0/1
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

            pool = CamelotLiquidityPool(
                address=pool_address,
                chain_id=chain_id,
                token0=token0,
                token1=token1,
                factory=factory,
                fee_token0=fee_token0_raw,
                fee_token1=fee_token1_raw,
                fee_denominator=fee_denominator,
                reserves_token0=reserves0,
                reserves_token1=reserves1,
                stable_swap=stable_swap,
                state_block=state_block,
            )
        except Exception:
            # Not a Camelot pool, use standard UniswapV2Pool
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
                state_block=state_block,
                deployer_address=deployer,
                init_hash=init_hash,
            )

        # Register pool
        self.pools.add(pool_address=pool.address, chain_id=chain_id, pool=pool)

        if not silent:
            logger.info(pool.name)
            logger.info(f"• Token 0: {token0} - Reserves: {reserves0}")
            logger.info(f"• Token 1: {token1} - Reserves: {reserves1}")

        return pool

    def get_token_balance(
        self,
        token: Erc20Token,
        address: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the ERC-20 balance for the given address."""

        address = get_checksum_address(address)
        provider = self.connections.get_provider(token.chain_id)

        block_number = (
            block_identifier
            if isinstance(block_identifier, int)
            else self._resolve_block_number(provider, block_identifier)
        )

        # Check cache
        if (balance := token.get_cached_balance(address, block_number)) is not None:
            return balance

        (balance,) = eth_abi.abi.decode(
            types=["uint256"],
            data=provider.call(
                to=token.address,
                data=Web3.keccak(text="balanceOf(address)")[:4]
                + eth_abi.abi.encode(types=["address"], args=[address]),
                block=block_number,
            ),
        )

        token.set_cached_balance(address, block_number, balance)
        return balance

    def get_token_approval(
        self,
        token: Erc20Token,
        owner: str,
        spender: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the amount that can be spent by `spender` on behalf of `owner`."""

        owner = get_checksum_address(owner)
        spender = get_checksum_address(spender)
        provider = self.connections.get_provider(token.chain_id)

        block_number = (
            block_identifier
            if isinstance(block_identifier, int)
            else self._resolve_block_number(provider, block_identifier)
        )

        # Check cache
        if (approval := token.get_cached_approval(block_number, owner, spender)) is not None:
            return approval

        (approval,) = eth_abi.abi.decode(
            types=["uint256"],
            data=provider.call(
                to=token.address,
                data=Web3.keccak(text="allowance(address,address)")[:4]
                + eth_abi.abi.encode(types=["address", "address"], args=[owner, spender]),
                block=block_number,
            ),
        )

        token.set_cached_approval(block_number, owner, spender, approval)
        return approval

    def get_token_total_supply(
        self,
        token: Erc20Token,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the total supply for this token."""

        provider = self.connections.get_provider(token.chain_id)

        block_number = (
            block_identifier
            if isinstance(block_identifier, int)
            else self._resolve_block_number(provider, block_identifier)
        )

        # Check cache
        if (total_supply := token.get_cached_total_supply(block_number)) is not None:
            return total_supply

        (total_supply,) = eth_abi.abi.decode(
            types=["uint256"],
            data=provider.call(
                to=token.address,
                data=Web3.keccak(text="totalSupply()")[:4],
                block=block_number,
            ),
        )

        token.set_cached_total_supply(block_number, total_supply)
        return total_supply

    def get_ether_balance(
        self,
        chain_id: ChainId,
        address: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the native ETH balance for the given address."""
        address = get_checksum_address(address)
        provider = self.connections.get_provider(chain_id)
        return provider.get_balance(address, block=block_identifier)

    @staticmethod
    def _resolve_block_number(provider: Any, block_identifier: BlockIdentifier | None) -> int:
        """Resolve a block identifier to a block number."""
        if block_identifier is None:
            return provider.get_block_number()
        if isinstance(block_identifier, int):
            return block_identifier
        # For string identifiers like 'latest', 'earliest', 'pending'
        return provider.get_block_number()

    def _make_tick_data_fetcher_v3(
        self, pool_address: ChecksumAddress, chain_id: int
    ) -> Callable[[int, int], None]:
        """Create a tick data fetcher callback for a V3 pool."""

        def fetcher(word_position: int, block_number: int) -> None:
            pool = self.pools.get(pool_address=pool_address, chain_id=chain_id)
            if pool is None:
                return
            assert isinstance(pool, UniswapV3Pool)

            provider = self.connections.get_provider(chain_id)
            working_tick_bitmap = dict(pool.tick_bitmap)
            working_tick_data = dict(pool.tick_data)

            bitmap_value = pool.get_tick_bitmap_at_word(
                provider, word_position=word_position, block_identifier=block_number
            )
            working_tick_bitmap[word_position] = UniswapV3BitmapAtWord(
                bitmap=bitmap_value, block=block_number
            )

            if bitmap_value != 0:
                populated_ticks = pool.get_populated_ticks_in_word(
                    provider, word_position=word_position, block_identifier=block_number
                )
                for tick, liquidity_gross, liquidity_net in populated_ticks:
                    working_tick_data[tick] = UniswapV3LiquidityAtTick(
                        liquidity_net=liquidity_net,
                        liquidity_gross=liquidity_gross,
                        block=block_number,
                    )

            new_state = dataclasses.replace(
                pool.state,
                tick_bitmap=working_tick_bitmap,
                tick_data=working_tick_data,
                block=max(pool.update_block, block_number),
            )
            pool._state_mgr.push_state(new_state)

        return fetcher

    def _make_tick_data_fetcher_v4(
        self, pool_id: HexBytes, pool_manager_address: str, state_view_address: str, chain_id: int
    ) -> Callable[[int, int], None]:
        """Create a tick data fetcher callback for a V4 pool."""

        def fetcher(word_position: int, block_number: int) -> None:
            pool = self.managed_pools.get(
                chain_id=chain_id,
                pool_manager_address=pool_manager_address,
                pool_id=pool_id,
            )
            if pool is None:
                return
            assert isinstance(pool, UniswapV4Pool)

            provider = self.connections.get_provider(chain_id)
            working_tick_bitmap = dict(pool.tick_bitmap)
            working_tick_data = dict(pool.tick_data)

            bitmap_value = pool.get_tick_bitmap_at_word(
                provider,
                word_position=word_position,
                block_identifier=block_number,
            )
            working_tick_bitmap[word_position] = UniswapV4BitmapAtWord(
                bitmap=bitmap_value, block=block_number
            )

            if bitmap_value != 0:
                populated_ticks = pool.get_populated_ticks_in_word(
                    provider,
                    word_position=word_position,
                    block_identifier=block_number,
                )
                for tick, liquidity_gross, liquidity_net in populated_ticks:
                    working_tick_data[tick] = UniswapV4LiquidityAtTick(
                        liquidity_net=liquidity_net,
                        liquidity_gross=liquidity_gross,
                        block=block_number,
                    )

            new_state = dataclasses.replace(
                pool.state,
                tick_bitmap=working_tick_bitmap,
                tick_data=working_tick_data,
                block=max(pool.update_block, block_number),
            )
            pool._state_mgr.push_state(new_state)

        return fetcher

    def build_v3_pool(
        self,
        pool_address: str,
        *,
        chain_id: ChainId | None = None,
        deployer_address: str | None = None,
        init_hash: str | None = None,
        state_block: int | None = None,
        tick_bitmap: dict[int, UniswapV3BitmapAtWord] | None = None,
        tick_data: dict[int, UniswapV3LiquidityAtTick] | None = None,
        silent: bool = False,
    ) -> UniswapV3Pool:
        """Fetch pool data from DB/RPC and construct an I/O-free UniswapV3Pool."""

        pool_address = get_checksum_address(pool_address)
        chain_id = chain_id or self.connections.default_chain_id
        provider = self.connections.get_provider(chain_id)

        state_block = state_block if state_block is not None else provider.get_block_number()

        # Try DB first
        pool_from_db = None
        with contextlib.suppress(Exception), self.db() as session:
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

            if pool_from_db.exchange.deployer is not None:
                deployer_address = pool_from_db.exchange.deployer
        else:
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
                fee_result = provider.call(
                    to=pool_address,
                    data=encode_function_calldata("fee()", None),
                )
                tick_spacing_result = provider.call(
                    to=pool_address,
                    data=encode_function_calldata("tickSpacing()", None),
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
        token0 = self.build_erc20token(token0_address, chain_id=chain_id, silent=silent)
        token1 = self.build_erc20token(token1_address, chain_id=chain_id, silent=silent)

        # Fetch slot0 + liquidity
        try:
            slot0_result = provider.call(
                to=pool_address,
                data=encode_function_calldata("slot0()", None),
                block=state_block,
            )
            liquidity_result = provider.call(
                to=pool_address,
                data=encode_function_calldata("liquidity()", None),
                block=state_block,
            )
        except Exception as exc:
            raise LiquidityPoolError(message="Could not decode contract data") from exc

        sqrt_price_x96, tick, *_ = eth_abi.abi.decode(
            types=["uint160", "int24", "uint16", "uint16", "uint16", "uint8", "bool"],
            data=slot0_result,
        )
        (liquidity,) = eth_abi.abi.decode(types=["uint128"], data=liquidity_result)

        # Fetch initial tick bitmap and tick data
        # Track if we have complete snapshot data (DB or explicit args).
        # Single-word fetches from chain are incomplete and should use sparse mode.
        db_snapshot_loaded = False
        working_tick_bitmap: dict[int, Any] = {}
        working_tick_data: dict[int, Any] = {}

        # Use provided tick data if given (snapshot or test fixtures)
        if tick_bitmap is not None and tick_data is not None:  # noqa:PLR1702
            working_tick_bitmap = dict(tick_bitmap)
            working_tick_data = dict(tick_data)
            # Assume provided tick data is complete
            db_snapshot_loaded = True
        elif tick_bitmap is not None or tick_data is not None:
            raise DegenbotValueError(message="Provide both tick_bitmap and tick_data, or neither.")
        else:
            # Try DB snapshot tables first
            db_snapshot_loaded = False
            if pool_from_db is not None and hasattr(pool_from_db, "liquidity_positions"):
                with contextlib.suppress(Exception), self.db() as session:
                    if hasattr(pool_from_db, "pool_id"):
                        # Reload to access relationships in a fresh session
                        pool_with_data = session.scalar(
                            select(type(pool_from_db)).where(  # type: ignore[arg-type]
                                LiquidityPoolTable.id == pool_from_db.id
                            )
                        )
                        if pool_with_data is not None:
                            init_maps = pool_with_data.initialization_maps
                            liq_positions = pool_with_data.liquidity_positions
                            if init_maps and liq_positions:
                                for init_map in init_maps:
                                    working_tick_bitmap[int(init_map.word)] = UniswapV3BitmapAtWord(
                                        bitmap=int(init_map.bitmap),
                                        block=pool_with_data.liquidity_update_block or 0,
                                    )
                                for pos in liq_positions:
                                    working_tick_data[int(pos.tick)] = UniswapV3LiquidityAtTick(
                                        liquidity_net=int(pos.liquidity_net),
                                        liquidity_gross=int(pos.liquidity_gross),
                                        block=pool_with_data.liquidity_update_block or 0,
                                    )
                                db_snapshot_loaded = True

            if not db_snapshot_loaded:
                word, _ = get_tick_word_and_bit_position(
                    tick=int(tick), tick_spacing=tick_spacing_for_pool
                )

                (bitmap_at_word,) = raw_call(
                    provider,
                    address=pool_address,
                    calldata=encode_function_calldata("tickBitmap(int16)", [word]),
                    return_types=["uint256"],
                    block_identifier=state_block,
                )

                if bitmap_at_word != 0:
                    # Fetch initialized ticks in this word
                    active_ticks = [
                        ((word << 8) + i) * tick_spacing_for_pool
                        for i in range(256)
                        if bitmap_at_word & (1 << i) > 0
                    ]

                    for active_tick in active_ticks:
                        result = provider.call(
                            to=pool_address,
                            data=encode_function_calldata("ticks(int24)", [active_tick]),
                            block=state_block,
                        )
                        liquidity_gross, liquidity_net, *_ = eth_abi.abi.decode(
                            types=[
                                "uint128",
                                "int128",
                                "uint256",
                                "uint256",
                                "int56",
                                "uint160",
                                "uint32",
                                "bool",
                            ],
                            data=result,
                        )
                        working_tick_data[active_tick] = UniswapV3LiquidityAtTick(
                            liquidity_net=int(liquidity_net),
                            liquidity_gross=int(liquidity_gross),
                            block=state_block,
                        )

                working_tick_bitmap[word] = UniswapV3BitmapAtWord(
                    bitmap=bitmap_at_word,
                    block=state_block,
                )

        # Determine deployer and init_hash
        deployer = factory
        init_hash = UniswapV3Pool.UNISWAP_V3_MAINNET_POOL_INIT_HASH
        with contextlib.suppress(KeyError):
            factory_deployment = _FACTORY_DEPLOYMENTS[chain_id][factory]
            init_hash = factory_deployment.pool_init_hash
            if factory_deployment.deployer is not None:
                deployer = factory_deployment.deployer

        deployer = deployer_address or deployer
        init_hash = init_hash or init_hash

        # Only pass tick data if we have a complete DB snapshot.
        # Single-word fetches from chain should use sparse mode so the pool
        # can fetch additional tick data on-demand during swaps.
        if db_snapshot_loaded and working_tick_data:
            tick_bitmap_arg = working_tick_bitmap
            tick_data_arg = working_tick_data
        else:
            tick_bitmap_arg = None
            tick_data_arg = None

        # Map factory addresses to pool classes for V3 variants
        v3_pool_class_map: dict[tuple[int, str], type[UniswapV3Pool]] = {
            # Sushiswap V3
            (1, "0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F"): SushiswapV3Pool,
            (42161, "0x1af415a1EbA07a4986a52B6f2e7dE7003D82231e"): SushiswapV3Pool,
            # Aerodrome V3
            (8453, "0x5e7BB104d84c7CB9B682AaC2F3d509f5F406809A"): AerodromeV3Pool,
            # Pancakeswap V3
            (1, "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"): PancakeswapV3Pool,
            (8453, "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"): PancakeswapV3Pool,
        }

        pool_class = v3_pool_class_map.get((chain_id, factory), UniswapV3Pool)

        pool = pool_class(
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
            state_block=state_block,
            tick_bitmap=tick_bitmap_arg,
            tick_data=tick_data_arg,
            deployer_address=deployer,
            init_hash=init_hash,
            tick_data_fetcher=self._make_tick_data_fetcher_v3(pool_address, chain_id),
        )

        # Register pool
        self.pools.add(pool_address=pool.address, chain_id=chain_id, pool=pool)

        if not silent:
            logger.info(pool.name)
            logger.info(f"• Address: {pool.address}")
            logger.info(f"• Token 0: {token0}")
            logger.info(f"• Token 1: {token1}")
            logger.info(f"• Fee: {fee}")
            logger.info(f"• Liquidity: {pool.liquidity}")
            logger.info(f"• SqrtPrice: {pool.sqrt_price_x96}")
            logger.info(f"• Tick: {pool.tick}")
            logger.info(f"• State Block (Initial): {state_block}")

        return pool

    def build_v4_pool(
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
        tick_bitmap: dict[int, UniswapV4BitmapAtWord] | None = None,
        tick_data: dict[int, UniswapV4LiquidityAtTick] | None = None,
        silent: bool = False,
    ) -> UniswapV4Pool:  # UniswapV4Pool — return type deferred to avoid circular import
        """Fetch pool data from DB/RPC and construct an I/O-free UniswapV4Pool."""

        pool_manager_address = get_checksum_address(pool_manager_address)
        pool_id_bytes = HexBytes(pool_id)
        chain_id = chain_id or self.connections.default_chain_id
        provider = self.connections.get_provider(chain_id)

        state_block = state_block if state_block is not None else provider.get_block_number()

        # Try DB first
        pool_from_db = None
        with contextlib.suppress(Exception), self.db() as session:
            pool_manager_in_db = session.scalar(
                select(PoolManagerTable).where(
                    PoolManagerTable.address == pool_manager_address,
                    PoolManagerTable.chain == chain_id,
                )
            )
            if pool_manager_in_db is not None:
                pool_from_db = session.scalar(
                    select(UniswapV4PoolTable).where(
                        UniswapV4PoolTable.pool_hash == pool_id_bytes.to_0x_hex(),
                        UniswapV4PoolTable.manager.has(id=pool_manager_in_db.id),
                    )
                )

        # Get immutable values
        if pool_from_db is not None:
            currency0_address = pool_from_db.currency0.address
            currency1_address = pool_from_db.currency1.address
            hook_address = get_checksum_address(pool_from_db.hooks)
            tick_spacing_for_pool = pool_from_db.tick_spacing
            fee_for_pool = pool_from_db.fee_currency0
            state_view_address = pool_from_db.manager.state_view
        else:
            if state_view_address is None:
                raise DegenbotValueError(
                    message="A state view contract address must be provided for a pool not in the database."  # noqa: E501
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

            state_view_address = get_checksum_address(state_view_address)
            currency0_address, currency1_address = sorted(
                [get_checksum_address(t) for t in tokens],
                key=lambda t: t.lower(),
            )
            hook_address = (
                get_checksum_address(hook_address) if hook_address is not None else _ZERO_ADDRESS
            )
            fee_for_pool = fee
            tick_spacing_for_pool = tick_spacing

        # Build tokens
        token0 = self.build_erc20token(currency0_address, chain_id=chain_id, silent=silent)
        token1 = self.build_erc20token(currency1_address, chain_id=chain_id, silent=silent)

        # Fetch slot0 + liquidity via state view contract
        try:
            slot0_calldata = encode_function_calldata(
                "getSlot0(bytes32)",
                [pool_id_bytes],
            )
            liquidity_calldata = encode_function_calldata(
                "getLiquidity(bytes32)",
                [pool_id_bytes],
            )

            slot0_result = provider.call(
                to=state_view_address,
                data=slot0_calldata,
                block=state_block,
            )
            liquidity_result = provider.call(
                to=state_view_address,
                data=liquidity_calldata,
                block=state_block,
            )
        except Exception as exc:
            raise LiquidityPoolError(message="Could not decode contract data") from exc

        price, tick_val, protocol_fee_val, lp_fee_val = eth_abi.abi.decode(
            types=["uint160", "int24", "uint24", "uint24"],
            data=slot0_result,
        )
        (liquidity_val,) = eth_abi.abi.decode(
            types=["uint256"],
            data=liquidity_result,
        )

        # Extract two fees (uint12) from packed uint24
        protocol_fee_one_to_zero = protocol_fee_val >> 12
        protocol_fee_zero_to_one = protocol_fee_val & 0xFFF

        # Fetch initial tick bitmap and tick data
        working_tick_bitmap: dict[int, Any] = {}
        working_tick_data: dict[int, Any] = {}

        # Use provided tick data if given (snapshot or test fixtures)
        if tick_bitmap is not None and tick_data is not None:  # noqa:PLR1702
            working_tick_bitmap = dict(tick_bitmap)
            working_tick_data = dict(tick_data)
        elif tick_bitmap is not None or tick_data is not None:
            raise DegenbotValueError(message="Provide both tick_bitmap and tick_data, or neither.")
        else:
            # Try DB snapshot tables first
            db_snapshot_loaded = False
            if pool_from_db is not None and hasattr(pool_from_db, "liquidity_positions"):
                with contextlib.suppress(Exception), self.db() as session:
                    if hasattr(pool_from_db, "managed_pool_id"):
                        pool_with_data = session.scalar(
                            select(type(pool_from_db)).where(  # type: ignore[arg-type]
                                UniswapV4PoolTable.id == pool_from_db.id
                            )
                        )
                        if pool_with_data is not None:
                            init_maps = pool_with_data.initialization_maps
                            liq_positions = pool_with_data.liquidity_positions
                            if init_maps and liq_positions:
                                for init_map in init_maps:
                                    working_tick_bitmap[int(init_map.word)] = UniswapV4BitmapAtWord(
                                        bitmap=int(init_map.bitmap),
                                        block=pool_with_data.liquidity_update_block or 0,
                                    )
                                for pos in liq_positions:
                                    working_tick_data[int(pos.tick)] = UniswapV4LiquidityAtTick(
                                        liquidity_net=int(pos.liquidity_net),
                                        liquidity_gross=int(pos.liquidity_gross),
                                        block=pool_with_data.liquidity_update_block or 0,
                                    )
                                db_snapshot_loaded = True

            if not db_snapshot_loaded:
                word, _ = get_tick_word_and_bit_position(
                    tick=int(tick_val), tick_spacing=tick_spacing_for_pool
                )

                (bitmap_at_word,) = raw_call(
                    provider,
                    address=state_view_address,
                    calldata=encode_function_calldata(
                        "getTickBitmap(bytes32,int16)",
                        [pool_id_bytes, word],
                    ),
                    return_types=["uint256"],
                    block_identifier=state_block,
                )

                if bitmap_at_word != 0:
                    active_ticks = [
                        ((word << 8) + i) * tick_spacing_for_pool
                        for i in range(256)
                        if bitmap_at_word & (1 << i) > 0
                    ]

                    for active_tick in active_ticks:
                        result = provider.call(
                            to=state_view_address,
                            data=encode_function_calldata(
                                "getTickLiquidity(bytes32,int24)",
                                [pool_id_bytes, active_tick],
                            ),
                            block=state_block,
                        )
                        liquidity_gross, liquidity_net = eth_abi.abi.decode(
                            types=["uint128", "int128"],
                            data=result,
                        )
                        working_tick_data[active_tick] = UniswapV4LiquidityAtTick(
                            liquidity_net=int(liquidity_net),
                            liquidity_gross=int(liquidity_gross),
                            block=state_block,
                        )

                working_tick_bitmap[word] = UniswapV4BitmapAtWord(
                    bitmap=bitmap_at_word,
                    block=state_block,
                )

        # If tick data was populated, pass both. Otherwise pass None (sparse mode).
        tick_bitmap_arg = working_tick_bitmap if working_tick_data else None
        tick_data_arg = working_tick_data or None

        pool = UniswapV4Pool(
            pool_id=pool_id_bytes,
            pool_manager_address=pool_manager_address,
            token0=token0,
            token1=token1,
            fee=fee_for_pool,
            tick_spacing=tick_spacing_for_pool,
            hook_address=hook_address,
            state_view_address=state_view_address,
            sqrt_price_x96=int(price),
            tick=int(tick_val),
            liquidity=int(liquidity_val),
            protocol_fee_zero_for_one=protocol_fee_zero_to_one,
            protocol_fee_one_for_zero=protocol_fee_one_to_zero,
            lp_fee=int(lp_fee_val),
            state_block=state_block,
            tick_bitmap=tick_bitmap_arg,
            tick_data=tick_data_arg,
            tick_data_fetcher=self._make_tick_data_fetcher_v4(
                pool_id_bytes, pool_manager_address, state_view_address, chain_id
            ),
        )

        # Register pool in managed pool registry
        self.managed_pools.add(
            pool=pool,
            chain_id=chain_id,
            pool_manager_address=pool.address,
            pool_id=pool.pool_id,
        )

        if not silent:
            logger.info(pool.name)
            logger.info(f"• ID: {pool.pool_id.to_0x_hex()}")
            logger.info(f"• Token 0: {token0}")
            logger.info(f"• Token 1: {token1}")
            logger.info(f"• Liquidity: {pool.liquidity}")
            logger.info(f"• SqrtPrice: {pool.sqrt_price_x96}")
            logger.info(f"• Tick: {pool.tick}")

        return pool

    def get_provider(self, *, chain_id: ChainId) -> Any:
        return self.connections.get_provider(chain_id)

    def get_web3(self, *, chain_id: ChainId) -> Any:
        return self.connections.get_web3(chain_id)

    def update(self, pool: Any, *, block_number: BlockIdentifier | None = None) -> bool:
        """
        Fetch the current state of a pool from the chain and apply it via
        ``pool.external_update()``.

        Returns True if the state changed, False if unchanged.
        """

        provider = self.connections.get_provider(pool.chain_id)

        if isinstance(pool, UniswapV2Pool) and not isinstance(pool, AerodromeV2Pool):
            return self._update_v2_pool(pool, provider=provider, block_number=block_number)
        if isinstance(pool, AerodromeV2Pool):
            return self._update_aerodrome_v2_pool(
                pool, provider=provider, block_number=block_number
            )
        if isinstance(pool, UniswapV3Pool) and not isinstance(pool, UniswapV4Pool):
            return self._update_v3_pool(pool, provider=provider, block_number=block_number)
        if isinstance(pool, UniswapV4Pool):
            return self._update_v4_pool(pool, provider=provider, block_number=block_number)
        raise TypeError(f"update() not implemented for pool type {type(pool).__name__}")

    def _update_v2_pool(
        self, pool: Any, *, provider: Any, block_number: BlockIdentifier | None
    ) -> bool:

        assert isinstance(pool, UniswapV2Pool)
        _block_number = block_number if block_number is not None else provider.get_block_number()
        reserves0, reserves1 = pool.get_reserves(provider, block_identifier=_block_number)

        if pool.reserves_token0 == reserves0 and pool.reserves_token1 == reserves1:
            return False

        update = UniswapV2PoolExternalUpdate(
            block_number=_block_number,
            reserves_token0=reserves0,
            reserves_token1=reserves1,
        )
        pool.external_update(update)
        return True

    def _update_aerodrome_v2_pool(
        self, pool: Any, *, provider: Any, block_number: BlockIdentifier | None
    ) -> bool:

        assert isinstance(pool, AerodromeV2Pool)
        _block_number = block_number if block_number is not None else provider.get_block_number()
        reserves0, reserves1 = pool.get_reserves(provider, block_identifier=_block_number)

        if pool.reserves_token0 == reserves0 and pool.reserves_token1 == reserves1:
            return False

        update = AerodromeV2PoolExternalUpdate(
            block_number=_block_number,
            reserves_token0=reserves0,
            reserves_token1=reserves1,
        )
        pool.external_update(update)
        return True

    def _update_v3_pool(
        self, pool: Any, *, provider: Any, block_number: BlockIdentifier | None
    ) -> bool:

        assert isinstance(pool, UniswapV3Pool)
        _block_number = block_number if block_number is not None else provider.get_block_number()

        slot0_result = provider.call(
            to=pool.address,
            data=encode_function_calldata("slot0()", None),
            block=_block_number,
        )
        sqrt_price_x96, tick, *_ = cast(
            "tuple[int, ...]",
            eth_abi.abi.decode(
                types=["uint160", "int24", "uint16", "uint16", "uint16"], data=slot0_result
            ),
        )
        (liquidity,) = cast(
            "tuple[int]",
            eth_abi.abi.decode(
                types=["uint256"],
                data=provider.call(
                    to=pool.address,
                    data=encode_function_calldata("liquidity()", None),
                    block=_block_number,
                ),
            ),
        )

        if (
            pool.sqrt_price_x96 == sqrt_price_x96
            and pool.liquidity == liquidity
            and pool.tick == tick
        ):
            return False

        update = UniswapV3PoolExternalUpdate(
            block_number=_block_number,
            sqrt_price_x96=sqrt_price_x96,
            tick=tick,
            liquidity=liquidity,
        )
        pool.external_update(update)
        return True

    def _update_v4_pool(
        self, pool: Any, *, provider: Any, block_number: BlockIdentifier | None
    ) -> bool:

        assert isinstance(pool, UniswapV4Pool)
        _block_number = block_number if block_number is not None else provider.get_block_number()

        slot0_calldata = encode_function_calldata("getSlot0(bytes32)", [pool.pool_id])
        slot0_result = provider.call(
            to=pool._state_view_address,
            data=slot0_calldata,
            block=_block_number,
        )
        price, tick, protocol_fee, lp_fee = cast(
            "tuple[int, ...]",
            eth_abi.abi.decode(types=["uint160", "int24", "uint24", "uint24"], data=slot0_result),
        )

        liquidity_calldata = encode_function_calldata("getLiquidity(bytes32)", [pool.pool_id])
        (liquidity_val,) = cast(
            "tuple[int]",
            eth_abi.abi.decode(
                types=["uint256"],
                data=provider.call(
                    to=pool._state_view_address,
                    data=liquidity_calldata,
                    block=_block_number,
                ),
            ),
        )

        if pool.sqrt_price_x96 == price and pool.liquidity == liquidity_val and pool.tick == tick:
            return False

        update = UniswapV4PoolExternalUpdate(
            block_number=_block_number,
            sqrt_price_x96=price,
            tick=tick,
            liquidity=liquidity_val,
        )
        pool.external_update(update)
        return True
