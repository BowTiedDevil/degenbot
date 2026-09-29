"""The shared fake-actor stack for boot-path runner tests.

``tests/arbitrage/test_posture_driven_boot.py`` and
``tests/dispatch/test_relay_posture.py`` both drive a real ``BotRunner``
``start()``/``run()`` on the same injected actor trio, so an FFI actor-surface
change would otherwise have to be mirrored in two files. This module is the one
home for those doubles and for the wiring that renders them to a runner.

Each caller keeps ownership of its own ``ArbitrageConfig`` construction: the two
boots build their config differently, so that logic does not belong here.
``FakeEngineRegistry`` stays in ``tests/fakes/engine.py`` — it is the
engine-registry contract double, not a boot-path actor.
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass
from typing import Any

from degenbot.runner import BotRunner
from degenbot.runner.bot_runner import InjectedActors
from degenbot.runner.config import ArbitrageConfig
from tests.fakes.engine import FakeEngineRegistry


def inline_registration_scheduler(coro: Any) -> Any:
    """Deterministic scheduler double for the run ritual's registration hand-off.

    The ritual's scheduler is DATA (the production default is
    ``asyncio.create_task``); there is no second production mode to select.
    Tests that must observe the hand-off deterministically (the trim fired
    before the main loop, builder kwargs captured, event ordering pinned)
    inject this double, which drives the coroutine to completion inline —
    through cooperative ``asyncio.sleep(0)`` yields, but through no real
    pending await — and returns a task that replays the outcome (so a fatal
    registration error still surfaces through the watch's fail-fast verdict).
    """
    outcome: BaseException | None = None
    while True:
        try:
            yielded = coro.send(None)
        except StopIteration:
            break
        except BaseException as exc:  # ruff: ignore[blind-except] replayed on the task below
            outcome = exc
            break
        if yielded is not None:
            coro.close()
            msg = "deterministic scheduler: the registration hand-off suspended on a real await"
            raise AssertionError(msg)

    async def _replay() -> None:
        # One cooperative yield: the hand-off already ran inline; this task
        # replays its outcome on the loop like any scheduled task would.
        await asyncio.sleep(0)
        if outcome is not None:
            raise outcome

    return asyncio.create_task(_replay(), name="registration-background")


def noop_coro():
    async def _n() -> None:
        pass

    return _n()


@dataclass(frozen=True)
class FakeBootReadiness:
    """The posture-readiness double: what the boot's readiness probe answers."""

    settlement_active: bool = False


class FakeEth:
    async def get_block(self, block_identifier: str):
        return {
            "number": 12_345,
            "baseFeePerGas": 10**9,
            "gasUsed": 0,
            "gasLimit": 30_000_000,
        }

    async def get_transaction_count(self, address: str):
        return 7


class FakeAsyncW3:
    """The boot-path provider double (non-Alloy, so no sim context is built)."""

    def __init__(self) -> None:
        self.eth = FakeEth()

    async def get_block(self, block_identifier: str):
        return await self.eth.get_block(block_identifier)

    async def get_transaction_count(self, address: str):
        return await self.eth.get_transaction_count(address)

    async def make_request(self, method: str, params: list):
        return {}

    @property
    def rpc_url(self) -> str:
        return "http://fake:8545"

    def as_async_alloy(self) -> None:
        return None


class FakeBot:
    """The boot-path bot double: records the snapshot-trim surface."""

    def __init__(self) -> None:
        self.chain_id = 1
        self.released = False
        self.block_stream_calls = 0

    def release_python_state(self) -> None:
        self.released = True

    def block_stream(self):
        self.block_stream_calls += 1
        return iter(())  # immediately exhausted


def boot_runner(
    cfg: ArbitrageConfig,
    *,
    path_builder=None,
    settlement_arm: bool = False,
    readiness=None,
    settlement_endpoints=None,
    install_sigint: bool = True,
    scheduler: Any = None,
) -> BotRunner:
    """A real ``BotRunner`` on the fake actor trio, with the boot posture gate live.

    ``readiness`` / ``settlement_endpoints`` are the activation-gate DI
    factories (``None`` = the real ``degenbot.strategy`` resolvers); a test
    injects a resolving factory or a raising refusal. ``scheduler`` defaults
    to the deterministic registration double so a settlement-active boot's
    hand-off (and its trim) completes inline; pass the production
    ``asyncio.create_task`` to observe the real decoupling instead.
    """

    return BotRunner(
        cfg,
        actors=InjectedActors(
            bot=FakeBot(),
            engine_registry=FakeEngineRegistry(backfill_target=12_000),
            async_w3=FakeAsyncW3(),
            snapshots=(None, None, None, None),
            path_builder=path_builder if path_builder is not None else (lambda **kw: None),
            consumer=lambda **kw: noop_coro(),
            settlement_arm=settlement_arm,
            readiness=readiness,
            settlement_endpoints=settlement_endpoints,
            scheduler=scheduler if scheduler is not None else inline_registration_scheduler,
        ),
        install_sigint=install_sigint,
    )
