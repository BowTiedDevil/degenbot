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
doubles; the one order assertion observes the teardown stack's own sequence.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest

from degenbot import bot_lifecycle
from degenbot.bot import Bot
from tests.fakes.pools import FakeSessionPool
from tests.helpers.bot_factory import bot_session

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


def _make_bot(tmp_path: pathlib.Path) -> tuple[Bot, _BoundaryProvider]:
    provider = _BoundaryProvider(1)
    return Bot(**bot_session(tmp_path), provider=provider), provider


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
        session = bot_session(tmp_path)
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
        session = bot_session(tmp_path)
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


class _ProbingProvider(_BoundaryProvider):
    """The provider boundary, recording what the Bot still held when closed.

    The teardown's load-bearing ordering edge is "provider first, holder drops
    after": a ``close()`` that ran once the ``Arc``-pinning holders were
    already gone would be closing a connection nobody pins. What the Bot still
    holds AT close is therefore the observable, not a timestamp.
    """

    def __init__(self) -> None:
        super().__init__(1)
        self.bot: Bot | None = None
        self.held_at_close: dict[str, bool] = {}

    def close(self) -> None:
        bot = self.bot
        assert bot is not None, "the probe must be bound to its bot"
        self.held_at_close = {
            "_py_bot": bot._py_bot is not None,
            "_io": bot._io is not None,
            "pools": bot.pools is not None,
        }
        super().close()

    def get_probe_recording(self) -> dict[str, bool]:
        """Return what the Bot held when this provider was closed."""
        return self.held_at_close


class _RecordingRegistry:
    """A registry double whose one teardown call lands in a shared event list."""

    def __init__(self, events: list[str], name: str) -> None:
        self._events = events
        self._name = name

    def _reset(self, *, propagate_to_rust: bool) -> None:
        self._events.append(f"{self._name}._reset(propagate_to_rust={propagate_to_rust})")

    def reset(self) -> None:
        self._events.append(f"{self._name}.reset()")


class _RecordingProvider:
    """A provider boundary double whose one close() lands in the shared event list."""

    def __init__(self, events: list[str]) -> None:
        self._events = events

    def close(self) -> None:
        self._events.append("provider.close()")


class _TeardownObserver:
    """A ``_BotLike``-shaped observer: the teardown ORDER is the observable.

    A real ``Bot``'s rungs are individually invisible from outside (a dropped
    attribute and a closed connection look the same afterwards). This object
    makes every rung's effect an entry in one list, so the stack's order is
    assertable as a sequence rather than inferred from prose.
    """

    def __init__(self) -> None:
        events: list[str] = []
        self.events = events
        object.__setattr__(self, "_recording", False)
        self.pools = _RecordingRegistry(events, "pools")
        self.tokens = _RecordingRegistry(events, "tokens")
        self.managed_pools = object()
        self._provider = _RecordingProvider(events)
        self._py_bot = object()
        self._io = object()
        self._async_adapter = object()
        self._erc20_builder = object()
        self._curve_builder = object()
        self._closed = False
        object.__setattr__(self, "_recording", True)

    def __setattr__(self, name: str, value: object) -> None:
        if getattr(self, "_recording", False):
            self.events.append(f"{name} = {'None' if value is None else 'kept'}")
        object.__setattr__(self, name, value)


class TestTeardownOrder:
    def test_provider_closes_before_the_arc_pinning_holders_are_dropped(
        self, tmp_path: pathlib.Path
    ) -> None:
        """The provider is closed while the Bot still pins its ``Arc``."""
        provider = _ProbingProvider()
        bot = Bot(**bot_session(tmp_path), provider=provider)
        provider.bot = bot
        _seed_pool_registry(bot)

        bot.close()

        assert provider.get_probe_recording() == {
            "_py_bot": True,
            "_io": True,
            "pools": True,
        }, "the provider must be closed before the handles that pin its Arc are dropped"

    def test_close_drives_the_teardown_stack_in_declared_order(self) -> None:
        """One ordered owner: the rungs run top to bottom, exactly once each."""
        observer = _TeardownObserver()

        bot_lifecycle.close(observer)

        assert observer.events == [
            "_closed = kept",  # the idempotence latch, set before any rung runs
            "pools._reset(propagate_to_rust=False)",
            "tokens.reset()",
            "provider.close()",
            "_py_bot = None",
            "_provider = None",
            "_io = None",
            "_async_adapter = None",
            "pools = None",
            "tokens = None",
            "managed_pools = None",
            "_erc20_builder = None",
            "_curve_builder = None",
        ]
