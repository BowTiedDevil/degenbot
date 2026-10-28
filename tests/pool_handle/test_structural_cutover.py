"""Public structural Pool handle contract.

These tests exercise the registered ``Pool`` handle as the companion boundary:
reads are atomic structural views and writes are verb-shaped commands. Family
identity still comes from the handle so companion construction can remain a
single-argument seam.

The surface asserted here is the PRESENT one. A read is an atomic view
(``reserve_pair()`` / ``balance_vector()`` / ``concentrated_liquidity()``), the one
write is the registration anchor ``seed_genesis``, and state changes arrive as
dispatched logs through ``Bot.dispatch_log`` rather than as setters on the handle.
"""

from __future__ import annotations

from fractions import Fraction

import pytest

from tests.helpers.erc20_factory import make_erc20
from tests.helpers.pool_handle_factory import make_balancer_weighted_handle, make_v2_handle
from tests.helpers.seam import Bot, Pool
from tests.helpers.synthetic_logs import v2_sync
from tests.helpers.v2_pool_factory import make_v2_pool


def _reserve_pair_bot() -> tuple[Bot, Pool]:
    """A registered V2 pool plus the Bot that owns it (the dispatch door)."""
    bot = Bot(1)
    return bot, make_v2_handle(bot=bot, reserve0=1_000, reserve1=2_000)


def _reserve_pair() -> Pool:
    return _reserve_pair_bot()[1]


def _balance_vector() -> Pool:
    return make_balancer_weighted_handle(
        address="0x3333333333333333333333333333333333333333",
        vault="0x4444444444444444444444444444444444444444",
        balances=(1_000, 2_000),
    )


def test_get_pool_returns_the_structural_handle() -> None:
    handle = _reserve_pair()

    assert type(handle) is Pool
    assert handle.reserve_pair().reserve0 == 1_000


def test_production_companion_uses_the_canonical_structural_handle() -> None:
    companion = make_v2_pool(
        address="0x" + "1" * 40,
        token0=make_erc20_for_pool("0x" + "2" * 40, "T0", 18),
        token1=make_erc20_for_pool("0x" + "3" * 40, "T1", 6),
        factory="0x" + "4" * 40,
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=10**18,
        reserves_token1=2 * 10**18,
    )

    assert type(companion._py_pool) is Pool
    assert companion._py_pool.reserve_pair().reserve1 == 2 * 10**18


def make_erc20_for_pool(address: str, symbol: str, decimals: int):
    return make_erc20(Bot(1), address, name=symbol, symbol=symbol, decimals=decimals)


def test_reserve_pair_view_is_the_atomic_read_and_a_dispatched_sync_is_the_command() -> None:
    bot, handle = _reserve_pair_bot()

    before = handle.reserve_pair()
    assert (before.reserve0, before.reserve1, before.update_block) == (1_000, 2_000, 100)

    # The ONLY write path: a WS Sync through the pump's own door.
    bot.dispatch_log(
        address="0x1111111111111111111111111111111111111111",
        block_number=101,
        **v2_sync(reserve0=3_000, reserve1=4_000),
    )

    after = handle.reserve_pair()
    assert (after.reserve0, after.reserve1, after.update_block) == (3_000, 4_000, 101)


def test_balance_vector_view_is_the_atomic_read_and_apply_balances_is_the_command() -> None:
    handle = _balance_vector()

    before = handle.balance_vector()
    assert (list(before.balances), before.update_block) == ([1_000, 2_000], 100)

    handle.apply_balances([3_000, 4_000], 101)

    after = handle.balance_vector()
    assert (list(after.balances), after.update_block) == ([3_000, 4_000], 101)


def test_the_handle_answers_its_family_view_and_refuses_a_foreign_one() -> None:
    """A read is the family's own atomic view; a foreign family is a loud refusal.

    The handle is a read surface: events land via ``Bot.dispatch_log`` (and reorgs
    via ``Bot.dispatch_reorg_log``), so a companion-shaped set of write methods on
    the handle would be a second, diverging write path. What it answers instead is
    the view for the family it was registered under.
    """
    v2 = _reserve_pair()
    assert v2.reserve_pair().reserve0 == 1_000

    weighted = _balance_vector()
    assert list(weighted.balance_vector().balances) == [1_000, 2_000]

    # A handle registered under one family refuses the other family's view, naming
    # the view it cannot answer (the Rust side raises the typed refusal).
    with pytest.raises(ValueError, match="not a balance-vector pool"):
        v2.balance_vector()
