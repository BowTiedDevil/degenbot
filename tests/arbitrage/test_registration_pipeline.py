"""The PRG-5 registration crawl: bounded-window submission over the fleet.

The bounded producer/consumer queue (`run_registration_pipeline`) retired
with the crawl shell — discovery iterates directly and every path leaves as
ONE `PoolStateUpdater` intake unit (build + verify lifecycles + path
registration, all Rust-coordinated). These tests prove, over receipt
doubles (the same scripted shape the intake-station tests use):

- the submission window: at most `REG_INTAKE_WINDOW` receipts outstanding,
  FIFO resolution order (the retired workers' ordering guarantee),
- the benign cap stop: the driver stops submitting after a cap outcome and
  drains the window (the engine is full — the drain is cheap),
- the fatal contract: a unit's VerificationMismatchError propagates through
  the receipt and aborts the crawl loudly,
- counter parity: `_absorb_outcome` folds the unit outcomes into the exact
  counter shapes the retired inline `_consume` produced.

All work runs through the SAME `_consume` body the operator surfaces use,
so behavior cannot diverge by input source (the NWTUM3 bar).

Adapted from the retired pipeline tests (the shell tests back to
5TSYKN/JKYVST, retargeted at the fleet intake by epic IRUMXD PRG-5).
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass
from types import SimpleNamespace
from typing import TYPE_CHECKING

import pytest

from degenbot._ffi import FleetIntakeFaultedError
from degenbot.checksum_cache import get_checksum_address
from degenbot.database.models.pools import UniswapV3PoolTable, UniswapV4PoolTable
from degenbot.exceptions import (
    DynamicFeePoolRejectedError,
    HighFeePoolRejectedError,
    HookedPoolRejectedError,
    PathRejectedError,
    VerificationMismatchError,
)
from degenbot.runner.build_paths import (
    REG_INTAKE_WINDOW,
    PathRegistrationPipeline,
    RegistrationUnitOutcome,
)

if TYPE_CHECKING:
    from collections.abc import AsyncIterator, Callable


class _Receipt:
    """A receipt double: wait_async runs the stored work (raising included)."""

    def __init__(self, work: Callable[[], object]) -> None:
        self._work = work
        self._run = False
        self._value: object | None = None

    async def wait_async(self) -> None:
        self._value = self._work()  # raises here exactly like the Rust seat
        self._run = True

    def result(self) -> object:
        assert self._run, "await wait_async() first"
        return self._value

    def done(self) -> bool:
        return self._run


class _FleetBot:
    """Bot double: intake surface + in-flight bookkeeping, FIFO seat pool."""

    def __init__(self, seats: int = 2) -> None:
        self.seats = seats
        self.submitted: list[Callable[[], object]] = []
        self.receipts: list[_Receipt] = []
        self.max_inflight = 0
        self.inflight = 0

    def registration_fleet_hosted(self) -> bool:
        return True

    def submit_registration_unit(self, fn: Callable[[], object]) -> _Receipt:
        self.submitted.append(fn)
        self.inflight += 1
        self.max_inflight = max(self.max_inflight, self.inflight)
        receipt = _Receipt(self._seat(fn))
        self.receipts.append(receipt)
        return receipt

    def _seat(self, fn: Callable[[], object]) -> Callable[[], object]:
        def _run() -> object:
            try:
                return fn()
            finally:
                self.inflight -= 1

        return _run


class _FaultingReceipt:
    """A receipt double that resolves terminally: the seat never ran.

    This is the T6/S2 lane-death shape — the held unit is RESOLVED (never
    executed), so ``wait_async`` raises the typed fault rather than
    returning a ``result()``.
    """

    async def wait_async(self) -> None:
        raise FleetIntakeFaultedError(
            "fleet registration intake faulted (lane-death): 1 held unit(s) "
            "resolved terminally and were never executed"
        )

    def result(self) -> object:
        raise AssertionError("result() must not be reached: the fault resolved the unit")

    def done(self) -> bool:
        return True


class _FaultingBot:
    """Bot double whose every receipt resolves with the intake fault."""

    def registration_fleet_hosted(self) -> bool:
        return True

    def submit_registration_unit(self, _fn: object) -> _FaultingReceipt:
        return _FaultingReceipt()


@dataclass
class _ScriptedPath:
    """An opaque path whose unit records its processing order."""

    name: str


@dataclass
class _OpaqueStep:
    """A step whose `type` is not a pool table — the unit skips it."""

    type: type
    address: str
    hash: object | None = None


def _pipeline_with_bot(
    constr_bot: object, engine_registry: object = None
) -> tuple[PathRegistrationPipeline, object]:
    """A pipeline over a supplied construction Bot (the skip-gate pattern)."""
    ctx = SimpleNamespace(
        bot=constr_bot,
        chain_id=1,
        db=None,
        uniswap_v3_tracker=None,
        sushiswap_v3_tracker=None,
        pancakeswap_v3_tracker=None,
        weth=None,
    )
    pipeline = PathRegistrationPipeline(
        context=ctx,
        engine_registry=engine_registry,
        # Keep progress logs out of the capture buffer for CI readability.
        progress_interval_secs=1_000_000.0,
    )
    return pipeline, constr_bot


async def _producer(paths: list[object]) -> AsyncIterator[object]:
    for path in paths:
        # Production discovery is a cooperative async iterator (the Rust DFS
        # yields between loop ticks) — mirror that cadence.
        await asyncio.sleep(0)
        yield path


async def test_crawl_submits_one_unit_per_path_and_resolves_fifo() -> None:
    """Every discovered path = one intake unit; receipts resolve in order."""
    order: list[str] = []
    bot = _FleetBot()
    pipeline, _bot = _pipeline_with_bot(bot)

    def unit_shim(path_steps: object, directions: object = None) -> object:
        order.append(path_steps.name)  # type: ignore[attr-defined]
        return RegistrationUnitOutcome(kind="registered", created=True)

    pipeline._registration_unit = unit_shim  # type: ignore[method-assign]

    await pipeline.run_registration(
        producer=_producer([_ScriptedPath(f"p{i}") for i in range(20)]),
    )

    assert len(bot.submitted) == 20
    assert order == [f"p{i}" for i in range(20)], "FIFO resolution order"
    assert pipeline.path_count == 20
    assert not pipeline.capped


async def test_crawl_window_bounds_in_flight_units() -> None:
    """Backpressure is the submission window — never more than its bound."""
    bot = _FleetBot()
    pipeline, _bot = _pipeline_with_bot(bot)

    def unit_shim(path_steps: object, directions: object = None) -> object:
        return RegistrationUnitOutcome(kind="registered", created=True)

    pipeline._registration_unit = unit_shim  # type: ignore[method-assign]

    await pipeline.run_registration(
        producer=_producer([_ScriptedPath(f"p{i}") for i in range(100)]),
    )

    assert bot.max_inflight <= REG_INTAKE_WINDOW
    assert len(bot.submitted) == 100


async def test_crawl_stops_submitting_after_the_cap_and_drains() -> None:
    """A cap outcome stops discovery; the window drains cheaply (PRG-4)."""
    bot = _FleetBot()
    pipeline, _bot = _pipeline_with_bot(bot)
    processed: list[str] = []

    def unit_shim(path_steps: object, directions: object = None) -> object:
        name = path_steps.name  # type: ignore[attr-defined]
        if len(processed) >= 3:
            return RegistrationUnitOutcome(kind="cap", tag="path-cap")
        processed.append(name)
        return RegistrationUnitOutcome(kind="registered", created=True)

    pipeline._registration_unit = unit_shim  # type: ignore[method-assign]

    await pipeline.run_registration(
        producer=_producer([_ScriptedPath(f"p{i}") for i in range(50)]),
    )

    assert pipeline.capped is True
    # The stop bounds the waste: discovery does not keep climbing to 50.
    assert len(bot.submitted) < 50
    # The benign stop folds the caps into the skip/cap counters.
    assert pipeline.cap_skip_count >= 1
    assert pipeline.path_count == 3


async def test_crawl_fatal_verification_error_aborts_loudly() -> None:
    """A unit's VerificationMismatchError propagates — no swallow, no cap."""
    bot = _FleetBot()
    pipeline, _bot = _pipeline_with_bot(bot)

    def unit_shim(path_steps: object, directions: object = None) -> object:
        msg = "boom"
        raise VerificationMismatchError(msg)

    pipeline._registration_unit = unit_shim  # type: ignore[method-assign]

    with pytest.raises(VerificationMismatchError, match="boom"):
        await pipeline.run_registration(
            producer=_producer([_ScriptedPath(f"p{i}") for i in range(10)]),
        )


