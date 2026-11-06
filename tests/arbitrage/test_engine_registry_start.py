"""Integration tests for EngineRegistry.start — the two-phase Layer A facade.

`start(...)` runs the pre-pump startup ritual (subscribe → backfill →
verify config) and stops at `PumpPhase::Subscribed`, BEFORE
`resume()`. This is the consumer-safety invariant: between subscribe and
resume (including stream/backfill/verify) zero result batches are emitted,
so the caller can attach its consumer any time before `resume()` without
risk of unbounded backlog or stale-batch dispatch.
"""

from __future__ import annotations

import pytest

import degenbot.arbitrage.engine_registry as runner
from tests.fakes.engine import FakeEngine
from tests.helpers.seam import Bot


def test_start_no_snapshots_calls_subscribe_then_verify_never_resume() -> None:
    """Tracer: with no snapshots, start() subscribes + sets verify config in
    order and never calls resume(). Skip stream/backfill (no snapshots)."""
    fake = FakeEngine(backfill_target=18_000_000)
    registry = runner.EngineRegistry(bot=None, engine=fake)

    backfill_target = registry.start(
        "http://localhost:8545",
        "ws://localhost:8546",
    )

    assert fake.calls == [
        "connect",
        "set_verify_rpc_url",
    ]
    assert "run" not in fake.calls
    assert backfill_target == 18_000_000


def test_start_reads_the_snapshot_seed_block_from_the_engine() -> None:
    """start() reads the snapshot seed block ``S`` back from the shared
    ``BotState`` (recorded at ``Bot`` construction by ``load_snapshot_from_db``)
    and stashes it as the per-pool step-1 verify seed, then configures verify.

    There is no injected ``v3_snapshot``/``v4_snapshot`` kwargs seam any more:
    the seed is already on the core ``BotState`` before ``subscribe()``, so
    ``subscribe`` lands the pump phase on ``subscribed``.
    """
    fake = FakeEngine(backfill_target=18_000_000, snapshot_seed_block=18_000_050)
    registry = runner.EngineRegistry(bot=None, engine=fake)

    registry.start("http://localhost:8545", "ws://localhost:8546")

    # The seed is READ (no setter call), then verify-config; never run.
    assert fake.calls == [
        "connect",
        "set_verify_rpc_url",
    ]
    assert fake.seed_args == []
    assert registry._verify_snapshot_block == 18_000_050
    assert "run" not in fake.calls


def test_start_passes_verify_state_view_when_supplied() -> None:
    """verify_state_view is set on the engine only when supplied."""
    fake = FakeEngine(backfill_target=18_000_000)
    registry = runner.EngineRegistry(bot=None, engine=fake)
    state_view = "0x0000000000000000000000000000000000000abc"

    registry.start(
        "http://localhost:8545",
        "ws://localhost:8546",
        verify_state_view=state_view,
    )

    assert "set_verify_state_view" in fake.calls


def test_start_skips_set_verify_state_view_when_none() -> None:
    """When verify_state_view is None, set_verify_state_view is not called."""
    fake = FakeEngine(backfill_target=18_000_000)
    registry = runner.EngineRegistry(bot=None, engine=fake)

    registry.start("http://localhost:8545", "ws://localhost:8546")

    assert "set_verify_state_view" not in fake.calls


def test_pybot_exposes_verify_methods_after_engine_attach() -> None:
    """T4 (ADR-006 D4): Bot exposes the verify CONFIG + per-pool two-step
    lifecycle entry points once an ArbitrageEngine is constructed against it.
    The per-pool two-step lifecycle is the verify authority."""
    from degenbot.arbitrage.engine_registry import ArbitrageEngine
    from tests.helpers.seam import Bot

    bot = Bot()
    ArbitrageEngine(py_bot=bot)  # attaches shared PumpState
    for method in (
        "set_verify_rpc_url",
        "set_verify_state_view",
    ):
        assert hasattr(bot, method), f"Bot must expose {method} after engine attach"


def test_pybot_verify_setters_fail_loud_without_an_attached_engine() -> None:
    """The verify setters RAISE on a bot with no engine attached (the old
    swallowed no-op is gone): the missing-engine case fails at the adapter."""
    bot = Bot()
    with pytest.raises(RuntimeError, match="No engine attached"):
        bot.set_verify_rpc_url("http://localhost:8545")
    with pytest.raises(RuntimeError, match="No engine attached"):
        bot.set_verify_state_view("0x0000000000000000000000000000000000000abc")


def test_set_verify_state_view_rejects_a_malformed_address() -> None:
    """A malformed StateView raises ValueError at the adapter and is never
    installed as the zero address (the configuration is left unchanged)."""
    bot = Bot()
    runner.ArbitrageEngine(py_bot=bot)
    with pytest.raises(ValueError, match="malformed StateView"):
        bot.set_verify_state_view("not-an-address")


