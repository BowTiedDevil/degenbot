"""Tests for Phase 3: I/O-free UniswapV2Pool construction via Bot."""

import pathlib
from fractions import Fraction
from unittest.mock import MagicMock

import eth_abi.abi
import pytest

from degenbot._ffi import Bot as _Engine
from degenbot.bot import Bot
from degenbot.checksum_cache import get_checksum_address
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.erc20.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import InvalidSwapInputAmount, LiquidityPoolError
from degenbot.provider import OfflineProvider
from degenbot.uniswap.trackers import UniswapV2PoolTracker
from degenbot.uniswap.v2_functions import (
    constant_product_calc_exact_in,
    constant_product_calc_exact_out,
)
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v2_types import UniswapV2PoolExternalUpdate, UniswapV2PoolState
from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool

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


WETH_USDC_V2_POOL = "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"
UNISWAP_V2_FACTORY = "0x5C69bEe701ef814E44274f655e7632cB715C14B6"


def _v2_offline_provider(
    *,
    weth_addr: str,
    usdc_addr: str,
    factory_addr: str,
    pool_addr: str,
    block: int = 18_000_000,
) -> OfflineProvider:
    """A one-block `OfflineProvider` serving the V2 build RPC responses.

    T4/6ZGF4V: the Rust `PoolBuilder` choreography is alloy-only, so the pool
    build + type-resolution probing must be served from recorded offline data
    (no Python mock double). The recorded calls mirror the alloy transport's
    exact-calldata keying (`{addr}:0x{data}`): `factory()`/`token0()`/`token1()`/
    `getReserves()` for the pool (with `slot0()` recorded as a redirect/revert
    so `probe_pool_type` resolves V2, not V3).
    """
    factory_enc = eth_abi.abi.encode(types=["address"], args=[factory_addr]).hex()
    token0_enc = eth_abi.abi.encode(types=["address"], args=[weth_addr]).hex()
    token1_enc = eth_abi.abi.encode(types=["address"], args=[usdc_addr]).hex()
    reserves_enc = eth_abi.abi.encode(
        types=["uint112", "uint112", "uint32"], args=[1000 * 10**18, 2_000_000 * 10**6, 0]
    ).hex()
    to = pool_addr.lower()
    calls = {
        f"{to}:0x0902f1ac": reserves_enc,  # getReserves()
        f"{to}:0xc45a0155": factory_enc,  # factory()
        f"{to}:0x0dfe1681": token0_enc,  # token0()
        f"{to}:0xd21220a7": token1_enc,  # token1()
        f"{to}:0x3850c7bd": None,  # slot0() reverts on a V2 pool
    }
    return OfflineProvider(
        chain_id=1,
        blocks={str(block): {"timestamp": 1_700_000_000, "calls": calls, "code": {}}},
    )


class TestV2PoolIOFreeConstructor:
    """UniswapV2Pool can be constructed with pre-fetched data only."""

    def test_io_free_constructor_basic(self) -> None:
        """An I/O-free V2 pool can be constructed with tokens, factory, fees, and reserves."""
        weth = _make_weth()
        usdc = _make_usdc()

        pool = make_v2_pool(
            address=WETH_USDC_V2_POOL,
            chain_id=1,
            token0=weth,
            token1=usdc,
            factory=UNISWAP_V2_FACTORY,
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1000 * 10**18,
            reserves_token1=2_000_000 * 10**6,
            state_block=18_000_000,
        )

        assert pool.address == WETH_USDC_V2_POOL
        assert pool.token0.address == weth.address
        assert pool.token1.address == usdc.address
        assert pool.factory == get_checksum_address(UNISWAP_V2_FACTORY)
        assert pool.fee_token0 == Fraction(3, 1000)
        assert pool.fee_token1 == Fraction(3, 1000)
        assert pool.reserves_token0 == 1000 * 10**18
        assert pool.reserves_token1 == 2_000_000 * 10**6
        assert pool.update_block == 18_000_000

    def test_io_free_pool_computation_works(self) -> None:
        """Simulations and calculations work on an I/O-free pool."""
        weth = _make_weth()
        usdc = _make_usdc()

        pool = make_v2_pool(
            address=WETH_USDC_V2_POOL,
            chain_id=1,
            token0=weth,
            token1=usdc,
            factory=UNISWAP_V2_FACTORY,
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1000 * 10**18,
            reserves_token1=2_000_000 * 10**6,
            state_block=18_000_000,
        )

        # external_update works
        pool.external_update(
            UniswapV2PoolExternalUpdate(
                block_number=18_000_001,
                reserves_token0=1100 * 10**18,
                reserves_token1=1_800_000 * 10**6,
            )
        )
        assert pool.reserves_token0 == 1100 * 10**18

    def test_io_free_constructor_with_split_fees(self) -> None:
        """I/O-free constructor supports split-fee pools."""
        weth = _make_weth()
        usdc = _make_usdc()

        pool = make_v2_pool(
            address=WETH_USDC_V2_POOL,
            chain_id=1,
            token0=weth,
            token1=usdc,
            factory=UNISWAP_V2_FACTORY,
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(2, 1000),
            reserves_token0=1000 * 10**18,
            reserves_token1=2_000_000 * 10**6,
            state_block=18_000_000,
        )

        assert pool.fee_token0 == Fraction(3, 1000)
        assert pool.fee_token1 == Fraction(2, 1000)