def test_absorb_outcome_counter_parity() -> None:
    """Outcome folds mirror the retired inline branches exactly."""
    bot = _FleetBot()
    pipeline, _bot = _pipeline_with_bot(bot)

    # Benign skip: skip_count + skip-reason tag.
    pipeline._absorb_outcome(RegistrationUnitOutcome(kind="skip", tag="build-v3:X"))
    assert pipeline.skip_count == 1
    assert pipeline._skip_reasons["build-v3:X"] == 1

    # V4 admission refusals are counted separately, NOT in skip_count.
    pipeline._absorb_outcome(
        RegistrationUnitOutcome(kind="skip", tag="v4-hook-rejected", counts_as_skip=False)
    )
    assert pipeline.v4_hook_rejected == 1
    assert pipeline.skip_count == 1
    pipeline._absorb_outcome(
        RegistrationUnitOutcome(kind="skip", tag="v4-dynamic-fee-rejected", counts_as_skip=False)
    )
    assert pipeline.v4_dynamic_fee_rejected == 1

    # Engine reject: engine_reject + other-Exception (parity with the two
    # retired except-branches that incremented both).
    pipeline._absorb_outcome(RegistrationUnitOutcome(kind="reject"))
    assert pipeline.engine_reject_count == 1
    assert pipeline.other_exc_count == 1

    # register-fail: its own counter + the bounded warning source.
    pipeline._absorb_outcome(RegistrationUnitOutcome(kind="register-fail", tag="ValueError: x"))
    assert pipeline.register_fail_count == 1

    # Registered: created and duplicate folds; v4 hops counted in BOTH
    # (the retired body incremented v4_pool_count pre-dedup).
    pipeline._absorb_outcome(RegistrationUnitOutcome(kind="registered", created=True, v4_hops=1))
    assert pipeline.path_count == 1
    assert pipeline.v4_pool_count == 1
    pipeline._absorb_outcome(RegistrationUnitOutcome(kind="registered", created=False, v4_hops=2))
    assert pipeline.path_count == 1
    assert pipeline.dup_count == 1
    assert pipeline.v4_pool_count == 3
    assert pipeline._skip_reasons["dup"] == 1
    # The cap fold stops the crawl.
    pipeline._absorb_outcome(RegistrationUnitOutcome(kind="cap", tag="path-cap"))
    assert pipeline.capped


