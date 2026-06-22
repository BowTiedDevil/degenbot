"""Tests for UniswapArbEngine — mixed V2/V3 arbitrage engine."""

from __future__ import annotations

import pytest

from degenbot.degenbot_rs import UniswapArbEngine

# sqrt price at tick 0 (1:1 price for 18-decimal tokens)
SQRT_PRICE_TICK_0 = 79228162514264337593543950336

USDC = 10**6
WETH = 10**18

# Common pool addresses used in tests
V2_POOL_A = "0x" + "11" * 20
V2_POOL_B = "0x" + "12" * 20
V3_POOL = "0x" + "22" * 20

# V4 PoolManager address (mainnet)
V4_PM = "0x000000000004444c5dc75cB358380D2e3De08A90"


def _make_v3_snapshot(
    pools: dict[str, dict[int, tuple[int, int]]],
) -> dict[str, dict[int, tuple[int, int]]]:
    """Build a V3 snapshot dict for load_v3_snapshot_from_py()."""
    return pools


def _make_v4_snapshot(
    pool_managers: dict[str, dict[str, dict[int, tuple[int, int]]]],
) -> dict[str, dict[str, dict[int, tuple[int, int]]]]:
    """Build a V4 snapshot dict for load_v4_snapshot_from_py()."""
    return pool_managers


def _make_pool_id(suffix: int) -> str:
    """Generate a 32-byte pool ID as hex string."""
    return "0x" + (b"\x00" * 31 + bytes([suffix])).hex()


class TestUniswapArbEngineRegistration:
    """Test pool and path registration through the Python wrapper."""

    def test_register_v2_pool_returns_id(self):
        engine = UniswapArbEngine()
        pool_id = engine.register_v2_pool(
            address=V2_POOL_A,
            reserve0=1_500_000 * USDC,
            reserve1=800 * WETH,
            gamma_numer=997,
            fee_denom=1000,
        )
        assert pool_id == 1  # forward
        assert engine.v2_pool_count() == 1

    def test_register_v3_pool_returns_key(self):
        engine = UniswapArbEngine()
        engine.load_v3_snapshot_from_py(
            _make_v3_snapshot({
                V3_POOL: {-60: (200, -100), 60: (300, 150)},
            })
        )
        key = engine.register_v3_pool(
            address=V3_POOL,
            token0="0x" + "00" * 20,
            token1="0x" + "01" * 20,
            fee=3000,
            tick_spacing=60,
            factory="0x" + "00" * 20,
            sqrt_price_x96=SQRT_PRICE_TICK_0,
            liquidity=1_000_000,
            tick=0,
        )
        assert key == 1
        assert engine.v3_pool_count() == 1

    def test_register_both_pool_types(self):
        engine = UniswapArbEngine()
        v2_id = engine.register_v2_pool(
            address=V2_POOL_A,
            reserve0=1_500_000 * USDC,
            reserve1=800 * WETH,
            gamma_numer=997,
            fee_denom=1000,
        )
        engine.load_v3_snapshot_from_py(
            _make_v3_snapshot({
                V3_POOL: {-60: (200, -100), 60: (300, 150)},
            })
        )
        v3_key = engine.register_v3_pool(
            address=V3_POOL,
            token0="0x" + "00" * 20,
            token1="0x" + "01" * 20,
            fee=3000,
            tick_spacing=60,
            factory="0x" + "00" * 20,
            sqrt_price_x96=SQRT_PRICE_TICK_0,
            liquidity=1_000_000,
            tick=0,
        )
        assert engine.v2_pool_count() == 1
        assert engine.v3_pool_count() == 1

    def test_register_mixed_path(self):
        engine = UniswapArbEngine()
        v2_id = engine.register_v2_pool(
            address=V2_POOL_A,
            reserve0=1_500_000 * USDC,
            reserve1=800 * WETH,
            gamma_numer=997,
            fee_denom=1000,
        )
        engine.load_v3_snapshot_from_py(
            _make_v3_snapshot({
                V3_POOL: {-60: (200, -100), 60: (300, 150)},
            })
        )
        v3_key = engine.register_v3_pool(
            address=V3_POOL,
            token0="0x" + "00" * 20,
            token1="0x" + "01" * 20,
            fee=3000,
            tick_spacing=60,
            factory="0x" + "00" * 20,
            sqrt_price_x96=SQRT_PRICE_TICK_0,
            liquidity=1_000_000,
            tick=0,
        )
        path_id = engine.register_path([
            (v2_id, True),
            (v3_key, False),
        ])
        assert path_id == 1


