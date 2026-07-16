"""PyBot pool-admission exceptions through the Python seam (F2EVV6).

Companion to ``test_v4_admission_exceptions.py`` (which pins the class
hierarchy shape) — this file exercises the *actual* PyO3 seam so a typed
``PoolRegistrationError`` subclass surfaces when a caller registers an
out-of-spec or duplicate pool via ``PyBot.register_v{2,3,4}_pool``.

The WOYYS2 epic (MSTAT2 / 24KNGF / K3IICB) made every
``register_vx_pool`` a typed ``Result<u64, RegisterVxPoolError>`` and gave
every variant set a ``SpecViolation`` that wraps the shared
``spec_bounds::SpecViolation { field, value, bound }``. F2EVV6 promoted the
stop-gap ``PyValueError`` mappers to a typed ``PoolRegistrationError``
hierarchy shared across V2/V3/V4:

    ValueError
    └─ PoolRegistrationError
       ├─ HookedPoolRejectedError       (V4 admission — amount-modifying hook)
       ├─ DynamicFeePoolRejectedError    (V4 admission — dynamic fee)
       ├─ PoolAlreadyRegisteredError     (V2/V3/V4 duplicate at registration)
       └─ SpecViolationError            (V2/V3/V4 out-of-spec field)

So Python callers can now ``except PoolRegistrationError:`` to scope just
admission refusals (a broad ``except ValueError:`` still catches them — backward
compatible with ``build_paths``'s existing skip-one-rejection-at-a-time net).

Cross-checked V2/V3 registration vectors (``UNISWAP_V2_FACTORY`` /
``V2_DAI_WETH_ADDR`` etc.) come from ``tests/registry/test_registration_verify``
so the CREATE2-verify step upstream of `register_vx_pool` passes — the spec
validator in the Rust core is what gets to fire. V4 admission doesn't do
CREATE2 verify (pool_manager + pool_id + pool_key tuple), so its vectors are
arbitrary.
"""

from __future__ import annotations

import pytest

import degenbot.exceptions
from degenbot.bot import PyBot
from degenbot.exceptions import (
    DynamicFeePoolRejectedError,
    HookedPoolRejectedError,
    PoolAlreadyRegisteredError,
    PoolRegistrationError,
    SpecViolationError,
)

# Cross-checked mainnet V2/V3 vectors (see tests/registry/test_registration_verify).
UNISWAP_V2_FACTORY = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
UNISWAP_V3_FACTORY = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
DAI = "0x6B175474E89094C44Da98b954EedeAC495271d0F"
USDC = "0xA0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"
V2_DAI_WETH_ADDR = "0xA478c2975Ab1Ea89e8196811F51A7B7Ade33eB11"
V3_UNI_USDC_WETH_500_ADDR = "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640"
# In-spec V3 sqrt price at the 1:1 tick (`1 << 96`); the previously-sloppy
# `sqrt_price_x96 = 1` was below MIN_SQRT_RATIO and is now rejected.
V3_SQRT_1_TO_1 = 1 << 96

# `uint112(-1)` — the V2 on-chain reserve bound (`UniswapV2Pair._update` asserts
# `balance <= uint112(-1)`). `1 << 112` is one above it.
UINT112_MAX = (1 << 112) - 1
V2_RESERVE_OVER_112BIT = 1 << 112


# -----------------------------------------------------------------------
# Hierarchy shape — pins the F2EVV6 class tree + the reparenting of the
# V4-specific names under the new shared base.
# -----------------------------------------------------------------------


def test_pool_registration_error_is_exposed_and_is_value_error() -> None:
    assert hasattr(degenbot.exceptions, "PoolRegistrationError")
    assert issubclass(PoolRegistrationError, ValueError)


def test_spec_violation_error_and_already_registered_are_exposed() -> None:
    assert hasattr(degenbot.exceptions, "SpecViolationError")
    assert hasattr(degenbot.exceptions, "PoolAlreadyRegisteredError")
    assert issubclass(SpecViolationError, PoolRegistrationError)
    assert issubclass(PoolAlreadyRegisteredError, PoolRegistrationError)


def test_v4_admission_errors_reparented_under_pool_registration_error() -> None:
    """F2EVV6 reparents the V4-specific names under `PoolRegistrationError`.

    A broad `except PoolRegistrationError:` must now catch V4 admission
    rejections too, not just the new V2/V3 spec/dup variants.
    """
    assert issubclass(HookedPoolRejectedError, PoolRegistrationError)
    assert issubclass(DynamicFeePoolRejectedError, PoolRegistrationError)