class TestBotBuildV2Pool:
    """Bot.build_pool() constructs I/O-free pools from on-chain data."""

    def test_build_pool_with_mock_provider(self, tmp_path: pathlib.Path) -> None:
        """build_pool fetches immutable values and reserves, constructs an I/O-free pool."""
        weth_addr = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
        usdc_addr = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
        factory_addr = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"

        config = _make_test_config(tmp_path)
        bot = Bot(
            config,
            provider=_v2_offline_provider(
                weth_addr=weth_addr,
                usdc_addr=usdc_addr,
                factory_addr=factory_addr,
                pool_addr=WETH_USDC_V2_POOL,
            ),
        )

        # Pre-register tokens so build_erc20token doesn't need RPC
        weth = make_erc20(
            bot._py_bot, weth_addr, chain_id=1, name="Wrapped Ether", symbol="WETH", decimals=18
        )
        usdc = make_erc20(
            bot._py_bot, usdc_addr, chain_id=1, name="USD Coin", symbol="USDC", decimals=6
        )
        bot.tokens.add(token_address=weth_addr, chain_id=1, token=weth)
        bot.tokens.add(token_address=usdc_addr, chain_id=1, token=usdc)

        pool = bot.build_pool(
            WETH_USDC_V2_POOL,
        )

        assert isinstance(pool, UniswapV2Pool)
        assert pool.address == get_checksum_address(WETH_USDC_V2_POOL)
        assert pool.token0.address == get_checksum_address(weth_addr)
        assert pool.token1.address == get_checksum_address(usdc_addr)
        assert pool.factory == get_checksum_address(factory_addr)
        assert pool.reserves_token0 == 1000 * 10**18
        assert pool.reserves_token1 == 2_000_000 * 10**6

        # Pool should be registered in bot's pool registry
        assert bot.pools.get(pool_address=pool.address, chain_id=1) is pool


class TestV2PoolTrackerWithBot:
    """UniswapV2PoolTracker delegates to Bot when available."""

    def test_tracker_uses_bot_build_pool(self, tmp_path: pathlib.Path) -> None:
        """When a manager has a bot, get_pool delegates to bot.build_pool."""
        config = _make_test_config(tmp_path)
        provider = MagicMock()
        provider.chain_id = 1
        provider.is_connected.return_value = True
        provider.get_block_number.return_value = 18_000_000
        bot = Bot(config, provider=provider)

        factory = "0x5C69bEe701ef814E44274f655e7632cB715C14B6"
        manager = bot.add_tracker(
            UniswapV2PoolTracker,
            factory_address=factory,
        )

        # The tracker's bot reference should be set
        assert manager._bot is bot

        # Calling get_pool should call bot.build_pool under the hood
        # We'll verify this by mocking bot.build_pool
        weth = _make_weth()
        usdc = _make_usdc()
        mock_pool = make_v2_pool(
            address=WETH_USDC_V2_POOL,
            chain_id=1,
            token0=weth,
            token1=usdc,
            factory=factory,
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1000 * 10**18,
            reserves_token1=2_000_000 * 10**6,
            state_block=18_000_000,
        )

        # Register in bot.pools so manager can find it after build
        bot.pools.add(pool_address=mock_pool.address, chain_id=1, pool=mock_pool)

        # Since the pool is already in bot.pools, manager should return it
        pool = manager.get_pool(WETH_USDC_V2_POOL)
        assert pool is mock_pool
        assert pool.address == get_checksum_address(WETH_USDC_V2_POOL)

    def test_manager_builds_pool_via_bot(self, tmp_path: pathlib.Path) -> None:
        """Manager builds a new pool via bot.build_pool when not in registry."""
        weth_addr = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
        usdc_addr = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
        factory_addr = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"

        config = _make_test_config(tmp_path)
        bot = Bot(
            config,
            provider=_v2_offline_provider(
                weth_addr=weth_addr,
                usdc_addr=usdc_addr,
                factory_addr=factory_addr,
                pool_addr=WETH_USDC_V2_POOL,
            ),
        )

        # Pre-register tokens so build_erc20token doesn't need RPC
        weth = make_erc20(
            bot._py_bot, weth_addr, chain_id=1, name="Wrapped Ether", symbol="WETH", decimals=18
        )
        usdc = make_erc20(
            bot._py_bot, usdc_addr, chain_id=1, name="USD Coin", symbol="USDC", decimals=6
        )
        bot.tokens.add(token_address=weth_addr, chain_id=1, token=weth)
        bot.tokens.add(token_address=usdc_addr, chain_id=1, token=usdc)

        manager = bot.add_tracker(
            UniswapV2PoolTracker,
            factory_address=factory_addr,
        )

        # Call get_pool — should call bot.build_pool internally
        pool = manager.get_pool(WETH_USDC_V2_POOL)
        assert isinstance(pool, UniswapV2Pool)
        assert pool.address == get_checksum_address(WETH_USDC_V2_POOL)
        assert pool.token0.address == get_checksum_address(weth_addr)
        assert pool.token1.address == get_checksum_address(usdc_addr)

        # Pool should be in both the manager's tracked pools AND bot.pools
        assert pool.address in manager._tracked_pools
        assert bot.pools.get(pool_address=pool.address, chain_id=1) is pool

        # Second call returns the same instance from tracking
        pool2 = manager.get_pool(WETH_USDC_V2_POOL)
        assert pool2 is pool


