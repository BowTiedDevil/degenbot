"""Tests for the ``[sim-fail]`` per-candidate simulator-failure renderer.

The ``[sim]`` summary line collapses every failed candidate into a
``name=count`` breakdown. ``_render_sim_failures`` augments that aggregate
with a per-candidate ``[sim-fail]`` line, joining the Rust core's per-path
record (``path_id`` + ``bucket`` + the failing call index + the raw revert
bytes) to the path's hop token summary so the operator can identify WHICH
path reverted against WHICH pools.

These tests stub the ``DispatchOutcome`` shape (the PyO3 pyclass is too
heavy to instantiate without a full simulate round-trip; the renderer only
reads the two attributes — ``failures: list[dict]`` and ``path_infos:
dict[int, dict]`` — so a ``SimpleNamespace`` is sufficient). WEFVGE:
``path_infos`` values are plain dicts (``{path_type, hops: [hop_dict, …]}``),
not the retired ``*HopInfo`` dataclasses.
"""

from __future__ import annotations

from types import SimpleNamespace
from typing import Any

import pytest

from degenbot.runner._render import _render_sim_failures

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
    """A stub ``DispatchOutcome`` exposing only the renderer-read attrs."""
    path_info = {"path_type": "V2-V2", "hops": _hops()}
    return SimpleNamespace(
        failures=failures,
        path_infos={1: path_info, 2: path_info},
    )


@pytest.fixture(autouse=True)
def _disable_sim_exit_on_fail(monkeypatch: pytest.MonkeyPatch) -> None:
    """Disable the ``sys.exit(3)`` trap for renderer unit tests.

    ``_render_sim_failures`` exits on the first non-ignored failure bucket when
    ``DEGENBOT_SIM_EXIT_ON_FAIL`` is set (default ``"1"`` — the aggressive
    production default per AGENTS-DEGENBOT-459). These tests exercise the
    RENDERING contract (the ``[sim-fail]`` / ``[sim-diag]`` lines), NOT the
    trap, so the trap is force-disabled here to keep the failure records
    renderable.
    """
    monkeypatch.setenv("DEGENBOT_SIM_EXIT_ON_FAIL", "0")


# ── Tests ─────────────────────────────────────────────────────────────────


def test_no_failures_emits_nothing(caplog: pytest.LogCaptureFixture) -> None:
    with caplog.at_level("INFO", logger="degenbot"):
        _render_sim_failures(_outcome([]), current_block=100)
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
        _render_sim_failures(_outcome(failures), current_block=100)

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
        _render_sim_failures(_outcome(failures), current_block=100)
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
        _render_sim_failures(_outcome(failures), current_block=100)
    lines = [r.message for r in caplog.records if r.message.startswith("[sim-fail]")]
    detail_lines = [m for m in lines if "path=" in m]
    summary_lines = [m for m in lines if "(+5 more)" in m]
    assert len(detail_lines) == 25
    assert len(summary_lines) == 1
    assert "(+5 more)" in summary_lines[0]


def test_reverting_frame_surfaces_deep_attribution(caplog: pytest.LogCaptureFixture) -> None:
    # Ergo epic 63I7WJ — the inspector-captured reverting frame: the CONTRACT
    # that reverted (not the top-level bubble), its call depth, selector, + the
    # classify_revert label. Plus the swaps captured before the revert.
    failures = [
        {
            "path_id": 1,
            "bucket": "unknown:0xcafebabe",
            "fail_index": 3,
            "revert_data": "0xcafebabe",
            "reverting_frame": {
                "depth": 2,
                "target": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "selector": "0xdeadbeef",
                "revert_data": "0xcafebabe",
                "label": "unknown:0xcafebabe",
            },
            "captured_swaps": [
                {
                    "family": "v2",
                    "emitter": "0x" + "bb" * 20,
                    "amount0": -1000,
                    "amount1": 990,
                    "sqrt_price_x96": 0,
                    "liquidity": 0,
                    "tick": 0,
                }
            ],
        }
    ]
    with caplog.at_level("INFO", logger="degenbot"):
        _render_sim_failures(_outcome(failures), current_block=100)
    lines = [r.message for r in caplog.records if r.message.startswith("[sim-fail]")]
    assert len(lines) == 1
    line = lines[0]
    # The deep attribution surfaces — NOT the top-level ``fail_idx=`` bubble.
    assert "revert@depth=2" in line
    assert "target=0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" in line
    assert "sel=0xdeadbeef" in line
    assert "label=unknown:0xcafebabe" in line
    assert "swaps_before=1" in line
    assert "revert=0xcafebabe" in line
    assert "bucket=unknown:0xcafebabe" in line
    assert "hops=" in line
    # The top-level bubble fallback must NOT appear when reverting_frame is set.
    assert "fail_idx=" not in line


