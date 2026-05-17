"""
Tests for Phase 4: I/O-free UniswapV3Pool construction via Bot.
"""

import pathlib
import pickle
from unittest.mock import MagicMock

import eth_abi.abi
from web3 import Web3

from degenbot.bot import Bot
from degenbot.checksum_cache import get_checksum_address
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.erc20.erc20 import Erc20Token
from degenbot.provider.call_helpers import encode_function_calldata
from degenbot.uniswap.trackers import UniswapV3PoolTracker
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.v3_types import (
    UniswapV3PoolExternalUpdate,
)


def _make_test_config(tmp_path: pathlib.Path) -> DegenbotConfig:
    return DegenbotConfig(
        database=DatabaseSettings(path=tmp_path / "test.db"),
        rpc={1: "https://eth.llamarpc.com/"},
    )


def _make_weth() -> Erc20Token:
    return Erc20Token(
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        chain_id=1,
        name="Wrapped Ether",
        symbol="WETH",
        decimals=18,
    )


def _make_usdc() -> Erc20Token:
    return Erc20Token(
        "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        chain_id=1,
        name="USD Coin",
        symbol="USDC",
        decimals=6,
    )


# USDC/WETH 0.3% pool on Ethereum mainnet
USDC_WETH_V3_POOL = "0x8ad599c3a0ff1de082011efddc58f1908eb6e6d8"
UNISWAP_V3_FACTORY = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
V3_FEE = 3000  # 0.3%
V3_TICK_SPACING = 60


class TestV3PoolIOFreeConstructor:
    """UniswapV3Pool can be constructed with pre-fetched data only."""

    def test_io_free_constructor_basic(self) -> None:
        """An I/O-free V3 pool can be constructed with all pre-fetched data."""
        weth = _make_weth()
        usdc = _make_usdc()

        pool = UniswapV3Pool(
            address=USDC_WETH_V3_POOL,
            chain_id=1,
            token0=weth,
            token1=usdc,
            factory=UNISWAP_V3_FACTORY,
            fee=V3_FEE,
            tick_spacing=V3_TICK_SPACING,
            sqrt_price_x96=2198666895605149686863,  # ~2000 USDC per WETH
            tick=-76020,
            liquidity=1234567890,
            state_block=18_000_000,
        )

        assert pool.address == get_checksum_address(USDC_WETH_V3_POOL)
        assert pool.chain_id == 1
        assert pool.token0 is weth
        assert pool.token1 is usdc
        assert pool.factory == get_checksum_address(UNISWAP_V3_FACTORY)
        assert pool.fee == V3_FEE
        assert pool.tick_spacing == V3_TICK_SPACING
        assert pool.sqrt_price_x96 == 2198666895605149686863
        assert pool.tick == -76020
        assert pool.liquidity == 1234567890
        assert pool.update_block == 18_000_000

    def test_io_free_constructor_with_tick_data(self) -> None:
        """I/O-free constructor accepts pre-fetched tick_bitmap and tick_data."""
        weth = _make_weth()
        usdc = _make_usdc()

        tick_data = {
            -76080: LiquidityAtTick(
                liquidity_gross=1000,
                liquidity_net=-500,
                block=18_000_000,
            ),
            -75960: LiquidityAtTick(
                liquidity_gross=2000,
                liquidity_net=300,
                block=18_000_000,
            ),
        }
        tick_bitmap = {
            -297: BitmapAtWord(bitmap=3, block=18_000_000),
        }

        pool = UniswapV3Pool(
            address=USDC_WETH_V3_POOL,
            chain_id=1,
            token0=weth,
            token1=usdc,
            factory=UNISWAP_V3_FACTORY,
            fee=V3_FEE,
            tick_spacing=V3_TICK_SPACING,
            sqrt_price_x96=2198666895605149686863,
            tick=-76020,
            liquidity=1234567890,
            state_block=18_000_000,
            tick_bitmap=tick_bitmap,
            tick_data=tick_data,
        )

        # Pool should have a non-sparse liquidity map
        assert pool.sparse_liquidity_map is False
        assert -76080 in pool.tick_data
        assert -75960 in pool.tick_data

    def test_io_free_pool_external_update(self) -> None:
        """external_update works on an I/O-free pool."""
        weth = _make_weth()
        usdc = _make_usdc()

        pool = UniswapV3Pool(
            address=USDC_WETH_V3_POOL,
            chain_id=1,
            token0=weth,
            token1=usdc,
            factory=UNISWAP_V3_FACTORY,
            fee=V3_FEE,
            tick_spacing=V3_TICK_SPACING,
            sqrt_price_x96=2198666895605149686863,
            tick=-76020,
            liquidity=1234567890,
            state_block=18_000_000,
        )

        # Apply external update
        updated = pool.external_update(
            UniswapV3PoolExternalUpdate(
                block_number=18_000_001,
                sqrt_price_x96=2200000000000000000000,
                tick=-75900,
                liquidity=9999999999,
            )
        )
        assert updated is True
        assert pool.tick == -75900
        assert pool.liquidity == 9999999999

    def test_io_free_pool_pickle(self) -> None:
        """I/O-free pools can be pickled and unpickled."""

        weth = _make_weth()
        usdc = _make_usdc()

        pool = UniswapV3Pool(
            address=USDC_WETH_V3_POOL,
            chain_id=1,
            token0=weth,
            token1=usdc,
            factory=UNISWAP_V3_FACTORY,
            fee=V3_FEE,
            tick_spacing=V3_TICK_SPACING,
            sqrt_price_x96=2198666895605149686863,
            tick=-76020,
            liquidity=1234567890,
            state_block=18_000_000,
        )

        pickled = pickle.dumps(pool)
        unpickled = pickle.loads(pickled)

        assert unpickled.address == pool.address
        assert unpickled.sqrt_price_x96 == pool.sqrt_price_x96
        assert unpickled.tick == pool.tick
        assert unpickled.liquidity == pool.liquidity


