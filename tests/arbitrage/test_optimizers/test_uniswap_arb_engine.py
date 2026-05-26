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
        engine.initial_solve(block_number=1)
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
            v4_swap_updates=[],
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

        # Initial solve
        engine.initial_solve(block_number=1)

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


class TestUniswapArbEngineV4:
    """Test V4 pool registration, hook filtering, and path solving."""

    # PoolManager address on Ethereum mainnet
    POOL_MANAGER = "0x000000000004444c5dc75cB358380D2e3De08A90"

    def _make_pool_id(self, suffix: int) -> str:
        """Generate a 32-byte pool ID as hex string."""
        return "0x" + (b"\x00" * 31 + bytes([suffix])).hex()

    def test_register_v4_pool_returns_key(self):
        engine = UniswapArbEngine()
        key = engine.register_v4_pool(
            pool_manager=self.POOL_MANAGER,
            pool_id_hex=self._make_pool_id(1),
            currency0="0x" + "00" * 20,
            currency1="0x" + "01" * 20,
            fee=3000,
            tick_spacing=60,
            hook_flags=0,
            sqrt_price_x96=SQRT_PRICE_TICK_0,
            liquidity=1_000_000,
            tick=0,
            tick_data={-60: (200, -100), 60: (300, 150)},
        )
        # Forward + reverse orientations: key=1 (forward), key=2 (reverse)
        assert key == 1
        assert engine.v4_pool_count() == 1

    def test_v4_hook_filtering_rejects_amount_modifying(self):
        """Pools with amount-modifying hooks should be rejected."""
        import pytest

        engine = UniswapArbEngine()

        # BEFORE_SWAP (0x80) — should be rejected
        with pytest.raises(ValueError, match="amount-modifying hooks"):
            engine.register_v4_pool(
                pool_manager=self.POOL_MANAGER,
                pool_id_hex=self._make_pool_id(10),
                currency0="0x" + "00" * 20,
                currency1="0x" + "01" * 20,
                fee=3000,
                tick_spacing=60,
                hook_flags=0x80,  # BEFORE_SWAP
                sqrt_price_x96=SQRT_PRICE_TICK_0,
                liquidity=1_000_000,
                tick=0,
                tick_data={},
            )

        # AFTER_SWAP_RETURNS_DELTA (0x04) — should be rejected
        with pytest.raises(ValueError, match="amount-modifying hooks"):
            engine.register_v4_pool(
                pool_manager=self.POOL_MANAGER,
                pool_id_hex=self._make_pool_id(11),
                currency0="0x" + "00" * 20,
                currency1="0x" + "01" * 20,
                fee=3000,
                tick_spacing=60,
                hook_flags=0x04,  # AFTER_SWAP_RETURNS_DELTA
                sqrt_price_x96=SQRT_PRICE_TICK_0,
                liquidity=1_000_000,
                tick=0,
                tick_data={},
            )

    def test_v4_dynamic_fee_rejected(self):
        """Pools with dynamic fees (0x100000) should be rejected."""
        import pytest

        engine = UniswapArbEngine()

        with pytest.raises(ValueError, match="dynamic fee"):
            engine.register_v4_pool(
                pool_manager=self.POOL_MANAGER,
                pool_id_hex=self._make_pool_id(12),
                currency0="0x" + "00" * 20,
                currency1="0x" + "01" * 20,
                fee=0x100000,  # dynamic fee flag
                tick_spacing=60,
                hook_flags=0,
                sqrt_price_x96=SQRT_PRICE_TICK_0,
                liquidity=1_000_000,
                tick=0,
                tick_data={},
            )

    def test_v4_allows_non_amount_hooks(self):
        """Pools with only non-amount hooks (e.g. BEFORE_DONATE) should be accepted."""
        engine = UniswapArbEngine()

        key = engine.register_v4_pool(
            pool_manager=self.POOL_MANAGER,
            pool_id_hex=self._make_pool_id(13),
            currency0="0x" + "00" * 20,
            currency1="0x" + "01" * 20,
            fee=3000,
            tick_spacing=60,
            hook_flags=0x30,  # BEFORE_DONATE | AFTER_DONATE
            sqrt_price_x96=SQRT_PRICE_TICK_0,
            liquidity=1_000_000,
            tick=0,
            tick_data={-60: (200, -100), 60: (300, 150)},
        )
        assert key == 1

    def test_v4_v4_path_registers_and_solves(self):
        """V4-V4 path should register and solve (same CL math as V3-V3)."""
        engine = UniswapArbEngine()

        v4_a = engine.register_v4_pool(
            pool_manager=self.POOL_MANAGER,
            pool_id_hex=self._make_pool_id(1),
            currency0="0x" + "00" * 20,
            currency1="0x" + "01" * 20,
            fee=3000,
            tick_spacing=60,
            hook_flags=0,
            sqrt_price_x96=SQRT_PRICE_TICK_0,
            liquidity=10_000_000_000_000,
            tick=0,
            tick_data={-60: (500, -200), 60: (800, 300)},
        )

        v4_b = engine.register_v4_pool(
            pool_manager=self.POOL_MANAGER,
            pool_id_hex=self._make_pool_id(2),
            currency0="0x" + "00" * 20,
            currency1="0x" + "01" * 20,
            fee=3000,
            tick_spacing=60,
            hook_flags=0,
            sqrt_price_x96=SQRT_PRICE_TICK_0,
            liquidity=20_000_000_000_000,
            tick=0,
            tick_data={-60: (600, -250), 60: (900, 350)},
        )

        path_id = engine.register_path(
            [
                ("V4", v4_a, True),
                ("V4", v4_b, False),
            ]
        )
        assert path_id == 1
        assert engine.path_count() == 1

    def test_v4_v2_mixed_path_registers(self):
        """V4-V2 mixed path should register and resolve."""
        engine = UniswapArbEngine()

        v4_key = engine.register_v4_pool(
            pool_manager=self.POOL_MANAGER,
            pool_id_hex=self._make_pool_id(1),
            currency0="0x" + "00" * 20,
            currency1="0x" + "01" * 20,
            fee=3000,
            tick_spacing=60,
            hook_flags=0,
            sqrt_price_x96=SQRT_PRICE_TICK_0,
            liquidity=10_000_000_000_000,
            tick=0,
            tick_data={-60: (200, -100), 60: (300, 150)},
        )

        v2_id = engine.register_v2_pool(
            address="0x" + "11" * 20,
            reserve0=1_500_000 * USDC,
            reserve1=800 * WETH,
            gamma_numer=997,
            fee_denom=1000,
        )

        path_id = engine.register_path(
            [
                ("V4", v4_key, True),
                ("V2", v2_id, False),
            ]
        )
        assert path_id == 1

    def test_v4_v3_mixed_path_registers(self):
        """V4-V3 mixed path should register and resolve (both CL, same solver)."""
        engine = UniswapArbEngine()

        v4_key = engine.register_v4_pool(
            pool_manager=self.POOL_MANAGER,
            pool_id_hex=self._make_pool_id(1),
            currency0="0x" + "00" * 20,
            currency1="0x" + "01" * 20,
            fee=3000,
            tick_spacing=60,
            hook_flags=0,
            sqrt_price_x96=SQRT_PRICE_TICK_0,
            liquidity=10_000_000_000_000,
            tick=0,
            tick_data={-60: (200, -100), 60: (300, 150)},
        )

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

        path_id = engine.register_path(
            [
                ("V4", v4_key, True),
                ("V3", v3_key, False),
            ]
        )
        assert path_id == 1