# ── Tripwire bucket-fatal semantics (fail hard + loud, no default mask) ──


def test_sim_failures_continue_by_default(
    caplog: pytest.LogCaptureFixture, monkeypatch: pytest.MonkeyPatch
) -> None:
    """ADR-040: the default ``sim_failure`` bucket action is ``event`` - the
    renderer logs the keyed loud event and the bot KEEPS RUNNING. The
    D63GSE-era fail-fast-by-default is retired; exit is now an explicit
    per-bucket operator override (``[failure_policy]`` in config.toml), not an
    implicit default.
    """
    monkeypatch.setenv("DEGENBOT_SIM_EXIT_ON_FAIL", "1")
    monkeypatch.delenv("DEGENBOT_SIM_EXIT_IGNORE_BUCKETS", raising=False)
    failures = [{"path_id": 1, "bucket": "empty", "fail_index": 3, "revert_data": "0x"}]
    with caplog.at_level("INFO", logger="degenbot"):
        _render_sim_failures(_outcome(failures), current_block=100)
    # No exit: the trap logs the loud continuing event instead.
    traps = [r.message for r in caplog.records if "[sim-trap]" in r.message]
    assert any("continuing" in m for m in traps), "default must continue, not exit"
    assert any("[sim-fail]" in r.message for r in caplog.records)


def test_explicitly_ignoring_a_bucket_opts_out(
    caplog: pytest.LogCaptureFixture, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Dumbing the tripwire down is an EXPLICIT operator opt-in (the env var),
    not a default: setting DEGENBOT_SIM_EXIT_IGNORE_BUCKETS=empty makes that
    bucket non-fatal. There is no implicit mask otherwise.
    """
    monkeypatch.setenv("DEGENBOT_SIM_EXIT_ON_FAIL", "1")
    monkeypatch.setenv("DEGENBOT_SIM_EXIT_IGNORE_BUCKETS", "empty")
    failures = [{"path_id": 1, "bucket": "empty", "fail_index": 3, "revert_data": "0x"}]
    with caplog.at_level("INFO", logger="degenbot"):
        _render_sim_failures(_outcome(failures), current_block=100)
    lines = [r.message for r in caplog.records if r.message.startswith("[sim-fail]")]
    assert len(lines) == 1
    assert "bucket=empty" in lines[0]
    assert not any("[sim-trap]" in r.message for r in caplog.records)


def test_operator_exit_override_still_traps(
    caplog: pytest.LogCaptureFixture, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The exit path survives ADR-040: when the operator's per-bucket policy
    says ``exit`` for ``sim_failure`` (a ``[failure_policy]`` override), the
    trap fires (``SystemExit(3)``) after the fixture dump. This test stubs the
    policy consult - the real resolution is boot-validated Rust-side; here we
    deterministically test the EXIT WIRING without mutating process-global
    Rust state.
    """
    import degenbot.diagnostics as diag

    monkeypatch.setattr(diag, "failure_action", lambda kind, reason=None: "exit")
    monkeypatch.setenv("DEGENBOT_SIM_EXIT_ON_FAIL", "1")
    monkeypatch.delenv("DEGENBOT_SIM_EXIT_IGNORE_BUCKETS", raising=False)
    failures = [
        {"path_id": 1, "bucket": "Error(string)", "fail_index": 3, "revert_data": "0x08c379a0"}
    ]
    with pytest.raises(SystemExit) as ei, caplog.at_level("INFO", logger="degenbot"):
        _render_sim_failures(_outcome(failures), current_block=100)
    assert ei.value.code == 3
    assert any("[sim-trap]" in r.message for r in caplog.records)