def test_set_verify_rpc_url_raises_immediately_on_a_provider_construction_failure() -> None:
    """A refused provider construction raises at the setter: the failure is
    never swallowed into a silently unconfigured verify home. This ws:// URL is
    refused at construction (the build-time WS connect fails); the error path
    under test is the construction `Result`, not an endpoint-liveness verdict —
    a lazy HTTP construction dials nothing and installs against a
    valid-but-unresponsive endpoint without complaint."""
    bot = Bot()
    runner.ArbitrageEngine(py_bot=bot)
    with pytest.raises(RuntimeError, match="failed to create verification provider"):
        bot.set_verify_rpc_url("ws://127.0.0.1:1")


def test_pybot_exposes_pump_lifecycle_methods_after_engine_attach() -> None:
    """T3 (ADR-006 D4): Bot exposes the pump-lifecycle moves (connect + run) as
     delegating entry points once a ArbitrageEngine is constructed against it
     (which attaches the shared PumpState). The Bot is the D4 pump owner; these
     methods drive the SAME PumpState the engine reads.

     The engine's own share is the start flow around the consumer's resume:
     ``connect`` (subscribe + backfill) then, once a session consumer holds the
     event hand-off, ``enable_facets`` → the consumer's ``resume`` →
     ``start_hosted_session``. Arming the pump is the consumer's move.

     The snapshot→WS gap is closed automatically inside the core
     `BlockPump::resume_from_subscribe`. `S` is recorded on the shared
     `BotState` by the DB load at construction; the core auto-backfill picks it
    up from there.
    """
    from degenbot.arbitrage.engine_registry import ArbitrageEngine
    from tests.helpers.seam import Bot

    bot = Bot()
    # Constructing the engine against the bot attaches the shared PumpState.
    engine = ArbitrageEngine(py_bot=bot)
    for method in ("connect", "run"):
        assert hasattr(bot, method), f"Bot must expose {method} after engine attach"
    # The engine's start flow around the session consumer's resume.
    for method in ("connect", "enable_facets", "start_hosted_session"):
        assert hasattr(engine, method), f"the engine must expose the start-flow half {method}"
    # The engine still exposes the snapshot seed block (getter+setter): the DB
    # load records `S` and `start()` reads it back.
    assert hasattr(engine, "snapshot_seed_block")  # getter+setter


def test_start_stashes_snapshot_block_for_two_step_verify() -> None:
    """T1 (ADR-006 D4 + two-step verify prep): start() stashes the snapshot
    seed block (read from the shared ``BotState``) on the registry, so the
    per-pool two-step verify (T6) can pass it to the verify closures without
    re-deriving. It is NOT wired into engine.set_verify_* (that dead path is
    deleted in T5) — it lives on the registry for T6 to read."""
    fake = FakeEngine(backfill_target=18_000_000, snapshot_seed_block=18_000_050)
    registry = runner.EngineRegistry(bot=None, engine=fake)

    registry.start("http://localhost:8545", "ws://localhost:8546")

    # The post-drain pin carries its OWN block (captured atomically with the
    # drain), so step-2 takes no `block` argument from the registry.
    assert registry._verify_snapshot_block == 18_000_050


def test_start_stashes_None_blocks_when_no_snapshots() -> None:
    """With no DB seed (cold start), the shared ``BotState`` carries no snapshot
    block, so the snapshot block stash is None (T6 will guard on None ==
    verify not applicable for this pool). Step-2 pins its own block."""
    fake = FakeEngine(backfill_target=18_000_000)
    registry = runner.EngineRegistry(bot=None, engine=fake)

    registry.start("http://localhost:8545", "ws://localhost:8546")

    assert registry._verify_snapshot_block is None


def test_start_delegates_the_ritual_to_one_engine_call() -> None:
    """EngineRegistry.start authors the connect -> verify ordering exactly
    once, through the engine's one-call ``start`` (this shell's adapter over
    `connect` plus the verify home's configuration) — not three separate FFI
    crossings.
    """
    fake = FakeEngine(backfill_target=18_000_000)
    registry = runner.EngineRegistry(bot=None, engine=fake)
    state_view = "0x0000000000000000000000000000000000000abc"

    target = registry.start(
        "http://localhost:8545",
        "ws://localhost:8546",
        verify_state_view=state_view,
    )

    assert fake.start_calls == [
        ("http://localhost:8545", "ws://localhost:8546", state_view),
    ]
    assert target == 18_000_000
    assert "run" not in fake.calls


def test_start_propagates_driver_session_error_on_double_start() -> None:
    """A re-entry surfaces the shared driver's typed session refusal (the
    engine owns the phase gate) rather than the registry re-authoring the
    three-call sequence.
    """
    fake = FakeEngine(backfill_target=18_000_000)
    registry = runner.EngineRegistry(bot=None, engine=fake)
    registry.start("http://localhost:8545", "ws://localhost:8546")

    with pytest.raises(RuntimeError, match="already connected"):
        registry.start("http://localhost:8545", "ws://localhost:8546")
