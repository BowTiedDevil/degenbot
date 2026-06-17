"""Tests for token_in/token_out pair selection in Curve pool to_hop_state().

Verifies that:
- Default (no token_in/token_out) uses zero_for_one for (0,1)/(1,0) pair selection
- Explicit token_in/token_out selects the specified pair in N-token pools
- Both-or-neither validation raises DegenbotValueError
- Non-top-level tokens raise DegenbotValueError with clear message
- swap_fn closure captures the correct indices
"""

from fractions import Fraction

import pytest

from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.degenbot_rs import PyBot
from degenbot.exceptions import DegenbotValueError
from degenbot.types.hop_types import PoolInvariant
from tests.fakes.curve_data_provider import FakeCurveDataProvider
from tests.fakes.tokens import FakeToken
from tests.helpers.erc20_factory import make_erc20

_PY_BOT = PyBot()

# --- Production token instances ---

USDC = make_erc20(
    _PY_BOT,
    "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
    name="USD Coin",
    symbol="USDC",
    decimals=6,
)
USDT = make_erc20(
    _PY_BOT,
    "0xdAC17F958D2ee523a2206206994597C13D831ec7",
    name="Tether USD",
    symbol="USDT",
    decimals=6,
)
DAI = make_erc20(
    _PY_BOT,
    "0x6B175474E89094C44Da98b954EedeAC495271d0F",
    name="DAI",
    symbol="DAI",
    decimals=18,
)

STATE_BLOCK = 18_000_000


def _make_2coin_pool() -> CurveStableswapPool:
    """2-coin pool: USDC/USDT (both 6 decimals)."""
    provider = FakeCurveDataProvider(block_timestamp=1_700_000_000)
    return CurveStableswapPool(
        address="0x00000000000000000000000000000000000000c1",  # type: ignore[arg-type]
        tokens=(USDC, USDT),
        a_coefficient=1000,
        fee=4_000_000,
        admin_fee=5_000_000_000,
        balances=(10_000_000 * 10**6, 10_000_000 * 10**6),
        state_block=STATE_BLOCK,
        data_provider=provider,
    )


def _make_3coin_pool() -> CurveStableswapPool:
    """3-coin pool: DAI/USDC/USDT."""
    provider = FakeCurveDataProvider(block_timestamp=1_700_000_000)
    return CurveStableswapPool(
        address="0x00000000000000000000000000000000000000c2",  # type: ignore[arg-type]
        tokens=(DAI, USDC, USDT),
        a_coefficient=1000,
        fee=4_000_000,
        admin_fee=5_000_000_000,
        balances=(5_000_000 * 10**18, 5_000_000 * 10**6, 5_000_000 * 10**6),
        state_block=STATE_BLOCK,
        data_provider=provider,
    )


def test_to_hop_state_default_pair_zero_for_one():
    """to_hop_state without token_in/token_out uses (0, 1) when zero_for_one=True."""
    pool = _make_2coin_pool()
    hop = pool.to_hop_state(zero_for_one=True)
    assert hop.token_index_in == 0
    assert hop.token_index_out == 1


def test_to_hop_state_default_pair_one_for_zero():
    """to_hop_state without token_in/token_out uses (1, 0) when zero_for_one=False."""
    pool = _make_2coin_pool()
    hop = pool.to_hop_state(zero_for_one=False)
    assert hop.token_index_in == 1
    assert hop.token_index_out == 0


def test_to_hop_state_explicit_pair():
    """to_hop_state with token_in/token_out selects the specified pair."""
    pool = _make_3coin_pool()
    hop = pool.to_hop_state(
        zero_for_one=True,
        token_in=pool.tokens[0],
        token_out=pool.tokens[2],
    )
    assert hop.token_index_in == 0
    assert hop.token_index_out == 2


def test_to_hop_state_explicit_reverse_pair():
    """to_hop_state with reversed token_in/token_out."""
    pool = _make_3coin_pool()
    hop = pool.to_hop_state(
        zero_for_one=False,
        token_in=pool.tokens[2],
        token_out=pool.tokens[0],
    )
    assert hop.token_index_in == 2
    assert hop.token_index_out == 0


