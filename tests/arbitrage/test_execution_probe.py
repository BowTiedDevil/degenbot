from __future__ import annotations

from typing import Any

import pytest

from degenbot.abi import encode_single
from degenbot.runner._sim_submit import (
    ExecutorInputs,
    SubmissionStance,
    build_batch_executor,
)
from degenbot.runner.session import SessionPhase
from tests.helpers.session import OFFLINE_RELAY, offline_cfg, session_for_test


class _Pool:
    def __init__(self, ratio: int) -> None:
        self.ratio = ratio

    def calculate_tokens_out(self, zero_for_one: bool, amount_in: int) -> int:
        assert isinstance(zero_for_one, bool)
        return amount_in * self.ratio


class _PyBot:
    def get_pool(self, pool_id: int) -> _Pool:
        return {12: _Pool(10), 13: _Pool(20), 14: _Pool(30)}[pool_id]


class _Bot:
    _py_bot = _PyBot()


class _Engine:
    def pool_id_for_pool(self, family: str, address: str) -> int:
        return {"v2": 12, "v3": 13}[family]

    def pool_id_for_v4_pool(self, manager: str, pool_id: str) -> int:
        return 14

    def payload_path_info(self, path_id: int) -> dict[str, Any]:
        assert path_id == 7
        return {
            "path_type": "V2-V3-V4",
            "hops": [
                {
                    "family": "v2",
                    "pool_address": "0x0000000000000000000000000000000000000002",
                    "zfo": True,
                },
                {
                    "family": "v3",
                    "pool_address": "0x0000000000000000000000000000000000000003",
                    "token0_address": "0x0000000000000000000000000000000000000010",
                    "token1_address": "0x0000000000000000000000000000000000000011",
                    "fee": 3000,
                    "zfo": False,
                },
                {
                    "family": "v4",
                    "pool_manager_address": "0x0000000000000000000000000000000000000004",
                    "pool_id_hex": "0x1234",
                    "currency0_address": "0x0000000000000000000000000000000000000011",
                    "currency1_address": "0x0000000000000000000000000000000000000012",
                    "fee": 3000,
                    "tick_spacing": 60,
                    "hook_address": "0x0000000000000000000000000000000000000000",
                    "zfo": True,
                },
            ],
        }


class _Registry:
    engine = _Engine()


class _Provider:
    def __init__(self) -> None:
        self.quotes = iter([20_000, 600_000])

    async def get_block(self, block: int) -> dict[str, int]:
        assert block == 12_345
        return {"base_fee_per_gas": 77, "timestamp": 1_700_000_001}

    async def call(self, target: str, data: bytes, block: int) -> bytes:
        assert block == 12_345
        return encode_single("uint256", next(self.quotes))


class _Pipeline:
    def __init__(self) -> None:
        self.kwargs = None

    async def probe_path(self, **kwargs: Any) -> dict[str, str]:
        self.kwargs = kwargs
        return {"outcome": "failed", "bucket": "InsufficientProfit"}


@pytest.mark.asyncio
async def test_probe_path_quotes_exact_inputs_and_uses_the_batch_executor() -> None:
    pipeline = _Pipeline()
    session = session_for_test(bot=_Bot(), engine_registry=_Registry(), async_w3=_Provider())
    session.phase = SessionPhase.RUNNING
    session.probe_pipeline = pipeline

    result = await session.probe_path(7, optimal_input=100)

    assert result == {"outcome": "failed", "bucket": "InsufficientProfit"}
    assert pipeline.kwargs == {
        "path_id": 7,
        "optimal_input": 100,
        "hop_outputs": [1000, 20_000, 600_000],
        "consumed_inputs": [100, 1000, 20_000],
        "current_block": 12_345,
        "base_fee_next": 77,
        "block_timestamp": 1_700_000_001,
    }


