"""Tests for UniswapArbEngine — mixed V2/V3 arbitrage engine."""

from __future__ import annotations

from degenbot.degenbot_rs import UniswapArbEngine


# sqrt price at tick 0 (1:1 price for 18-decimal tokens)
SQRT_PRICE_TICK_0 = 79228162514264337593543950336

USDC = 10**6
WETH = 10**18


class TestUniswapArbEngineRegistration:
    """Test pool and path registration through the Python wrapper."""

    def test_register_v2_pool_returns_id(self):
        engine = UniswapArbEngine()
        pool_id = engine.register_v2_pool(
            address="0x" + "11" * 20,
            reserve0=1_500_000 * USDC,
            reserve1=800 * WETH,
            gamma_numer=997,
            fee_denom=1000,
        )
        assert pool_id == 1  # forward
        assert engine.v2_pool_count() == 1

    def test_register_v3_pool_returns_key(self):
        engine = UniswapArbEngine()
        key = engine.register_v3_pool(
            address="0x" + "22" * 20,
            token0="0x" + "00" * 20,
            token1="0x" + "01" * 20,
            fee=3000,
            tick_spacing=60,
            factory="0x" + "00" * 20,
            sqrt_price_x96=SQRT_PRICE_TICK_0,
            liquidity=1_000_000,
            tick=0,
            tick_data={-60: (200, -100), 60: (300, 150)},
        )
        assert key == 1
        assert engine.v3_pool_count() == 1

    def test_register_both_pool_types(self):
        engine = UniswapArbEngine()
        v2_id = engine.register_v2_pool(
            address="0x" + "11" * 20,
            reserve0=1_500_000 * USDC,
            reserve1=800 * WETH,
            gamma_numer=997,
            fee_denom=1000,
        )
        v3_key = engine.register_v3_pool(
            address="0x" + "22" * 20,
            token0="0x" + "00" * 20,
            token1="0x" + "01" * 20,
            fee=3000,
            tick_spacing=60,
            factory="0x" + "00" * 20,
            sqrt_price_x96=SQRT_PRICE_TICK_0,
            liquidity=1_000_000,
            tick=0,
            tick_data={-60: (200, -100), 60: (300, 150)},
        )
        assert engine.v2_pool_count() == 1
        assert engine.v3_pool_count() == 1

    def test_register_mixed_path(self):
        engine = UniswapArbEngine()
        v2_id = engine.register_v2_pool(
            address="0x" + "11" * 20,
            reserve0=1_500_000 * USDC,
            reserve1=800 * WETH,
            gamma_numer=997,
            fee_denom=1000,
        )
        v3_key = engine.register_v3_pool(
            address="0x" + "22" * 20,
            token0="0x" + "00" * 20,
            token1="0x" + "01" * 20,
            fee=3000,
            tick_spacing=60,
            factory="0x" + "00" * 20,
            sqrt_price_x96=SQRT_PRICE_TICK_0,
            liquidity=1_000_000,
            tick=0,
            tick_data={-60: (200, -100), 60: (300, 150)},
        )
        path_id = engine.register_path(
            [
                ("V2", v2_id, True),
                ("V3", v3_key, False),
            ]
        )
        assert path_id == 1
        assert engine.path_count() == 1


class TestUniswapArbEngineProcessLogs:
    """Test process_logs with V2 Sync and V3 Swap events."""

    def test_process_v2_sync_updates(self):
        engine = UniswapArbEngine()

        # Two V2 pools
        v2_a = engine.register_v2_pool(
            address="0x" + "11" * 20,
            reserve0=1_500_000 * USDC,
            reserve1=800 * WETH,
            gamma_numer=997,
            fee_denom=1000,
        )
        v2_b = engine.register_v2_pool(
            address="0x" + "12" * 20,
            reserve0=800 * WETH,
            reserve1=1_600_000 * USDC,
            gamma_numer=997,
            fee_denom=1000,
        )

        # V2-only path
        engine.register_path(
            [
                ("V2", v2_a, True),
                ("V2", v2_b, True),
            ]
        )

        engine.freeze()
        engine.process_logs(
            v2_sync_updates=[],
            v3_swap_updates=[],
            block_number=1,
        )
        results, block = engine.latest_results()
        assert block == 1

    def test_process_mixed_v2_v3_updates(self):
        engine = UniswapArbEngine()

        # V2 pool
        v2_id = engine.register_v2_pool(
            address="0x" + "11" * 20,
            reserve0=1_500_000 * USDC,
            reserve1=800 * WETH,
            gamma_numer=997,
            fee_denom=1000,
        )

        # V3 pool
        v3_key = engine.register_v3_pool(
            address="0x" + "22" * 20,
            token0="0x" + "00" * 20,
            token1="0x" + "01" * 20,
            fee=3000,
            tick_spacing=60,
            factory="0x" + "00" * 20,
            sqrt_price_x96=SQRT_PRICE_TICK_0,
            liquidity=10_000_000_000_000,
            tick=0,
            tick_data={-60: (200, -100), 60: (300, 150)},
        )

        # Mixed path
        engine.register_path(
            [
                ("V2", v2_id, True),
                ("V3", v3_key, False),
            ]
        )

        engine.freeze()
        engine.process_logs(
            v2_sync_updates=[
                ("0x" + "11" * 20, 1_400_000 * USDC, 750 * WETH),
            ],
            v3_swap_updates=[],
            block_number=42,
        )

        results, block = engine.latest_results()
        assert block == 42

    def test_pure_v2_path_finds_arb(self):
        engine = UniswapArbEngine()

        # Two V2 pools with price divergence
        v2_a = engine.register_v2_pool(
            address="0x" + "11" * 20,
            reserve0=1_500_000 * USDC,
            reserve1=800 * WETH,
            gamma_numer=997,
            fee_denom=1000,
        )
        v2_b = engine.register_v2_pool(
            address="0x" + "12" * 20,
            reserve0=800 * WETH,
            reserve1=1_600_000 * USDC,
            gamma_numer=997,
            fee_denom=1000,
        )

        # V2-only path
        engine.register_path(
            [
                ("V2", v2_a, True),
                ("V2", v2_b, True),
            ]
        )

        engine.freeze()

        # Process empty block to trigger solve
        engine.process_logs(
            v2_sync_updates=[],
            v3_swap_updates=[],
            block_number=1,
        )

        results, block = engine.latest_results()
        assert block == 1
        # Should find profitable arb
        assert len(results) >= 3  # At least (path_id, input, profit)
        assert results[0] == 1  # path_id
        assert results[1] > 0  # optimal_input
        assert results[2] > 0  # profit


class TestUniswapArbEngineFreeze:
    """Test registration freeze behavior."""

    def test_freeze_prevents_registration(self):
        """After freeze, registering a V2 pool should raise a PanicException."""
        import pytest

        engine = UniswapArbEngine()
        engine.freeze()
        assert engine.is_running()

        # Rust assert! panics propagate as PanicException(BaseException) in Python
        with pytest.raises(BaseException, match="cannot register pools after start"):
            engine.register_v2_pool(
                address="0x" + "11" * 20,
                reserve0=1_500_000 * USDC,
                reserve1=800 * WETH,
                gamma_numer=997,
                fee_denom=1000,
            )

    def test_start_freezes_registration(self):
        engine = UniswapArbEngine()
        engine.start()
        assert engine.is_running()
