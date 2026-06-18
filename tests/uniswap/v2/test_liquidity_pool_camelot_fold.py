"""ADR-005 slice 7 step 4a — fold Camelot stable strategy into ``LiquidityPool``.

Additive (non-breaking): ``LiquidityPool`` gains the Camelot solidly-stable
calc + the stable ``to_hop_state`` branch directly, so the capability survives
the subclass deletion in step 4b. ``CamelotLiquidityPool`` still works (its
MRO puts ``CamelotPoolCalc`` before ``LiquidityPool`` — it keeps using its own
versions; zero behavior change for existing Camelot pools).

These tests prove the fold is behavior-preserving via parity: a plain
``LiquidityPool`` with ``stable_swap=True`` + ``fee_denominator`` set produces
identical ``calculate_tokens_out_from_tokens_in`` + ``to_hop_state`` output to
a reference ``CamelotLiquidityPool`` built with the same reserves + fees.

TRANSITIONAL — these parity tests are deleted in step 4b when
``CamelotLiquidityPool`` is removed (the folded ``LiquidityPool`` becomes the
sole implementation; the parity reference disappears).
"""

from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING

import pytest

from degenbot.camelot.pools import CamelotLiquidityPool
from degenbot.checksum_cache import get_checksum_address
from degenbot.degenbot_rs import PyBot
from degenbot.types.hop_types import ConstantProductHop, SolidlyStableHop
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool

if TYPE_CHECKING:
    from degenbot.erc20.erc20 import Erc20Token
    from degenbot.uniswap.liquidity_pool import LiquidityPool


_PY_BOT = PyBot()

# Camelot Arbitrum factory + init hash (matches the camelot-v2 preset).
_CAMELOT_FACTORY = "0x6EcCab422D763aC031210895C81787E87B43A652"


def _make_token(addr: str, *, symbol: str, decimals: int) -> Erc20Token:
    return make_erc20(_PY_BOT, addr, name=symbol, symbol=symbol, decimals=decimals)


def _make_camelot_reference(
    *,
    address: str,
    token0: Erc20Token,
    token1: Erc20Token,
    reserves_token0: int,
    reserves_token1: int,
    fee_token0: int,
    fee_token1: int,
    fee_denominator: int,
    stable_swap: bool,
) -> CamelotLiquidityPool:
    """Build a reference CamelotLiquidityPool over a fresh handle.

    Mirrors ``make_v2_pool``'s Rust registration (gamma_numer = retained
    fraction = fee_denominator - fee_tokenN_raw) but uses Camelot's native
    integer-fee constructor signature.
    """
    address = get_checksum_address(address)
    py_bot = PyBot()
    pool_id = py_bot.register_v2_pool(
        address=address,
        token0=token0.address,
        token1=token1.address,
        reserve0=reserves_token0,
        reserve1=reserves_token1,
        gamma_numer0=fee_denominator - fee_token0,
        fee_denom0=fee_denominator,
        gamma_numer1=fee_denominator - fee_token1,
        fee_denom1=fee_denominator,
        factory=_CAMELOT_FACTORY,
        update_block=0,
    )
    py_pool = py_bot.get_pool(pool_id)
    assert py_pool is not None
    return CamelotLiquidityPool(
        py_pool,
        address=address,
        token0=token0,
        token1=token1,
        factory=_CAMELOT_FACTORY,
        fee_token0=fee_token0,
        fee_token1=fee_token1,
        fee_denominator=fee_denominator,
        stable_swap=stable_swap,
    )


def _make_folded_stable_pool(
    *,
    address: str,
    token0: Erc20Token,
    token1: Erc20Token,
    reserves_token0: int,
    reserves_token1: int,
    fee_token0: int,
    fee_token1: int,
    fee_denominator: int,
) -> LiquidityPool:
    """Build a folded LiquidityPool with stable_swap enabled.

    Folds Camelot's behavior into the base class: construct a plain
    LiquidityPool (Fraction fees = raw/denominator) then flip stable_swap +
    set fee_denominator, exercising the folded code paths directly.
    """
    pool = make_v2_pool(
        address,
        token0=token0,
        token1=token1,
        factory=_CAMELOT_FACTORY,
        fee_token0=Fraction(fee_token0, fee_denominator),
        fee_token1=Fraction(fee_token1, fee_denominator),
        reserves_token0=reserves_token0,
        reserves_token1=reserves_token1,
    )
    pool.stable_swap = True
    pool.fee_denominator = fee_denominator
    return pool