def test_legacy_stance_pipeline_construction_refuses() -> None:
    """The hard cutover: no fleet intake, no crawl (loud, actionable)."""
    ctx = SimpleNamespace(
        bot=SimpleNamespace(registration_fleet_hosted=lambda: False),
        chain_id=1,
        db=None,
        uniswap_v3_tracker=None,
        sushiswap_v3_tracker=None,
        pancakeswap_v3_tracker=None,
        weth=None,
    )
    with pytest.raises(RuntimeError, match="fleet-hosted only"):
        PathRegistrationPipeline(context=ctx, engine_registry=None)


def test_retired_skip_gate_pipeline_tests_upgraded_shape() -> None:
    """The skip-gate pipeline shape still builds under the fleet intake."""
    bot = _FleetBot()
    pipeline, _bot = _pipeline_with_bot(bot)
    pipeline._record_skip("v4-no-hash")
    assert pipeline._skip_reasons["v4-no-hash"] == 1


# The retired helper's tests (backpressure/FIFO/validation/fatal abort over a
# `run_registration_pipeline` queue) were retargeted above: the queue is the
# submission WINDOW, the fatal contract is the receipt re-raise, and the
# non-fatal isolation is the outcome fold. The offload-executor probe (a
# build running off the event loop thread) retired with the executor — the
# seat execution is proven by the intake-station subprocess test.


WETH_CHECKSUM = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
T1_CHECKSUM = get_checksum_address("0x" + "11" * 20)
POOL_A = "0x" + "aa" * 20
POOL_B = "0x" + "bb" * 20
POOL_C = "0x" + "cc" * 20


class _FakeV3Pool:
    """Pool double deep enough for direction resolution + engine ids."""

    def __init__(self, address: str, token0: str, token1: str, pool_id: int) -> None:
        self.address = address
        self.token0 = SimpleNamespace(address=token0)
        self.token1 = SimpleNamespace(address=token1)
        self._py_pool = SimpleNamespace(pool_id=pool_id)


class _RecordingRegistry:
    """Engine-registry double: records verify + crawl-path calls."""

    def __init__(self) -> None:
        self.verifies: list[str] = []
        self.registrations: list[list] = []
        self._next_path_id = 0
        self.path_predicate = SimpleNamespace(evaluate=lambda pools_and_zfos: None)

    def run_v3_verify_lifecycle_sync(self, address: str) -> None:
        self.verifies.append(address)

    def register_crawl_path(self, engine_hops: list) -> tuple[int, bool]:
        self.registrations.append(list(engine_hops))
        self._next_path_id += 1
        return (self._next_path_id, True)