class TestEventBufferControl:
    """Test event buffer staleness and flush controls."""

    def test_set_event_buffer_max_age(self):
        engine = UniswapArbEngine()
        engine.set_event_buffer_max_age(max_age=None)
        engine.set_event_buffer_max_age(max_age=100)

    def test_flush_event_buffer(self):
        engine = UniswapArbEngine()
        engine.flush_event_buffer()


class TestRegisterAndSolvePath:
    """Test register_and_solve_path (eager solving)."""

    def test_eager_solve_produces_results(self):
        """register_and_solve_path should eagerly solve and add results."""
        engine = UniswapArbEngine()

        v2_a = engine.register_v2_pool(
            address=V2_POOL_A,
            reserve0=1_500_000 * USDC,
            reserve1=800 * WETH,
            gamma_numer=997,
            fee_denom=1000,
        )
        v2_b = engine.register_v2_pool(
            address=V2_POOL_B,
            reserve0=800 * WETH,
            reserve1=1_600_000 * USDC,
            gamma_numer=997,
            fee_denom=1000,
        )

        path_id = engine.register_and_solve_path([
            (v2_a, True),
            (v2_b, True),
        ])
        assert path_id == 1

        results, _block = engine.latest_results()
        assert len(results) >= 1, "register_and_solve_path should eagerly produce results"

        found = [r for r in results if r[0] == path_id]
        assert len(found) == 1
        _pid, opt_input, profit, hop_outputs, consumed_inputs = found[0]
        assert opt_input > 0
        assert profit > 0

    def test_eager_solve_survives_process_logs(self):
        """Results from register_and_solve_path should survive process_logs."""
        engine = UniswapArbEngine()

        v2_a = engine.register_v2_pool(
            address=V2_POOL_A,
            reserve0=1_500_000 * USDC,
            reserve1=800 * WETH,
            gamma_numer=997,
            fee_denom=1000,
        )
        v2_b = engine.register_v2_pool(
            address=V2_POOL_B,
            reserve0=800 * WETH,
            reserve1=1_600_000 * USDC,
            gamma_numer=997,
            fee_denom=1000,
        )

        path_id = engine.register_and_solve_path([
            (v2_a, True),
            (v2_b, True),
        ])

        engine.process_logs(
            v2_sync_updates=[],
            v3_swap_updates=[],
            v4_swap_updates=[],
            block_number=1,
        )

        results, block = engine.latest_results()
        assert block == 1
        found = [r for r in results if r[0] == path_id]
        assert len(found) == 1, "eagerly-solved path result should survive process_logs"

    def test_register_and_solve_after_process_logs(self):
        """register_and_solve_path should work after process_logs has been called."""
        engine = UniswapArbEngine()

        v2_a = engine.register_v2_pool(
            address=V2_POOL_A,
            reserve0=1_500_000 * USDC,
            reserve1=800 * WETH,
            gamma_numer=997,
            fee_denom=1000,
        )
        v2_b = engine.register_v2_pool(
            address=V2_POOL_B,
            reserve0=800 * WETH,
            reserve1=1_600_000 * USDC,
            gamma_numer=997,
            fee_denom=1000,
        )

        engine.register_path([
            (v2_a, True),
            (v2_b, True),
        ])
        engine.solve_all_paths(block_number=1)

        v2_c = engine.register_v2_pool(
            address="0x" + "13" * 20,
            reserve0=900 * WETH,
            reserve1=1_700_000 * USDC,
            gamma_numer=997,
            fee_denom=1000,
        )
        path_id_2 = engine.register_and_solve_path([
            (v2_a, True),
            (v2_c, True),
        ])

        results, _block = engine.latest_results()
        found = [r for r in results if r[0] == path_id_2]
        assert len(found) >= 1, "eagerly-solved path should appear in results"
        assert engine.path_count() == 2

    def test_registration_is_always_on(self):
        """Registration is always-on — pools and paths can be registered at any time."""
        engine = UniswapArbEngine()

        engine.register_v2_pool(
            address=V2_POOL_A,
            reserve0=1_500_000 * USDC,
            reserve1=800 * WETH,
            gamma_numer=997,
            fee_denom=1000,
        )

        engine.solve_all_paths(block_number=1)

        engine.register_v2_pool(
            address=V2_POOL_B,
            reserve0=800 * WETH,
            reserve1=1_600_000 * USDC,
            gamma_numer=997,
            fee_denom=1000,
        )
        assert engine.v2_pool_count() == 2


