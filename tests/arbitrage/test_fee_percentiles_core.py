"""The driver polls the core's fee-history percentiles, not a Python mirror.

``_apply_block_if_ready`` asks the Rust ``fetch_fee_history`` leaf for
``eth_feeHistory`` reward percentiles. The pair is a core constant
(``degenbot-arbitrage::MIN_PRIORITY_FEE_PERCENTILE`` /
``MAX_PRIORITY_FEE_PERCENTILE``); the driver reads it back over the FFI so the
two cannot drift.

The routing test injects a distinguishable pair at the leaf's
``reward_percentiles`` seam: a call site that hardcodes, caches, or mirrors the
values fails it even while the current numbers happen to match.
"""

from __future__ import annotations

import asyncio
import re
from pathlib import Path

from degenbot.config import resolved_config
from degenbot.dispatch import Dispatcher
from degenbot.runner._consume import BlockLoop, _apply_block_if_ready
from tests.fakes.runner_pipelines import StubPipeline
from tests.helpers.session import session_for_test

_REPO_ROOT = Path(__file__).resolve().parents[2]
_SIMULATOR_RS = _REPO_ROOT / "rust/crates/engine/degenbot-arbitrage/src/simulator.rs"
_CORE_FEE_PERCENTILES_RS = (
    _REPO_ROOT / "rust/crates/foundation/degenbot-core/src/fee_percentiles.rs"
)


class _AlloyW3:
    """Async w3 double whose provider arm is present (the fee-history precondition)."""

    def as_async_alloy(self) -> object:
        return object()


async def _loop(recorder) -> BlockLoop:
    session = session_for_test(
        async_w3=_AlloyW3(),
        dispatcher=Dispatcher.for_block(0),
        pipeline_factory=StubPipeline,
        fee_history_fetcher=recorder,
    )
    await session.build_pipeline()
    return session.block_loop()


def _tick() -> dict[str, int]:
    return {
        "number": 101,
        "timestamp": 1_700_000_000,
        "base_fee_per_gas": 1_000_000_000,
        "gas_used": 15_000_000,
        "gas_limit": 30_000_000,
    }


async def _drive(
    loop: BlockLoop,
    *,
    reward_percentiles: tuple[int, int] | None = None,
) -> None:
    fut: asyncio.Future[dict[str, int]] = asyncio.get_running_loop().create_future()
    fut.set_result(_tick())
    await _apply_block_if_ready(fut, loop, reward_percentiles=reward_percentiles)


async def test_head_tick_requests_core_percentiles() -> None:
    seen: list[list[float]] = []

    async def _record(**kwargs) -> bool:
        await asyncio.sleep(0)  # yield: the fetcher is an awaitable seam
        seen.append(list(kwargs["reward_percentiles"]))
        return True

    await _drive(await _loop(_record), reward_percentiles=(7, 77))

    assert seen == [[7.0, 77.0]], (
        "the head tick must poll the percentiles the core reader returns; a "
        "hardcoded or cached pair escapes this sentinel"
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


# live-invariant gate (ADR-087 D2): watches the live `MIN`/`MAX_PRIORITY_FEE_PERCENTILE`
# declarations in `degenbot-arbitrage/src/simulator.rs`, which must derive from the
# foundation pair; not tripwire precedent.
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
    assert resolved_config().fee_percentiles == _core_percentiles()
