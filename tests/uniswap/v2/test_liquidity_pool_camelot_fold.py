"""ADR-005 slice 7 step 4b — the folded Camelot stable strategy lives on
``LiquidityPool`` after the subclass collapse (step 4a folded; step 4b deleted
``CamelotLiquidityPool``).

These are the surviving fold regression tests (the 4a PARITY tests — which
compared a folded ``LiquidityPool`` against a reference
``CamelotLiquidityPool`` — were transitional + deleted when the subclass
went away in step 4b). They cover the structural contract of the fold:

- volatile (``stable_swap=False``) → ``ConstantProductHop`` + Rust-delegated
  calc (the slice-5 hot path, unperturbed by the fold).
- stable (``stable_swap=True``) → ``SolidlyStableHop`` (the Camelot
  solidly-stable branch, folded into the base class).
"""

from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING

from degenbot.degenbot_rs import PyBot
from degenbot.types.hop_types import ConstantProductHop, SolidlyStableHop
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool

if TYPE_CHECKING:
    from degenbot.erc20.erc20 import Erc20Token

_PY_BOT = PyBot()

_CAMELOT_FACTORY = "0x6EcCab422D763aC031210895C81787E87B43A652"


def _make_token(addr: str, *, symbol: str, decimals: int) -> Erc20Token:
    return make_erc20(_PY_BOT, addr, name=symbol, symbol=symbol, decimals=decimals)


class TestFoldedToHopState:
    """The folded LiquidityPool.to_hop_state picks the right hop by stable_swap."""

    def test_volatile_pool_returns_constant_product_hop(self) -> None:
        """stable_swap=False (default) → ConstantProductHop (unchanged base)."""
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

    def test_stable_pool_returns_solidly_stable_hop(self) -> None:
        """stable_swap=True → SolidlyStableHop (the folded Camelot branch)."""
        token0 = _make_token("0x" + "0a" * 20, symbol="TK0", decimals=18)
        token1 = _make_token("0x" + "0b" * 20, symbol="TK1", decimals=6)
        folded = make_v2_pool(
            "0x" + "c3" * 20,
            token0=token0,
            token1=token1,
            factory=_CAMELOT_FACTORY,
            fee_token0=Fraction(5, 1000),
            fee_token1=Fraction(7, 1000),
            reserves_token0=5_000_000_000_000,
            reserves_token1=8_000_000_000_000,
        )
        folded.stable_swap = True
        folded.fee_denominator = 1000
        hop = folded.to_hop_state(zero_for_one=True)
        assert isinstance(hop, SolidlyStableHop)


class TestFoldedVolatileCalcUnchanged:
    """A volatile LiquidityPool (stable_swap=False) still delegates calc to Rust.

    The fold must NOT perturb the slice-5 Rust-delegation hot path for the
    99% case (non-stable V2 pools).
    """

    def test_volatile_calc_matches_direct_rust_call(self) -> None:
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
        direct = pool._py_pool.calculate_tokens_out(
            zero_for_one=True, amount_in=amount
        )
        # The folded LiquidityPool.calculate_tokens_out_from_tokens_in (routes
        # via super() → UniswapV2PoolCalc → Rust, when stable_swap=False).
        folded = pool.calculate_tokens_out_from_tokens_in(pool.token0, amount)
        assert folded == direct


class TestFoldedStableHopSwapFn:
    """The folded Camelot stable ``to_hop_state`` swap_fn must actually work.

    Pre-fix (TODO-7ea2e7d9): the stable branch passed ``get_y_camelot``
    (3-arg) to ``calc_exact_in_stable``, which calls ``get_y_func`` with 5
    args — so the hop's ``swap_fn`` raised ``TypeError`` for every input
    (dead code). The direct stable calc
    (``_calculate_tokens_out_from_tokens_in_stable_swap``) bypasses
    ``calc_exact_in_stable`` and calls ``k_camelot``/``get_y_camelot``
    directly with the right arity, so it is the value-parity reference.
    """

    def test_stable_hop_swap_fn_matches_direct_stable_calc(self) -> None:
        """The hop's swap_fn must return the same value as the direct calc.

        Red before the arity fix: ``swap_fn(amount_in)`` raises ``TypeError``
        (get_y_camelot arity mismatch). Green after: value parity with the
        direct stable calc.
        """
        token0 = _make_token("0x" + "0a" * 20, symbol="TK0", decimals=18)
        token1 = _make_token("0x" + "0b" * 20, symbol="TK1", decimals=6)
        folded = make_v2_pool(
            "0x" + "d2" * 20,
            token0=token0,
            token1=token1,
            factory=_CAMELOT_FACTORY,
            fee_token0=Fraction(5, 1000),
            fee_token1=Fraction(7, 1000),
            reserves_token0=5_000_000_000_000,
            reserves_token1=8_000_000_000_000,
        )
        folded.stable_swap = True
        folded.fee_denominator = 1000

        hop = folded.to_hop_state(zero_for_one=True)
        assert isinstance(hop, SolidlyStableHop)
        assert hop.swap_fn is not None

        amount_in = 1_000_000_000_000
        # Reference: the direct stable calc (calls k_camelot/get_y_camelot
        # with the correct arity — unaffected by the bug).
        reference = folded._calculate_tokens_out_from_tokens_in_stable_swap(
            token_in=folded.token0,
            token_in_quantity=amount_in,
        )
        # Hop swap_fn (the previously-broken path through calc_exact_in_stable).
        via_hop = hop.swap_fn(amount_in)
        assert via_hop == reference

    def test_stable_hop_swap_fn_does_not_raise(self) -> None:
        """The stable hop swap_fn returns a non-negative int (smoke).

        Guards against the arity regression returning (any call raising
        ``TypeError``) without depending on the value-parity reference.
        """
        token0 = _make_token("0x" + "1a" * 20, symbol="TK0", decimals=6)
        token1 = _make_token("0x" + "1b" * 20, symbol="TK1", decimals=6)
        folded = make_v2_pool(
            "0x" + "d3" * 20,
            token0=token0,
            token1=token1,
            factory=_CAMELOT_FACTORY,
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1_000_000_000_000,
            reserves_token1=1_000_000_000_000,
        )
        folded.stable_swap = True
        folded.fee_denominator = 1000

        hop = folded.to_hop_state(zero_for_one=False)
        assert isinstance(hop, SolidlyStableHop)
        assert hop.swap_fn is not None
        out = hop.swap_fn(10_000)
        assert isinstance(out, int)
        assert out >= 0
