"""The pricing ladder is one mixin, and every priced family satisfies PoolPricing."""

from __future__ import annotations

import inspect
from fractions import Fraction

import pytest

from degenbot.aerodrome.v2_pool_calc import AerodromeV2PoolCalc
from degenbot.exceptions import DegenbotValueError
from degenbot.types.pool_protocols import PoolPricing
from degenbot.types.price_ladder import PriceLadder
from degenbot.uniswap.v2_pool_calc import UniswapV2PoolCalc
from degenbot.uniswap.v3_pool_calc import UniswapV3PoolCalc
from degenbot.uniswap.v4_pool_calc import UniswapV4PoolCalc

_FAMILY_MIXINS = (
    AerodromeV2PoolCalc,
    UniswapV2PoolCalc,
    UniswapV3PoolCalc,
    UniswapV4PoolCalc,
)


class _FakeToken:
    """A token-shaped stand-in: the ladder compares tokens by identity."""

    def __init__(self, name: str, decimals: int) -> None:
        self.name = name
        self.decimals = decimals

    def __repr__(self) -> str:
        return f"_FakeToken({self.name})"


class _FixedRateLadder(PriceLadder):
    """A family leaf answering a fixed rate, so the ladder's own verbs are observable."""

    _token0 = _FakeToken("token0", 6)
    _token1 = _FakeToken("token1", 18)

    @property
    def tokens(self) -> tuple[_FakeToken, _FakeToken]:
        return (self._token0, self._token1)

    @property
    def state(self) -> object:
        return None

    def _absolute_rate(self, token: _FakeToken, state: object) -> Fraction:
        return Fraction(4)


class TestPoolPricingProtocol:
    def test_protocol_declares_the_ladder_verbs(self) -> None:
        """PoolPricing names the four surviving verbs and nothing else."""
        assert PoolPricing.__protocol_attrs__ == {
            "get_absolute_exchange_rate",
            "get_absolute_price",
            "get_nominal_exchange_rate",
            "get_nominal_price",
        }

    def test_every_verb_takes_the_token_first(self) -> None:
        """One call shape across families: (token, override_state)."""
        for verb in sorted(PoolPricing.__protocol_attrs__):
            params = list(inspect.signature(getattr(PoolPricing, verb)).parameters)
            assert params == ["self", "token", "override_state"]


class TestPriceLadderIsTheOneImplementation:
    def test_every_family_calc_mixin_inherits_the_ladder(self) -> None:
        """The four family mixins share ONE declaration of the verb family."""
        for mixin in _FAMILY_MIXINS:
            assert issubclass(mixin, PriceLadder), mixin.__name__

    def test_every_family_supplies_its_rate_leaf(self) -> None:
        """A family answers the one question the ladder leaves open."""
        for mixin in _FAMILY_MIXINS:
            assert "_absolute_rate" in vars(mixin), f"{mixin.__name__} has no rate leaf"

    def test_the_unknown_token_refusal_is_shared(self) -> None:
        """The ladder owns the refusal, so no family can drift from it."""
        ladder = _FixedRateLadder()

        with pytest.raises(DegenbotValueError, match="Unknown token"):
            ladder.get_absolute_exchange_rate(_FakeToken("stranger", 18))

    def test_price_is_the_reciprocal_of_the_rate(self) -> None:
        """The ladder's own relation: price = 1 / rate."""
        ladder = _FixedRateLadder()

        assert ladder.get_absolute_exchange_rate(ladder._token0) == 4
        assert ladder.get_absolute_price(ladder._token0) == Fraction(1, 4)

    def test_nominal_verbs_correct_for_the_pair_decimals(self) -> None:
        """The decimal correction is the ladder's, applied to the family's rate."""
        ladder = _FixedRateLadder()

        # token0 has 6 decimals against token1's 18: 10^18 / 10^6 scales the rate.
        assert ladder.get_nominal_exchange_rate(ladder._token0) == 4 * Fraction(10**18, 10**6)
        assert ladder.get_nominal_price(ladder._token0) == 1 / (4 * Fraction(10**18, 10**6))