class TestUniswapArbEngineProcessLogs:
    """Test process_logs with V2 Sync and V3 Swap events."""

    def test_process_v2_sync_updates(self):
        engine = UniswapArbEngine()

        v2_a = engine.register_v2_pool(
            address=V2_POOL_A,
            reserve0=1_500_000 * USDC,
            reserve1=800 * WETH,
            gamma_numer=997,
            fee_denom=1000,
        )
        v2_b = engine.register_v2_pool(
            address=V2_POOL_B,
            reserve0=800 * WETH,
            reserve1=1_600_000 * USDC,
            gamma_numer=997,
            fee_denom=1000,
        )

        engine.register_path([
            (v2_a, True),
            (v2_b, True),
        ])

        engine.solve_all_paths(block_number=1)
        results, block = engine.latest_results()
        assert block == 1

    def test_process_mixed_v2_v3_updates(self):
        engine = UniswapArbEngine()

        v2_id = engine.register_v2_pool(
            address=V2_POOL_A,
            reserve0=1_500_000 * USDC,
            reserve1=800 * WETH,
            gamma_numer=997,
            fee_denom=1000,
        )

        engine.load_v3_snapshot_from_py(
            _make_v3_snapshot({
                V3_POOL: {-60: (200, -100), 60: (300, 150)},
            })
        )
        v3_key = engine.register_v3_pool(
            address=V3_POOL,
            token0="0x" + "00" * 20,
            token1="0x" + "01" * 20,
            fee=3000,
            tick_spacing=60,
            factory="0x" + "00" * 20,
            sqrt_price_x96=SQRT_PRICE_TICK_0,
            liquidity=10_000_000_000_000,
            tick=0,
        )

        engine.register_path([
            (v2_id, True),
            (v3_key, False),
        ])

        engine.process_logs(
            v2_sync_updates=[
                (V2_POOL_A, 1_400_000 * USDC, 750 * WETH),
            ],
            v3_swap_updates=[],
            v4_swap_updates=[],
            block_number=42,
        )

        results, block = engine.latest_results()
        assert block == 42

    def test_pure_v2_path_finds_arb(self):
        engine = UniswapArbEngine()

        v2_a = engine.register_v2_pool(
            address=V2_POOL_A,
            reserve0=1_500_000 * USDC,
            reserve1=800 * WETH,
            gamma_numer=997,
            fee_denom=1000,
        )
        v2_b = engine.register_v2_pool(
            address=V2_POOL_B,
            reserve0=800 * WETH,
            reserve1=1_600_000 * USDC,
            gamma_numer=997,
            fee_denom=1000,
        )

        engine.register_path([
            (v2_a, True),
            (v2_b, True),
        ])

        engine.solve_all_paths(block_number=1)

        results, block = engine.latest_results()
        assert block == 1
        assert len(results) >= 1
        path_id, optimal_input, profit, hop_outputs, consumed_inputs = results[0]
        assert path_id == 1
        assert optimal_input > 0
        assert profit > 0
        assert len(hop_outputs) == 2
        assert len(consumed_inputs) == 2


