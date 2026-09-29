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
dict[int, dict]`` — so ``FakeDispatchOutcome`` carries exactly those attrs). WEFVGE:
``path_infos`` values are plain dicts (``{path_type, hops: [hop_dict, …]}``),
not the retired ``*HopInfo`` dataclasses.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

from degenbot.diagnostics import FailureAction, failure_action
from degenbot.exceptions.base import DegenbotValueError
from degenbot.runner import _render
from degenbot.runner._render import _render_sim_failures, format_failure_breakdown, hop_fields
from tests.fakes.session import FakeDispatchOutcome
from tests.helpers import verdict_probe as probe

# ── Fixtures ─────────────────────────────────────────────────────────────

#: One failure in the bucket the ignore-set tests name.
_EMPTY_FAILURE: dict[str, Any] = {
    "path_id": 1,
    "bucket": "empty",
    "fail_index": 3,
    "revert_data": "0x",
}

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
    return FakeDispatchOutcome(
        failures=failures,
        path_infos={1: path_info, 2: path_info},
    )


def _render(
    outcome: Any,
    *,
    armed: bool = False,
    ignore: str = "",
) -> None:
    """Render one failure batch with the tripwire values threaded explicitly.

    The renderer takes the resolved trap values as parameters, so a test arms
    or disarms by passing them rather than by poking the process verdict.
    """
    _render_sim_failures(
        outcome,
        current_block=100,
        sim_exit_on_fail=armed,
        exit_ignore_buckets=ignore,
    )


# ── Tests ─────────────────────────────────────────────────────────────────


def test_no_failures_emits_nothing(caplog: pytest.LogCaptureFixture) -> None:
    with caplog.at_level("INFO", logger="degenbot"):
        _render(_outcome([]))
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
        _render(_outcome(failures))

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
        _render(_outcome(failures))
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
        _render(_outcome(failures))
    lines = [r.message for r in caplog.records if r.message.startswith("[sim-fail]")]
    detail_lines = [m for m in lines if "path=" in m]
    summary_lines = [m for m in lines if "(+5 more)" in m]
    assert len(detail_lines) == 25
    assert len(summary_lines) == 1
    assert "(+5 more)" in summary_lines[0]


def test_reverting_frame_surfaces_deep_attribution(caplog: pytest.LogCaptureFixture) -> None:
    # The inspector-captured reverting frame: the CONTRACT
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
        _render(_outcome(failures))
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


# ── format_failure_breakdown ─────────────────────────────────────────────


def test_format_breakdown_sorts_by_count_desc_then_name() -> None:
    """Breakdown lists the most common root cause first for at-a-glance reading."""
    buckets = {
        "CurrencyNotSettled": 9,
        "no-profit": 5,
        "ERC20: transfer amount exceeds balance": 2,
        "rpc-failed": 9,  # tie with CurrencyNotSettled → name breaks tie
    }
    breakdown = format_failure_breakdown(buckets)
    # Ties (9==9) broken by name → "CurrencyNotSettled" before "rpc-failed".
    # Remaining entries ordered by count desc → no-profit(5) before ERC20…(2).
    assert breakdown == (
        "CurrencyNotSettled=9 rpc-failed=9 no-profit=5 ERC20: transfer amount exceeds balance=2"
    )


def test_format_breakdown_empty() -> None:
    """An empty tally yields the empty string (caller skips the suffix)."""
    result = format_failure_breakdown({})
    assert isinstance(result, str)
    assert len(result) == 0


# ── hop_fields: the ONE family-string → PoolKind read ────────────────────


def test_unknown_hop_family_raises_in_strict_mode() -> None:
    """A family string outside the PoolKind taxonomy raises in strict mode (the
    [profit] render path) — an unknown render-seam family is loud, never
    silently branched. Lenient mode (``default=``) keeps the fixture-dump
    semantics and is exercised by every tripwire test above.
    """
    with pytest.raises(ValueError, match="Unsupported hop family: 'V5'"):
        hop_fields({"family": "V5"})


# ── failure_action: the real Rust matrix → FailureAction members ─────────


