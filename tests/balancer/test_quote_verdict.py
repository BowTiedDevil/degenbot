"""The Balancer quote verdict: the quoter states it, the companion verb renders it.

A quoter answers a ``QuoteVerdict`` — the computed amounts plus the species' fact
about how far the answer can be trusted — so no exception carries a swap amount.
The companion's ``calculate_tokens_*`` verbs are the public boundary: they unwrap
the verdict and render the stale-rate caveat as ``StaleRateResult``, whose payload
stays readable.
"""

from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING, Any

import pytest

from degenbot.balancer.pool_quoter import QuoteVerdict
from degenbot.exceptions.pool import StaleRateResult
from tests.helpers.balancer_pool_factory import (
    make_balancer_stable_pool,
    make_balancer_weighted_pool,
)
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.seam import Bot, to_checksum_address

if TYPE_CHECKING:
    from degenbot.balancer.pools import BalancerV2Pool
    from degenbot.balancer.stable_pools import BalancerV2StablePool

_VAULT = "0xBA12222222228d8Ba445958a75a0704d566BF2C8"
ONE = 10**18


def _addr(prefix: int) -> str:
    return to_checksum_address("0x" + f"{prefix:08x}" + "00" * 16)


def _token(bot: Bot, address: str, *, decimals: int, name: str) -> Any:
    return make_erc20(bot, address, name=name, symbol=name, decimals=decimals, chain_id=31337)


def _weighted_pool() -> BalancerV2Pool:
    bot = Bot()
    tokens = [
        _token(bot, _addr(0xCC10), decimals=18, name="WETH"),
        _token(bot, _addr(0xCC11), decimals=18, name="BAL"),
    ]
    return make_balancer_weighted_pool(
        _addr(0xCC12),
        pool_id=bytes(range(32)),
        vault=_VAULT,
        tokens=tokens,
        balances=[100_000 * ONE, 50_000 * ONE],
        fee=Fraction(3, 1000),
        weights=[50, 50],
    )


def _composable_pool() -> BalancerV2StablePool:
    bot = Bot()
    tokens = [
        _token(bot, _addr(0xCC20), decimals=18, name="TUSD"),
        _token(bot, _addr(0xCC21), decimals=18, name="BPT"),
        _token(bot, _addr(0xCC22), decimals=6, name="USDC"),
    ]
    return make_balancer_stable_pool(
        _addr(0xCC23),
        pool_id=bytes(range(32)),
        vault=_VAULT,
        tokens=tokens,
        balances=[100_000 * ONE, 10_000_000 * ONE, 200_000 * 10**6],
        fee=Fraction(3, 10000),
        amp=600 * 1000,
        scaling_factors=[ONE, ONE, ONE * 10 ** (18 - 6)],
        bpt_idx=1,
    )


def _metastable_pool() -> BalancerV2StablePool:
    bot = Bot()
    tokens = [
        _token(bot, _addr(0xCC30), decimals=18, name="wstETH"),
        _token(bot, _addr(0xCC31), decimals=18, name="WETH"),
    ]
    return make_balancer_stable_pool(
        _addr(0xCC32),
        pool_id=bytes(range(32)),
        vault=_VAULT,
        tokens=tokens,
        balances=[100_000 * ONE, 110_000 * ONE],
        fee=Fraction(4, 10000),
        amp=50_000,
        scaling_factors=[11 * 10**17, ONE],
        bpt_idx=None,
    )


def test_weighted_quoter_states_an_exact_verdict() -> None:
    """A species with no caveat states an exact verdict; the companion returns its amount."""
    pool = _weighted_pool()
    verdict = pool.swap_quoter.exact_in(pool.tokens[0], pool.tokens[1], ONE)
    assert isinstance(verdict, QuoteVerdict)
    assert verdict.stale_rates is False
    assert verdict.amount_in == ONE
    assert verdict.amount_out > 0
    assert (
        pool.calculate_tokens_out_from_tokens_in(pool.tokens[0], pool.tokens[1], ONE)
        == verdict.amount_out
    )


def test_metastable_quoter_states_an_exact_verdict() -> None:
    """A MetaStable pool's near-static rates carry no stale-rate caveat."""
    pool = _metastable_pool()
    verdict = pool.swap_quoter.exact_in(pool.tokens[0], pool.tokens[1], ONE)
    assert verdict.stale_rates is False
    assert (
        pool.calculate_tokens_out_from_tokens_in(pool.tokens[0], pool.tokens[1], ONE)
        == verdict.amount_out
    )


def test_composable_quoter_states_stale_rates_instead_of_raising() -> None:
    """The quoter answers the stale-rate caveat as a value, not an exception."""
    pool = _composable_pool()
    verdict = pool.swap_quoter.exact_in(pool.tokens[0], pool.tokens[2], ONE)
    assert verdict.stale_rates is True
    assert verdict.amount_in == ONE
    assert verdict.amount_out > 0


def test_companion_renders_the_stale_rate_verdict() -> None:
    """The companion's verb raises the caveat as ``StaleRateResult``, payload intact."""
    pool = _composable_pool()
    verdict = pool.swap_quoter.exact_in(pool.tokens[0], pool.tokens[2], ONE)
    with pytest.raises(StaleRateResult) as exc_info:
        pool.calculate_tokens_out_from_tokens_in(
            token_in=pool.tokens[0],
            token_out=pool.tokens[2],
            token_in_quantity=ONE,
        )
    assert exc_info.value.amount_in == verdict.amount_in
    assert exc_info.value.amount_out == verdict.amount_out


def test_companion_renders_the_stale_rate_verdict_given_out() -> None:
    """The given-out verb renders the same caveat, naming the reverse side's amount."""
    pool = _composable_pool()
    amount_out = 200_000 * 10**6 // 100
    verdict = pool.swap_quoter.exact_out(pool.tokens[0], pool.tokens[2], amount_out)
    assert verdict.stale_rates is True
    assert verdict.amount_out == amount_out
    with pytest.raises(StaleRateResult) as exc_info:
        pool.calculate_tokens_in_from_tokens_out(
            token_in=pool.tokens[0],
            token_out=pool.tokens[2],
            token_out_quantity=amount_out,
        )
    assert exc_info.value.amount_in == verdict.amount_in
    assert exc_info.value.amount_out == verdict.amount_out
