"""Tests for Phase 5: I/O-free UniswapV4Pool construction via Bot."""

import pathlib
import pickle
from unittest.mock import MagicMock

import eth_abi.abi
from hexbytes import HexBytes
from web3 import Web3

from degenbot.bot import Bot
from degenbot.checksum_cache import get_checksum_address
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.constants import ZERO_ADDRESS
from degenbot.degenbot_rs import PyBot
from degenbot.erc20.erc20 import Erc20Token
from degenbot.provider.call_helpers import encode_function_calldata
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool
from degenbot.uniswap.v4_types import (
    UniswapV4PoolExternalUpdate,
    UniswapV4PoolKey,
)
from tests.helpers.erc20_factory import make_erc20

_PY_BOT = PyBot()


def _make_test_config(tmp_path: pathlib.Path) -> DegenbotConfig:
    return DegenbotConfig(
        database=DatabaseSettings(path=tmp_path / "test.db"),
        rpc={1: "https://eth.llamarpc.com/"},
    )


def _make_native_eth() -> Erc20Token:
    """Create a token representing native ETH (address 0x0...0)."""
    return make_erc20(
        _PY_BOT,
        ZERO_ADDRESS,
        chain_id=1,
        name="Ether",
        symbol="ETH",
        decimals=18,
    )


def _make_usdc() -> Erc20Token:
    return make_erc20(
        _PY_BOT,
        "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        chain_id=1,
        name="USD Coin",
        symbol="USDC",
        decimals=6,
    )


# ETH/USDC V4 pool on Ethereum mainnet
V4_POOL_MANAGER = "0x000000000004444c5dc75cB358380D2e3dE08A90"
V4_STATE_VIEW = "0x7fFE42C4a5DEeA5b0feC41C94C136Cf115597227"
V4_POOL_ID = "0x21c67e77068de97969ba93d4aab21826d33ca12bb9f565d8496e8fda8a82ca27"
V4_FEE = 500
V4_TICK_SPACING = 10
V4_HOOKS = ZERO_ADDRESS


