"""ADR-005 slice 9 — V4 calc delegation to ``LiquidityPool``.

The V4 swap-calc path (``UniswapV4PoolCalc`` mixin is pricing-only by design;
swap math lives on ``UniswapV4Pool``) routes both
``calculate_tokens_out_from_tokens_in`` and
``calculate_tokens_in_from_tokens_out`` through the Rust
``LiquidityPool.simulate_swap_with_fetch`` /
``simulate_exact_output_swap_with_fetch`` seams.

This is the **delegation-detection** counterpart to the parity tests in
``tests/test_v4_rust_swap_parity.py`` (which pin the *numbers*). Per the
three-layer rubric §4.5, a routing cutover where the math already has a parity
test should add a spy that proves the seam was hit with the right args — not
just "the result matches". Mirrors
``tests/uniswap/v2/test_v2_pool_io_free.py::TestV2CalcDelegation``.
"""

from __future__ import annotations

import eth_abi.abi

from degenbot._ffi import Bot
from degenbot.constants import ZERO_ADDRESS
from degenbot.crypto import keccak256
from degenbot.uniswap.concentrated.types import LiquidityAtTick
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v4_pool_factory import make_v4_pool

# 1:1 price (sqrt_price_x96 = 2**96), tick 0, 0.3% fee, tick_spacing 60.
_SQRT_PRICE_1TO1 = 1 << 96
_LIQUIDITY = 10_000_000_000_000
_FEE = 3000
_TICK_SPACING = 60
_V4_POOL_MANAGER = "0x000000000004444c5dc75cB358380D2e3dE08A90"
# No-fee Dynamic-fee flag is 0x100000; a static non-zero LP fee is encoded
# directly in the upper bits of the pool-id-derived hook flags. The factories
# pass ``hook_flags`` via ``hook_address``; ZERO_ADDRESS ⇒ static fee mode.
_LP_FEE = _FEE


def _compute_v4_pool_id(
    currency0: str,
    currency1: str,
    fee: int,
    tick_spacing: int,
    hooks: str,
) -> str:
    """Mirror UniswapV4Pool's pool-id derivation so the test pool validates."""
    return (
        "0x"
        + keccak256(
            eth_abi.abi.encode(
                types=["address", "address", "uint24", "int24", "address"],
                args=[currency0, currency1, fee, tick_spacing, hooks],
            ),
        ).hex()
    )


def _make_tokens(py_bot: Bot, tag: str):
    token0 = make_erc20(
        py_bot,
        address=f"0x{(tag * 40)[:40]}",
        name="T0",
        symbol="T0",
        decimals=18,
    )
    token1 = make_erc20(
        py_bot,
        address=f"0x{(tag * 39 + 'e')[:40]}",
        name="T1",
        symbol="T1",
        decimals=18,
    )
    return token0, token1


class _DelegateSpy:
    """Wraps a ``LiquidityPool`` to record V4 swap-seam calls.

    Pass-through for all other handle methods via ``__getattr__``. Records
    ``simulate_swap_with_fetch`` (exact-input) and
    ``simulate_exact_output_swap_with_fetch`` (exact-output) invocations.
    """

    def __init__(self, real_pool):
        self._real = real_pool
        self.swap_with_fetch_calls: list[tuple[bool, int]] = []
        self.exact_output_with_fetch_calls: list[tuple[bool, int]] = []

    def simulate_swap_with_fetch(self, *, zero_for_one, amount_in, **_):
        self.swap_with_fetch_calls.append((bool(zero_for_one), int(amount_in)))
        return self._real.simulate_swap_with_fetch(
            zero_for_one=zero_for_one,
            amount_in=amount_in,
            **_,
        )

    def simulate_exact_output_swap_with_fetch(self, *, zero_for_one, amount_out, **_):
        self.exact_output_with_fetch_calls.append((bool(zero_for_one), int(amount_out)))
        return self._real.simulate_exact_output_swap_with_fetch(
            zero_for_one=zero_for_one,
            amount_out=amount_out,
            **_,
        )

    def __getattr__(self, name):
        return getattr(self._real, name)


