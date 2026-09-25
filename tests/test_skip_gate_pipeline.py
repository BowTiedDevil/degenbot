"""Pipeline-side skip recording for path registration (INN6TK + PRG-2).

The reason-tagged skip breakdown ([build_paths] Progress lines) records every
candidate skip; PRG-2 additionally lands each skip in the Rust
`degenbot.registration.skips` meter family through the FFI. The former
SkipGate fatal memo retired: immutable V4 admission verdicts are refused
pre-RPC by the core registration gate (bot_core/registration_gate.rs), and
raced duplicate builds self-heal in the engine single-flight path (PRG-1).
"""

from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace

from degenbot.runner.build_paths import PathRegistrationPipeline


def make_pipeline(py_bot: object | None = None) -> PathRegistrationPipeline:
    ctx = SimpleNamespace(
        bot=SimpleNamespace(_py_bot=py_bot, registration_fleet_hosted=lambda: True),
        chain_id=1,
        database_path=Path("unused.db"),
        uniswap_v3_tracker=None,
        sushiswap_v3_tracker=None,
        pancakeswap_v3_tracker=None,
        weth=None,
    )
    return PathRegistrationPipeline(context=ctx, engine_registry=None)


class _RecordingPyBot:
    def __init__(self) -> None:
        self.reasons: list[str] = []

    def record_registration_skip(self, reason: str) -> None:
        self.reasons.append(reason)


def test_record_skip_counts_the_reason() -> None:
    p = make_pipeline()
    p._record_skip("build-v3:ConnectionError")
    p._record_skip("build-v3:ConnectionError")
    p._record_skip("dup")
    assert p._skip_reasons["build-v3:ConnectionError"] == 2
    assert p._skip_reasons["dup"] == 1


def test_record_skip_forwards_to_the_rust_meter() -> None:
    meter = _RecordingPyBot()
    p = make_pipeline(py_bot=meter)
    p._record_skip("v4-dynamic-fee-rejected")
    p._record_skip("path-cap")
    assert meter.reasons == ["v4-dynamic-fee-rejected", "path-cap"]


def test_record_skip_without_a_bot_only_counts() -> None:
    # Construction contexts without a live core (pipeline tests) must not
    # attempt the meter record.
    ctx = SimpleNamespace(
        bot=SimpleNamespace(_py_bot=None, registration_fleet_hosted=lambda: True),
        chain_id=1,
        database_path=Path("unused.db"),
        uniswap_v3_tracker=None,
        sushiswap_v3_tracker=None,
        pancakeswap_v3_tracker=None,
        weth=None,
    )
    p = PathRegistrationPipeline(context=ctx, engine_registry=None)
    p._record_skip("v4-no-hash")
    assert p._skip_reasons["v4-no-hash"] == 1