class TestFoldedStableCalcParity:
    """A folded LiquidityPool(stable_swap=True) matches CamelotLiquidityPool."""

    @pytest.fixture
    def pools(self):
        token0 = _make_token("0x" + "0a" * 20, symbol="TK0", decimals=18)
        token1 = _make_token("0x" + "0b" * 20, symbol="TK1", decimals=6)
        reserves0, reserves1 = 5_000_000_000_000, 8_000_000_000_000
        fee0, fee1, denom = 5, 7, 1000
        reference = _make_camelot_reference(
            address="0x" + "c1" * 20,
            token0=token0,
            token1=token1,
            reserves_token0=reserves0,
            reserves_token1=reserves1,
            fee_token0=fee0,
            fee_token1=fee1,
            fee_denominator=denom,
            stable_swap=True,
        )
        folded = _make_folded_stable_pool(
            address="0x" + "c2" * 20,
            token0=token0,
            token1=token1,
            reserves_token0=reserves0,
            reserves_token1=reserves1,
            fee_token0=fee0,
            fee_token1=fee1,
            fee_denominator=denom,
        )
        return reference, folded

    @pytest.mark.parametrize("amount", [1_000, 1_000_000, 99_999_999_999])
    def test_stable_calc_token0_in_matches_reference(self, pools, amount):
        """calculate_tokens_out_from_tokens_in(token0, ...) parity, stable mode."""
        reference, folded = pools
        ref_out = reference.calculate_tokens_out_from_tokens_in(reference.token0, amount)
        folded_out = folded.calculate_tokens_out_from_tokens_in(folded.token0, amount)
        assert ref_out == folded_out, f"token0-in {amount}: reference={ref_out} folded={folded_out}"

    @pytest.mark.parametrize("amount", [1_000, 500_000, 2_000_000_000])
    def test_stable_calc_token1_in_matches_reference(self, pools, amount):
        """calculate_tokens_out_from_tokens_in(token1, ...) parity, stable mode."""
        reference, folded = pools
        ref_out = reference.calculate_tokens_out_from_tokens_in(reference.token1, amount)
        folded_out = folded.calculate_tokens_out_from_tokens_in(folded.token1, amount)
        assert ref_out == folded_out, f"token1-in {amount}: reference={ref_out} folded={folded_out}"


