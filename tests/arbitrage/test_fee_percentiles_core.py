"""The driver polls the core's fee-history percentiles, not a Python mirror.

``_apply_block_if_ready`` asks the Rust ``fetch_fee_history`` leaf for
``eth_feeHistory`` reward percentiles. The pair is a core constant
(``degenbot-arbitrage::MIN_PRIORITY_FEE_PERCENTILE`` /
``MAX_PRIORITY_FEE_PERCENTILE``); the driver reads it back over the FFI so the
two cannot drift, and the retired ``FEE_PERCENTILES`` module literal is gone.

The routing test injects a distinguishable pair at the leaf's
``reward_percentiles`` seam: a call site that hardcodes, caches, or mirrors the
values fails it even while the current numbers happen to match.
"""

from __future__ import annotations

import asyncio
import re
from pathlib import Path

from degenbot.dispatch import Dispatcher
from degenbot.runner import identity
from degenbot.runner._consume import _apply_block_if_ready
from degenbot.runner.bot_runner import _SessionState
from tests.fakes.engine import FakeEngine, FakeEngineRegistry
from tests.fakes.runner_pipelines import StubPipeline
from tests.fakes.session import FakeRunnerConfig

_REPO_ROOT = Path(__file__).resolve().parents[2]
_SIMULATOR_RS = _REPO_ROOT / "rust/crates/engine/degenbot-arbitrage/src/simulator.rs"
_CORE_FEE_PERCENTILES_RS = (
    _REPO_ROOT / "rust/crates/foundation/degenbot-core/src/fee_percentiles.rs"
)


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
        cfg=FakeRunnerConfig(  # type: ignore[arg-type]
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


async def _drive(
    session: _SessionState,
    *,
    reward_percentiles: tuple[int, int] | None = None,
) -> None:
    fut: asyncio.Future[dict[str, int]] = asyncio.get_running_loop().create_future()
    fut.set_result(_tick())
    await _apply_block_if_ready(fut, session, reward_percentiles=reward_percentiles)


async def test_head_tick_requests_core_percentiles() -> None:
    seen: list[list[float]] = []

    async def _record(**kwargs) -> bool:
        seen.append(list(kwargs["reward_percentiles"]))
        return True

    await _drive(_session(_record), reward_percentiles=(7, 77))

    assert seen == [[7.0, 77.0]], (
        "the head tick must poll the percentiles the core reader returns; a "
        "hardcoded or cached pair escapes this sentinel"
    )


def test_retired_python_literal_is_gone() -> None:
    assert not hasattr(identity, "FEE_PERCENTILES"), (
        "identity.FEE_PERCENTILES is the Python mirror of the core "
        "percentile pair; the driver reads the core value over the FFI instead"
    )


def _core_percentiles() -> tuple[int, int]:
    source = _CORE_FEE_PERCENTILES_RS.read_text(encoding="utf-8")
    pair = re.search(
        r"PRIORITY_FEE_PERCENTILES:\s*\[u64;\s*2\]\s*=\s*\[(\d+),\s*(\d+)\]",
        source,
    )
    assert pair is not None, (
        "the shared priority-fee percentile declarations moved; update this pin"
    )
    return (int(pair.group(1)), int(pair.group(2)))


def test_engine_carries_no_second_percentile_literal() -> None:
    """The pair has one home; the engine derives from it instead of copying it."""
    source = _SIMULATOR_RS.read_text(encoding="utf-8")
    assert not re.search(r"MIN_PRIORITY_FEE_PERCENTILE: u64 = \d+", source), (
        "the engine percentile literal is a second home; derive it from the "
        "shared foundation declaration"
    )
    assert not re.search(r"MAX_PRIORITY_FEE_PERCENTILE: u64 = \d+", source), (
        "the engine percentile literal is a second home; derive it from the "
        "shared foundation declaration"
    )


def test_the_verdict_carries_the_core_pair() -> None:
    """The verdict is the one FFI door for the pair, not a module function."""
    from degenbot.config import resolved_config

    assert resolved_config().fee_percentiles == _core_percentiles()
