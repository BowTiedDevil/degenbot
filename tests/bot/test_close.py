"""Behavioral tests for Bot's context-manager / close() resource lifecycle.

``with Bot(...) as bot:`` exit (and an explicit ``close()``) must:

(a) close the provider connection,
(b) remove + dispose the scoped DB session,
(c) release the tracker/registry caches,
(d) be idempotent, and
(e) never suppress an exception raised by the ``with`` body.

These are behavioural postcondition tests against the *real* objects Bot
owns — a real ``DatabaseSessionManager`` bound to a real SQLAlchemy engine, a
real ``UniswapV2PoolTracker`` populated through its own ``get_pool()`` path,
and real Rust-registered pools. The only stand-in is the injected provider
(the external RPC boundary, whose real ``AlloyProvider.close()`` is an
unobservable no-op). No call-count assertions on doubles.
"""

from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING

import pytest

from degenbot.bot import Bot
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.exceptions.pool import PoolNotAssociated
from degenbot.uniswap.trackers import UniswapV2PoolTracker
from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool

if TYPE_CHECKING:
    import pathlib

# Canonical Uniswap V2 factory (the tracker resolves its deployment from it).
_V2_FACTORY = "0x5C69bEe701ef814E44274f655e7632cB715C14B6"
# A distinct, unrelated factory so a registry pool can be driven into the
# tracker's *untracked* cache through the real get_pool() path.
_OTHER_FACTORY = "0x1111111111111111111111111111111111111111"
_TOKEN0 = "0x0000000000000000000000000000000000000011"
_TOKEN1 = "0x0000000000000000000000000000000000000012"
_TRACKED_POOL = "0x0000000000000000000000000000000000000021"
_UNTRACKED_POOL = "0x0000000000000000000000000000000000000022"


class _BoundaryProvider:
    """The external RPC boundary, as a real object with an observable state.

    Bot only touches the network through the injected provider. The real
    ``AlloyProvider.close()`` releases its Rust connection pool on Arc drop
    with no queryable "closed" flag, so the *boundary* is represented by a
    real object whose ``closed`` flag is the observable postcondition. It
    raises if closed twice, which makes provider-close idempotency a
    behavioural assertion rather than a call count.
    """

    def __init__(self, chain_id: int = 1) -> None:
        self.chain_id = chain_id
        self.closed = False

    def close(self) -> None:
        if self.closed:
            msg = "provider closed twice"
            raise RuntimeError(msg)
        self.closed = True


def _make_test_config(tmp_path: pathlib.Path, chain_id: int = 1) -> DegenbotConfig:
    return DegenbotConfig(
        database=DatabaseSettings(path=tmp_path / "test.db"),
        rpc={1: ETHEREUM_ARCHIVE_NODE_HTTP_URI},
        default_chain_id=chain_id,
    )


def _make_bot(tmp_path: pathlib.Path) -> tuple[Bot, _BoundaryProvider]:
    provider = _BoundaryProvider(1)
    return Bot(_make_test_config(tmp_path), provider=provider), provider


def _seed_tracker_caches(bot: Bot, tracker: UniswapV2PoolTracker) -> None:
    """Populate both tracker caches via the tracker's own ``get_pool()`` path.

    Builds two real, Rust-registered V2 pools and adds them to the Bot's
    Python pool registry: one with the tracker's own factory (driven into
    ``_tracked_pools``) and one with an unrelated factory (driven into
    ``_untracked_pools``). No private-cache mutation and no mocks.
    """
    token0 = make_erc20(bot._py_bot, _TOKEN0, name="Token0", symbol="TK0", decimals=18)
    token1 = make_erc20(bot._py_bot, _TOKEN1, name="Token1", symbol="TK1", decimals=18)
    fees = {"fee_token0": Fraction(3, 1000), "fee_token1": Fraction(3, 1000)}

    tracked = make_v2_pool(
        _TRACKED_POOL,
        token0=token0,
        token1=token1,
        factory=_V2_FACTORY,
        reserves_token0=1,
        reserves_token1=1,
        py_bot=bot._py_bot,
        **fees,
    )
    bot.pools.add(pool=tracked, chain_id=1, pool_address=tracked.address)
    assert tracker.get_pool(tracked.address) is tracked

    untracked = make_v2_pool(
        _UNTRACKED_POOL,
        token0=token0,
        token1=token1,
        factory=_OTHER_FACTORY,
        reserves_token0=1,
        reserves_token1=1,
        py_bot=bot._py_bot,
        **fees,
    )
    bot.pools.add(pool=untracked, chain_id=1, pool_address=untracked.address)
    with pytest.raises(PoolNotAssociated):
        tracker.get_pool(untracked.address)


