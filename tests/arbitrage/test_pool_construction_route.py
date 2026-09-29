"""Pool construction route — the loud-abort classification contract.

The construction route (CONTEXT.md, pool-construction card) owns ONE core
entry from a requested pool to a constructed, registered pool. Its failures
classify on the build-refusal taxonomy:

- a STABLE family refusal (the route serves no rung for the pool's factory)
  aborts LOUDLY — the typed ``UnsupportedPoolFamilyError`` propagates out of
  the registration unit, never a silent skip (ADR-055 D4);
- a TRANSIENT failure keeps skip semantics — a counted, retriable skip;
- the V4 admission refusals keep their stable-fact memo + skip behavior.

The retired driver-side fallback chain (three V3 trackers + the generic
builder, bare except-and-continue between rungs) is the bug these tests pin
the fix for: the route lives core-side and the driver supplies resolved
policy values only.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

from degenbot.exceptions import (
    DynamicFeePoolRejectedError,
    HookedPoolRejectedError,
    UnsupportedPoolFamilyError,
)
from degenbot.pathfinding import PoolKind
from degenbot.runner.build_paths import ConstructionContext, PathRegistrationPipeline
from degenbot.runner.identity import (
    PANCAKESWAP_V3_MAINNET_FACTORY,
    SUSHISWAP_V3_MAINNET_FACTORY,
    UNISWAP_V3_MAINNET_FACTORY,
    WETH_ADDRESS,
)


class _RouteRecordingBot:
    """A fake construction bot: records the build call, raises on demand."""

    def __init__(self, *, exc: BaseException | None = None) -> None:
        self.chain_id = 1
        self.database_path = Path("unused.db")
        self.exc = exc
        self.build_calls: list[dict[str, Any]] = []

    def registration_fleet_hosted(self) -> bool:
        return True

    def build_erc20token(self, address: str) -> Any:
        return f"weth:{address}"

    def build_pool(self, address: str, *, silent: bool = False, **kwargs: Any) -> Any:
        self.build_calls.append({"address": address, **kwargs})
        if self.exc is not None:
            raise self.exc
        return f"pool:{address}"

    def build_managed_pool(self, pool_manager: str, request: Any) -> Any:
        # The V4 arm routes through the managed-pool entry; the fake raises
        # the same injected refusal so the classification is exercised.
        if self.exc is not None:
            raise self.exc
        return f"pool:{getattr(request, 'pool_id', '?')}"


def _make_pipeline(bot: Any) -> PathRegistrationPipeline:
    ctx = ConstructionContext.for_bot(bot)
    return PathRegistrationPipeline(
        context=ctx,
        engine_registry=None,
        max_paths=0,
        discovery_batch_size=8,
    )


def _step(*, pool_type: Any = PoolKind.V3, v4_hash: Any = None) -> Any:
    class _Step:
        type = pool_type
        address = "0x" + "22" * 20
        hash = v4_hash

    return _Step()


def _v4_step() -> Any:
    # A V4 hop carries its on-chain pool id pre-build (the DB edge), so a
    # stable admission refusal is recognizable without an RPC.
    return _step(pool_type=PoolKind.V4, v4_hash=b"\\xab" * 32)


class TestConstructionContext:
    def test_for_bot_resolves_policy_values_without_trackers(self) -> None:
        """The context shrinks to resolved policy values: the three V3
        trackers leave it (the core route owns the construction order), and
        the resolved route policy enters."""

        class _NoTrackerBot(_RouteRecordingBot):
            def add_tracker(self, *_a: Any, **_k: Any) -> Any:  # pragma: no cover
                msg = "the construction route is core policy — the driver must not build trackers"
                raise AssertionError(msg)

        ctx = ConstructionContext.for_bot(_NoTrackerBot())
        assert ctx.bot.chain_id == 1
        assert ctx.weth == f"weth:{WETH_ADDRESS}"
        route = ctx.construction_route
        assert route.generic is True
        assert set(route.factories) == {
            UNISWAP_V3_MAINNET_FACTORY,
            SUSHISWAP_V3_MAINNET_FACTORY,
            PANCAKESWAP_V3_MAINNET_FACTORY,
        }
        # The context carries NO tracker attributes.
        assert not hasattr(ctx, "uniswap_v3_tracker")
        assert not hasattr(ctx, "sushiswap_v3_tracker")
        assert not hasattr(ctx, "pancakeswap_v3_tracker")


class TestRouteFailureClassification:
    def test_stable_factory_failure_aborts_loudly(self) -> None:
        """A stable family refusal (the core route's loud arm) propagates out
        of the unit as the typed fatal — never a silent skip."""
        bot = _RouteRecordingBot(
            exc=UnsupportedPoolFamilyError(
                "construction route refused pool at 0x22: factory matches no rung"
            )
        )
        pipeline = _make_pipeline(bot)
        with pytest.raises(UnsupportedPoolFamilyError):
            pipeline._build_hop_pools([_step()], ["V3"])

    def test_transient_rpc_failure_is_a_counted_skip(self) -> None:
        """A transient failure keeps skip semantics: a counted skip outcome,
        never memoized (a retriable blip must stay retryable)."""
        bot = _RouteRecordingBot(exc=RuntimeError("connection blip"))
        pipeline = _make_pipeline(bot)
        outcome = pipeline._build_hop_pools([_step()], ["V3"])
        assert outcome is not None
        assert outcome.kind == "skip"
        assert outcome.tag == "build-v3-refused"
        assert outcome.counts_as_skip is True
        assert (
            pipeline._ledger.unregistrable_record(pipeline._ledger.pool_memo_key(_step(), "V3"))
            is None
        ), "a transient failure is never memoized"

    def test_v4_admission_refusal_keeps_stable_fact_semantics(self) -> None:
        """The V4 admission refusals are stable pool facts: memoized with
        their own counters (counts_as_skip False) — unchanged."""
        bot = _RouteRecordingBot(exc=HookedPoolRejectedError("hooked"))
        pipeline = _make_pipeline(bot)
        step = _v4_step()
        outcome = pipeline._build_hop_pools([step], ["V4"])
        assert outcome is not None
        assert outcome.kind == "skip"
        assert outcome.tag == "v4-hook-rejected"
        assert outcome.counts_as_skip is False
        assert (
            pipeline._ledger.unregistrable_record(pipeline._ledger.pool_memo_key(step, "V4"))
            is not None
        ), "a stable admission fact memoizes"

    def test_v4_dynamic_fee_refusal_keeps_stable_fact_semantics(self) -> None:
        bot = _RouteRecordingBot(exc=DynamicFeePoolRejectedError("dynamic"))
        pipeline = _make_pipeline(bot)
        outcome = pipeline._build_hop_pools([_v4_step()], ["V4"])
        assert outcome is not None
        assert outcome.tag == "v4-dynamic-fee-rejected"


class TestRoutePolicyFlows:
    def test_v3_build_call_carries_the_resolved_route(self) -> None:
        """The cockpit supplies the resolved route as a policy VALUE on the
        one core build entry — no tracker rungs, no driver-side chain."""
        bot = _RouteRecordingBot()
        pipeline = _make_pipeline(bot)
        outcome = pipeline._build_hop_pools([_step()], ["V3"])
        assert isinstance(outcome, list), "a successful build returns the pools, not an outcome"
        assert len(bot.build_calls) == 1, "one core entry, not a rung chain"
        call = bot.build_calls[0]
        assert call["construction_route"] is pipeline.construction_route
        assert call["construction_route"].generic is True