def _pipeline_over_registry(
    registry: _RecordingRegistry,
) -> PathRegistrationPipeline:
    """A pipeline whose V3 builds answer from a fixed address -> pool map."""

    pools = {
        POOL_A: _FakeV3Pool(POOL_A, WETH_CHECKSUM, T1_CHECKSUM, pool_id=101),
        POOL_B: _FakeV3Pool(POOL_B, T1_CHECKSUM, WETH_CHECKSUM, pool_id=202),
    }

    class _Tracker:
        def get_pool(
            self,
            *,
            pool_address: str,
            silent: bool = True,
        ) -> _FakeV3Pool:
            return pools[pool_address]

    bot = SimpleNamespace(registration_fleet_hosted=lambda: True)
    ctx = SimpleNamespace(
        bot=bot,
        chain_id=1,
        db=None,
        uniswap_v3_tracker=_Tracker(),
        sushiswap_v3_tracker=None,
        pancakeswap_v3_tracker=None,
        weth=SimpleNamespace(address=WETH_CHECKSUM),
    )
    return PathRegistrationPipeline(
        context=ctx,
        engine_registry=registry,  # type: ignore[arg-type]
        progress_interval_secs=1_000_000.0,
    )


def _closed_v3_cycle_steps() -> list[_OpaqueStep]:
    """WETH->T1 (pool A) then T1->WETH (pool B): the cycle closes."""
    return [
        _OpaqueStep(type=UniswapV3PoolTable, address=POOL_A, hash=None),
        _OpaqueStep(type=UniswapV3PoolTable, address=POOL_B, hash=None),
    ]


def test_duplicate_candidate_short_circuits_before_verify_and_engine() -> None:
    """A hop signature already registered answers dup WITHOUT verify or RPC.

    The PRG-4 cutover moved dedup behind the V3/V4 verify lifecycles, so every
    duplicate candidate re-ran the full choreography (observed live: 945
    verify lifecycles for 122 unique pools). The memo in front of verify must
    return the SAME outcome shape the engine dedup produces (created=False)
    while leaving the engine's registry untouched after the first sighting.
    """
    registry = _RecordingRegistry()
    pipeline = _pipeline_over_registry(registry)
    steps = _closed_v3_cycle_steps()

    first = pipeline._registration_unit(steps)
    assert first.kind == "registered"
    assert first.created is True
    pipeline._absorb_outcome(first)
    assert pipeline.path_count == 1
    assert registry.registrations == [[(101, True), (202, True)]]

    second = pipeline._registration_unit(steps)
    assert second.kind == "registered"
    assert second.created is False
    pipeline._absorb_outcome(second)
    # Counter parity: the dup fold matches the engine-dedup fold.
    assert pipeline.path_count == 1
    assert pipeline.dup_count == 1
    assert pipeline._skip_reasons["dup"] == 1

    # The engine saw each hop verified exactly once and registered once.
    assert registry.verifies == [POOL_A, POOL_B]
    assert len(registry.registrations) == 1


def test_path_predicate_evaluates_before_verify() -> None:
    """D7KMQO policy enforcement happens before any verify choreography.

    The predicate docstring promises "before any work"; PRG-4 left it after
    the verify loop, so a policy-rejected path paid lifecycles first. A
    rejection must now surface with the engine never having verified.
    """
    registry = _RecordingRegistry()
    pipeline = _pipeline_over_registry(registry)

    class _PolicyRejection(Exception):
        """The recorded D7KMQO refusal."""

    def _refuse(pools_and_zfos: object) -> None:
        msg = "policy: not deployed"
        raise _PolicyRejection(msg)

    registry.path_predicate = SimpleNamespace(evaluate=_refuse)

    outcome = pipeline._registration_unit(_closed_v3_cycle_steps())
    assert outcome.kind == "reject"
    assert registry.verifies == []
    assert len(registry.registrations) == 0


# ---------------------------------------------------------------------------
# Cold-soak negative-memoization (2026-09-11 follow-up to W73FVY): the dup
# memo only answers candidates whose registration COMPLETED. Stable-negative
# outcomes - pool-level build refusals and the D7KMQO policy gate - returned
# unmemoized, so the DFS re-paid full build+verify choreographies on every
# re-sighting (measured live: 1206 verify lifecycles / 132 unique pools,
# top pools ~50x, registered paths flat at the boot value for 1h+). Three
# memos close the hole WITHOUT classifying transient failures as permanent:
#   1. verify-once-per-pipeline  (a completed verify lifecycle is a pool fact)
#   2. unregistrable-pool memo   (stable build refusals are pool facts)
#   3. rejected-path memo        (the policy gate deny is deterministic per
#                                  hop signature); TRANSIENT register-fails
#                                  are deliberately NOT memoized.
# ---------------------------------------------------------------------------