class TestV4PoolIOFreeConstructor:
    """UniswapV4Pool can be constructed with pre-fetched data only."""

    def test_io_free_constructor_basic(self) -> None:
        """I/O-free constructor sets all attributes from pre-fetched data."""
        native_eth = _make_native_eth()
        usdc = _make_usdc()

        sqrt_price = 2198666895605149686863
        tick = -76020
        liquidity = 1234567890

        # protocol_fee and lp_fee are additional V4 state
        protocol_fee_zero_for_one = 100
        protocol_fee_one_for_zero = 200
        lp_fee = 500000

        pool = UniswapV4Pool(
            pool_id=V4_POOL_ID,
            pool_manager_address=V4_POOL_MANAGER,
            token0=native_eth,
            token1=usdc,
            fee=V4_FEE,
            tick_spacing=V4_TICK_SPACING,
            hook_address=V4_HOOKS,
            sqrt_price_x96=sqrt_price,
            tick=tick,
            liquidity=liquidity,
            protocol_fee_zero_for_one=protocol_fee_zero_for_one,
            protocol_fee_one_for_zero=protocol_fee_one_for_zero,
            lp_fee=lp_fee,
            state_block=18_000_000,
        )

        assert pool.address == get_checksum_address(V4_POOL_MANAGER)
        assert pool.pool_id == HexBytes(V4_POOL_ID)
        assert pool.token0 == native_eth
        assert pool.token1 == usdc
        assert pool.fee == V4_FEE
        assert pool.tick_spacing == V4_TICK_SPACING
        assert pool.sqrt_price_x96 == sqrt_price
        assert pool.tick == tick
        assert pool.liquidity == liquidity
        assert pool.protocol_fee.zero_for_one == protocol_fee_zero_for_one
        assert pool.protocol_fee.one_for_zero == protocol_fee_one_for_zero
        assert pool.lp_fee == lp_fee

    def test_io_free_with_tick_data(self) -> None:
        """I/O-free pool with tick data is not sparse."""
        native_eth = _make_native_eth()
        usdc = _make_usdc()

        tick_bitmap = {0: BitmapAtWord(bitmap=1, block=18_000_000)}
        tick_data = {
            -10: LiquidityAtTick(liquidity_net=100, liquidity_gross=200, block=18_000_000),
        }

        pool = UniswapV4Pool(
            pool_id=V4_POOL_ID,
            pool_manager_address=V4_POOL_MANAGER,
            token0=native_eth,
            token1=usdc,
            fee=V4_FEE,
            tick_spacing=V4_TICK_SPACING,
            hook_address=V4_HOOKS,
            sqrt_price_x96=2198666895605149686863,
            tick=-76020,
            liquidity=1234567890,
            protocol_fee_zero_for_one=0,
            protocol_fee_one_for_zero=0,
            lp_fee=500000,
            state_block=18_000_000,
            tick_bitmap=tick_bitmap,
            tick_data=tick_data,
        )

        assert pool.sparse_liquidity_map is False
        assert pool.tick_bitmap[0].bitmap == 1
        assert pool.tick_data[-10].liquidity_net == 100

    def test_io_free_external_update(self) -> None:
        """I/O-free pool accepts external updates."""
        native_eth = _make_native_eth()
        usdc = _make_usdc()

        pool = UniswapV4Pool(
            pool_id=V4_POOL_ID,
            pool_manager_address=V4_POOL_MANAGER,
            token0=native_eth,
            token1=usdc,
            fee=V4_FEE,
            tick_spacing=V4_TICK_SPACING,
            hook_address=V4_HOOKS,
            sqrt_price_x96=2198666895605149686863,
            tick=-76020,
            liquidity=1234567890,
            protocol_fee_zero_for_one=0,
            protocol_fee_one_for_zero=0,
            lp_fee=500000,
            state_block=18_000_000,
        )

        update = UniswapV4PoolExternalUpdate(
            block_number=18_000_001,
            sqrt_price_x96=2198666895605150000000,
            tick=-76010,
            liquidity=1234567999,
        )
        assert pool.external_update(update) is True
        assert pool.tick == -76010
        assert pool.liquidity == 1234567999

    def test_io_free_pickle(self) -> None:
        """I/O-free pool can be pickled and unpickled."""
        native_eth = _make_native_eth()
        usdc = _make_usdc()

        pool = UniswapV4Pool(
            pool_id=V4_POOL_ID,
            pool_manager_address=V4_POOL_MANAGER,
            token0=native_eth,
            token1=usdc,
            fee=V4_FEE,
            tick_spacing=V4_TICK_SPACING,
            hook_address=V4_HOOKS,
            sqrt_price_x96=2198666895605149686863,
            tick=-76020,
            liquidity=1234567890,
            protocol_fee_zero_for_one=0,
            protocol_fee_one_for_zero=0,
            lp_fee=500000,
            state_block=18_000_000,
        )

        data = pickle.dumps(pool)
        unpickled = pickle.loads(data)
        assert unpickled.pool_id == pool.pool_id
        assert unpickled.tick == pool.tick
        assert unpickled.liquidity == pool.liquidity
        assert unpickled.fee == pool.fee
        assert unpickled.tick_spacing == pool.tick_spacing

    def test_io_free_pool_key_and_hooks(self) -> None:
        """I/O-free pool correctly sets pool_key, hook_address, and active_hooks."""
        native_eth = _make_native_eth()
        usdc = _make_usdc()

        pool = UniswapV4Pool(
            pool_id=V4_POOL_ID,
            pool_manager_address=V4_POOL_MANAGER,
            token0=native_eth,
            token1=usdc,
            fee=V4_FEE,
            tick_spacing=V4_TICK_SPACING,
            hook_address=V4_HOOKS,
            sqrt_price_x96=2198666895605149686863,
            tick=-76020,
            liquidity=1234567890,
            protocol_fee_zero_for_one=0,
            protocol_fee_one_for_zero=0,
            lp_fee=500000,
            state_block=18_000_000,
        )

        assert pool.pool_key == UniswapV4PoolKey(
            currency0=native_eth.address,
            currency1=usdc.address,
            fee=V4_FEE,
            tick_spacing=V4_TICK_SPACING,
            hooks=V4_HOOKS,
        )
        assert pool.hook_address == ZERO_ADDRESS
        assert pool.active_hooks == frozenset()

    def test_io_free_state_view_address(self) -> None:
        """I/O-free pool stores state_view_address when provided."""
        native_eth = _make_native_eth()
        usdc = _make_usdc()

        state_view = get_checksum_address(V4_STATE_VIEW)

        pool = UniswapV4Pool(
            pool_id=V4_POOL_ID,
            pool_manager_address=V4_POOL_MANAGER,
            token0=native_eth,
            token1=usdc,
            fee=V4_FEE,
            tick_spacing=V4_TICK_SPACING,
            hook_address=V4_HOOKS,
            state_view_address=state_view,
            sqrt_price_x96=2198666895605149686863,
            tick=-76020,
            liquidity=1234567890,
            protocol_fee_zero_for_one=0,
            protocol_fee_one_for_zero=0,
            lp_fee=500000,
            state_block=18_000_000,
        )

        assert pool._state_view_address == state_view


