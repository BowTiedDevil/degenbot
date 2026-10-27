"""Public structural Pool cutover contract.

These tests exercise the registered ``Pool`` handle as the companion boundary:
reads are atomic structural views and writes are verb-shaped commands. Family
identity still comes from the handle so companion construction can remain a
single-argument seam.
"""

from __future__ import annotations

from fractions import Fraction

from tests.helpers.erc20_factory import make_erc20
from tests.helpers.pool_handle_factory import make_balancer_weighted_handle, make_v2_handle
from tests.helpers.seam import REGISTRATION_SURFACE as ffi
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


def test_get_pool_returns_the_structural_handle_and_retires_prototype_accessor() -> None:
    handle = _reserve_pair()

    assert type(handle) is Pool
    assert not hasattr(handle, "reserve0")
    assert not hasattr(handle, "snapshot")
    assert not hasattr(Bot(1), "py_pool")


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
    assert not hasattr(companion._py_pool, "snapshot")
    assert not hasattr(companion._py_pool, "sync_reserves")


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


def test_pool_class_write_doors_are_gone() -> None:
    """State moves through the pump only — no pool-class write door survives.

    The handle is a read surface: events land via ``Bot.dispatch_log`` (and
    reorgs via ``Bot.dispatch_reorg_log``), so a companion-shaped set of write
    methods on the handle would be a second, diverging write path.
    """
    retired = (
        "apply_swap",
        "apply_sync",
        "apply_liquidity_update",
        "update_tick_data",
        "ensure_word_known",
        "discard_before_block",
        "restore_before_block",
    )
    for handle in (_reserve_pair(), _balance_vector()):
        for door in retired:
            assert not hasattr(handle, door), door
    # The one write left is the registration anchor production still calls.
    assert hasattr(_reserve_pair(), "seed_genesis")


def test_legacy_liquidity_pool_handle_is_not_registered() -> None:
    assert not hasattr(ffi, "LiquidityPool")