class _RegisterThenFailRegistry(_RecordingRegistry):
    """First crawl-path registration succeeds; every later one fails."""

    def register_crawl_path(self, engine_hops: list) -> tuple[int, bool]:
        if self.registrations:
            scripted = RuntimeError("-scripted-register-fail")
            raise scripted
        return super().register_crawl_path(engine_hops)


def _pipeline_over_registry_three_pools(
    registry: _RecordingRegistry,
) -> PathRegistrationPipeline:
    """Like _pipeline_over_registry but with a third pool; V3-only chain."""
    pools = {
        POOL_A: _FakeV3Pool(POOL_A, WETH_CHECKSUM, T1_CHECKSUM, pool_id=101),
        POOL_B: _FakeV3Pool(POOL_B, T1_CHECKSUM, WETH_CHECKSUM, pool_id=202),
        POOL_C: _FakeV3Pool(POOL_C, WETH_CHECKSUM, T1_CHECKSUM, pool_id=303),
    }

    class _Tracker:
        def get_pool(
            self,
            *,
            pool_address: str,
            silent: bool = True,
        ) -> _FakeV3Pool:
            return pools[pool_address]

    bot = SimpleNamespace(registration_fleet_hosted=lambda: True)
    ctx = SimpleNamespace(
        bot=bot,
        chain_id=1,
        db=None,
        uniswap_v3_tracker=_Tracker(),
        sushiswap_v3_tracker=None,
        pancakeswap_v3_tracker=None,
        weth=SimpleNamespace(address=WETH_CHECKSUM),
    )
    return PathRegistrationPipeline(
        context=ctx,
        engine_registry=registry,  # type: ignore[arg-type]
        progress_interval_secs=1_000_000.0,
    )


def test_verify_lifecycle_runs_once_per_pool_across_sightings() -> None:
    """A completed verify lifecycle is never re-run on later sightings.

    The seat claims table dedups CONCURRENT windows only; a register-fail
    candidate re-submitted later re-ran the FULL verify choreography for
    every hop it shared with earlier candidates (measured live: same pool
    verified ~50x while registered paths stayed flat). Verify-once per
    pipeline is exact: the lifecycle is a pool-fact operation, and a failed
    registration does NOT un-verify a pool.
    """
    registry = _RegisterThenFailRegistry()
    pipeline = _pipeline_over_registry_three_pools(registry)

    first = pipeline._registration_unit([
        _OpaqueStep(type=UniswapV3PoolTable, address=POOL_A, hash=None),
        _OpaqueStep(type=UniswapV3PoolTable, address=POOL_B, hash=None),
    ])
    assert first.kind == "registered"

    shared_hop = [
        _OpaqueStep(type=UniswapV3PoolTable, address=POOL_A, hash=None),
        _OpaqueStep(type=UniswapV3PoolTable, address=POOL_C, hash=None),
    ]
    second = pipeline._registration_unit(shared_hop)
    assert second.kind == "register-fail"
    pipeline._absorb_outcome(second)
    third = pipeline._registration_unit(shared_hop)
    assert third.kind == "register-fail"
    pipeline._absorb_outcome(third)

    # Exactness preserved: register-fail is NOT negatively memoized, so the
    # candidate re-attempted registration on every sighting.
    assert len(registry.registrations) == 1
    assert pipeline.register_fail_count == 2

    # But a pool never re-verifies: A was verified under the FIRST candidate;
    # C under the second; the third sighting re-verified neither.
    assert registry.verifies == [POOL_A, POOL_B, POOL_C]
    assert registry.verifies.count(POOL_A) == 1
    assert registry.verifies.count(POOL_C) == 1


