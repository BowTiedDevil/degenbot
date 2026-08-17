"""BalancerV2StablePool direct construction is forbidden (ADR-005 sealed seam).

A ``BalancerV2StablePool`` is a Python companion over a Rust-owned
``LiquidityPool`` handle. The handle can only be produced by registering a
pool in a ``RustBot`` (production: ``BalancerBuilder``; tests:
``make_balancer_stable_pool``). Direct constructor calls are rejected so that
the only paths to a pool instance are the ones that wire the handle —
mirroring Polars' ``_from_pydf`` pattern. The seam also asserts the handle is
the Balancer-stable family, so a V2/weighted/Curve handle raises.
"""

from unittest.mock import MagicMock

import pytest

from degenbot.balancer.stable_pools import BalancerV2StablePool
from degenbot.bot import RustBot
from degenbot.exceptions import DegenbotValueError
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool


class TestDirectConstructionForbidden:
    """``BalancerV2StablePool(...)`` raises ``TypeError`` with a helpful message."""

    def test_no_args_raises_type_error(self) -> None:
        """Calling the constructor with no arguments is rejected."""
        with pytest.raises(TypeError) as exc_info:
            BalancerV2StablePool()

        message = str(exc_info.value)
        assert "Bot.build_pool" in message
        assert "make_balancer_stable_pool" in message

    def test_with_handle_and_kwargs_raises_type_error(self) -> None:
        """Passing a fake handle + the old kwarg shape is rejected."""
        fake_handle = MagicMock()
        with pytest.raises(TypeError) as exc_info:
            BalancerV2StablePool(
                fake_handle,
                address="0x" + "0" * 40,
                pool_id=b"\x00" * 32,
                vault="0x" + "b" * 40,
                tokens=[MagicMock()],
                fee=MagicMock(),
                amp=100,
                scaling_factors=[10**18],
            )
        assert "make_balancer_stable_pool" in str(exc_info.value)


class TestWrongFamilyHandle:
    """``_from_py_pool`` rejects a non-Balancer-stable handle."""

    def test_v2_handle_raises_degenbot_value_error(self) -> None:
        """A V2-family handle is not a Balancer stable pool."""
        bot = RustBot()
        t0 = make_erc20(bot, address="0x" + "a1" * 20, name="T0", symbol="T0", decimals=18)
        t1 = make_erc20(bot, address="0x" + "b2" * 20, name="T1", symbol="T1", decimals=18)
        v2_pool = make_v2_pool(
            "0x" + "c3" * 20,
            token0=t0,
            token1=t1,
            factory="0x" + "ff" * 20,
            fee_token0=__import__("fractions").Fraction(3, 1000),
            fee_token1=__import__("fractions").Fraction(3, 1000),
            reserves_token0=10**18,
            reserves_token1=10**18,
            py_bot=bot,
        )

        # The V2 pool's handle is a V2-family handle; wrapping it as a
        # Balancer stable companion must raise.
        with pytest.raises(DegenbotValueError) as exc_info:
            BalancerV2StablePool._from_py_pool(v2_pool._py_pool)

        message = str(exc_info.value)
        assert "Balancer stable" in message


class TestRateProviderIsStored:
    """The rate provider is the stored Rust trait object, read off the handle."""

    def test_static_pool_reports_static_provider(self) -> None:
        """A pool with no injected provider reports the static fallback."""
        from tests.helpers.balancer_pool_factory import make_balancer_stable_pool

        bot = RustBot()
        t0 = make_erc20(bot, address="0x" + "d4" * 20, name="S0", symbol="S0", decimals=18)
        t1 = make_erc20(bot, address="0x" + "e5" * 20, name="S1", symbol="S1", decimals=18)
        pool = make_balancer_stable_pool(
            address="0x" + "f6" * 20,
            pool_id=bytes.fromhex("f6" * 20 + "0002" + "0" * 20),
            vault="0x" + "ba" * 20,
            tokens=[t0, t1],
            balances=[10**18, 2 * 10**18],
            fee=__import__("fractions").Fraction(3, 1000),
            amp=100_000,
            scaling_factors=[10**18, 10**18],
            py_bot=bot,
        )

        # No live provider → static fallback → these all report "no I/O".
        assert pool._rate_provider_is_static is True
        assert pool.rate_provider is None
        assert not pool.requires_io_at_calculation_time
        # If token list does include BPT... here it doesn't, so also check.

    def test_dynamic_pool_reports_non_static(self) -> None:
        """A pool with an injected provider reports the stored (dynamic) provider."""

        class _CaptureProvider:
            def __init__(self, rates: tuple[int, ...]) -> None:
                self._rates = rates

            def get_rates(self, block_identifier=None):
                return self._rates

        from tests.helpers.balancer_pool_factory import make_balancer_stable_pool

        bot = RustBot()
        t0 = make_erc20(bot, address="0x" + "d4" * 20, name="S0", symbol="S0", decimals=18)
        t1 = make_erc20(bot, address="0x" + "e5" * 20, name="S1", symbol="S1", decimals=18)
        # ComposableStablePool (bpt_idx=0) + dynamic provider.
        rates = (10**18, 2 * 10**18, 3 * 10**18)
        pool = make_balancer_stable_pool(
            address="0x" + "07" * 20,
            pool_id=bytes.fromhex("07" * 20 + "0002" + "0" * 20),
            vault="0x" + "ba" * 20,
            tokens=[t0, t1],
            balances=[10**18, 2 * 10**18],
            fee=__import__("fractions").Fraction(3, 1000),
            amp=100_000,
            scaling_factors=[10**18, 2 * 10**18],
            bpt_idx=None,
            rate_provider=_CaptureProvider(rates),
            py_bot=bot,
        )

        assert pool._rate_provider_is_static is False
        assert pool.rate_provider is not None
        # The adapter delegates get_rates to the stored Rust trait object.
        assert pool.rate_provider.get_rates(42) == rates