@pytest.mark.parametrize(
    ("kind", "reason", "expected"),
    [
        ("verify_mismatch", None, FailureAction.QUARANTINE),
        ("sim_failure", None, FailureAction.EVENT),
        ("sim_failure", "revert_pool_state", FailureAction.EVENT),
        ("sim_failure", "pre_encode", FailureAction.QUARANTINE),
        ("sim_failure", "revert_economics", FailureAction.OBSERVE),
        ("sim_failure", "rpc", FailureAction.EVENT),
        ("submit_failure", None, FailureAction.EVENT),
        ("monitor_failure", None, FailureAction.EVENT),
        ("ws_completeness", None, FailureAction.EXIT),
        ("drain_stall", None, FailureAction.EXIT),
        ("drain_dead", None, FailureAction.EXIT),
        ("late_log", None, FailureAction.EVENT),
        ("brand_new_bucket", None, FailureAction.EVENT),  # the evolution floor
    ],
)
def test_failure_action_maps_the_rust_matrix(
    kind: str, reason: str | None, expected: FailureAction
) -> None:
    """The Rust ``failure_policy`` matrix is the single source of truth; the
    wrapper returns its spellings as FailureAction MEMBERS (this consults the
    real PyO3 pyfunction — no stub, no process-global override installed)."""
    assert failure_action(kind, reason) is expected


