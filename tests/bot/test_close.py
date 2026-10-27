"""Behavioral tests for Bot's context-manager / close() resource lifecycle.

``with Bot(...) as bot:`` exit (and an explicit ``close()``) must:

(a) close the provider connection,
(b) release the pool/token registry caches,
(c) be idempotent, and
(d) never suppress an exception raised by the ``with`` body.

These are behavioural postcondition tests against the *real* objects Bot owns
— a real Python pool registry populated through its own ``add()`` path. The
only stand-in is the injected provider (the external RPC boundary, whose real
``AlloyProvider.close()`` is an unobservable no-op). No call-count assertions on
doubles.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest

from degenbot.bot import Bot
from tests.fakes.pools import FakeSessionPool

if TYPE_CHECKING:
    import pathlib


# A sentinel pool address seeded into the Python pool registry.
_SEEDED_POOL = "0x0000000000000000000000000000000000000021"


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
        self._chain_id = chain_id
        self.closed = False

    def get_chain_id(self) -> int:
        return self._chain_id

    def close(self) -> None:
        if self.closed:
            msg = "provider closed twice"
            raise RuntimeError(msg)
        self.closed = True


def _test_session(tmp_path: pathlib.Path, chain_id: int = 1) -> dict[str, object]:
    """The explicit keyword overrides a session under test runs with."""
    return {"chain_id": chain_id, "database": str(tmp_path / "test.db")}


def _make_bot(tmp_path: pathlib.Path) -> tuple[Bot, _BoundaryProvider]:
    provider = _BoundaryProvider(1)
    return Bot(**_test_session(tmp_path), provider=provider), provider


def _seed_pool_registry(bot: Bot) -> None:
    """Populate the Python pool registry through its own ``add()`` path."""
    bot.pools.add(
        pool_address=_SEEDED_POOL,
        pool=FakeSessionPool(_SEEDED_POOL),  # type: ignore[arg-type]
        chain_id=1,
    )


class TestBotContextManager:
    def test_context_manager_releases_all_handles_on_exit(self, tmp_path: pathlib.Path) -> None:
        """Contracts (a)-(b): exit closes the provider and drops caches."""
        session = _test_session(tmp_path)
        provider = _BoundaryProvider(1)

        with Bot(**session, provider=provider) as bot:
            # Observable pre-state: the pool registry is populated.
            _seed_pool_registry(bot)
            assert len(bot.pools) == 1
            assert provider.closed is False

        # (a) provider connection closed and the Bot's handle released
        assert provider.closed is True
        assert bot._provider is None
        # (b) pool registry released
        assert bot.pools is None
        assert bot._closed is True

    def test_close_is_idempotent(self, tmp_path: pathlib.Path) -> None:
        """Contract (c): a second close() is a no-op (provider double raises if re-closed)."""
        bot, provider = _make_bot(tmp_path)
        bot.close()

        after_first = (provider.closed, bot._closed, bot._provider, bot.pools)
        assert after_first == (True, True, None, None)

        bot.close()  # must not re-close the provider or raise
        assert (
            provider.closed,
            bot._closed,
            bot._provider,
            bot.pools,
        ) == after_first

        # The context-manager exit path is also a no-op after an explicit close.
        bot.__exit__(None, None, None)
        assert bot._closed is True

    def test_exit_does_not_suppress_exceptions(self, tmp_path: pathlib.Path) -> None:
        """Contract (d): the exception propagates *and* teardown still runs."""
        session = _test_session(tmp_path)
        provider = _BoundaryProvider(1)
        boom_msg = "boom"

        with pytest.raises(RuntimeError, match=boom_msg), Bot(**session, provider=provider):
            raise RuntimeError(boom_msg)

        assert provider.closed is True

    def test_close_composes_release_python_state(self, tmp_path: pathlib.Path) -> None:
        """Contract (b): release_python_state mid-lifecycle, then close stays safe."""
        bot, provider = _make_bot(tmp_path)
        _seed_pool_registry(bot)
        assert len(bot.pools) == 1

        # Mid-lifecycle release drops Python caches but keeps the live
        # connections (the Bot is still running).
        bot.release_python_state()
        assert len(bot.pools) == 0
        assert provider.closed is False

        # End-of-life close must not raise despite the already-released caches,
        # and must still complete connection teardown.
        bot.close()
        assert bot._closed is True
        assert provider.closed is True
