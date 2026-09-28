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

from degenbot.runner import BotRunner
from degenbot.runner.bot_runner import InjectedActors
from degenbot.runner.config import ArbitrageConfig
from tests.fakes.engine import FakeEngineRegistry


def noop_coro():
    async def _n() -> None:
        pass

    return _n()


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
    install_sigint: bool = True,
) -> BotRunner:
    """A real ``BotRunner`` on the fake actor trio, with the boot posture gate live."""

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
        ),
        install_sigint=install_sigint,
    )
