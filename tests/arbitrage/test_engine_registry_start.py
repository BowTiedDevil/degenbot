"""Integration tests for EngineRegistry.start — the two-phase Layer A facade.

`start(...)` runs the pre-pump startup ritual (subscribe → backfill →
verify config) and stops at `PumpPhase::Backfilled`, BEFORE
`resume()`. This is the consumer-safety invariant: between subscribe and
resume (including stream/backfill/verify) zero result batches are emitted,
so the caller can attach its consumer any time before `resume()` without
risk of unbounded backlog or stale-batch dispatch.
"""

from __future__ import annotations

import pytest

import degenbot.arbitrage.engine_registry as runner
from tests.fakes.engine import FakeEngine


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
        "subscribe",
        "set_verify_rpc_url",
    ]
    assert "resume" not in fake.calls
    assert backfill_target == 18_000_000


def test_start_reads_the_snapshot_seed_block_from_the_engine() -> None:
    """start() reads the snapshot seed block ``S`` back from the shared
    ``BotState`` (recorded at ``Bot`` construction by ``load_snapshot_from_db``)
    and stashes it as the per-pool step-1 verify seed, then configures verify.

    There is no injected ``v3_snapshot``/``v4_snapshot`` kwargs seam any more:
    the seed is already on the core ``BotState`` before ``subscribe()``, so
    ``after_subscribe`` advances the phase to ``SnapshotLoaded``.
    """
    fake = FakeEngine(backfill_target=18_000_000, snapshot_seed_block=18_000_050)
    registry = runner.EngineRegistry(bot=None, engine=fake)

    registry.start("http://localhost:8545", "ws://localhost:8546")

    # The seed is READ (no setter call), then verify-config; never resume.
    assert fake.calls == [
        "subscribe",
        "set_verify_rpc_url",
    ]
    assert fake.seed_args == []
    assert registry._verify_snapshot_block == 18_000_050
    assert "resume" not in fake.calls


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


def test_pybot_exposes_pump_lifecycle_methods_after_engine_attach() -> None:
    """T3 (ADR-006 D4): Bot exposes subscribe/resume as delegating entry
     points once a ArbitrageEngine is constructed against it (which attaches
     the shared PumpState). The Bot is the D4 pump owner; these methods drive
     the SAME PumpState the engine reads.

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
    for method in ("subscribe", "resume"):
        assert hasattr(bot, method), f"Bot must expose {method} after engine attach"
    # The engine still exposes subscribe/resume too (reads the same shared state).
    for method in ("subscribe", "resume"):
        assert hasattr(engine, method)
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
    """EngineRegistry.start authors the subscribe -> verify ordering exactly
    once, through the engine's one-call ``start`` (the Rust
    ``EngineDriver::start`` mirror) — not three separate FFI crossings.
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
    assert "resume" not in fake.calls


def test_start_propagates_driver_session_error_on_double_start() -> None:
    """A re-entry surfaces the shared driver's typed session refusal (the
    engine owns the phase gate) rather than the registry re-authoring the
    three-call sequence.
    """
    fake = FakeEngine(backfill_target=18_000_000)
    registry = runner.EngineRegistry(bot=None, engine=fake)
    registry.start("http://localhost:8545", "ws://localhost:8546")

    with pytest.raises(RuntimeError, match="already subscribed"):
        registry.start("http://localhost:8545", "ws://localhost:8546")
