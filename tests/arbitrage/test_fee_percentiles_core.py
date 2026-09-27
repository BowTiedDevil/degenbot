"""The driver polls the core's fee-history percentiles, not a Python mirror.

``_apply_block_if_ready`` asks the Rust ``fetch_fee_history`` leaf for
``eth_feeHistory`` reward percentiles. The pair is a core constant
(``degenbot-arbitrage::MIN_PRIORITY_FEE_PERCENTILE`` /
``MAX_PRIORITY_FEE_PERCENTILE``); the driver reads it back over the FFI so the
two cannot drift, and the retired ``FEE_PERCENTILES`` module literal is gone.

The routing test monkeypatches the core reader to a distinguishable pair: a
call site that hardcodes, caches, or mirrors the values fails it even while the
current numbers happen to match.
"""

from __future__ import annotations

import asyncio
import re
from pathlib import Path
from types import SimpleNamespace

from degenbot.dispatch import Dispatcher
from degenbot.runner import _driver_constants
from degenbot.runner._consume import _apply_block_if_ready
from degenbot.runner.bot_runner import _SessionState
from tests.fakes.engine import FakeEngine, FakeEngineRegistry
from tests.fakes.runner_pipelines import StubPipeline

_REPO_ROOT = Path(__file__).resolve().parents[2]
_SIMULATOR_RS = _REPO_ROOT / "rust/crates/engine/degenbot-arbitrage/src/simulator.rs"


class _AlloyW3:
    """Async w3 double whose provider arm is present (the fee-history precondition)."""

    def as_async_alloy(self) -> object:
        return object()


def _session(recorder) -> _SessionState:
    return _SessionState(
        engine_registry=FakeEngineRegistry(FakeEngine(hosted_activity=True)),  # type: ignore[arg-type]
        async_w3=_AlloyW3(),  # type: ignore[arg-type]
        sim_ctx=None,
        dispatcher=Dispatcher.for_block(0),
        cfg=SimpleNamespace(  # type: ignore[arg-type]
            operator_address="0x9C56a29c7231974c269E24F9FB3c29203039089E"
        ),
        current_block=0,
        pipeline_factory=StubPipeline,
        fee_history_fetcher=recorder,
    )


def _tick() -> dict[str, int]:
    return {
        "number": 101,
        "timestamp": 1_700_000_000,
        "base_fee_per_gas": 1_000_000_000,
        "gas_used": 15_000_000,
        "gas_limit": 30_000_000,
    }


async def _drive(session: _SessionState) -> None:
    fut: asyncio.Future[dict[str, int]] = asyncio.get_running_loop().create_future()
    fut.set_result(_tick())
    await _apply_block_if_ready(fut, session)


async def test_head_tick_requests_core_percentiles(monkeypatch) -> None:
    seen: list[list[float]] = []

    async def _record(**kwargs) -> bool:
        seen.append(list(kwargs["reward_percentiles"]))
        return True

    monkeypatch.setattr("degenbot.runner._consume.fee_percentiles", lambda: (7, 77))

    await _drive(_session(_record))

    assert seen == [[7.0, 77.0]], (
        "the head tick must poll the percentiles the core reader returns; a "
        "hardcoded or cached pair escapes this sentinel"
    )


def test_retired_python_literal_is_gone() -> None:
    assert not hasattr(_driver_constants, "FEE_PERCENTILES"), (
        "_driver_constants.FEE_PERCENTILES is the Python mirror of the core "
        "percentile pair; the driver reads the core value over the FFI instead"
    )


def _core_percentiles() -> tuple[int, int]:
    source = _SIMULATOR_RS.read_text(encoding="utf-8")
    low = re.search(r"MIN_PRIORITY_FEE_PERCENTILE: u64 = (\d+)", source)
    high = re.search(r"MAX_PRIORITY_FEE_PERCENTILE: u64 = (\d+)", source)
    assert low is not None and high is not None, (
        "the core priority-fee percentile declarations moved; update this pin"
    )
    return (int(low.group(1)), int(high.group(1)))


def test_ffi_reader_matches_core_declaration() -> None:
    from degenbot.arbitrage import fee_percentiles

    assert fee_percentiles() == _core_percentiles()
