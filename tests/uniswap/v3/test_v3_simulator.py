"""Tests for v3_simulator — pure swap calculation extracted from V3Pool.

These tests verify that ``v3_simulator.calculate_swap`` produces *identical*
results to ``UniswapV3Pool._calculate_swap``. The simulator consumes a frozen
``LiquidityMapSnapshot`` so it is deterministic and has no side effects.
"""

import pytest

from degenbot.exceptions.evm import EVMRevertError
from degenbot.uniswap.concentrated.liquidity_map import LiquidityMapSnapshot, MissingLiquidityData
from degenbot.uniswap.concentrated.v3_simulator import calculate_swap
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v3_types import UniswapV3BitmapAtWord, UniswapV3LiquidityAtTick


class TestV3SimulatorMatchesPool:
    """Byte-for-byte exact-match against the existing V3Pool._calculate_swap."""

    def test_exact_input_zero_for_one(self, offline_wbtc_weth_v3_pool: UniswapV3Pool) -> None:
        """1 WBTC → WETH (zero_for_one=True, amount_specified > 0)."""
        pool = offline_wbtc_weth_v3_pool
        amount_in = 1 * 10**8

        old = pool._calculate_swap(
            zero_for_one=True,
            amount_specified=amount_in,
            sqrt_price_limit_x96=4295128740,
        )

        snap = LiquidityMapSnapshot.from_pool(pool)
        new = calculate_swap(
            snapshot=snap,
            zero_for_one=True,
            amount_specified=amount_in,
            sqrt_price_limit_x96=4295128740,
            fee=pool.fee,
            liquidity_start=pool.liquidity,
            sqrt_price_x96_start=pool.sqrt_price_x96,
            tick_start=pool.tick,
        )

        assert new.amount0 == old[0]
        assert new.amount1 == old[1]
        assert new.sqrt_price_x96 == old[2]
        assert new.liquidity == old[3]
        assert new.tick == old[4]

    def test_exact_input_one_for_zero(self, offline_wbtc_weth_v3_pool: UniswapV3Pool) -> None:
        """1 WETH → WBTC (zero_for_one=False, amount_specified > 0)."""
        pool = offline_wbtc_weth_v3_pool
        amount_in = 1 * 10**18

        old = pool._calculate_swap(
            zero_for_one=False,
            amount_specified=amount_in,
            sqrt_price_limit_x96=1461446703485210103287273052203988822378723970341,
        )

        snap = LiquidityMapSnapshot.from_pool(pool)
        new = calculate_swap(
            snapshot=snap,
            zero_for_one=False,
            amount_specified=amount_in,
            sqrt_price_limit_x96=1461446703485210103287273052203988822378723970341,
            fee=pool.fee,
            liquidity_start=pool.liquidity,
            sqrt_price_x96_start=pool.sqrt_price_x96,
            tick_start=pool.tick,
        )

        assert new.amount0 == old[0]
        assert new.amount1 == old[1]
        assert new.sqrt_price_x96 == old[2]
        assert new.liquidity == old[3]
        assert new.tick == old[4]

    def test_exact_output_zero_for_one(self, offline_wbtc_weth_v3_pool: UniswapV3Pool) -> None:
        """Want exactly 1 WBTC out (zero_for_one=True, amount_specified < 0)."""
        pool = offline_wbtc_weth_v3_pool
        amount_out = 1 * 10**8

        old = pool._calculate_swap(
            zero_for_one=True,
            amount_specified=-amount_out,
            sqrt_price_limit_x96=4295128740,
        )

        snap = LiquidityMapSnapshot.from_pool(pool)
        new = calculate_swap(
            snapshot=snap,
            zero_for_one=True,
            amount_specified=-amount_out,
            sqrt_price_limit_x96=4295128740,
            fee=pool.fee,
            liquidity_start=pool.liquidity,
            sqrt_price_x96_start=pool.sqrt_price_x96,
            tick_start=pool.tick,
        )

        assert new.amount0 == old[0]
        assert new.amount1 == old[1]
        assert new.sqrt_price_x96 == old[2]
        assert new.liquidity == old[3]
        assert new.tick == old[4]

    def test_exact_output_one_for_zero(self, offline_wbtc_weth_v3_pool: UniswapV3Pool) -> None:
        """Want exactly 1 WETH out (zero_for_one=False, amount_specified < 0)."""
        pool = offline_wbtc_weth_v3_pool
        amount_out = 1 * 10**18

        old = pool._calculate_swap(
            zero_for_one=False,
            amount_specified=-amount_out,
            sqrt_price_limit_x96=1461446703485210103287273052203988822378723970341,
        )

        snap = LiquidityMapSnapshot.from_pool(pool)
        new = calculate_swap(
            snapshot=snap,
            zero_for_one=False,
            amount_specified=-amount_out,
            sqrt_price_limit_x96=1461446703485210103287273052203988822378723970341,
            fee=pool.fee,
            liquidity_start=pool.liquidity,
            sqrt_price_x96_start=pool.sqrt_price_x96,
            tick_start=pool.tick,
        )

        assert new.amount0 == old[0]
        assert new.amount1 == old[1]
        assert new.sqrt_price_x96 == old[2]
        assert new.liquidity == old[3]
        assert new.tick == old[4]

    def test_state_override(self, offline_wbtc_weth_v3_pool: UniswapV3Pool) -> None:
        """Swap against an explicit state rather than the pool's current state."""
        pool = offline_wbtc_weth_v3_pool
        state = pool.state
        amount_in = 1 * 10**8

        old = pool._calculate_swap(
            zero_for_one=True,
            amount_specified=amount_in,
            sqrt_price_limit_x96=4295128740,
            override_state=state,
        )

        snap = LiquidityMapSnapshot.from_state(
            state,
            tick_spacing=pool.tick_spacing,
            sparse=pool.sparse_liquidity_map,
        )
        new = calculate_swap(
            snapshot=snap,
            zero_for_one=True,
            amount_specified=amount_in,
            sqrt_price_limit_x96=4295128740,
            fee=pool.fee,
            liquidity_start=state.liquidity,
            sqrt_price_x96_start=state.sqrt_price_x96,
            tick_start=state.tick,
        )

        assert new.amount0 == old[0]
        assert new.amount1 == old[1]
        assert new.sqrt_price_x96 == old[2]
        assert new.liquidity == old[3]
        assert new.tick == old[4]

    def test_zero_specified_reverts(self, offline_wbtc_weth_v3_pool: UniswapV3Pool) -> None:
        """amount_specified == 0 raises EVMRevertError (AS)."""
        pool = offline_wbtc_weth_v3_pool
        snap = LiquidityMapSnapshot.from_pool(pool)

        with pytest.raises(EVMRevertError):
            calculate_swap(
                snapshot=snap,
                zero_for_one=True,
                amount_specified=0,
                sqrt_price_limit_x96=4295128740,
                fee=pool.fee,
                liquidity_start=pool.liquidity,
                sqrt_price_x96_start=pool.sqrt_price_x96,
                tick_start=pool.tick,
            )


