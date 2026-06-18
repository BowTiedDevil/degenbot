"""Tests for Phase 3: I/O-free LiquidityPool construction via Bot."""

import pathlib
import pickle
from fractions import Fraction
from unittest.mock import MagicMock

import eth_abi.abi
import pytest
from web3.exceptions import Web3Exception

from degenbot.bot import Bot
from degenbot.checksum_cache import get_checksum_address
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.degenbot_rs import PyBot
from degenbot.erc20.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import InvalidSwapInputAmount, LiquidityPoolError
from degenbot.provider.call_helpers import encode_function_calldata
from degenbot.uniswap.liquidity_pool import LiquidityPool
from degenbot.uniswap.trackers import UniswapV2PoolTracker
from degenbot.uniswap.v2_functions import (
    constant_product_calc_exact_in,
    constant_product_calc_exact_out,
)
from degenbot.uniswap.v2_types import UniswapV2PoolExternalUpdate, UniswapV2PoolState
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool

_PY_BOT = PyBot()


def _make_test_config(tmp_path: pathlib.Path) -> DegenbotConfig:
    return DegenbotConfig(
        database=DatabaseSettings(path=tmp_path / "test.db"),
        rpc={1: "https://eth.llamarpc.com/"},
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


class TestV2PoolIOFreeConstructor:
    """LiquidityPool can be constructed with pre-fetched data only."""

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
        assert pool.chain_id == 1
        assert pool.token0 is weth
        assert pool.token1 is usdc
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

        # simulate_swap works
        result = pool.simulate_swap(
            token_in=weth.address,
            amount_in=10**18,
            token_out=usdc.address,
        )
        assert result.amount_out > 0

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

    def test_io_free_pool_pickle(self) -> None:
        """I/O-free pools pickle their immutable identity (transient — slice 15).

        The ``PyLiquidityPool`` handle is not picklable, so ``__getstate__``
        drops it: an unpickled pool is a detached snapshot. Mutable state
        reads (reserves/update_block) raise ``AttributeError`` on a detached
        pool (no ``_py_pool``); only immutable identity survives the round-trip.
        Slice 15 (TODO-cddc72d1) retires pickle entirely and deletes this test.
        """
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

        pickled = pickle.dumps(pool)
        unpickled = pickle.loads(pickled)

        # Immutable identity survives; reserves require the Rust handle (dropped).
        assert unpickled.address == pool.address
        assert not hasattr(unpickled, "_py_pool")
        with pytest.raises(AttributeError):
            _ = unpickled.reserves_token0


class TestBotBuildV2Pool:
    """Bot.build_pool() constructs I/O-free pools from on-chain data."""

    def test_build_pool_with_mock_provider(self, tmp_path: pathlib.Path) -> None:
        """build_pool fetches immutable values and reserves, constructs an I/O-free pool."""
        weth_addr = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
        usdc_addr = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
        factory_addr = "0x5C69bEe701ef814E44274f655e7632cB715C14B6"

        config = _make_test_config(tmp_path)
        bot = Bot(config)

        provider = MagicMock()
        provider.chain_id = 1
        provider.is_connected.return_value = True
        provider.get_block_number.return_value = 18_000_000

        # Mock RPC responses for immutable values and reserves
        def mock_call(*, to, data, block=None):
            # We'll check the method selector from the calldata
            # factory() selector
            if data[:4] == b"\x1a\x1a\xb0\xa5":  # not real, just placeholder
                pass
            # Simplify: use side_effect to return appropriate values
            return b""  # default

        # Instead of mocking individual calls, mock the provider responses
        # in the order Bot.build_pool calls them
        #
        # The bot calls:
        #   1. factory() -> address
        #   2. token0() -> address
        #   3. token1() -> address
        #   4. getReserves() -> (uint112, uint112)
        #
        # Plus token metadata calls for both tokens (6 calls each = 12)
        #
        # Actually, build_erc20token will also need to make calls...
        # Let's use a simpler approach: provide tokens directly via bot.tokens registry

        # Pre-register tokens so build_erc20token doesn't need RPC
        weth = make_erc20(
            _PY_BOT, weth_addr, chain_id=1, name="Wrapped Ether", symbol="WETH", decimals=18
        )
        usdc = make_erc20(
            _PY_BOT, usdc_addr, chain_id=1, name="USD Coin", symbol="USDC", decimals=6
        )
        bot.tokens.add(token_address=weth_addr, chain_id=1, token=weth)
        bot.tokens.add(token_address=usdc_addr, chain_id=1, token=usdc)

        bot.connections.register_provider(provider)
        bot.connections.set_default_chain(1)

        # Mock the raw_call / provider responses for:
        # 1. factory() call
        # 2. token0() call
        # 3. token1() call
        # 4. getReserves() call
        #
        # We need to encode the return values
        factory_encoded = eth_abi.abi.encode(types=["address"], args=[factory_addr])
        token0_encoded = eth_abi.abi.encode(types=["address"], args=[weth_addr])
        token1_encoded = eth_abi.abi.encode(types=["address"], args=[usdc_addr])
        reserves_encoded = eth_abi.abi.encode(
            types=["uint256", "uint256"],
            args=[1000 * 10**18, 2_000_000 * 10**6],
        )

        # The bot's build_pool will call provider.call() 4 times for:
        # - `factory()`
        # - `token0()`
        # - `token1()`
        # - `getReserves()`
        # But we need to decode the selectors...
        # Use encode_function_calldata to find out what selectors are used

        factory_calldata = encode_function_calldata("factory()", None)
        token0_calldata = encode_function_calldata("token0()", None)
        token1_calldata = encode_function_calldata("token1()", None)
        reserves_calldata = encode_function_calldata("getReserves()", None)

        slot0_calldata = encode_function_calldata("slot0()", None)

        call_responses = {
            factory_calldata: factory_encoded,
            token0_calldata: token0_encoded,
            token1_calldata: token1_encoded,
            slot0_calldata: None,  # marker: will raise Web3Exception
        }

        def mock_get_reserves_call(*, to, data, block=None):
            if data in call_responses:
                if data == slot0_calldata:
                    # V3 slot0() reverts on a V2 pool
                    msg = "revert"
                    raise Web3Exception(msg)
                return call_responses[data]
            if data == reserves_calldata:
                # getReserves returns (uint112, uint112, uint32)
                return reserves_encoded
            msg = f"Unexpected call with data={data!r}"
            raise ValueError(msg)

        provider.call = MagicMock(side_effect=mock_get_reserves_call)

        pool = bot.build_pool(
            WETH_USDC_V2_POOL,
            chain_id=1,
        )

        assert isinstance(pool, LiquidityPool)
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
        bot = Bot(config)

        provider = MagicMock()
        provider.chain_id = 1
        provider.is_connected.return_value = True
        provider.get_block_number.return_value = 18_000_000
        bot.connections.register_provider(provider)
        bot.connections.set_default_chain(1)

        factory = "0x5C69bEe701ef814E44274f655e7632cB715C14B6"
        manager = bot.add_tracker(
            UniswapV2PoolTracker,
            factory_address=factory,
            chain_id=1,
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
        factory_addr = "0x5C69bEe701ef814E44274f655e7632cB715C14B6"

        config = _make_test_config(tmp_path)
        bot = Bot(config)

        provider = MagicMock()
        provider.chain_id = 1
        provider.is_connected.return_value = True
        provider.get_block_number.return_value = 18_000_000
        bot.connections.register_provider(provider)
        bot.connections.set_default_chain(1)

        # Pre-register tokens so build_erc20token doesn't need RPC
        weth = _make_weth()
        usdc = _make_usdc()
        bot.tokens.add(token_address=weth_addr, chain_id=1, token=weth)
        bot.tokens.add(token_address=usdc_addr, chain_id=1, token=usdc)

        # Mock RPC responses
        factory_calldata = encode_function_calldata("factory()", None)
        token0_calldata = encode_function_calldata("token0()", None)
        token1_calldata = encode_function_calldata("token1()", None)
        reserves_calldata = encode_function_calldata("getReserves()", None)
        slot0_calldata = encode_function_calldata("slot0()", None)

        call_responses = {
            factory_calldata: eth_abi.abi.encode(types=["address"], args=[factory_addr]),
            token0_calldata: eth_abi.abi.encode(types=["address"], args=[weth_addr]),
            token1_calldata: eth_abi.abi.encode(types=["address"], args=[usdc_addr]),
            slot0_calldata: None,  # marker: slot0() reverts on V2 pools
        }

        def mock_call(*, to, data, block=None):
            if data in call_responses:
                if data == slot0_calldata:
                    msg = "revert"
                    raise Web3Exception(msg)
                return call_responses[data]
            if data == reserves_calldata:
                return eth_abi.abi.encode(
                    types=["uint256", "uint256"],
                    args=[1000 * 10**18, 2_000_000 * 10**6],
                )
            msg = f"Unexpected call with data={data!r}"
            raise ValueError(msg)

        provider.call = MagicMock(side_effect=mock_call)

        manager = bot.add_tracker(
            UniswapV2PoolTracker,
            factory_address=factory_addr,
            chain_id=1,
        )

        # Call get_pool — should call bot.build_pool internally
        pool = manager.get_pool(WETH_USDC_V2_POOL)
        assert isinstance(pool, LiquidityPool)
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
    """Wraps a ``PyLiquidityPool`` to record ``calculate_tokens_out/in`` calls.

    ADR-005 slice 5 delegation-test for the V2 constant-product calc path:
    ``LiquidityPool.calculate_tokens_out_from_tokens_in`` (no override) routes
    through ``PyLiquidityPool.calculate_tokens_out``. Pass-through for all other
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
    """ADR-005 slice 5 — V2 calc delegation to ``PyLiquidityPool``.

    The constant-product calc math delegates to Rust's
    ``PyLiquidityPool.calculate_tokens_out/in`` when no ``override_state`` is
    given (single read guard — no separate Python state read before the calc,
    so no pump-interleave risk). The override path stays Python
    (``constant_product_calc_exact_in/out`` against the override reserves) so
    ``simulate_*`` can hold one snapshot Python-side for delta + final_state
    consistency (slice 4's fix).
    """

    @staticmethod
    def _make_pool() -> LiquidityPool:
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
        """No override_state: calc delegates to PyLiquidityPool.calculate_tokens_out."""
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
        """No override_state: calc delegates to PyLiquidityPool.calculate_tokens_in."""
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