class TestBotBuildV4Pool:
    """Bot.build_pool() constructs I/O-free V4 pools from on-chain data."""

    def test_build_pool_with_mock_provider(self, tmp_path: pathlib.Path) -> None:
        """build_pool fetches immutable + mutable values and constructs an I/O-free pool."""
        config = _make_test_config(tmp_path)
        bot = Bot(config)

        provider = MagicMock()
        provider.chain_id = 1
        provider.is_connected.return_value = True
        provider.get_block_number.return_value = 18_000_000
        bot.connections.register_provider(provider)
        bot.connections.set_default_chain(1)

        # Pre-register tokens
        native_eth = _make_native_eth()
        usdc = _make_usdc()
        bot.tokens.add(token_address=ZERO_ADDRESS, chain_id=1, token=native_eth)
        bot.tokens.add(
            token_address=get_checksum_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            chain_id=1,
            token=usdc,
        )

        # Mock RPC responses
        sqrt_price = 2198666895605149686863
        tick = -76020
        protocol_fee = 0  # packed as (zero_for_one << 12) | one_for_zero
        lp_fee = 500000
        liquidity = 1234567890

        slot0_encoded = eth_abi.abi.encode(
            types=["uint160", "int24", "uint24", "uint24"],
            args=[sqrt_price, tick, protocol_fee, lp_fee],
        )
        liquidity_encoded = eth_abi.abi.encode(types=["uint256"], args=[liquidity])

        # Build call matchers for getSlot0(bytes32) and getLiquidity(bytes32)
        slot0_calldata = encode_function_calldata("getSlot0(bytes32)", [HexBytes(V4_POOL_ID)])
        liquidity_calldata = encode_function_calldata(
            "getLiquidity(bytes32)", [HexBytes(V4_POOL_ID)]
        )

        # getTickBitmap(bytes32,int16) selector

        tick_bitmap_selector = Web3.keccak(text="getTickBitmap(bytes32,int16)")[:4]

        def mock_call(*, to, data, block=None):
            if data == slot0_calldata:
                return slot0_encoded
            if data == liquidity_calldata:
                return liquidity_encoded
            if data[:4] == tick_bitmap_selector:
                return eth_abi.abi.encode(types=["uint256"], args=[0])
            msg = f"Unexpected call with data={data!r}"
            raise ValueError(msg)

        provider.call = MagicMock(side_effect=mock_call)

        pool = bot.build_managed_pool(
            V4_POOL_MANAGER,
            V4_POOL_ID,
            state_view_address=V4_STATE_VIEW,
            chain_id=1,
            fee=V4_FEE,
            tick_spacing=V4_TICK_SPACING,
            hook_address=V4_HOOKS,
            tokens=[ZERO_ADDRESS, "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"],
        )

        assert isinstance(pool, UniswapV4Pool)
        assert pool.pool_id == HexBytes(V4_POOL_ID)
        assert pool.address == get_checksum_address(V4_POOL_MANAGER)
        assert pool.fee == V4_FEE
        assert pool.tick_spacing == V4_TICK_SPACING
        assert pool.sqrt_price_x96 == sqrt_price
        assert pool.tick == tick
        assert pool.liquidity == liquidity
        assert pool.lp_fee == lp_fee

        # Pool should be registered in bot's managed pool registry
        found = bot.managed_pools.get(
            chain_id=1,
            pool_manager_address=V4_POOL_MANAGER,
            pool_id=HexBytes(V4_POOL_ID),
        )
        assert found is pool
