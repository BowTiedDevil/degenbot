"""Tests for Phase 4: I/O-free UniswapV3Pool construction via Bot."""

import pathlib
from unittest.mock import MagicMock

from degenbot._ffi import Bot as _Engine
from degenbot.abi import encode as abi_encode
from degenbot.bot import Bot
from degenbot.checksum_cache import get_checksum_address
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.erc20.erc20 import Erc20Token
from degenbot.provider import OfflineProvider
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.trackers import UniswapV3PoolTracker
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v3_types import (
    UniswapV3PoolExternalUpdate,
)
from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v3_pool_factory import make_v3_pool

_PY_BOT = _Engine()


def _make_test_config(tmp_path: pathlib.Path) -> DegenbotConfig:
    return DegenbotConfig(
        database=DatabaseSettings(path=tmp_path / "test.db"),
        rpc={1: ETHEREUM_ARCHIVE_NODE_HTTP_URI},
        default_chain_id=1,
    )


def _make_weth() -> Erc20Token:
    return make_erc20(
        _PY_BOT,
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        chain_id=1,
        name="Wrapped Ether",
        symbol="WETH",
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


# USDC/WETH 0.3% pool on Ethereum mainnet
USDC_WETH_V3_POOL = "0x8ad599c3a0ff1de082011efddc58f1908eb6e6d8"
UNISWAP_V3_FACTORY = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
V3_FEE = 3000  # 0.3%
V3_TICK_SPACING = 60


def _v3_offline_provider(
    *,
    weth_addr: str,
    usdc_addr: str,
    factory_addr: str,
    pool_addr: str,
    sqrt_price: int,
    tick: int,
    liquidity: int,
    block: int = 18_000_000,
) -> OfflineProvider:
    """A one-block `OfflineProvider` serving the V3 build RPC responses.

    T4/6ZGF4V: the Rust `PoolBuilder` choreography is alloy-only, so the build
    must be served from recorded offline data (no Python mock double). The
    recorded calls key on the alloy transport's exact-calldata format
    (`{addr}:0x{data}`): `factory()`/`token0()`/`token1()`/`fee()`/`tickSpacing()`/
    `slot0()`/`liquidity()` plus a `tickBitmap(int16)` read at the single
    sparse seed word (empty → no `ticks()` calls).

    The seed word is computed with the core's `get_tick_word_and_bit_position`
    (tick ÷ spacing, then word = `compressed >> 8`, bit = `% 256` non-negative)
    so the recorded `tickBitmap` calldata exactly matches the builder's
    request at the current tick.
    """
    factory_enc = abi_encode(types=["address"], args=[factory_addr]).hex()
    token0_enc = abi_encode(types=["address"], args=[weth_addr]).hex()
    token1_enc = abi_encode(types=["address"], args=[usdc_addr]).hex()
    fee_enc = abi_encode(types=["uint24"], args=[V3_FEE]).hex()
    spacing_enc = abi_encode(types=["int24"], args=[V3_TICK_SPACING]).hex()
    slot0_enc = abi_encode(
        types=["uint160", "int24", "uint16", "uint16", "uint16", "uint8", "bool"],
        args=[sqrt_price, tick, 0, 0, 0, 0, False],
    ).hex()
    liquidity_enc = abi_encode(types=["uint128"], args=[liquidity]).hex()
    to = pool_addr.lower()
    # Sparse seed word for the current tick (tick ÷ spacing, word = >> 8).
    compressed = tick // V3_TICK_SPACING
    word = compressed >> 8
    tick_bitmap_arg = word.to_bytes(32, "big", signed=True).hex()
    calls = {
        f"{to}:0xc45a0155": factory_enc,  # factory()
        f"{to}:0x0dfe1681": token0_enc,  # token0()
        f"{to}:0xd21220a7": token1_enc,  # token1()
        f"{to}:0xddca3f43": fee_enc,  # fee()
        f"{to}:0xd0c93a7c": spacing_enc,  # tickSpacing()
        f"{to}:0x3850c7bd": slot0_enc,  # slot0()
        f"{to}:0x1a686502": liquidity_enc,  # liquidity()
        f"{to}:0x5339c296{tick_bitmap_arg}": abi_encode(
            types=["uint256"], args=[0]
        ).hex(),  # tickBitmap(current_word) → empty
    }
    return OfflineProvider(
        chain_id=1,
        blocks={str(block): {"timestamp": 1_700_000_000, "calls": calls, "code": {}}},
    )


class TestV3PoolIOFreeConstructor:
    """UniswapV3Pool can be constructed with pre-fetched data only."""

    def test_io_free_constructor_basic(self) -> None:
        """An I/O-free V3 pool can be constructed with all pre-fetched data."""
        weth = _make_weth()
        usdc = _make_usdc()

        pool = make_v3_pool(
            address=USDC_WETH_V3_POOL,
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
        assert pool.token0.address == weth.address
        assert pool.token1.address == usdc.address
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

        pool = make_v3_pool(
            address=USDC_WETH_V3_POOL,
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

        pool = make_v3_pool(
            address=USDC_WETH_V3_POOL,
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


class TestBotBuildV3Pool:
    """Bot.build_pool() constructs I/O-free V3 pools from on-chain data."""

    def test_build_pool_with_mock_provider(self, tmp_path: pathlib.Path) -> None:
        """build_pool fetches immutable + mutable values and constructs an I/O-free pool."""
        weth_addr = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
        usdc_addr = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
        factory_addr = "0x1F98431c8aD98523631AE4a59f267346ea31F984"

        sqrt_price = 2198666895605149686863
        tick = -76020
        liquidity = 1234567890

        config = _make_test_config(tmp_path)
        bot = Bot(
            config,
            provider=_v3_offline_provider(
                weth_addr=weth_addr,
                usdc_addr=usdc_addr,
                factory_addr=factory_addr,
                pool_addr=USDC_WETH_V3_POOL,
                sqrt_price=sqrt_price,
                tick=tick,
                liquidity=liquidity,
            ),
        )

        # Pre-register tokens (ADR-006: tokens must be in the same _Engine as the
        # pool — _from_py_pool recovers them via py_pool.get_token0/get_token1).
        weth = _make_weth()
        usdc = _make_usdc()
        for tok in (weth, usdc):
            if bot._py_bot.get_token(tok.address) is None:
                bot._py_bot.register_token(tok.address, tok.name, tok.symbol, tok.decimals, 1)
        bot.tokens.add(token_address=weth_addr, chain_id=1, token=weth)
        bot.tokens.add(token_address=usdc_addr, chain_id=1, token=usdc)

        pool = bot.build_pool(
            USDC_WETH_V3_POOL,
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
        provider = MagicMock()
        provider.chain_id = 1
        provider.is_connected.return_value = True
        provider.get_block_number.return_value = 18_000_000
        bot = Bot(config, provider=provider)

        factory = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
        manager = bot.add_tracker(
            UniswapV3PoolTracker,
            factory_address=factory,
        )

        assert manager._bot is bot

        # Create an I/O-free pool and register in bot.pools
        weth = _make_weth()
        usdc = _make_usdc()
        mock_pool = make_v3_pool(
            address=USDC_WETH_V3_POOL,
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