class _DelegateSpy:
    """Wraps a ``LiquidityPool`` to record ``calculate_tokens_out/in`` calls.

    ADR-005 slice 5 delegation-test for the V2 constant-product calc path:
    ``UniswapV2Pool.calculate_tokens_out_from_tokens_in`` (no override) routes
    through ``LiquidityPool.calculate_tokens_out``. Pass-through for all other
    handle methods via ``__getattr__``.
    """

    def __init__(self, real_pool):
        self._real = real_pool
        self.calc_out_calls: list[tuple[bool, int]] = []
        self.calc_in_calls: list[tuple[bool, int]] = []

    def calculate_tokens_out(self, *, zero_for_one, amount_in):
        self.calc_out_calls.append((zero_for_one, int(amount_in)))
        return self._real.calculate_tokens_out(zero_for_one=zero_for_one, amount_in=amount_in)

    def calculate_tokens_in(self, *, zero_for_one, amount_out):
        self.calc_in_calls.append((zero_for_one, int(amount_out)))
        return self._real.calculate_tokens_in(zero_for_one=zero_for_one, amount_out=amount_out)

    def __getattr__(self, name):
        return getattr(self._real, name)


class TestV2CalcDelegation:
    """ADR-005 slice 5 — V2 calc delegation to ``LiquidityPool``.

    The constant-product calc math delegates to Rust's
    ``LiquidityPool.calculate_tokens_out/in`` when no ``override_state`` is
    given (single read guard — no separate Python state read before the calc,
    so no pump-interleave risk). The override path stays Python
    (``constant_product_calc_exact_in/out`` against the override reserves) so
    ``simulate_*`` can hold one snapshot Python-side for delta + final_state
    consistency (slice 4's fix).
    """

    @staticmethod
    def _make_pool() -> UniswapV2Pool:
        weth = _make_weth()
        usdc = _make_usdc()
        return make_v2_pool(
            address=WETH_USDC_V2_POOL,
            chain_id=1,
            token0=weth,
            token1=usdc,
            factory=UNISWAP_V2_FACTORY,
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1000 * 10**18,
            reserves_token1=2_000_000 * 10**6,
            state_block=18_000_000,
        )

    def test_calculate_tokens_out_delegates_to_rust_no_override(self) -> None:
        """No override_state: calc delegates to LiquidityPool.calculate_tokens_out."""
        pool = self._make_pool()
        weth = pool.token0
        spy = _DelegateSpy(pool._py_pool)
        pool._py_pool = spy

        result = pool.calculate_tokens_out_from_tokens_in(token_in=weth, token_in_quantity=10**18)

        # token0 in → zero_for_one=True
        assert spy.calc_out_calls == [(True, 10**18)]
        assert spy.calc_in_calls == []
        assert result > 0

    def test_calculate_tokens_out_reverse_delegates_to_rust(self) -> None:
        """token1 in → zero_for_one=False, still delegates to Rust."""
        pool = self._make_pool()
        usdc = pool.token1
        spy = _DelegateSpy(pool._py_pool)
        pool._py_pool = spy

        result = pool.calculate_tokens_out_from_tokens_in(token_in=usdc, token_in_quantity=10**6)

        assert spy.calc_out_calls == [(False, 10**6)]
        assert result > 0

    def test_calculate_tokens_out_uses_python_path_with_override(self) -> None:
        """override_state: calc stays Python (constant_product_calc_exact_in
        against the override reserves). simulate_* relies on this."""
        pool = self._make_pool()
        weth = pool.token0
        spy = _DelegateSpy(pool._py_pool)
        pool._py_pool = spy

        override = UniswapV2PoolState(
            address=pool.address,
            block=None,
            reserves_token0=2000 * 10**18,
            reserves_token1=4_000_000 * 10**6,
        )

        result = pool.calculate_tokens_out_from_tokens_in(
            token_in=weth, token_in_quantity=10**18, override_state=override
        )

        # Rust untouched — pure Python path
        assert spy.calc_out_calls == []
        assert spy.calc_in_calls == []
        assert result == constant_product_calc_exact_in(
            amount_in=10**18,
            reserves_in=override.reserves_token0,
            reserves_out=override.reserves_token1,
            fee=Fraction(3, 1000),
        )

    def test_calculate_tokens_in_delegates_to_rust_no_override(self) -> None:
        """No override_state: calc delegates to LiquidityPool.calculate_tokens_in."""
        pool = self._make_pool()
        usdc = pool.token1
        spy = _DelegateSpy(pool._py_pool)
        pool._py_pool = spy

        result = pool.calculate_tokens_in_from_tokens_out(token_out_quantity=10**6, token_out=usdc)

        # token1 out (received by user) → t0 in → zero_for_one=True
        assert spy.calc_in_calls == [(True, 10**6)]
        assert spy.calc_out_calls == []
        assert result > 0

    def test_calculate_tokens_in_reverse_delegates_to_rust(self) -> None:
        """token0 out → zero_for_one=False, still delegates to Rust."""
        pool = self._make_pool()
        weth = pool.token0
        spy = _DelegateSpy(pool._py_pool)
        pool._py_pool = spy

        result = pool.calculate_tokens_in_from_tokens_out(token_out_quantity=10**18, token_out=weth)

        assert spy.calc_in_calls == [(False, 10**18)]
        assert result > 0

    def test_calculate_tokens_in_uses_python_path_with_override(self) -> None:
        """override_state: calc-in stays Python (constant_product_calc_exact_out)."""
        pool = self._make_pool()
        usdc = pool.token1
        spy = _DelegateSpy(pool._py_pool)
        pool._py_pool = spy

        override = UniswapV2PoolState(
            address=pool.address,
            block=None,
            reserves_token0=2000 * 10**18,
            reserves_token1=4_000_000 * 10**6,
        )

        result = pool.calculate_tokens_in_from_tokens_out(
            token_out_quantity=10**6, token_out=usdc, override_state=override
        )

        assert spy.calc_in_calls == []
        assert spy.calc_out_calls == []
        assert result == constant_product_calc_exact_out(
            amount_out=10**6,
            reserves_in=override.reserves_token0,
            reserves_out=override.reserves_token1,
            fee=Fraction(3, 1000),
        )

    def test_calculate_tokens_in_raises_on_overdraw_via_rust(self) -> None:
        """Non-override overdraw: Rust returns 0, Python raises LiquidityPoolError
        (preserves the original Python contract). Spy confirms Rust WAS hit."""
        pool = self._make_pool()
        usdc = pool.token1
        spy = _DelegateSpy(pool._py_pool)
        pool._py_pool = spy

        # token1 reserves = 2_000_000 * 10^6 = 2_000_000_000_000 — request more
        with pytest.raises(LiquidityPoolError):
            pool.calculate_tokens_in_from_tokens_out(
                token_out_quantity=2_000_000_000_001,
                token_out=usdc,
            )

        # Rust was called with the overdraw amount and returned 0 (Python raises)
        assert spy.calc_in_calls == [(True, 2_000_000_000_001)]

    def test_calculate_tokens_out_rejects_zero_input(self) -> None:
        """InvalidSwapInputAmount still raised before delegation is attempted."""
        pool = self._make_pool()
        weth = pool.token0
        spy = _DelegateSpy(pool._py_pool)
        pool._py_pool = spy

        with pytest.raises(InvalidSwapInputAmount):
            pool.calculate_tokens_out_from_tokens_in(token_in=weth, token_in_quantity=0)

        # Delegation NOT attempted — pre-check failed first
        assert spy.calc_out_calls == []

    def test_calculate_tokens_out_rejects_unknown_token(self) -> None:
        """Unknown token_in raises DegenbotValueError before delegation."""
        pool = self._make_pool()
        bogus = make_erc20(
            _PY_BOT,
            "0x0000000000000000000000000000000000000009",
            chain_id=1,
            name="Bogus",
            symbol="BOG",
            decimals=18,
        )
        spy = _DelegateSpy(pool._py_pool)
        pool._py_pool = spy

        with pytest.raises(DegenbotValueError):
            pool.calculate_tokens_out_from_tokens_in(token_in=bogus, token_in_quantity=10**18)

        assert spy.calc_out_calls == []