def test_stable_build_refusal_memoizes_the_pool() -> None:
    """A stably-refused pool (typed build rejection) skips at O(hops).

    v4-hook-rejected is a POOL fact, not a transient failure: any later
    candidate containing that pool cannot register. The unregistrable-pool
    memo must answer BEFORE the build is re-attempted (the pathological
    region re-yielded the same refused pool in ~111k candidates).
    """
    build_calls: list[object] = []

    def _build_managed_pool(address: object, request: object) -> None:
        build_calls.append(getattr(request, "pool_id", None))
        hooked: HookedPoolRejectedError = HookedPoolRejectedError
        raise hooked

    bot = SimpleNamespace(
        registration_fleet_hosted=lambda: True,
        build_managed_pool=_build_managed_pool,
        build_pool=lambda *a, **k: None,
    )
    pipeline, _ = _pipeline_with_bot(bot)
    steps = [
        _OpaqueStep(type=UniswapV4PoolTable, address=None, hash=0xDEAD),
        _OpaqueStep(type=UniswapV4PoolTable, address=None, hash=0xBEEF),
    ]

    first = pipeline._registration_unit(steps)
    assert first.kind == "skip"
    assert first.tag == "v4-hook-rejected"
    assert first.counts_as_skip is False
    pipeline._absorb_outcome(first)

    second = pipeline._registration_unit(steps)
    assert second.kind == "skip"
    assert second.tag == "v4-hook-rejected"
    assert second.counts_as_skip is False

    # The refused pool's build is attempted exactly ONCE; the second sighting
    # is answered from the unregistrable-pool memo without any build.
    assert build_calls == [0xDEAD]


def test_policy_gate_denial_memoizes_the_path() -> None:
    """A D7KMQO policy deny is deterministic per hop signature.

    The gate evaluates before any work (order preserved), and its denial is
    stable: re-yielded candidates answer from the rejected-path memo without
    a second gate evaluation (and without verify - the gate precedes it).
    """
    gate_calls: list[list] = []

    class _ScriptedRegistry(_RecordingRegistry):
        pass

    def _evaluate(pools_and_zfos: list) -> None:
        gate_calls.append(list(pools_and_zfos))
        policy_denied: PathRejectedError = PathRejectedError
        raise policy_denied

    registry = _ScriptedRegistry()
    registry.path_predicate = SimpleNamespace(evaluate=_evaluate)
    pipeline = _pipeline_over_registry(registry)
    steps = _closed_v3_cycle_steps()

    first = pipeline._registration_unit(steps)
    assert first.kind == "reject"
    assert first.tag is not None
    assert first.tag.startswith("PathRejectedError")
    assert registry.verifies == []  # the gate precedes verify
    pipeline._absorb_outcome(first)

    second = pipeline._registration_unit(steps)
    assert second.kind == "reject"
    # The memo answers with a STABLE tag (the first sighting's tag
    # interpolates the exception text; the memo's must stay greppable).
    assert second.tag == "path-rejected-memo"
    assert registry.verifies == []

    # The gate ran exactly once; the second sighting answered from the memo.
    assert len(gate_calls) == 1


async def test_trigger_discovery_stops_once_sweep_completes_for_edition() -> None:
    """Once a full (uncapped, unbounded-cut) sweep completes for a graph
    edition, later triggers with the SAME edition stop immediately: the
    unchanged structure can only re-yield already-registered paths.
    """
    bot = _FleetBot()
    pipeline, _bot = _pipeline_with_bot(bot)

    paths = [_closed_v3_cycle_steps() for _ in range(3)]
    sweep_count = {"n": 0}

    def _sweep() -> AsyncIterator[object]:
        sweep_count["n"] += 1
        return _producer(list(paths))

    pipeline.discovery_sweep = _sweep  # type: ignore[method-assign]
    edition = {"v": (10, 20, 3, 30)}
    pipeline._graph_edition = lambda: edition["v"]  # type: ignore[method-assign]

    # First sweep runs and registers every path.
    assert await pipeline.trigger_discovery() == 3
    assert sweep_count["n"] == 1
    assert bot.submitted  # units were processed

    # Unchanged edition: the trigger stops before enumerating.
    bot.submitted.clear()
    assert await pipeline.trigger_discovery() == 0
    assert sweep_count["n"] == 1
    assert bot.submitted == []

    # New edition (pool added): a fresh sweep runs.
    edition["v"] = (11, 21, 3, 30)
    assert await pipeline.trigger_discovery() == 3
    assert sweep_count["n"] == 2


async def test_trigger_discovery_bound_truncation_does_not_latch() -> None:
    """A bound-truncated sweep did NOT see the last path — later triggers
    must still enumerate."""
    bot = _FleetBot()
    pipeline, _bot = _pipeline_with_bot(bot)

    paths = [_closed_v3_cycle_steps() for _ in range(3)]
    sweep_count = {"n": 0}

    def _sweep() -> AsyncIterator[object]:
        sweep_count["n"] += 1
        return _producer(list(paths))

    pipeline.discovery_sweep = _sweep  # type: ignore[method-assign]
    pipeline._graph_edition = lambda: (10, 20, 3, 30)  # type: ignore[method-assign]

    assert await pipeline.trigger_discovery(bound=2) == 2
    assert sweep_count["n"] == 1

    # Same edition, but the prior sweep was truncated: it must re-run.
    assert await pipeline.trigger_discovery() == 3
    assert sweep_count["n"] == 2


