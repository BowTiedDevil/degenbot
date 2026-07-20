"""Tests for the ``[sim-fail]`` per-candidate simulator-failure renderer.

The ``[sim]`` summary line collapses every failed candidate into a
``name=count`` breakdown. ``_render_sim_failures`` augments that aggregate
with a per-candidate ``[sim-fail]`` line, joining the Rust core's per-path
record (``path_id`` + ``bucket`` + the failing call index + the raw revert
bytes) to the path's hop token summary so the operator can identify WHICH
path reverted against WHICH pools.

These tests stub the ``PyDispatchOutcome`` shape (the PyO3 pyclass is too
heavy to instantiate without a full simulate round-trip; the renderer only
reads the two attributes — ``failures: list[dict]`` and ``path_infos:
dict[int, dict]`` — so a ``SimpleNamespace`` is sufficient). WEFVGE:
``path_infos`` values are plain dicts (``{path_type, hops: [hop_dict, …]}``),
not the retired ``*HopInfo`` dataclasses.
"""

from __future__ import annotations

from types import SimpleNamespace
from typing import TYPE_CHECKING, Any

from examples.eth_backrun_v2_v3_v4_rust import _render_sim_failures

if TYPE_CHECKING:
    import pytest

# ── Fixtures ─────────────────────────────────────────────────────────────

WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
USDC = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"


def _hops() -> list[dict[str, Any]]:
    """A 2-hop WETH→USDC→WETH path for the hop-token-summary check.

    WEFVGE: plain dicts (the ``outcome.path_infos`` render shape) — the
    retired ``V2HopInfo`` dataclass is gone. The renderer reads ``family``
    + ``token0/1_address`` / ``zfo`` off the dict directly.
    """
    return [
        {
            "family": "V2",
            "pool_address": "0x" + "b1" * 20,
            "token0_address": WETH,
            "token1_address": USDC,
            "fee": 30,
            "zfo": True,
        },
        {
            "family": "V2",
            "pool_address": "0x" + "b2" * 20,
            "token0_address": USDC,
            "token1_address": WETH,
            "fee": 30,
            "zfo": False,
        },
    ]


def _outcome(failures: list[dict[str, Any]]) -> Any:
    """A stub ``PyDispatchOutcome`` exposing only the renderer-read attrs."""
    path_info = {"path_type": "V2-V2", "hops": _hops()}
    return SimpleNamespace(
        failures=failures,
        path_infos={1: path_info, 2: path_info},
    )


# ── Tests ─────────────────────────────────────────────────────────────────


def test_no_failures_emits_nothing(caplog: pytest.LogCaptureFixture) -> None:
    with caplog.at_level("INFO", logger="degenbot"):
        _render_sim_failures(_outcome([]))
    assert not any("[sim-fail]" in r.message for r in caplog.records)


def test_each_failure_emits_one_line_with_attribution(caplog: pytest.LogCaptureFixture) -> None:
    failures = [
        {
            "path_id": 1,
            "bucket": "Panic(0x11)",
            "fail_index": 3,
            "revert_data": "0x4e487b71" + "0" * 56 + "11",
        },
        {
            "path_id": 2,
            "bucket": "CurrencyNotSettled",
            "fail_index": None,
            "revert_data": "0x5212cba1",
        },
    ]
    with caplog.at_level("INFO", logger="degenbot"):
        _render_sim_failures(_outcome(failures))

    lines = [r.message for r in caplog.records if r.message.startswith("[sim-fail]")]
    assert len(lines) == 2
    assert "path=1" in lines[0]
    assert "type=V2-V2" in lines[0]
    assert "bucket=Panic(0x11)" in lines[0]
    assert "fail_idx=3" in lines[0]
    assert "revert=0x4e487b71" in lines[0]
    assert "hops=" in lines[0]
    # Non-revert bucket has fail_idx=None + a short revert selector.
    assert "path=2" in lines[1]
    assert "bucket=CurrencyNotSettled" in lines[1]
    assert "fail_idx=None" in lines[1]
    assert "revert=0x5212cba1" in lines[1]


def test_missing_path_info_falls_back_gracefully(caplog: pytest.LogCaptureFixture) -> None:
    # path_id 99 isn't in path_infos → renderer must not crash, emits "(path_info missing)".
    failures = [{"path_id": 777, "bucket": "rpc-failed", "fail_index": None, "revert_data": "0x"}]
    with caplog.at_level("INFO", logger="degenbot"):
        _render_sim_failures(_outcome(failures))
    lines = [r.message for r in caplog.records if r.message.startswith("[sim-fail]")]
    assert len(lines) == 1
    assert "path=777" in lines[0]
    assert "type=?" in lines[0]
    assert "bucket=rpc-failed" in lines[0]
    assert "fail_idx=None" in lines[0]
    assert "revert=0x" in lines[0]
    assert "hops=(path_info missing)" in lines[0]


def test_overflow_emits_summary_trailing_line(caplog: pytest.LogCaptureFixture) -> None:
    # 30 failures over the cap (25) → 25 lines + 1 "… (+5 more)" trailing line.
    failures = [
        {"path_id": i, "bucket": "no-profit", "fail_index": None, "revert_data": "0x"}
        for i in range(30)
    ]
    with caplog.at_level("INFO", logger="degenbot"):
        _render_sim_failures(_outcome(failures))
    lines = [r.message for r in caplog.records if r.message.startswith("[sim-fail]")]
    detail_lines = [m for m in lines if "path=" in m]
    summary_lines = [m for m in lines if "(+5 more)" in m]
    assert len(detail_lines) == 25
    assert len(summary_lines) == 1
    assert "(+5 more)" in summary_lines[0]