class TestLiquidityMapSnapshot:
    """Unit tests for the snapshot abstraction."""

    def test_from_pool(self, offline_wbtc_weth_v3_pool: UniswapV3Pool) -> None:
        snap = LiquidityMapSnapshot.from_pool(offline_wbtc_weth_v3_pool)
        assert snap.tick_spacing == offline_wbtc_weth_v3_pool.tick_spacing
        assert snap.sparse == offline_wbtc_weth_v3_pool.sparse_liquidity_map
        assert len(snap.tick_bitmap) > 0
        assert len(snap.tick_data) > 0

    def test_from_state(self, offline_wbtc_weth_v3_pool: UniswapV3Pool) -> None:
        state = offline_wbtc_weth_v3_pool.state
        snap = LiquidityMapSnapshot.from_state(
            state,
            tick_spacing=offline_wbtc_weth_v3_pool.tick_spacing,
            sparse=offline_wbtc_weth_v3_pool.sparse_liquidity_map,
        )
        assert snap.tick_spacing == offline_wbtc_weth_v3_pool.tick_spacing
        assert len(snap.tick_bitmap) > 0

    def test_sparse_raises_on_missing_word(self) -> None:
        snap = LiquidityMapSnapshot(
            tick_data={},
            tick_bitmap={},
            tick_spacing=10,
            sparse=True,
        )
        with pytest.raises(MissingLiquidityData):
            calculate_swap(
                snapshot=snap,
                zero_for_one=True,
                amount_specified=1000,
                sqrt_price_limit_x96=4295128740,
                fee=3000,
                liquidity_start=1_000_000,
                sqrt_price_x96_start=79228162514264337593543950336,
                tick_start=0,
            )

    def test_non_sparse_no_ticks_yields_boundary(self) -> None:
        """Even with no initialized ticks, gen_ticks yields boundary ticks."""
        snap = LiquidityMapSnapshot(
            tick_data={},
            tick_bitmap={},
            tick_spacing=10,
            sparse=False,
        )
        _tick, initialized = snap.next_initialized_tick(tick=0, zero_for_one=True)
        assert initialized is False

    def test_non_sparse_empty_tick_data_swap(self) -> None:
        """No liquidity → zero amounts, price hits the limit."""
        snap = LiquidityMapSnapshot(
            tick_data={},
            tick_bitmap={},
            tick_spacing=10,
            sparse=False,
        )
        result = calculate_swap(
            snapshot=snap,
            zero_for_one=True,
            amount_specified=100,
            sqrt_price_limit_x96=4295128740,
            fee=3000,
            liquidity_start=0,
            sqrt_price_x96_start=79228162514264337593543950336,
            tick_start=0,
        )
        # With no liquidity, the swap exhausts immediately and price goes to limit
        assert result.amount0 == 0
        assert result.amount1 == 0
        assert result.sqrt_price_x96 == 4295128740  # price limit reached
        assert result.liquidity == 0
        assert result.tick == -887272