async def test_trigger_discovery_no_db_never_latches() -> None:
    """Without a DB handle the edition probe is None — the latch stays
    disabled and every trigger enumerates (the pre-existing behavior)."""
    bot = _FleetBot()
    pipeline, _bot = _pipeline_with_bot(bot)  # context db=None

    paths = [_closed_v3_cycle_steps() for _ in range(2)]
    sweep_count = {"n": 0}

    def _sweep() -> AsyncIterator[object]:
        sweep_count["n"] += 1
        return _producer(list(paths))

    pipeline.discovery_sweep = _sweep  # type: ignore[method-assign]

    assert await pipeline.trigger_discovery() == 2
    assert await pipeline.trigger_discovery() == 2
    assert sweep_count["n"] == 2


async def test_intake_fault_propagates_through_consume() -> None:
    """The operator `_consume` seam surfaces the typed lane-death fault.

    FALSIFICATION: a swallowed fault (the counters folding a non-outcome) or
    a parked waiter; the typed error must escape `_consume` unchanged.
    """
    pipeline, _bot = _pipeline_with_bot(_FaultingBot())
    with pytest.raises(FleetIntakeFaultedError):
        await pipeline._consume(_ScriptedPath("p"))
    assert pipeline.path_count == 0, "a faulted unit is never folded as registered"


async def test_intake_fault_propagates_through_run_registration() -> None:
    """The crawl's `_resolve` seam surfaces the typed lane-death fault.

    FALSIFICATION: the crawl returning normally with the fault unobserved.
    """
    pipeline, _bot = _pipeline_with_bot(_FaultingBot())
    with pytest.raises(FleetIntakeFaultedError):
        await pipeline.run_registration(producer=_producer([_ScriptedPath("p")]))
    assert pipeline.path_count == 0


# ---------------------------------------------------------------------------
# N3IRYT: the cockpit-private registration outcome ledger owns the one memo
# concept. The four ad-hoc collections retire from the pipeline; build-refusal
# stability is classified by exception TYPE (never the exception class name),
# and the metric tag path draws from a bounded outcome vocabulary.
# ---------------------------------------------------------------------------


def test_registration_ledger_owns_the_four_memo_concepts() -> None:
    """The four memo collections live on ONE ledger, not on the pipeline.

    FALSIFICATION: any retired instance attribute still answering, or a
    concept the unit uses but the ledger does not own.
    """
    from degenbot.runner._registration_ledger import RegistrationLedger

    pipeline, _bot = _pipeline_with_bot(_FleetBot())
    assert isinstance(pipeline._ledger, RegistrationLedger)
    for retired in (
        "_registered_paths_seen",
        "_verified_once",
        "_unregistrable_pools",
        "_rejected_paths_seen",
    ):
        assert not hasattr(pipeline, retired), f"{retired} must be owned by the ledger"

    # Registered-path + verify-once concepts, after one real unit.
    registry = _RecordingRegistry()
    pipeline = _pipeline_over_registry(registry)
    steps = _closed_v3_cycle_steps()
    first = pipeline._registration_unit(steps)
    assert first.kind == "registered"
    hop_sig = ((101, True), (202, True))
    assert pipeline._ledger.path_registered(hop_sig), "registered-path memo"
    assert pipeline._ledger.pool_verified(f"v3:{POOL_A}"), "verify-once memo"
    assert pipeline._ledger.pool_verified(f"v3:{POOL_B}")

    # Rejected-path concept (the D7KMQO deny memoizes per hop signature).
    denied = _RecordingRegistry()
    denied.path_predicate = SimpleNamespace(evaluate=_raise_path_rejected)
    pipeline2 = _pipeline_over_registry(denied)
    assert pipeline2._registration_unit(_closed_v3_cycle_steps()).kind == "reject"
    assert pipeline2._ledger.path_rejected(hop_sig), "rejected-path memo"

    # Unregistrable-pool concept (a stable V4 admission refusal).
    def _build_managed_pool(_address: object, _request: object) -> None:
        raise HookedPoolRejectedError

    bot = SimpleNamespace(
        registration_fleet_hosted=lambda: True,
        build_managed_pool=_build_managed_pool,
    )
    pipeline3, _ = _pipeline_with_bot(bot)
    v4_steps = [_OpaqueStep(type=UniswapV4PoolTable, address=None, hash=0xDEAD)]
    assert pipeline3._registration_unit(v4_steps).tag == "v4-hook-rejected"
    key = pipeline3._ledger.pool_memo_key(v4_steps[0], "V4")
    assert pipeline3._ledger.unregistrable_record(key) is not None, "unregistrable-pool memo"


