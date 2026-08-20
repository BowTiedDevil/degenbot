"""Cockpit session state — the one owner of pump-session coordination state.

Epic Y7PA5A, task UKXADE. The block loop (``consume``) and the dispatch
leaf (``dispatch``) must read the SAME ``_SessionState`` owned by the
runner, instead of the session travelling as a 10-parameter signature
+ 9 kwargs.

Seams (confirmed with operator): the public BotRunner lifecycle
(injected consumer fake observes the owner) and the private loop seam
(``consume_result_batches`` driven with an explicit owner + injected
streams, dispatch leaf monkeypatched). No anvil, no live RPC.
"""

from __future__ import annotations

import signal

import pytest

from degenbot.runner import BotRunner
from degenbot.runner.config import ArbitrageConfig


@pytest.fixture(autouse=True)
def _rpc_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("DEGENBOT_RPC_HTTP_CHAINID_1", "http://localhost:8545")
    monkeypatch.setenv("DEGENBOT_RPC_WS_CHAINID_1", "ws://localhost:8546")


@pytest.fixture(autouse=True)
def _restore_sigint() -> None:
    yield
    signal.signal(signal.SIGINT, signal.SIG_DFL)


def _cfg() -> ArbitrageConfig:
    return ArbitrageConfig.from_env(
        {
            "OPERATOR_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
            "OPERATOR_PRIVATE_KEY": "0x" + "a" * 64,
            "EXECUTOR_CONTRACT_ADDRESS": "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5",
            "INJECT_EXECUTOR_CODE": "0",
        },
        live=True,
        permutation=None,
    )


class _FakeEngine:
    def resume(self) -> None:
        pass

    def stop(self) -> None:
        pass

    def v2_pool_count(self) -> int:
        return 0

    def v3_pool_count(self) -> int:
        return 0

    def v4_pool_count(self) -> int:
        return 0

    def path_count(self) -> int:
        return 0

    async def block_stream(self):
        return
        yield  # pragma: no cover - async generator marker


class _FakeEngineRegistry:
    def __init__(self) -> None:
        self.engine = _FakeEngine()
        self.start_calls = 0

    def start(self, node_http, node_ws, *, v3_snapshot, v4_snapshot, verify_state_view) -> int:
        self.start_calls += 1
        return 12_000


class _FakeBot:
    chain_id = 1

    def release_python_state(self) -> None:
        pass


class _FakeAsyncW3:
    async def get_block(self, block_identifier: str):
        return {"number": 12_345, "baseFeePerGas": 10**9, "gasUsed": 0, "gasLimit": 30_000_000}

    async def get_transaction_count(self, address: str) -> int:
        return 7

    def as_async_alloy(self) -> None:
        return None


class _FakeDispatcher:
    """The dispatcher-clock surface the loop + dispatch leaf touch."""

    def __init__(self, current_block: int = 12_346) -> None:
        self.current_block = current_block

    def record_block_time(self, block_number: int, block_timestamp: int) -> None:
        pass

    def block_time_count(self) -> int:
        return 0

    def block_times_oldest(self) -> tuple[int, int]:  # pragma: no cover - gate off
        return (0, 0)

    def advance_block(self, block_number: int) -> None:
        self.current_block = block_number

    def discard_path(self, path_id: int) -> None:
        pass

    def block_timestamp_for(self, block_number: int) -> int | None:
        return 1_700_000_000


def _noop():
    async def _n() -> None:
        pass

    return _n()


class AsyncOnce:
    """Yield one item, then StopAsyncIteration."""

    def __init__(self, item: object) -> None:
        self._item = item
        self._done = False

    def __aiter__(self) -> AsyncOnce:
        return self

    async def __anext__(self) -> object:
        if self._done:
            raise StopAsyncIteration
        self._done = True
        return self._item


def _block_tick(number: int) -> dict[str, int]:
    return {
        "number": number,
        "timestamp": 1_700_000_000 + number,
        "base_fee_per_gas": 1_000_000_000,
        "gas_used": 15_000_000,
        "gas_limit": 30_000_000,
    }