class TestBotContextManager:
    def test_context_manager_releases_all_handles_on_exit(self, tmp_path: pathlib.Path) -> None:
        """Contracts (a)-(c): exit closes provider, removes/disposes DB, drops caches."""
        config = _make_test_config(tmp_path)
        provider = _BoundaryProvider(1)

        with Bot(config, provider=provider) as bot:
            # Observable pre-state: engine bound, a live scoped session open,
            # and both tracker caches + the pool registry populated.
            assert bot.db._engine is not None
            bot.db()  # open a scoped session so its removal is observable
            assert bot.db._session.registry.has()
            tracker = bot.add_tracker(UniswapV2PoolTracker, factory_address=_V2_FACTORY)
            _seed_tracker_caches(bot, tracker)
            assert tracker.tracked_pool_count() == 1
            assert tracker.untracked_pool_count() == 1
            assert len(bot.pools) == 2
            assert provider.closed is False

        # (a) provider connection closed and the Bot's handle released
        assert provider.closed is True
        assert bot._provider is None
        # (b) scoped session removed + its engine disposed (probe: no engine)
        assert not bot.db._session.registry.has()
        assert bot.db._engine is None
        # (c) tracker caches and pool registry released
        assert tracker.tracked_pool_count() == 0
        assert tracker.untracked_pool_count() == 0
        assert bot.pools is None
        assert bot._closed is True

    def test_close_is_idempotent(self, tmp_path: pathlib.Path) -> None:
        """Contract (d): a second close() is a no-op (provider double raises if re-closed)."""
        bot, provider = _make_bot(tmp_path)
        bot.close()

        after_first = (provider.closed, bot.db._engine, bot._closed, bot._provider, bot.pools)
        assert after_first == (True, None, True, None, None)

        bot.close()  # must not re-close the provider or raise
        assert (
            provider.closed,
            bot.db._engine,
            bot._closed,
            bot._provider,
            bot.pools,
        ) == after_first

        # The context-manager exit path is also a no-op after an explicit close.
        bot.__exit__(None, None, None)
        assert bot._closed is True

    def test_exit_does_not_suppress_exceptions(self, tmp_path: pathlib.Path) -> None:
        """Contract (e): the exception propagates *and* teardown still runs."""
        config = _make_test_config(tmp_path)
        provider = _BoundaryProvider(1)
        boom_msg = "boom"

        with pytest.raises(RuntimeError, match=boom_msg), Bot(config, provider=provider):
            raise RuntimeError(boom_msg)

        assert provider.closed is True

    def test_close_disposes_real_database_engine(self, tmp_path: pathlib.Path) -> None:
        """Contract (b): close() disposes the SQLAlchemy engine bound by __init__.

        ``db.remove()`` only returns the thread-local Session; the Engine's
        connection pool would keep the ``sqlite3.Connection`` alive
        (``ResourceWarning: unclosed database`` under GC). The real DSM's
        ``_engine`` going from bound to ``None`` is the observable proof that
        the engine was disposed — no call-count spy needed.
        """
        bot, provider = _make_bot(tmp_path)
        assert bot.db._engine is not None

        bot.close()

        assert bot.db._engine is None
        assert provider.closed is True

    def test_close_removes_scoped_session(self, tmp_path: pathlib.Path) -> None:
        """Contract (b): close() removes the live scoped session from its registry."""
        bot, _provider = _make_bot(tmp_path)
        bot.db()  # register a live scoped session
        assert bot.db._session.registry.has()

        bot.close()

        assert not bot.db._session.registry.has()

    def test_close_composes_release_python_state(self, tmp_path: pathlib.Path) -> None:
        """Contract (c): release_python_state mid-lifecycle, then close stays safe."""
        bot, provider = _make_bot(tmp_path)
        tracker = bot.add_tracker(UniswapV2PoolTracker, factory_address=_V2_FACTORY)
        _seed_tracker_caches(bot, tracker)

        # Mid-lifecycle release drops Python caches but keeps the live
        # connections (the Bot is still running).
        bot.release_python_state()
        assert tracker.tracked_pool_count() == 0
        assert tracker.untracked_pool_count() == 0
        assert len(bot.pools) == 0
        assert bot.db._engine is not None
        assert provider.closed is False

        # End-of-life close must not raise despite the already-released caches,
        # and must still complete connection teardown.
        bot.close()
        assert bot._closed is True
        assert provider.closed is True
        assert bot.db._engine is None