def _make_dense_v4_pool(py_bot: Bot):
    token0, token1 = _make_tokens(py_bot, tag="4")
    pool_id = _compute_v4_pool_id(
        token0.address,
        token1.address,
        _FEE,
        _TICK_SPACING,
        ZERO_ADDRESS,
    )
    tick_data = {
        -60: LiquidityAtTick(
            liquidity_net=_LIQUIDITY,
            liquidity_gross=_LIQUIDITY,
            block=0,
        ),
        60: LiquidityAtTick(
            liquidity_net=-_LIQUIDITY,
            liquidity_gross=_LIQUIDITY,
            block=0,
        ),
    }
    return make_v4_pool(
        pool_id=pool_id,
        pool_manager_address=_V4_POOL_MANAGER,
        token0=token0,
        token1=token1,
        fee=_FEE,
        tick_spacing=_TICK_SPACING,
        hook_address=None,
        sqrt_price_x96=_SQRT_PRICE_1TO1,
        tick=0,
        liquidity=_LIQUIDITY,
        protocol_fee_zero_for_one=0,
        protocol_fee_one_for_zero=0,
        lp_fee=_LP_FEE,
        tick_data=tick_data,
        py_bot=py_bot,
    )


class TestV4CalcDelegation:
    """V4 calc delegation — the mainline swap path hits the Rust seam."""

    def test_calculate_tokens_out_delegates_to_rust_no_override(self) -> None:
        """No override_state: ``calculate_tokens_out_from_tokens_in`` routes
        to ``LiquidityPool.simulate_swap_with_fetch`` (token0 in → zfo=True)."""
        py_bot = Bot()
        pool = _make_dense_v4_pool(py_bot)
        spy = _DelegateSpy(pool._py_pool)
        pool._py_pool = spy

        result = pool.calculate_tokens_out_from_tokens_in(
            token_in=pool.token0,
            token_in_quantity=1_000,
        )

        assert spy.swap_with_fetch_calls == [(True, 1_000)]
        assert spy.exact_output_with_fetch_calls == []
        assert result > 0

    def test_calculate_tokens_out_reverse_delegates_to_rust(self) -> None:
        """token1 in → zero_for_one=False, still delegates to the Rust seam."""
        py_bot = Bot()
        pool = _make_dense_v4_pool(py_bot)
        spy = _DelegateSpy(pool._py_pool)
        pool._py_pool = spy

        result = pool.calculate_tokens_out_from_tokens_in(
            token_in=pool.token1,
            token_in_quantity=1_000,
        )

        assert spy.swap_with_fetch_calls == [(False, 1_000)]
        assert result > 0

    def test_calculate_tokens_in_delegates_to_rust_no_override(self) -> None:
        """No override_state: ``calculate_tokens_in_from_tokens_out`` routes
        to ``LiquidityPool.simulate_exact_output_swap_with_fetch``
        (token1 out → zfo=True)."""
        py_bot = Bot()
        pool = _make_dense_v4_pool(py_bot)
        spy = _DelegateSpy(pool._py_pool)
        pool._py_pool = spy

        result = pool.calculate_tokens_in_from_tokens_out(
            token_out=pool.token1,
            token_out_quantity=1_000,
        )

        assert spy.exact_output_with_fetch_calls == [(True, 1_000)]
        assert spy.swap_with_fetch_calls == []
        assert result > 0

    def test_calculate_tokens_in_reverse_delegates_to_rust(self) -> None:
        """token0 out → zero_for_one=False, still delegates to the Rust seam."""
        py_bot = Bot()
        pool = _make_dense_v4_pool(py_bot)
        spy = _DelegateSpy(pool._py_pool)
        pool._py_pool = spy

        result = pool.calculate_tokens_in_from_tokens_out(
            token_out=pool.token0,
            token_out_quantity=1_000,
        )

        assert spy.exact_output_with_fetch_calls == [(False, 1_000)]
        assert result > 0