@pytest.mark.parametrize(
    "exc_name",
    [
        "PoolRegistrationError",
        "PoolAlreadyRegisteredError",
        "SpecViolationError",
        "HookedPoolRejectedError",
        "DynamicFeePoolRejectedError",
    ],
)
def test_admission_errors_catchable_as_value_error(exc_name: str) -> None:
    """The whole hierarchy subclasses ValueError (backward compat with the
    broad `except ValueError:` net in `build_paths`)."""
    exc_type = getattr(degenbot.exceptions, exc_name)
    msg = "rejected"
    with pytest.raises(ValueError):  # noqa: PT011 — broad catch is the contract
        raise exc_type(msg)


# -----------------------------------------------------------------------
# Seam-triggered tests — exercise the actual PyBot.register_v{2,3,4}_pool
# path so the F2EVV6 typed mappers surface the right subclass at the boundary.
# -----------------------------------------------------------------------


class TestV2SeamAdmission:
    def test_duplicate_address_raises_pool_already_registered(self) -> None:
        bot = PyBot(chain_id=1)
        bot.register_v2_pool(
            address=V2_DAI_WETH_ADDR,
            token0=WETH,
            token1=DAI,
            reserve0=1,
            reserve1=2,
            gamma_numer0=997,
            fee_denom0=1000,
            gamma_numer1=997,
            fee_denom1=1000,
            factory=UNISWAP_V2_FACTORY,
        )
        # Second registration at the same address: the Rust core's
        # `AlreadyRegistered` rejection now surfaces (was an `assert!` panic
        # pre-MSTAT2) as the typed `PoolAlreadyRegisteredError`.
        with pytest.raises(PoolAlreadyRegisteredError) as exc_info:
            bot.register_v2_pool(
                address=V2_DAI_WETH_ADDR,
                token0=WETH,
                token1=DAI,
                reserve0=1,
                reserve1=2,
                gamma_numer0=997,
                fee_denom0=1000,
                gamma_numer1=997,
                fee_denom1=1000,
                factory=UNISWAP_V2_FACTORY,
            )
        assert "already registered" in str(exc_info.value).lower()
        assert str(V2_DAI_WETH_ADDR).lower() in str(exc_info.value).lower()

    def test_overlarge_reserve0_raises_spec_violation_error(self) -> None:
        """V2 reserves are `uint112` on-chain; > `uint112(-1)` is rejected up
        front (MSTAT2 admission floor)."""
        bot = PyBot(chain_id=1)
        with pytest.raises(SpecViolationError) as exc_info:
            bot.register_v2_pool(
                address=V2_DAI_WETH_ADDR,
                token0=WETH,
                token1=DAI,
                reserve0=V2_RESERVE_OVER_112BIT,
                reserve1=2,
                gamma_numer0=997,
                fee_denom0=1000,
                gamma_numer1=997,
                fee_denom1=1000,
                factory=UNISWAP_V2_FACTORY,
            )
        msg = str(exc_info.value)
        assert "reserve0" in msg
        assert "uint112" in msg


class TestV3SeamAdmission:
    def _in_spec_kwargs(self) -> dict:
        return {
            "address": V3_UNI_USDC_WETH_500_ADDR,
            "token0": USDC,
            "token1": WETH,
            "fee": 500,
            "tick_spacing": 10,
            "factory": UNISWAP_V3_FACTORY,
            "sqrt_price_x96": V3_SQRT_1_TO_1,
            "liquidity": 0,
            "tick": 0,
        }

    def test_duplicate_address_raises_pool_already_registered(self) -> None:
        bot = PyBot(chain_id=1)
        bot.register_v3_pool(**self._in_spec_kwargs())
        with pytest.raises(PoolAlreadyRegisteredError):
            bot.register_v3_pool(**self._in_spec_kwargs())

    def test_out_of_spec_sqrt_price_raises_spec_violation_error(self) -> None:
        bot = PyBot(chain_id=1)
        kw = self._in_spec_kwargs()
        kw["sqrt_price_x96"] = 1  # below MIN_SQRT_RATIO = 4_295_128_740
        with pytest.raises(SpecViolationError) as exc_info:
            bot.register_v3_pool(**kw)
        assert "sqrtPriceX96" in str(exc_info.value)

    def test_out_of_range_tick_raises_spec_violation_error(self) -> None:
        bot = PyBot(chain_id=1)
        kw = self._in_spec_kwargs()
        kw["tick"] = -887_273  # one below MIN_TICK = -887_272
        with pytest.raises(SpecViolationError) as exc_info:
            bot.register_v3_pool(**kw)
        assert "tick" in str(exc_info.value)

    def test_sqrt_price_at_max_raises_spec_violation_error(self) -> None:
        bot = PyBot(chain_id=1)
        kw = self._in_spec_kwargs()
        # MAX_SQRT_RATIO is the strict upper bound (TickMath rejects `>= MAX`).
        # `sqrt_price_x96` is snapshot state, not part of CREATE2 — so verify_v3
        # still passes, leaving the Rust core's `validate_sqrt_price` to reject.
        sqrt_ratio_max = 1461446703485210103287273052203988822378723970342
        kw["sqrt_price_x96"] = sqrt_ratio_max
        with pytest.raises(SpecViolationError) as exc_info:
            bot.register_v3_pool(**kw)
        assert "sqrtPriceX96" in str(exc_info.value)

    def test_out_of_range_tick_spacing_raises_spec_violation_error(self) -> None:
        bot = PyBot(chain_id=1)
        kw = self._in_spec_kwargs()
        kw["tick_spacing"] = 32_768  # one above MAX_TICK_SPACING = 32_767
        with pytest.raises(SpecViolationError) as exc_info:
            bot.register_v3_pool(**kw)
        assert "tickSpacing" in str(exc_info.value)