def _batch() -> dict[str, object]:
    # One fresh result (path_id, optimal_input, profit, hop_outs,
    # consumed_ins, state_nonces) so the dispatch leaf fires.
    return {
        "fresh": [(1, 1_000, 5_000, [2_000], [1_000], [5])],
        "updated": [],
        "removed": [],
        "solve_block": 12_346,
        "base_fee_per_gas": 1_000_000_000,
        "gas_used": 15_000_000,
        "gas_limit": 30_000_000,
    }


class TestSessionOwner:
    async def test_runner_consumer_receives_the_owner(self) -> None:
        """The consumer task is handed the SAME owner the runner holds."""
        captured: dict[str, object] = {}

        def capturing_consumer(*args: object, **kwargs: object) -> object:
            captured.update(kwargs)
            return _noop()

        session_runner = BotRunner(
            _cfg(),
            bot=_FakeBot(),
            engine_registry=_FakeEngineRegistry(),
            async_w3=_FakeAsyncW3(),
            snapshots=(object(), object(), None, None),
            path_builder=lambda **kw: _noop(),
            consumer=capturing_consumer,
            install_sigint=False,
        )
        async with session_runner:
            await session_runner.run()
        owner = captured.get("session")
        assert owner is not None, "consumer must be handed the session owner"
        assert owner is session_runner._session  # whitebox: the owner is private API
        assert owner.dispatcher is session_runner.dispatcher

    async def test_dispatch_leaf_receives_the_same_owner(
        self,
        monkeypatch: pytest.MonkeyPatch,
    ) -> None:
        """The loop and the dispatch leaf read one and the same owner."""
        from degenbot.runner._consume import consume_result_batches
        from degenbot.runner.bot_runner import _SessionState

        captured: dict[str, object] = {}

        def fake_dispatch_leaf(*args: object, **kwargs: object) -> object:
            captured["positional"] = args
            captured.update(kwargs)
            return _noop()

        monkeypatch.setattr("degenbot.runner._consume._dispatch_profitable", fake_dispatch_leaf)

        dispatcher = _FakeDispatcher(current_block=12_346)
        owner = _SessionState(
            engine_registry=_FakeEngineRegistry(),
            async_w3=_FakeAsyncW3(),
            sim_ctx=None,
            dispatcher=dispatcher,
            cfg=_cfg(),
            current_block=12_345,
        )
        await consume_result_batches(
            owner,
            block_stream=AsyncOnce(_block_tick(12_347)),
            result_iter=AsyncOnce(_batch()),
        )
        assert owner.current_block == 12_347
        leaf_args = list(captured.get("positional", []))
        leaf_kwargs = {k: v for k, v in captured.items() if k != "positional"}
        handed = leaf_args + list(leaf_kwargs.values())
        assert owner in handed, f"dispatch leaf did not receive the owner: {leaf_kwargs!r}"

    async def test_owner_advances_with_the_block_clock(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """The owner mirrors the newHeads clock as the loop applies it."""
        from degenbot.runner._consume import consume_result_batches
        from degenbot.runner.bot_runner import _SessionState

        def fake_dispatch_leaf(*args: object, **kwargs: object) -> object:
            return _noop()

        monkeypatch.setattr(
            "degenbot.runner._consume._dispatch_profitable",
            fake_dispatch_leaf,
        )
        owner = _SessionState(
            engine_registry=_FakeEngineRegistry(),
            async_w3=_FakeAsyncW3(),
            sim_ctx=None,
            dispatcher=_FakeDispatcher(current_block=12_346),
            cfg=_cfg(),
            current_block=12_345,
        )
        await consume_result_batches(
            owner,
            block_stream=AsyncOnce(_block_tick(12_347)),
            result_iter=AsyncOnce(_batch()),
        )
        assert owner.current_block == 12_347