def _raise_path_rejected(_pools_and_zfos: object) -> None:
    raise PathRejectedError(message="policy deny")


def test_build_refusal_classification_is_typed_not_class_name_based() -> None:
    """Stability matches exception TYPE; a same-named class stays transient.

    FALSIFICATION: classification on the class name — the impostor
    below would be judged stable.
    """
    from degenbot.runner._registration_ledger import RegistrationLedger

    impostor = type("HighFeePoolRejectedError", (RuntimeError,), {})
    transient = RegistrationLedger.classify_build_refusal(impostor("x"), pool_type="V3")
    assert transient.stable is False

    high_fee = RegistrationLedger.classify_build_refusal(HighFeePoolRejectedError(), pool_type="V3")
    assert high_fee.stable is True
    assert high_fee.counts_as_skip is True

    hooked = RegistrationLedger.classify_build_refusal(HookedPoolRejectedError(), pool_type="V4")
    assert hooked.stable is True
    assert hooked.counts_as_skip is False

    dynamic = RegistrationLedger.classify_build_refusal(
        DynamicFeePoolRejectedError(), pool_type="V4"
    )
    assert dynamic.stable is True
    assert dynamic.counts_as_skip is False


def test_impostor_class_name_is_never_memoized() -> None:
    """A name-only stable match stays retryable (build re-attempted).

    FALSIFICATION: the retired class-name set would have memoized the
    impostor, answering the second sighting without a build.
    """
    impostor = type("HighFeePoolRejectedError", (RuntimeError,), {})
    builds: list[str] = []

    def _build_pool(address: str, *, silent: bool = True) -> None:
        builds.append(address)
        msg = "name matches, type does not"
        raise impostor(msg)

    bot = SimpleNamespace(registration_fleet_hosted=lambda: True, build_pool=_build_pool)
    pipeline, _ = _pipeline_with_bot(bot)
    steps = [_OpaqueStep(type=UniswapV3PoolTable, address=POOL_A, hash=None)]

    assert pipeline._registration_unit(steps).kind == "skip"
    assert pipeline._registration_unit(steps).kind == "skip"
    assert builds == [POOL_A, POOL_A], "a name-only match must never be memoized"
    assert (
        pipeline._ledger.unregistrable_record(pipeline._ledger.pool_memo_key(steps[0], "V3"))
        is None
    )


def test_real_typed_stable_refusal_is_memoized() -> None:
    """The real typed refusal IS a pool fact: the second sighting memoizes."""
    builds: list[str] = []

    def _build_pool(address: str, *, silent: bool = True) -> None:
        builds.append(address)
        raise HighFeePoolRejectedError

    bot = SimpleNamespace(registration_fleet_hosted=lambda: True, build_pool=_build_pool)
    pipeline, _ = _pipeline_with_bot(bot)
    steps = [_OpaqueStep(type=UniswapV3PoolTable, address=POOL_A, hash=None)]

    assert pipeline._registration_unit(steps).kind == "skip"
    assert pipeline._registration_unit(steps).kind == "skip"
    assert builds == [POOL_A], "the typed stable refusal memoizes the pool"


def test_transient_build_skip_tag_uses_the_bounded_vocabulary() -> None:
    """Every production skip tag is a member of the bounded vocabulary.

    FALSIFICATION: an interpolated class-name tag (build-v3:ConnectionError)
    reaching the metric path; the tag must be closed-set, the class name
    log-only in the detail.
    """
    from degenbot.runner._registration_ledger import RegistrationOutcome

    def _build_pool(address: str, *, silent: bool = True) -> None:
        msg = "rpc blip"
        raise ConnectionError(msg)

    bot = SimpleNamespace(registration_fleet_hosted=lambda: True, build_pool=_build_pool)
    pipeline, _ = _pipeline_with_bot(bot)
    outcome = pipeline._registration_unit([
        _OpaqueStep(type=UniswapV3PoolTable, address=POOL_A, hash=None)
    ])
    assert outcome.tag in {member.value for member in RegistrationOutcome}
    assert outcome.detail is not None
    assert "ConnectionError" in outcome.detail
    pipeline._absorb_outcome(outcome)
    assert pipeline._skip_reasons[outcome.tag] == 1