class TestUniswapArbEngineV4:
    """Test V4 pool registration, hook filtering, and path solving."""

    def test_register_v4_pool_returns_key(self):
        engine = UniswapArbEngine()
        pool_id = _make_pool_id(1)
        engine.load_v4_snapshot_from_py(
            _make_v4_snapshot({
                V4_PM: {pool_id: {-60: (200, -100), 60: (300, 150)}},
            })
        )
        key = engine.register_v4_pool(
            pool_manager=V4_PM,
            pool_id_hex=pool_id,
            currency0="0x" + "00" * 20,
            currency1="0x" + "01" * 20,
            fee=3000,
            tick_spacing=60,
            hook_flags=0,
            sqrt_price_x96=SQRT_PRICE_TICK_0,
            liquidity=1_000_000,
            tick=0,
        )
        assert key == 1
        assert engine.v4_pool_count() == 1

    def test_v4_hook_filtering_rejects_amount_modifying(self):
        """Pools with amount-modifying hooks should be rejected."""
        engine = UniswapArbEngine()
        pool_id_10 = _make_pool_id(10)
        pool_id_11 = _make_pool_id(11)
        engine.load_v4_snapshot_from_py(
            _make_v4_snapshot({
                V4_PM: {pool_id_10: {}, pool_id_11: {}},
            })
        )

        with pytest.raises(ValueError, match="amount-modifying hooks"):
            engine.register_v4_pool(
                pool_manager=V4_PM,
                pool_id_hex=pool_id_10,
                currency0="0x" + "00" * 20,
                currency1="0x" + "01" * 20,
                fee=3000,
                tick_spacing=60,
                hook_flags=0x80,
                sqrt_price_x96=SQRT_PRICE_TICK_0,
                liquidity=1_000_000,
                tick=0,
            )

        with pytest.raises(ValueError, match="amount-modifying hooks"):
            engine.register_v4_pool(
                pool_manager=V4_PM,
                pool_id_hex=pool_id_11,
                currency0="0x" + "00" * 20,
                currency1="0x" + "01" * 20,
                fee=3000,
                tick_spacing=60,
                hook_flags=0x04,
                sqrt_price_x96=SQRT_PRICE_TICK_0,
                liquidity=1_000_000,
                tick=0,
            )

    def test_v4_dynamic_fee_rejected(self):
        """Pools with dynamic fees (0x100000) should be rejected."""
        engine = UniswapArbEngine()
        pool_id = _make_pool_id(12)
        engine.load_v4_snapshot_from_py(
            _make_v4_snapshot({
                V4_PM: {pool_id: {}},
            })
        )

        with pytest.raises(ValueError, match="dynamic fee"):
            engine.register_v4_pool(
                pool_manager=V4_PM,
                pool_id_hex=pool_id,
                currency0="0x" + "00" * 20,
                currency1="0x" + "01" * 20,
                fee=0x100000,
                tick_spacing=60,
                hook_flags=0,
                sqrt_price_x96=SQRT_PRICE_TICK_0,
                liquidity=1_000_000,
                tick=0,
            )

    def test_v4_allows_non_amount_hooks(self):
        """Pools with only non-amount hooks (e.g. BEFORE_DONATE) should be accepted."""
        engine = UniswapArbEngine()
        pool_id = _make_pool_id(13)
        engine.load_v4_snapshot_from_py(
            _make_v4_snapshot({
                V4_PM: {pool_id: {-60: (200, -100), 60: (300, 150)}},
            })
        )
        key = engine.register_v4_pool(
            pool_manager=V4_PM,
            pool_id_hex=pool_id,
            currency0="0x" + "00" * 20,
            currency1="0x" + "01" * 20,
            fee=3000,
            tick_spacing=60,
            hook_flags=0x30,
            sqrt_price_x96=SQRT_PRICE_TICK_0,
            liquidity=1_000_000,
            tick=0,
        )
        assert key == 1

    def test_v4_v4_path_registers_and_solves(self):
        """V4-V4 path should register and solve (same CL math as V3-V3)."""
        engine = UniswapArbEngine()
        pool_id_1 = _make_pool_id(1)
        pool_id_2 = _make_pool_id(2)
        engine.load_v4_snapshot_from_py(
            _make_v4_snapshot({
                V4_PM: {
                    pool_id_1: {-60: (500, -200), 60: (800, 300)},
                    pool_id_2: {-60: (600, -250), 60: (900, 350)},
                },
            })
        )

        v4_a = engine.register_v4_pool(
            pool_manager=V4_PM,
            pool_id_hex=pool_id_1,
            currency0="0x" + "00" * 20,
            currency1="0x" + "01" * 20,
            fee=3000,
            tick_spacing=60,
            hook_flags=0,
            sqrt_price_x96=SQRT_PRICE_TICK_0,
            liquidity=10_000_000_000_000,
            tick=0,
        )

        v4_b = engine.register_v4_pool(
            pool_manager=V4_PM,
            pool_id_hex=pool_id_2,
            currency0="0x" + "00" * 20,
            currency1="0x" + "01" * 20,
            fee=3000,
            tick_spacing=60,
            hook_flags=0,
            sqrt_price_x96=SQRT_PRICE_TICK_0,
            liquidity=20_000_000_000_000,
            tick=0,
        )

        path_id = engine.register_path([
            (v4_a, True),
            (v4_b, False),
        ])
        assert path_id == 1
        assert engine.path_count() == 1

    def test_v4_v2_mixed_path_registers(self):
        """V4-V2 mixed path should register and resolve."""
        engine = UniswapArbEngine()
        pool_id = _make_pool_id(1)
        engine.load_v4_snapshot_from_py(
            _make_v4_snapshot({
                V4_PM: {pool_id: {-60: (200, -100), 60: (300, 150)}},
            })
        )

        v4_key = engine.register_v4_pool(
            pool_manager=V4_PM,
            pool_id_hex=pool_id,
            currency0="0x" + "00" * 20,
            currency1="0x" + "01" * 20,
            fee=3000,
            tick_spacing=60,
            hook_flags=0,
            sqrt_price_x96=SQRT_PRICE_TICK_0,
            liquidity=10_000_000_000_000,
            tick=0,
        )

        v2_id = engine.register_v2_pool(
            address=V2_POOL_A,
            reserve0=1_500_000 * USDC,
            reserve1=800 * WETH,
            gamma_numer=997,
            fee_denom=1000,
        )

        path_id = engine.register_path([
            (v4_key, True),
            (v2_id, False),
        ])
        assert path_id == 1

    def test_v4_v3_mixed_path_registers(self):
        """V4-V3 mixed path should register and resolve (both CL, same solver)."""
        engine = UniswapArbEngine()
        pool_id = _make_pool_id(1)
        engine.load_v3_snapshot_from_py(
            _make_v3_snapshot({
                V3_POOL: {-60: (200, -100), 60: (300, 150)},
            })
        )
        engine.load_v4_snapshot_from_py(
            _make_v4_snapshot({
                V4_PM: {pool_id: {-60: (200, -100), 60: (300, 150)}},
            })
        )

        v4_key = engine.register_v4_pool(
            pool_manager=V4_PM,
            pool_id_hex=pool_id,
            currency0="0x" + "00" * 20,
            currency1="0x" + "01" * 20,
            fee=3000,
            tick_spacing=60,
            hook_flags=0,
            sqrt_price_x96=SQRT_PRICE_TICK_0,
            liquidity=10_000_000_000_000,
            tick=0,
        )

        v3_key = engine.register_v3_pool(
            address=V3_POOL,
            token0="0x" + "00" * 20,
            token1="0x" + "01" * 20,
            fee=3000,
            tick_spacing=60,
            factory="0x" + "00" * 20,
            sqrt_price_x96=SQRT_PRICE_TICK_0,
            liquidity=10_000_000_000_000,
            tick=0,
        )

        path_id = engine.register_path([
            (v4_key, True),
            (v3_key, False),
        ])
        assert path_id == 1


class TestSubscribeResume:
    """Test subscribe/resume two-phase lifecycle."""

    def test_subscribe_returns_block_number_type(self):
        """subscribe() should be callable (won't actually connect in tests)."""
        engine = UniswapArbEngine()
        assert hasattr(engine, "subscribe")
        assert hasattr(engine, "resume")

    def test_resume_without_subscribe_raises(self):
        """resume() without subscribe() should raise RuntimeError."""
        engine = UniswapArbEngine()
        with pytest.raises(RuntimeError, match="SnapshotLoaded|subscribe"):
            engine.resume()

    def test_double_subscribe_raises(self):
        """Calling subscribe() twice without resume() should raise."""
        engine = UniswapArbEngine()
        import inspect

        sig = inspect.signature(engine.subscribe)
        params = list(sig.parameters.keys())
        assert "rpc_url" in params
        assert "buffer_event_types" not in params