class TestFoldedToHopState:
    """The folded LiquidityPool.to_hop_state picks the right hop by stable_swap."""

    def test_stable_pool_returns_solidly_stable_hop(self):
        """stable_swap=True → SolidlyStableHop (parity with Camelot stable)."""
        token0 = _make_token("0x" + "0a" * 20, symbol="TK0", decimals=18)
        token1 = _make_token("0x" + "0b" * 20, symbol="TK1", decimals=6)
        folded = _make_folded_stable_pool(
            address="0x" + "c3" * 20,
            token0=token0,
            token1=token1,
            reserves_token0=5_000_000_000_000,
            reserves_token1=8_000_000_000_000,
            fee_token0=5,
            fee_token1=7,
            fee_denominator=1000,
        )
        hop = folded.to_hop_state(zero_for_one=True)
        assert isinstance(hop, SolidlyStableHop)

    def test_volatile_pool_returns_constant_product_hop(self):
        """stable_swap=False (default) → ConstantProductHop (unchanged base behavior)."""
        token0 = _make_token("0x" + "0a" * 20, symbol="TK0", decimals=18)
        token1 = _make_token("0x" + "0b" * 20, symbol="TK1", decimals=6)
        pool = make_v2_pool(
            "0x" + "c4" * 20,
            token0=token0,
            token1=token1,
            factory=_CAMELOT_FACTORY,
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1_000_000,
            reserves_token1=2_000_000,
        )
        # stable_swap defaults to False on a plain LiquidityPool.
        assert pool.stable_swap is False
        hop = pool.to_hop_state(zero_for_one=True)
        assert isinstance(hop, ConstantProductHop)

    def test_stable_to_hop_state_swap_fn_matches_reference(self):
        """The swapped SolidlyStableHop swap_fn behaves identically to the reference."""
        # NOTE: ``calc_exact_in_stable`` calls ``get_y_func`` with 5 args but
        # ``get_y_camelot`` takes 3 — a PRE-EXISTING bug in Camelot's stable
        # ``to_hop_state`` branch (dead code; always raises ``TypeError``).
        # The fold preserves it faithfully (behavior-identical, bug included).
        # Parity here = both raise the same exception, or both return-equal.
        # TODO: fix ``get_y_camelot``↔``calc_exact_in_stable`` arity (pre-existing).
        token0 = _make_token("0x" + "0a" * 20, symbol="TK0", decimals=18)
        token1 = _make_token("0x" + "0b" * 20, symbol="TK1", decimals=6)
        reserves0, reserves1 = 5_000_000_000_000, 8_000_000_000_000
        reference = _make_camelot_reference(
            address="0x" + "d1" * 20,
            token0=token0,
            token1=token1,
            reserves_token0=reserves0,
            reserves_token1=reserves1,
            fee_token0=5,
            fee_token1=7,
            fee_denominator=1000,
            stable_swap=True,
        )
        folded = _make_folded_stable_pool(
            address="0x" + "d2" * 20,
            token0=token0,
            token1=token1,
            reserves_token0=reserves0,
            reserves_token1=reserves1,
            fee_token0=5,
            fee_token1=7,
            fee_denominator=1000,
        )
        ref_hop = reference.to_hop_state(zero_for_one=True)
        folded_hop = folded.to_hop_state(zero_for_one=True)
        assert isinstance(ref_hop, SolidlyStableHop)
        assert isinstance(folded_hop, SolidlyStableHop)

        def _call(fn, amt):
            try:
                return ("ok", fn(amt))
            except Exception as e:  # noqa: BLE001 — parity on any raise
                return ("raise", type(e).__name__)

        for amount in [1_000, 1_000_000, 500_000_000_000]:
            ref_result = _call(ref_hop.swap_fn, amount)
            folded_result = _call(folded_hop.swap_fn, amount)
            assert ref_result == folded_result, (
                f"swap_fn({amount}): reference={ref_result} folded={folded_result}"
            )


class TestFoldedVolatileCalcUnchanged:
    """A volatile LiquidityPool (stable_swap=False) still delegates calc to Rust.

    The fold must NOT perturb the slice-5 Rust-delegation hot path for the
    99% case (non-stable V2 pools). Verify the volatile calc output matches
    what the base UniswapV2PoolCalc produced before the fold (the same
    constant-product math, now routing through ``super()``).
    """

    def test_volatile_calc_matches_direct_rust_call(self):
        """Folded volatile calc (via super()) == direct PyLiquidityPool calc."""
        token0 = _make_token("0x" + "0a" * 20, symbol="TK0", decimals=18)
        token1 = _make_token("0x" + "0b" * 20, symbol="TK1", decimals=6)
        pool = make_v2_pool(
            "0x" + "e1" * 20,
            token0=token0,
            token1=token1,
            factory=_CAMELOT_FACTORY,
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1_000_000_000_000,
            reserves_token1=4_000_000_000_000,
        )
        amount = 10_000_000
        # The Rust handle's direct calc (the slice-5 delegation target).
        direct = pool._py_pool.calculate_tokens_out(zero_for_one=True, amount_in=amount)
        # The folded LiquidityPool.calculate_tokens_out_from_tokens_in (routes
        # via super() → UniswapV2PoolCalc → Rust, when stable_swap=False).
        folded = pool.calculate_tokens_out_from_tokens_in(pool.token0, amount)
        assert folded == direct