class TestV4SeamAdmission:
    """V4 admission doesn't do CREATE2 verify; the V4 vectors are arbitrary
    (the team manager + pool_id just need to be fresh per test)."""

    PM = "0x" + "44" * 20
    POOL_ID = "0x" + "ee" * 32
    C0 = "0x" + "00" * 20
    C1 = "0x" + "01" * 20

    def _in_spec_kwargs(self, pool_id_hex: str = POOL_ID) -> dict:
        return {
            "pool_manager": self.PM,
            "pool_id_hex": pool_id_hex,
            "currency0": self.C0,
            "currency1": self.C1,
            "fee": 500,
            "tick_spacing": 10,
            "hook_flags": 0,
            "sqrt_price_x96": V3_SQRT_1_TO_1,
            "liquidity": 1_000_000,
            "tick": 0,
            "block": 0,
        }

    def test_duplicate_raises_pool_already_registered(self) -> None:
        bot = PyBot(chain_id=1)
        bot.register_v4_pool(**self._in_spec_kwargs())
        with pytest.raises(PoolAlreadyRegisteredError):
            bot.register_v4_pool(**self._in_spec_kwargs())

    def test_out_of_spec_sqrt_price_raises_spec_violation_error(self) -> None:
        bot = PyBot(chain_id=1)
        kw = self._in_spec_kwargs("0x" + "e1" * 32)
        kw["sqrt_price_x96"] = 1
        with pytest.raises(SpecViolationError) as exc_info:
            bot.register_v4_pool(**kw)
        assert "sqrtPriceX96" in str(exc_info.value)

    def test_overlarge_v4_fee_raises_spec_violation_else_than_dynamic_fee(
        self,
    ) -> None:
        """V4 `fee >= 1 << 24` (`V4_FEE_MAX`) is rejected as `SpecViolationError`,
        NOT as `DynamicFeePoolRejectedError` — the `0x800000` high bit is the
        dynamic-fee flag (handled as a separate, more-specific rejection);
        `fee >= 1 << 24` is a different and more primitive kind of out-of-spec."""
        bot = PyBot(chain_id=1)
        kw = self._in_spec_kwargs("0x" + "e2" * 32)
        kw["fee"] = 1 << 24  # V4_FEE_MAX
        with pytest.raises(SpecViolationError) as exc_info:
            bot.register_v4_pool(**kw)
        assert "fee" in str(exc_info.value)

    def test_hooked_pool_still_raises_hooked_pool_rejected_error(self) -> None:
        """Pin that F2EVV6 didn't accidentally reparent HookedPoolRejectedError
        out of its admission-floor nature — a hooked pool surfaces the same
        typed V4 variant as before (now also a PoolRegistrationError). The
        hooked check runs AFTER the spec validators, so the in-spec sqrt/fee/
        tick/spacing must pass first for the hooked variant to fire."""
        bot = PyBot(chain_id=1)
        kw = self._in_spec_kwargs("0x" + "e3" * 32)
        kw["hook_flags"] = 0x80  # BEFORE_SWAP — amount-modifying
        with pytest.raises(HookedPoolRejectedError):
            bot.register_v4_pool(**kw)