@pytest.mark.asyncio
async def test_zero_balance_probe_builds_flash_funded_pipeline_without_capital(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    pipeline = _Pipeline()
    session = session_for_test(bot=_Bot(), engine_registry=_Registry(), async_w3=_Provider())
    session.phase = SessionPhase.RUNNING
    calls = []

    async def build(inputs: Any, **kwargs: Any) -> _Pipeline:
        calls.append(kwargs)
        return pipeline

    monkeypatch.setattr("degenbot.runner.session.build_batch_executor", build)
    monkeypatch.setattr(
        "degenbot.runner.session.ExecutorInputs.from_session", lambda _session: object()
    )
    result = await session.probe_path(7, optimal_input=100, zero_balance=True)

    assert result == {"outcome": "failed", "bucket": "InsufficientProfit"}
    assert calls == [
        {"self_fund": False, "zero_balance": True, "submission": SubmissionStance.PROBE}
    ]
    assert session.probe_zero_balance is True


class _ConstructionProvider:
    """The session provider a construction boundary reads (never dialed).

    ``get_transaction_count`` IS called: the FFI's nonce lane is seeded from the
    chain read at construction. The probe's lane is never leased, which is the
    property the stance pins — the seed read is a read, not a consumption.
    """

    def __init__(self) -> None:
        self.nonce_reads: list[str] = []

    async def get_transaction_count(self, address: str) -> int:
        self.nonce_reads.append(address)
        return 5

    def as_async_alloy(self) -> object:
        return object()


def _inputs(*, live: bool = True, relay_posture: Any = None) -> ExecutorInputs:
    """The construction inputs one pipeline is built over.

    ``live=True`` is the LIVE configuration (``cfg.dry_run is False``) that a
    probe has to be safe under.
    """
    cfg = offline_cfg(live=live)
    assert cfg.dry_run is not live
    return ExecutorInputs(
        sim_ctx=object(),
        dispatcher=object(),
        engine=object(),
        provider=_ConstructionProvider(),
        cfg=cfg,
        relay_posture=relay_posture,
    )


def _record_construction() -> tuple[list[dict[str, Any]], Any]:
    """The construction seam: record the FFI kwargs instead of building Rust."""
    calls: list[dict[str, Any]] = []

    def build(**kwargs: Any) -> str:
        calls.append(kwargs)
        return "executor"

    return calls, build


@pytest.mark.asyncio
async def test_a_live_probe_pipeline_is_built_non_submitting_without_relay_fan_out(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A probe under a LIVE config is non-submitting by construction.

    The construction hands the FFI the dry-run stance (no nonce lease, no
    signature, no broadcast) and no relay fan-out at all, so nothing is dialed.
    """
    dialed: list[Any] = []

    def dial(*args: Any, **kwargs: Any) -> list[str]:
        dialed.append((args, kwargs))
        return ["relay-provider"]

    monkeypatch.setattr("degenbot.runner._sim_submit._resolve_relay_providers", dial)
    calls, build = _record_construction()

    executor = await build_batch_executor(
        _inputs(relay_posture=OFFLINE_RELAY),
        engine_builder=build,
        submission=SubmissionStance.PROBE,
    )

    assert executor == "executor"
    assert dialed == [], "a probe construction dials no relay"
    (kwargs,) = calls
    assert kwargs["dry_run"] is True, "a probe never takes the live stance"
    assert kwargs["broadcast_providers"] is None, "a probe fans out to no relay"


@pytest.mark.asyncio
async def test_a_production_pipeline_keeps_the_run_mode_and_the_relay_fan_out(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The production stance is unchanged: the run mode and the posture decide."""
    resolved: list[list[str]] = []

    async def resolve(urls: list[str], providers: Any) -> list[str]:
        resolved.append(list(urls))
        return ["relay-provider"]

    monkeypatch.setattr("degenbot.runner._sim_submit._resolve_relay_providers", resolve)
    calls, build = _record_construction()

    await build_batch_executor(
        _inputs(relay_posture=OFFLINE_RELAY),
        engine_builder=build,
        submission=SubmissionStance.PRODUCTION,
    )

    assert resolved == [list(OFFLINE_RELAY.relay_urls)]
    (kwargs,) = calls
    assert kwargs["dry_run"] is False, "a live production executor still submits"
    assert kwargs["broadcast_providers"] == ["relay-provider"]


@pytest.mark.asyncio
async def test_a_dry_run_session_keeps_its_production_relay_fan_out(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A dry-run production session is unchanged: the fan-out is still resolved."""
    resolves: list[list[str]] = []

    async def resolve(urls: list[str], providers: Any) -> list[str]:
        resolves.append(list(urls))
        return ["relay-provider"]

    monkeypatch.setattr("degenbot.runner._sim_submit._resolve_relay_providers", resolve)
    calls, build = _record_construction()

    await build_batch_executor(
        _inputs(live=False, relay_posture=OFFLINE_RELAY), engine_builder=build
    )

    assert resolves == [list(OFFLINE_RELAY.relay_urls)]
    (kwargs,) = calls
    assert kwargs["dry_run"] is True
    assert kwargs["broadcast_providers"] == ["relay-provider"]


@pytest.mark.asyncio
async def test_the_session_builds_its_probe_pipeline_with_the_probe_stance(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The one probe construction site asks for the probe stance explicitly."""
    pipeline = _Pipeline()
    session = session_for_test(bot=_Bot(), engine_registry=_Registry(), async_w3=_Provider())
    session.phase = SessionPhase.RUNNING
    assert session.cfg.dry_run is False, "the session under test is LIVE"
    calls: list[dict[str, Any]] = []

    async def build(inputs: Any, **kwargs: Any) -> _Pipeline:
        calls.append(kwargs)
        return pipeline

    monkeypatch.setattr("degenbot.runner.session.build_batch_executor", build)
    monkeypatch.setattr(
        "degenbot.runner.session.ExecutorInputs.from_session", lambda _session: object()
    )

    await session.probe_path(7, optimal_input=100)

    assert calls == [
        {"self_fund": True, "zero_balance": False, "submission": SubmissionStance.PROBE}
    ]