class TestBotBuildV3Pool:
    """Bot.build_pool() constructs I/O-free V3 pools from on-chain data."""

    def test_build_pool_with_mock_provider(self, tmp_path: pathlib.Path) -> None:
        """build_pool fetches immutable + mutable values and constructs an I/O-free pool."""

        weth_addr = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
        usdc_addr = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
        factory_addr = "0x1F98431c8aD98523631AE4a59f267346ea31F984"

        config = _make_test_config(tmp_path)
        bot = Bot(config)

        provider = MagicMock()
        provider.chain_id = 1
        provider.is_connected.return_value = True
        provider.get_block_number.return_value = 18_000_000
        bot.connections.register_provider(provider)
        bot.connections.set_default_chain(1)

        # Pre-register tokens
        weth = _make_weth()
        usdc = _make_usdc()
        bot.tokens.add(token_address=weth_addr, chain_id=1, token=weth)
        bot.tokens.add(token_address=usdc_addr, chain_id=1, token=usdc)

        # Mock RPC responses
        factory_calldata = encode_function_calldata("factory()", None)
        token0_calldata = encode_function_calldata("token0()", None)
        token1_calldata = encode_function_calldata("token1()", None)
        fee_calldata = encode_function_calldata("fee()", None)
        tick_spacing_calldata = encode_function_calldata("tickSpacing()", None)
        slot0_calldata = encode_function_calldata("slot0()", None)
        liquidity_calldata = encode_function_calldata("liquidity()", None)

        sqrt_price = 2198666895605149686863
        tick = -76020
        liquidity = 1234567890

        slot0_encoded = eth_abi.abi.encode(
            types=["uint160", "int24", "uint16", "uint16", "uint16", "uint8", "bool"],
            args=[sqrt_price, tick, 0, 0, 0, 0, False],
        )
        liquidity_encoded = eth_abi.abi.encode(types=["uint128"], args=[liquidity])

        immutable_responses = {
            factory_calldata: eth_abi.abi.encode(types=["address"], args=[factory_addr]),
            token0_calldata: eth_abi.abi.encode(types=["address"], args=[weth_addr]),
            token1_calldata: eth_abi.abi.encode(types=["address"], args=[usdc_addr]),
            fee_calldata: eth_abi.abi.encode(types=["uint24"], args=[V3_FEE]),
            tick_spacing_calldata: eth_abi.abi.encode(types=["int24"], args=[V3_TICK_SPACING]),
        }

        # tickBitmap(int16) selector = first 4 bytes of keccak256("tickBitmap(int16)")
        tick_bitmap_selector = Web3.keccak(text="tickBitmap(int16)")[:4]

        def mock_call(*, to, data, block=None):
            if data in immutable_responses:
                return immutable_responses[data]
            if data == slot0_calldata:
                return slot0_encoded
            if data == liquidity_calldata:
                return liquidity_encoded
            # tickBitmap returns 0 (no initialized ticks)
            if data[:4] == tick_bitmap_selector:
                return eth_abi.abi.encode(types=["uint256"], args=[0])
            msg = f"Unexpected call with data={data!r}"
            raise ValueError(msg)

        provider.call = MagicMock(side_effect=mock_call)

        pool = bot.build_pool(
            USDC_WETH_V3_POOL,
            chain_id=1,
        )

        assert isinstance(pool, UniswapV3Pool)
        assert pool.address == get_checksum_address(USDC_WETH_V3_POOL)
        assert pool.token0.address == get_checksum_address(weth_addr)
        assert pool.token1.address == get_checksum_address(usdc_addr)
        assert pool.factory == get_checksum_address(factory_addr)
        assert pool.fee == V3_FEE
        assert pool.tick_spacing == V3_TICK_SPACING
        assert pool.sqrt_price_x96 == sqrt_price
        assert pool.tick == tick
        assert pool.liquidity == liquidity

        # Pool should be registered in bot's pool registry
        assert bot.pools.get(pool_address=pool.address, chain_id=1) is pool


class TestV3PoolTrackerWithBot:
    """UniswapV3PoolTracker delegates to Bot when available."""

    def test_tracker_uses_bot_pools_registry(self, tmp_path: pathlib.Path) -> None:
        """When a manager has a bot, get_pool checks bot.pools first."""

        config = _make_test_config(tmp_path)
        bot = Bot(config)

        provider = MagicMock()
        provider.chain_id = 1
        provider.is_connected.return_value = True
        provider.get_block_number.return_value = 18_000_000
        bot.connections.register_provider(provider)
        bot.connections.set_default_chain(1)

        factory = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
        manager = bot.add_tracker(
            UniswapV3PoolTracker,
            factory_address=factory,
            chain_id=1,
        )

        assert manager._bot is bot

        # Create an I/O-free pool and register in bot.pools
        weth = _make_weth()
        usdc = _make_usdc()
        mock_pool = UniswapV3Pool(
            address=USDC_WETH_V3_POOL,
            chain_id=1,
            token0=weth,
            token1=usdc,
            factory=factory,
            fee=V3_FEE,
            tick_spacing=V3_TICK_SPACING,
            sqrt_price_x96=2198666895605149686863,
            tick=-76020,
            liquidity=1234567890,
            state_block=18_000_000,
        )
        bot.pools.add(pool_address=mock_pool.address, chain_id=1, pool=mock_pool)

        # Manager should find the pool in bot.pools and return it
        pool = manager.get_pool(USDC_WETH_V3_POOL)
        assert pool is mock_pool
