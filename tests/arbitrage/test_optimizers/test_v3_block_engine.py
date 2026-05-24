"""Tests for V3ArbEngine — Python-accessible V3 block engine."""

from __future__ import annotations

from degenbot.degenbot_rs import V3ArbEngine


# sqrt price at tick 0 (1:1 price for 18-decimal tokens)
SQRT_PRICE_TICK_0 = 79228162514264337593543950336


class TestV3ArbEngineRegistration:
    """Test pool and path registration through the Python wrapper."""

    def test_register_pool_returns_key(self):
        engine = V3ArbEngine()
        key = engine.register_pool(
            address="0x" + "11" * 20,
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

    def test_register_two_pools_and_path(self):
        engine = V3ArbEngine()

        key0 = engine.register_pool(
            address="0x" + "11" * 20,
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

        key1 = engine.register_pool(
            address="0x" + "22" * 20,
            token0="0x" + "02" * 20,
            token1="0x" + "03" * 20,
            fee=3000,
            tick_spacing=60,
            factory="0x" + "00" * 20,
            sqrt_price_x96=SQRT_PRICE_TICK_0,
            liquidity=2_000_000,
            tick=0,
            tick_data={-60: (250, -80), 60: (350, 120)},
        )

        assert key0 == 1
        assert key1 == 2

        path_id = engine.register_path([(key0, True), (key1, False)])
        assert path_id == 1

    def test_pool_count_and_path_count(self):
        engine = V3ArbEngine()
        assert engine.pool_count() == 0
        assert engine.path_count() == 0

        engine.register_pool(
            address="0x" + "11" * 20,
            token0="0x" + "00" * 20,
            token1="0x" + "01" * 20,
            fee=3000,
            tick_spacing=60,
            factory="0x" + "00" * 20,
            sqrt_price_x96=SQRT_PRICE_TICK_0,
            liquidity=1_000_000,
            tick=0,
            tick_data={60: (200, 100)},
        )

        assert engine.pool_count() == 1

    def test_freeze_prevents_registration(self):
        engine = V3ArbEngine()
        engine.freeze()
        assert engine.is_running()

        import pytest

        with pytest.raises(BaseException):  # PanicException from Rust assert!
            engine.register_pool(
                address="0x" + "33" * 20,
                token0="0x" + "00" * 20,
                token1="0x" + "01" * 20,
                fee=3000,
                tick_spacing=60,
                factory="0x" + "00" * 20,
                sqrt_price_x96=SQRT_PRICE_TICK_0,
                liquidity=100,
                tick=0,
                tick_data={},
            )


class TestV3ArbEngineProcessLogs:
    """Test synchronous log processing through the Python wrapper."""

    def test_process_logs_updates_block_number(self):
        engine = V3ArbEngine()

        addr = "0x" + "11" * 20
        engine.register_pool(
            address=addr,
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

        engine.freeze()

        # Process a swap update
        engine.process_logs(
            [
                (
                    addr,
                    79466191966197645195421774833,  # sqrt price at tick 60
                    900_000,
                    60,
                    [],  # no tick priors
                )
            ],
            42,
        )

        results, block_number = engine.latest_results()
        assert block_number == 42

    def test_process_logs_empty_is_noop(self):
        engine = V3ArbEngine()
        engine.freeze()

        engine.process_logs([], 1)

        results, block_number = engine.latest_results()
        assert block_number == 1
        assert len(results) == 0

    def test_latest_results_returns_flat_list(self):
        engine = V3ArbEngine()

        # Register two pools with different prices to create an arb opportunity
        addr0 = "0x" + "11" * 20
        addr1 = "0x" + "22" * 20

        # Pool 0: high liquidity, standard 0.3% fee
        key0 = engine.register_pool(
            address=addr0,
            token0="0x" + "00" * 20,
            token1="0x" + "01" * 20,
            fee=3000,
            tick_spacing=60,
            factory="0x" + "00" * 20,
            sqrt_price_x96=SQRT_PRICE_TICK_0,
            liquidity=10_000_000_000_000,
            tick=0,
            tick_data={-60: (500, -200), 60: (800, 300)},
        )

        # Pool 1: high liquidity, slightly different price
        key1 = engine.register_pool(
            address=addr1,
            token0="0x" + "02" * 20,
            token1="0x" + "03" * 20,
            fee=3000,
            tick_spacing=60,
            factory="0x" + "00" * 20,
            sqrt_price_x96=79466191966197645195421774833,  # price at tick 60
            liquidity=10_000_000_000_000,
            tick=60,
            tick_data={0: (500, -200), 120: (800, 300)},
        )

        path_id = engine.register_path([(key0, True), (key1, False)])
        engine.freeze()

        # Process with no updates — just trigger a solve
        engine.process_logs([], 100)

        results, block_number = engine.latest_results()
        assert block_number == 100
        # Results is a flat list: [path_id, optimal_input, profit, ...] or empty
        assert len(results) % 3 == 0
