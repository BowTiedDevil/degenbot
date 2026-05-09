from __future__ import annotations

import contextlib
import dataclasses
from fractions import Fraction
from typing import TYPE_CHECKING, Any, cast

import eth_abi.abi
import sqlalchemy.exc
from alembic.runtime.migration import MigrationContext
from alembic.script import ScriptDirectory
from eth_abi.exceptions import DecodingError
from hexbytes import HexBytes
from sqlalchemy import select
from web3 import Web3
from web3.exceptions import Web3Exception
from web3.types import TxParams

from degenbot.aerodrome.pools import AerodromeV2Pool, AerodromeV3Pool
from degenbot.aerodrome.types import AerodromeV2PoolExternalUpdate
from degenbot.camelot.pools import CamelotLiquidityPool
from degenbot.checksum_cache import get_checksum_address
from degenbot.config import DegenbotConfig, _init_config
from degenbot.connection.connection_manager import ConnectionManager
from degenbot.constants import ZERO_ADDRESS as _ZERO_ADDRESS
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.curve.deployments import CURVE_V1_FACTORY_ADDRESS, CURVE_V1_REGISTRY_ADDRESS
from degenbot.curve.types import CurveStableswapPoolExternalUpdate
from degenbot.database.models.pools import LiquidityPoolTable, PoolManagerTable, UniswapV4PoolTable
from degenbot.database.operations import get_alembic_config, get_scoped_sqlite_session
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.erc20 import EtherPlaceholder
from degenbot.erc20.erc20 import (
    UNKNOWN_DECIMALS,
    UNKNOWN_NAME,
    UNKNOWN_SYMBOL,
    Erc20Token,
    get_token_from_database,
)
from degenbot.exceptions.base import DegenbotValueError
from degenbot.exceptions.liquidity_pool import BrokenPool, LiquidityPoolError
from degenbot.exceptions.manager import ManagerAlreadyInitialized
from degenbot.functions import encode_function_calldata, raw_call
from degenbot.logging import logger
from degenbot.pancakeswap.pools import PancakeswapV2Pool, PancakeswapV3Pool
from degenbot.registry import ManagedPoolRegistry, PoolRegistry, TokenRegistry
from degenbot.sushiswap.pools import SushiswapV2Pool, SushiswapV3Pool
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
from degenbot.version import __version__

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

        # Check database migration version
        self._check_database_version()

    @property
    def chain_id(self) -> ChainId:
        """Return the default chain ID from the connection manager."""
        return self.connections.default_chain_id

    def _check_database_version(self) -> None:
        """Warn if the database schema is out of date."""

        try:
            with self.db() as session:
                current_version = MigrationContext.configure(
                    connection=self.db.connection()
                ).get_current_revision()
        except Exception:  # noqa: BLE001
            return

        latest_version = ScriptDirectory.from_config(
            config=get_alembic_config(database_path=self.config.database.path)
        ).get_current_head()

        if current_version is not None and current_version != latest_version:
            logger.warning(
                f"The current database revision ({current_version}) does not match the latest "
                f"({latest_version}) for {__package__} version {__version__}!"
                "\n"
                "Database-related features may raise exceptions if you continue. Perform database "
                "migrations with 'degenbot database upgrade'."
            )

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
                    fetched_name = UNKNOWN_NAME

                for func_prototype in ("symbol()", "SYMBOL()"):
                    try:
                        fetched_symbol = Erc20Token.fetch_symbol(
                            address=address, provider=provider, func_prototype=func_prototype
                        )
                        break
                    except (Web3Exception, DecodingError):
                        continue
                else:
                    fetched_symbol = UNKNOWN_SYMBOL

                for func_prototype in ("decimals()", "DECIMALS()"):
                    try:
                        fetched_decimals = Erc20Token.fetch_decimals(
                            address=address, provider=provider, func_prototype=func_prototype
                        )
                        break
                    except (Web3Exception, DecodingError):
                        continue
                else:
                    fetched_decimals = UNKNOWN_DECIMALS

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
            # Not a Camelot pool, select the appropriate V2 pool subclass
            v2_pool_class_map: dict[tuple[int, str], type[UniswapV2Pool]] = {
                # Sushiswap V2
                (1, "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"): SushiswapV2Pool,
                (8453, "0x71524B4f93c58fcbF659783284E38825f0622859"): SushiswapV2Pool,
                (42161, "0xc35DADB65012eC5796536bD9864eD8773aBc74C4"): SushiswapV2Pool,
                # Pancakeswap V2
                (1, "0x1097053Fd2ea711dad45caCcc45EfF7548fCB362"): PancakeswapV2Pool,
                (8453, "0x02a84c1b3BBD7401a5f7fa98a384EBC70bB5749E"): PancakeswapV2Pool,
                # Note: AerodromeV2Pool is NOT included here because:
                # 1. It has a different constructor signature (stable, fee)
                # 2. AerodromeV2PoolManager already creates AerodromeV2Pool correctly
            }

            pool_class = v2_pool_class_map.get((chain_id, factory), UniswapV2Pool)

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

            try:
                bitmap_value = pool.get_tick_bitmap_at_word(
                    provider, word_position=word_position, block_identifier=block_number
                )
            except Exception:
                # If fetching fails (e.g., historical block unavailable),
                # don't update the pool state - let the caller handle the missing word
                return

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

            try:
                bitmap_value = pool.get_tick_bitmap_at_word(
                    provider,
                    word_position=word_position,
                    block_identifier=block_number,
                )
            except Exception:
                # If fetching fails (e.g., historical block unavailable),
                # don't update the pool state - let the caller handle the missing word
                return
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

    def build_curve_pool(
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
        chain_id = chain_id or self.connections.default_chain_id
        provider = self.connections.get_provider(chain_id)

        state_block = state_block if state_block is not None else provider.get_block_number()

        # Fetch pool parameters - Curve pools don't have a standard N_COINS
        # function, so we iterate until we hit a revert
        w3 = self.connections.get_web3(chain_id)

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
            self.build_erc20token(addr, chain_id=chain_id, silent=silent)
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
            lp_token = self.build_erc20token(lp_token_address, chain_id=chain_id, silent=silent)

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
                base_pool = self.build_curve_pool(
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
                        self.build_erc20token(underlying_addr, chain_id=chain_id, silent=silent)
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
            virtual_price_fetcher=self._make_curve_virtual_price_fetcher(
                pool_address, chain_id, base_pool_address=base_pool_address if base_pool else None
            ),
            base_virtual_price_fetcher=self._make_curve_base_virtual_price_fetcher(
                pool_address, chain_id
            ),
            timestamp_fetcher=self._make_curve_timestamp_fetcher(chain_id),
            redemption_price_fetcher=self._make_curve_redemption_price_fetcher(
                pool_address, chain_id
            ),
            admin_balances_fetcher=self._make_curve_admin_balances_fetcher(pool_address, chain_id),
            block_number_fetcher=self._make_curve_block_number_fetcher(chain_id),
            total_supply_fetcher=self._make_curve_total_supply_fetcher(chain_id),
            token_balance_fetcher=self._make_curve_token_balance_fetcher(chain_id),
            provider_call=self._make_curve_provider_call(chain_id),
            # Crypto pool fetchers (only useful for crypto pools like Tricrypto)
            D_fetcher=self._make_curve_D_fetcher(chain_id, pool_address)
            if pool_fee_gamma
            else None,
            gamma_fetcher=self._make_curve_gamma_fetcher(chain_id, pool_address)
            if pool_fee_gamma
            else None,
            price_scale_fetcher=self._make_curve_price_scale_fetcher(
                chain_id, pool_address, len(tokens)
            )
            if pool_fee_gamma
            else None,
        )

        # Register pool
        self.pools.add(pool, chain_id=chain_id, pool_address=pool.address)

        if not silent:
            logger.info(pool.name)
            logger.info(f"• Address: {pool.address}")
            logger.info(f"• Tokens: {[t.symbol for t in pool.tokens]}")
            logger.info(f"• A: {pool.a_coefficient}")
            logger.info(f"• Fee: {100 * pool.fee / pool.FEE_DENOMINATOR:.4f}%")

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
        if isinstance(pool, CurveStableswapPool):
            return self._update_curve_pool(pool, provider=provider, block_number=block_number)
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

    # ── Curve fetcher factories ───────────────────────────────────

    def _make_curve_virtual_price_fetcher(
        self,
        pool_address: ChecksumAddress,
        chain_id: ChainId,
        base_pool_address: ChecksumAddress | None = None,
    ) -> Any:
        """Create a virtual price fetcher closure for a Curve pool.

        For metapools, this calls get_virtual_price() on the base pool's contract.
        For non-metapools, this calls get_virtual_price() on the pool itself.
        """
        target_address = base_pool_address if base_pool_address is not None else pool_address

        def virtual_price_fetcher(block_number: int) -> int:
            w3 = self.connections.get_web3(chain_id)
            (vp,) = cast(
                "tuple[int]",
                eth_abi.abi.decode(
                    types=["uint256"],
                    data=w3.eth.call(
                        {
                            "to": target_address,
                            "data": Web3.keccak(text="get_virtual_price()")[:4],
                        },
                        block_identifier=block_number,
                    ),
                ),
            )
            return vp

        return virtual_price_fetcher

    def _make_curve_base_virtual_price_fetcher(
        self, pool_address: ChecksumAddress, chain_id: ChainId
    ) -> Any:
        """Create a base virtual price fetcher closure for a Curve metapool.

        Calls base_virtual_price() on the metapool contract, which returns the
        virtual price of the base pool LP token.
        """

        def base_virtual_price_fetcher(block_number: int) -> int:
            w3 = self.connections.get_web3(chain_id)
            (vp,) = cast(
                "tuple[int]",
                eth_abi.abi.decode(
                    types=["uint256"],
                    data=w3.eth.call(
                        {
                            "to": pool_address,
                            "data": Web3.keccak(text="base_virtual_price()")[:4],
                        },
                        block_identifier=block_number,
                    ),
                ),
            )
            return vp

        return base_virtual_price_fetcher

    def _make_curve_timestamp_fetcher(self, chain_id: ChainId) -> Any:
        """Create a timestamp fetcher closure for a Curve pool."""

        def timestamp_fetcher(block_number: int) -> int:
            w3 = self.connections.get_web3(chain_id)
            block = w3.eth.get_block(block_identifier=block_number)
            return block["timestamp"]

        return timestamp_fetcher

    def _make_curve_redemption_price_fetcher(
        self, pool_address: ChecksumAddress, chain_id: ChainId
    ) -> Any:
        """Create a redemption price fetcher closure for a Curve pool."""

        def redemption_price_fetcher(block_number: int) -> int:
            w3 = self.connections.get_web3(chain_id)
            redemption_price_scale = 10**9

            (snap_contract_address,) = cast(
                "tuple[str]",
                eth_abi.abi.decode(
                    types=["address"],
                    data=w3.eth.call(
                        {
                            "to": pool_address,
                            "data": Web3.keccak(text="redemption_price_snap()")[:4],
                        },
                        block_identifier=block_number,
                    ),
                ),
            )

            (rate,) = cast(
                "tuple[int]",
                eth_abi.abi.decode(
                    types=["uint256"],
                    data=w3.eth.call(
                        {
                            "to": get_checksum_address(snap_contract_address),
                            "data": Web3.keccak(text="snappedRedemptionPrice()")[:4],
                        },
                        block_identifier=block_number,
                    ),
                ),
            )
            return rate // redemption_price_scale

        return redemption_price_fetcher

    def _make_curve_admin_balances_fetcher(
        self, pool_address: ChecksumAddress, chain_id: ChainId
    ) -> Any:
        """Create an admin balances fetcher closure for a Curve pool."""

        def admin_balances_fetcher(block_number: int) -> tuple[int, ...]:
            provider = self.connections.get_provider(chain_id)
            admin_balances: list[int] = []
            for token_index in range(8):  # max 8 tokens for Curve V1
                try:
                    (admin_balance,) = cast(
                        "tuple[int]",
                        eth_abi.abi.decode(
                            types=["uint256"],
                            data=provider.call(
                                to=pool_address,
                                data=encode_function_calldata(
                                    function_prototype="admin_balances(uint256)",
                                    function_arguments=[token_index],
                                ),
                                block=block_number,
                            ),
                        ),
                    )
                    admin_balances.append(admin_balance)
                except Exception:  # noqa: BLE001
                    break
            return tuple(admin_balances)

        return admin_balances_fetcher

    def _make_curve_block_number_fetcher(self, chain_id: ChainId) -> Any:
        """Create a block number fetcher closure for a Curve pool."""

        def block_number_fetcher() -> int:
            provider = self.connections.get_provider(chain_id)
            return provider.get_block_number()

        return block_number_fetcher

    def _make_curve_total_supply_fetcher(self, chain_id: ChainId) -> Any:
        """Create a total supply fetcher closure for a Curve pool."""

        def total_supply_fetcher(token: Any, *, block_identifier: int | None = None) -> int:
            return self.get_token_total_supply(token, block_identifier=block_identifier)

        return total_supply_fetcher

    def _make_curve_token_balance_fetcher(self, chain_id: ChainId) -> Any:
        """Create a token balance fetcher closure for a Curve pool."""

        def token_balance_fetcher(
            token: Any, address: Any, *, block_identifier: int | None = None
        ) -> int:
            return self.get_token_balance(token, address, block_identifier=block_identifier)

        return token_balance_fetcher

    def _make_curve_provider_call(self, chain_id: ChainId) -> Any:
        """Create a raw provider.call() closure for a Curve pool.

        This is used by pool-type-specific rate fetching methods that need
        low-level contract calls (e.g. cToken exchangeRateStored, oracle_method, etc.).
        """

        def provider_call(*, to: Any, data: Any, block: int) -> bytes:
            w3 = self.connections.get_web3(chain_id)
            return w3.eth.call(
                {"to": to, "data": data},
                block_identifier=block,
            )

        return provider_call

    def _make_curve_D_fetcher(self, chain_id: ChainId, pool_address: ChecksumAddress) -> Any:
        """Create a D() fetcher closure for a crypto Curve pool."""

        def D_fetcher(block_number: int) -> int:

            w3 = self.connections.get_web3(chain_id)
            d: int
            (d,) = eth_abi.abi.decode(
                types=["uint256"],
                data=w3.eth.call(
                    TxParams(
                        to=pool_address,
                        data=Web3.keccak(text="D()")[:4],
                    ),
                    block_identifier=block_number,
                ),
            )
            return d

        return D_fetcher

    def _make_curve_gamma_fetcher(self, chain_id: ChainId, pool_address: ChecksumAddress) -> Any:
        """Create a gamma() fetcher closure for a crypto Curve pool."""

        def gamma_fetcher(block_number: int) -> int:

            w3 = self.connections.get_web3(chain_id)
            gamma: int
            (gamma,) = eth_abi.abi.decode(
                types=["uint256"],
                data=w3.eth.call(
                    TxParams(
                        to=pool_address,
                        data=Web3.keccak(text="gamma()")[:4],
                    ),
                    block_identifier=block_number,
                ),
            )
            return gamma

        return gamma_fetcher

    def _make_curve_price_scale_fetcher(
        self, chain_id: ChainId, pool_address: ChecksumAddress, n_coins: int
    ) -> Any:
        """Create a price_scale() fetcher closure for a crypto Curve pool."""

        def price_scale_fetcher(block_number: int) -> tuple[int, ...]:

            w3 = self.connections.get_web3(chain_id)
            price_scale = [0] * (n_coins - 1)
            for token_index in range(n_coins - 1):
                (price_scale[token_index],) = eth_abi.abi.decode(
                    types=["uint256"],
                    data=w3.eth.call(
                        TxParams(
                            to=pool_address,
                            data=Web3.keccak(text="price_scale(uint256)")[:4]
                            + eth_abi.abi.encode(types=["uint256"], args=[token_index]),
                        ),
                        block_identifier=block_number,
                    ),
                )
            return tuple(price_scale)

        return price_scale_fetcher

    def _update_curve_pool(
        self, pool: Any, *, provider: Any, block_number: BlockIdentifier | None
    ) -> bool:

        assert isinstance(pool, CurveStableswapPool)
        _block_number = block_number if block_number is not None else provider.get_block_number()

        # Fetch balances for each token in the pool
        w3 = self.connections.get_web3(pool.chain_id)
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