def test_failure_action_wire_drift_raises(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A wire string outside the FailureAction vocabulary is Rust/Python
    matrix drift — loud. Stubbed at the ``_diagnostics.failure_action`` FFI
    boundary (the module attribute; the wrapper exposes no injectable seam)."""
    import degenbot.diagnostics as diag

    monkeypatch.setattr(
        diag._diagnostics,
        "failure_action",
        lambda kind, reason=None: "obliterate",
    )
    with pytest.raises(DegenbotValueError, match="Unrecognized failure action 'obliterate'"):
        failure_action("sim_failure", None)


# ── Tripwire bucket-fatal semantics (fail hard + loud, no default mask) ──


def test_sim_failures_continue_by_default(caplog: pytest.LogCaptureFixture) -> None:
    """ADR-040: the default ``sim_failure`` bucket action is ``event`` - the
    renderer logs the keyed loud event and the bot KEEPS RUNNING. The
    D63GSE-era fail-fast-by-default is retired; exit is now an explicit
    per-bucket operator override (``[failure_policy]`` in config.toml), not an
    implicit default.
    """
    failures = [{"path_id": 1, "bucket": "empty", "fail_index": 3, "revert_data": "0x"}]
    with caplog.at_level("INFO", logger="degenbot"):
        _render(_outcome(failures), armed=True)
    # No exit: the trap logs the loud continuing event instead.
    traps = [r.message for r in caplog.records if "[sim-trap]" in r.message]
    assert any("continuing" in m for m in traps), "default must continue, not exit"
    assert any("[sim-fail]" in r.message for r in caplog.records)


def test_ignoring_a_bucket_opts_the_trap_out(
    caplog: pytest.LogCaptureFixture, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Narrowing an armed trap is an EXPLICIT operator opt-in, not a default.
    The ignore set is the declared ``simulation.exit_ignore_buckets`` key, so
    the trap stays armed and the named bucket stops counting. There is no
    implicit mask: an armed process with no list still traps.
    """
    import degenbot.diagnostics as diag

    armed, ignore = _arm_from(
        {"DEGENBOT_SIM_EXIT_ON_FAIL": "1", "DEGENBOT_SIM_EXIT_IGNORE_BUCKETS": "empty"},
        operator_file=None,
    )
    monkeypatch.setattr(diag, "failure_action", lambda kind, reason=None: FailureAction.EXIT)
    with caplog.at_level("INFO", logger="degenbot"):
        _render(_outcome([_EMPTY_FAILURE]), armed=armed, ignore=ignore)
    assert armed is True
    # The ignored bucket short-circuits the trap: reaching here without a
    # SystemExit is the assertion, and the failure line still rendered.
    assert any("[sim-fail]" in r.message for r in caplog.records)


def test_the_operator_file_can_ignore_a_bucket_too(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The file layer narrows the trap, which only the environment could."""
    import degenbot.diagnostics as diag

    body = "sim_exit_on_fail = true\nexit_ignore_buckets = 'empty'\n"
    with probe.operator_file(f"[simulation]\n{body}") as written:
        armed, ignore = _arm_from(operator_file=written)
    monkeypatch.setattr(diag, "failure_action", lambda kind, reason=None: FailureAction.EXIT)
    _render(_outcome([_EMPTY_FAILURE]), armed=armed, ignore=ignore)
    assert armed is True
    assert ignore == "empty"


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

    monkeypatch.setattr(diag, "failure_action", lambda kind, reason=None: FailureAction.EXIT)
    failures = [
        {"path_id": 1, "bucket": "Error(string)", "fail_index": 3, "revert_data": "0x08c379a0"}
    ]
    with pytest.raises(SystemExit) as ei, caplog.at_level("INFO", logger="degenbot"):
        _render(_outcome(failures), armed=True)
    assert ei.value.code == 3
    assert any("[sim-trap]" in r.message for r in caplog.records)


# ── Which layer decides the armed state ──────────────────────────────────


def _arm_from(
    env: dict[str, str] | None = None,
    *,
    operator_file: Path | None = None,
) -> tuple[bool, str]:
    """The trap's armed state and ignore set for a hypothetical cascade.

    The renderer takes both as values its caller resolved, so a test resolves
    them through ``resolve_hypothetical`` and hands them in: the declared
    ``simulation.*`` keys decide, with no fresh interpreter involved.
    """
    values = probe.hypothetical_values(env, operator_file=operator_file)
    return (
        bool(values["simulation.sim_exit_on_fail"]),
        str(values["simulation.exit_ignore_buckets"]),
    )


def test_a_bare_invocation_leaves_the_tripwire_disarmed(tmp_path: Path) -> None:
    """THE POSTURE, named: nothing arms the tripwire, so it does not fire.

    ``simulation.sim_exit_on_fail`` declares ``false``, and the trap reads that
    declared value, where it used to default to armed inside Python. So an
    invocation that names no layer at all is DISARMED. The shipped run is
    unaffected: ``run_bot.sh`` already exports
    ``DEGENBOT_SIM_EXIT_ON_FAIL=0``, so the deployment posture is the one the
    schema already declared. What this pins is that a process which
    configures nothing no longer inherits a tripwire from Python.
    """
    with probe.operator_file("[simulation]\n") as written:
        armed, ignore = _arm_from(operator_file=written)
    assert armed is False, "an invocation that names no layer is disarmed"
    _render(_outcome([_EMPTY_FAILURE]), armed=armed, ignore=ignore)


def test_the_operator_file_can_arm_the_tripwire(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The file layer reaches the trap in the arming direction too.

    The pair of file-layer tests is the point: a change that left the trap
    permanently off would pass the disarming test alone.
    """
    import degenbot.diagnostics as diag

    with probe.operator_file("[simulation]\nsim_exit_on_fail = true\n") as written:
        armed, ignore = _arm_from(operator_file=written)
    assert armed is True, "the file layer must be able to arm the trap"
    monkeypatch.setattr(diag, "failure_action", lambda kind, reason=None: FailureAction.EXIT)
    with pytest.raises(SystemExit) as ei:
        _render(_outcome([_EMPTY_FAILURE]), armed=armed, ignore=ignore)
    assert ei.value.code == 3


def test_the_operator_file_can_disarm_the_tripwire(tmp_path: Path) -> None:
    """The file layer is honoured in the direction that used to be impossible.

    An operator who wrote ``sim_exit_on_fail = false`` into the operator file
    -- the primary layer -- had it ignored, because the trap read only the
    process environment and armed itself otherwise.
    """
    with probe.operator_file("[simulation]\nsim_exit_on_fail = false\n") as written:
        armed, ignore = _arm_from(operator_file=written)
    assert armed is False, "the operator file must be able to disarm"
    _render(_outcome([_EMPTY_FAILURE]), armed=armed, ignore=ignore)
