"""``UniswapV2Pool.state`` raises after the pool is unregistered (ADR-007).

``state`` reads one atomic Rust snapshot (``_py_pool.snapshot()``) and raises
``DegenbotValueError`` when the snapshot is ``None``. The only reachable path
to ``None`` for a validly-constructed companion is post-deregistration:
``unregister_pool`` drops the whole ``PoolEntry`` (identity + journal) from
``BotState.pools`` (ADR-005 identity/state split footer + ADR-007), so the
companion is a stale handle whose ``pool_id`` no longer resolves.

This test pins the load-bearing ``None``-check (the runtime half of the
deregistration contract) — without it the companion would surface
``TypeError: cannot unpack non-iterable NoneType`` instead of a typed error.
"""

from fractions import Fraction

import pytest

from degenbot.checksum_cache import get_checksum_address
from degenbot._ffi import PyBot
from degenbot.exceptions import DegenbotValueError
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool


class TestStateRaisesAfterUnregister:
    """``state`` raises ``DegenbotValueError`` once the Rust state is gone."""

    def test_state_raises_after_unregister(self) -> None:
        """A deregistered pool's ``state`` raises, not unpack-NoneType."""
        py_bot = PyBot()

        token0 = make_erc20(
            py_bot=py_bot,
            address="0x" + "1" * 40,
            name="Token0",
            symbol="T0",
            decimals=18,
            chain_id=1,
        )
        token1 = make_erc20(
            py_bot=py_bot,
            address="0x" + "2" * 40,
            name="Token1",
            symbol="T1",
            decimals=18,
            chain_id=1,
        )

        pool_address = "0x" + "a" * 40
        pool = make_v2_pool(
            address=pool_address,
            token0=token0,
            token1=token1,
            factory="0x" + "f" * 40,
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1_000_000,
            reserves_token1=2_000_000,
            chain_id=1,
            py_bot=py_bot,
        )

        # Sanity: the pool is live, state reads cleanly before unregister.
        assert pool.state.reserves_token0 == 1_000_000

        # ADR-007: dropping the PoolEntry removes the snapshot source.
        removed = py_bot.unregister_pool(address=get_checksum_address(pool_address))
        assert removed is True

        # The companion is now a stale handle — the only reachable path to
        # ``snapshot() is None``. Fail loudly with a typed error rather than
        # ``TypeError: cannot unpack non-iterable NoneType``.
        with pytest.raises(DegenbotValueError, match="No V2 pool state available"):
            pool.state

    def test_state_reads_cleanly_before_unregister(self) -> None:
        """Guard against the test becoming vacuous: live pools read state.

        If ``snapshot()`` ever returned ``None`` for a live pool (e.g. the
        ``pool_id`` failed to resolve for a registered entry), the
        ``None``-check would raise on the happy path — this test would surface
        that regression independently of the unregister path.
        """
        py_bot = PyBot()

        token0 = make_erc20(
            py_bot=py_bot,
            address="0x" + "3" * 40,
            name="A",
            symbol="A",
            decimals=18,
            chain_id=1,
        )
        token1 = make_erc20(
            py_bot=py_bot,
            address="0x" + "4" * 40,
            name="B",
            symbol="B",
            decimals=18,
            chain_id=1,
        )

        pool = make_v2_pool(
            address="0x" + "b" * 40,
            token0=token0,
            token1=token1,
            factory="0x" + "f" * 40,
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1_000,
            reserves_token1=2_000,
            chain_id=1,
            py_bot=py_bot,
        )

        state = pool.state
        assert state.reserves_token0 == 1_000
        assert state.reserves_token1 == 2_000
        # A registered pool always has a non-zero update block.
        assert state.block >= 0

    def test_unregister_unknown_address_is_silent_noop(self) -> None:
        """Unregistering an unknown address returns False (ADR-007 U2 asymmetry).

        Confirms the precondition for the ``state`` test: the ``True`` return
        we assert there is meaningful, not vacuous — unregister distinguishes
        removed-vs-never-registered.
        """
        py_bot = PyBot()
        removed = py_bot.unregister_pool(address=get_checksum_address("0x" + "9" * 40))
        assert removed is False


def test_from_py_pool_still_usable_after_unregister() -> None:
    """The companion's identity getters survive deregistration; only ``state`` raises.

    ADR-005's identity/state split: ``BotState`` drops the whole ``PoolEntry``
    on unregister, but the companion caches identity off the handle at
    ``_from_py_pool`` time. So identity reads (``address``/``factory``/fees/
    tokens) keep working post-unregister; only the live-state read (``state``
    /``update_block``) raises. This is the contract the ``state`` ``None``
    -check enforces at runtime.
    """
    py_bot = PyBot()

    token0 = make_erc20(
        py_bot=py_bot,
        address="0x" + "5" * 40,
        name="A",
        symbol="A",
        decimals=18,
        chain_id=1,
    )
    token1 = make_erc20(
        py_bot=py_bot,
        address="0x" + "6" * 40,
        name="B",
        symbol="B",
        decimals=18,
        chain_id=1,
    )

    pool_address = "0x" + "c" * 40
    pool = make_v2_pool(
        address=pool_address,
        token0=token0,
        token1=token1,
        factory="0x" + "f" * 40,
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=1_000,
        reserves_token1=2_000,
        chain_id=1,
        py_bot=py_bot,
    )

    checksum_address = get_checksum_address(pool_address)
    factory_checksum = get_checksum_address("0x" + "f" * 40)

    py_bot.unregister_pool(address=checksum_address)

    # Identity reads off the handle (cached identity / handle still alive) —
    # these must NOT raise; only ``state`` (live Rust snapshot) does.
    assert pool.address == checksum_address
    assert pool.factory == factory_checksum
    assert pool.fee_token0 == Fraction(3, 1000)
    assert pool.fee_token1 == Fraction(3, 1000)

    with pytest.raises(DegenbotValueError, match="No V2 pool state available"):
        pool.state