class TestSyntheticMap:
    """Simulator tests with a hand-built liquidity map (no RPC at all)."""

    @staticmethod
    def _single_position_map(
        *,
        tick_lower: int = -1000,
        tick_upper: int = 1000,
        tick_spacing: int = 10,
        liquidity: int = 1_000_000,
    ) -> LiquidityMapSnapshot:
        compressed_lower = tick_lower // tick_spacing
        compressed_upper = tick_upper // tick_spacing

        bitmap: dict[int, UniswapV3BitmapAtWord] = {}
        for compressed in (compressed_lower, compressed_upper):
            word = compressed >> 8
            bit = compressed & 0xFF
            if word not in bitmap:
                bitmap[word] = UniswapV3BitmapAtWord(bitmap=0, block=0)
            bitmap[word] = UniswapV3BitmapAtWord(
                bitmap=bitmap[word].bitmap | (1 << bit),
                block=0,
            )

        tick_data = {
            tick_lower: UniswapV3LiquidityAtTick(
                liquidity_net=liquidity,
                liquidity_gross=liquidity,
                block=0,
            ),
            tick_upper: UniswapV3LiquidityAtTick(
                liquidity_net=-liquidity,
                liquidity_gross=liquidity,
                block=0,
            ),
        }
        return LiquidityMapSnapshot(
            tick_data=tick_data,
            tick_bitmap=bitmap,
            tick_spacing=tick_spacing,
            sparse=False,
        )

    def test_small_exact_input(self) -> None:
        """Small swap stays within one liquidity range."""
        snap = self._single_position_map(liquidity=10_000_000)
        result = calculate_swap(
            snapshot=snap,
            zero_for_one=True,
            amount_specified=1000,
            sqrt_price_limit_x96=4295128740,
            fee=500,
            liquidity_start=10_000_000,
            sqrt_price_x96_start=79228162514264337593543950336,
            tick_start=0,
        )
        assert result.amount0 > 0  # deposited
        assert result.amount1 < 0  # withdrawn
        assert result.liquidity == 10_000_000  # didn't cross a tick

    def test_exact_output(self) -> None:
        snap = self._single_position_map(liquidity=10_000_000)
        result = calculate_swap(
            snapshot=snap,
            zero_for_one=False,
            amount_specified=-500,
            sqrt_price_limit_x96=1461446703485210103287273052203988822378723970341,
            fee=500,
            liquidity_start=10_000_000,
            sqrt_price_x96_start=79228162514264337593543950336,
            tick_start=0,
        )
        assert result.amount0 < 0  # withdrawn
        assert result.amount1 > 0  # deposited
        assert result.amount1 >= 500

    def test_crosses_tick_boundary(self) -> None:
        """Large swap exhausts position and exits into zero liquidity."""
        snap = self._single_position_map(
            tick_lower=-100000,
            tick_upper=100000,
            liquidity=1_000_000,
        )
        result = calculate_swap(
            snapshot=snap,
            zero_for_one=True,
            amount_specified=999_999_999_999,
            sqrt_price_limit_x96=4295128740,
            fee=500,
            liquidity_start=1_000_000,
            sqrt_price_x96_start=79228162514264337593543950336,
            tick_start=0,
        )
        assert result.liquidity == 0
