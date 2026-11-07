"""The pool handle's swap-calc refusal contract: typed errors, computed zeros.

Every collapsed read outcome — an unknown pool, a not-computable swap, a
failed word fetch, an exhausted recovery — raises through
``Pool.calculate_tokens_out`` / ``Pool.calculate_tokens_in`` /
``Pool.calculate_tokens_out_with_fetch``. ``0`` is returned only for a
successfully computed zero, so a failing pool cannot masquerade as a zero
quote. No live RPC: pools register in an in-memory ``Bot`` with fake fetchers.
"""

from __future__ import annotations

import pytest

from tests.helpers.pool_handle_factory import make_v2_handle
from tests.helpers.seam import Bot

_TOKEN0 = "0x" + "a" * 40
_TOKEN1 = "0x" + "b" * 40


def _sparse_v3_pool(bot: Bot, *, tick_data_fetcher=None):
    """A sparse V3 pool (empty tick data): the first calc must miss."""
    pool_id = bot.register_v3_pool(
        address="0x" + "c" * 40,
        token0=_TOKEN0,
        token1=_TOKEN1,
        fee=3000,
        tick_spacing=60,
        factory="0x" + "d" * 40,
        sqrt_price_x96=1 << 96,
        liquidity=1_000_010_000_000,
        tick=0,
        tick_data_fetcher=tick_data_fetcher,
    )
    return bot.get_pool(pool_id)


def test_unknown_pool_refuses_in_every_calc() -> None:
    bot = Bot(chain_id=1)
    address = "0x" + "1" * 40
    pool = make_v2_handle(bot=bot, address=address)
    assert bot.unregister_pool(address)

    for refused in (
        lambda: pool.calculate_tokens_out(zero_for_one=True, amount_in=100),
        lambda: pool.calculate_tokens_in(zero_for_one=True, amount_out=100),
        lambda: pool.calculate_tokens_out_with_fetch(
            zero_for_one=True,
            amount_in=100,
            block=100,
        ),
    ):
        with pytest.raises(ValueError, match=r"is not registered"):
            refused()


def test_not_computable_overdraw_refuses_not_zero() -> None:
    # amount_out == the output reserve: the exact-output overdraw is the
    # not-computable class, never a zero quote.
    pool = make_v2_handle()
    with pytest.raises(ValueError, match=r"not computable"):
        pool.calculate_tokens_in(zero_for_one=True, amount_out=2_000_000_000)


def test_math_overflow_refuses_not_zero() -> None:
    # 2**250 fits the signed request but overflows the constant-product
    # multiply: the on-chain-revert class raises.
    pool = make_v2_handle()
    with pytest.raises(ValueError, match=r"overflowed uint256"):
        pool.calculate_tokens_out(zero_for_one=True, amount_in=2**250)


def test_fetch_failed_refuses_not_zero() -> None:
    def failing_fetcher(word: int, block: int) -> dict[int, tuple[int, int, int]]:
        raise RuntimeError

    bot = Bot(chain_id=1)
    pool = _sparse_v3_pool(bot, tick_data_fetcher=failing_fetcher)
    with pytest.raises(ValueError, match=r"fetch failed"):
        pool.calculate_tokens_out_with_fetch(zero_for_one=True, amount_in=1000, block=0)


def test_fetch_exhausted_refuses_not_zero() -> None:
    bot = Bot(chain_id=1)
    pool = _sparse_v3_pool(bot)
    with pytest.raises(ValueError, match=r"recovery exhausted"):
        pool.calculate_tokens_out(zero_for_one=True, amount_in=1000)
    with pytest.raises(ValueError, match=r"recovery exhausted"):
        pool.calculate_tokens_out_with_fetch(zero_for_one=True, amount_in=1000, block=0)


def test_computed_zero_still_succeeds() -> None:
    # The zero request is the computed 0 -> 0, and a floored output is a
    # computed zero (input consumed, output rounds to nothing). Both return
    # 0 — the only zeros the calc surfaces.
    pool = make_v2_handle()
    assert int(pool.calculate_tokens_out(zero_for_one=True, amount_in=0)) == 0
    assert int(pool.calculate_tokens_in(zero_for_one=True, amount_out=0)) == 0

    floored = make_v2_handle(
        address="0x" + "3" * 40,
        reserve0=1_000_000,
        reserve1=1_000_000,
    )
    assert int(floored.calculate_tokens_out(zero_for_one=True, amount_in=1)) == 0


if __name__ == "__main__":
    pytest.main([__file__, "-vv"])