def test_to_hop_state_explicit_middle_pair():
    """to_hop_state with non-edge token pair (1, 2)."""
    pool = _make_3coin_pool()
    hop = pool.to_hop_state(
        zero_for_one=True,
        token_in=pool.tokens[1],
        token_out=pool.tokens[2],
    )
    assert hop.token_index_in == 1
    assert hop.token_index_out == 2


def test_to_hop_state_both_or_neither_token_in_only():
    """Providing only token_in (without token_out) raises DegenbotValueError."""
    pool = _make_3coin_pool()
    with pytest.raises(DegenbotValueError, match="both be provided"):
        pool.to_hop_state(zero_for_one=True, token_in=pool.tokens[0])


def test_to_hop_state_both_or_neither_token_out_only():
    """Providing only token_out (without token_in) raises DegenbotValueError."""
    pool = _make_3coin_pool()
    with pytest.raises(DegenbotValueError, match="both be provided"):
        pool.to_hop_state(zero_for_one=True, token_out=pool.tokens[2])


def test_to_hop_state_non_top_level_token_in():
    """Passing a token not in self.tokens as token_in raises DegenbotValueError."""
    pool = _make_2coin_pool()
    other_token = FakeToken(
        address="0xdead000000000000000000000000000000000000",
        symbol="OTH",
    )
    with pytest.raises(DegenbotValueError, match="not a top-level pool token"):
        pool.to_hop_state(
            zero_for_one=True,
            token_in=other_token,  # type: ignore[arg-type]
            token_out=pool.tokens[1],
        )


def test_to_hop_state_non_top_level_token_out():
    """Passing a token not in self.tokens as token_out raises DegenbotValueError."""
    pool = _make_2coin_pool()
    other_token = FakeToken(
        address="0xdead000000000000000000000000000000000000",
        symbol="OTH",
    )
    with pytest.raises(DegenbotValueError, match="not a top-level pool token"):
        pool.to_hop_state(
            zero_for_one=True,
            token_in=pool.tokens[0],
            token_out=other_token,  # type: ignore[arg-type]
        )


def test_to_hop_state_swap_fn_uses_correct_indices():
    """The swap_fn closure captures the correct i, j indices."""
    pool = _make_3coin_pool()
    hop = pool.to_hop_state(
        zero_for_one=True,
        token_in=pool.tokens[0],
        token_out=pool.tokens[2],
    )
    # swap_fn should call get_dy(i=0, j=2, ...)
    dx = 10**18
    expected = pool.get_dy(0, 2, dx, block_identifier=STATE_BLOCK)
    assert hop.swap_fn(dx) == expected


def test_to_hop_state_reserves_match_selected_pair():
    """Reserve_in and reserve_out match the balances for the selected pair."""
    pool = _make_3coin_pool()
    hop = pool.to_hop_state(
        zero_for_one=True,
        token_in=pool.tokens[1],
        token_out=pool.tokens[2],
    )
    assert hop.reserve_in == pool.balances[1]
    assert hop.reserve_out == pool.balances[2]


def test_to_hop_state_invariant_is_curve_stableswap():
    """Hop invariant is CURVE_STABLESWAP regardless of pair selection method."""
    pool = _make_3coin_pool()
    hop = pool.to_hop_state(
        zero_for_one=True,
        token_in=pool.tokens[0],
        token_out=pool.tokens[2],
    )
    assert hop.invariant == PoolInvariant.CURVE_STABLESWAP


def test_to_hop_state_fee_matches_pool_fee():
    """Fee is correctly represented as a Fraction."""
    pool = _make_3coin_pool()
    hop = pool.to_hop_state(
        zero_for_one=True,
        token_in=pool.tokens[0],
        token_out=pool.tokens[2],
    )
    assert hop.fee == Fraction(pool.fee, pool.FEE_DENOMINATOR)
