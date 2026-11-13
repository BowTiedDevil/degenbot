from __future__ import annotations

import importlib.util
import itertools
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts" / "executor_three_hop_matrix.py"
SPEC = importlib.util.spec_from_file_location("executor_three_hop_matrix", MODULE_PATH)
assert SPEC is not None
assert SPEC.loader is not None
matrix = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(matrix)


def test_mainnet_token_triangle_generates_every_three_hop_permutation() -> None:
    requests = matrix.path_requests()

    assert len(requests) == 27
    assert {request["permutation"] for request in requests} == {
        "-".join(families) for families in itertools.product(("V2", "V3", "V4"), repeat=3)
    }
    for request in requests:
        steps = request["request"]["payload"]["steps"]
        assert len(steps) == 3
        assert [step["family"] for step in steps] == request["permutation"].split("-")
        assert len({(step["address"], step.get("hash")) for step in steps}) == 3


def test_report_uses_exact_path_ids_and_fails_missing_coverage() -> None:
    registrations = [
        {"permutation": request["permutation"], "response": {"path_id": index}}
        for index, request in enumerate(matrix.path_requests(), start=1)
    ]
    lines = [
        "[sim-ok] path=1 type=V2-V2-V2 gross=12 net=0 gas=123 hops=",
        "[sim-fail] path=2 type=V2-V2-V3 bucket=TF fail_idx=0",
        "candidate simulation raised an exception {path_id=3, error=boom}",
        "[sim-ok] path=999 type=V2-V2-V4 gross=12 net=0 gas=123 hops=",
    ]

    report = matrix.build_report(lines, registrations)

    assert report["summary"] == {
        "valid": 1,
        "failed": 1,
        "exceptions": 1,
        "missing": 24,
        "broadcast_observed": False,
    }
    assert report["paths"][0]["outcome"] == "sim_ok"
    assert report["paths"][1]["outcome"] == "failed"
    assert report["paths"][2]["outcome"] == "exception"
    assert report["paths"][3]["outcome"] == "missing"
    assert report["paths"][3]["sim_ok_count"] == 0


def test_report_fails_if_dry_run_broadcast_is_observed() -> None:
    registrations = [
        {"permutation": request["permutation"], "response": {"path_id": index}}
        for index, request in enumerate(matrix.path_requests(), start=1)
    ]

    report = matrix.build_report(["Submitted path 1 hash=0xabc nonce=3"], registrations)

    assert report["summary"]["broadcast_observed"] is True


def test_bidirectional_requests_cover_each_permutation_in_both_directions() -> None:
    requests = matrix.path_requests(bidirectional=True)

    assert len(requests) == 54
    for permutation in {request["permutation"] for request in requests}:
        directions = [request for request in requests if request["permutation"] == permutation]
        assert {request["direction"] for request in directions} == {"forward", "reverse"}
    forward = next(request for request in requests if request["direction"] == "forward")
    reverse = next(request for request in requests if request["direction"] == "reverse")
    assert reverse["request"]["payload"]["steps"][0] != forward["request"]["payload"]["steps"][0]
    assert [step["family"] for step in reverse["request"]["payload"]["steps"]] == [
        step["family"] for step in forward["request"]["payload"]["steps"]
    ]


def test_report_aggregates_both_directions_into_27_permutations() -> None:
    registrations = []
    for index, request in enumerate(matrix.path_requests(bidirectional=True), start=1):
        registrations.append({
            "permutation": request["permutation"],
            "direction": request["direction"],
            "response": {"path_id": index},
        })
    reverse_v2 = next(
        registration
        for registration in registrations
        if registration["permutation"] == "V2-V2-V2" and registration["direction"] == "reverse"
    )

    report = matrix.build_report(
        [f"[sim-ok] path={reverse_v2['response']['path_id']} type=V2-V2-V2"],
        registrations,
    )

    assert len(report["paths"]) == 27
    assert report["summary"]["valid"] == 1
    assert report["summary"]["missing"] == 26
    assert len(report["paths"][0]["path_ids"]) == 2


def test_probe_report_counts_economic_reverts_as_executed() -> None:
    responses = []
    for index, request in enumerate(matrix.path_requests(bidirectional=True), start=1):
        responses.append({
            "permutation": request["permutation"],
            "direction": request["direction"],
            "path_id": index,
            "response": {
                "ok": True,
                "outcome": "failed",
                "capital": "self_fund",
                "bucket": "InsufficientProfit",
            },
        })

    report = matrix.build_probe_report(responses)

    assert report["summary"] == {
        "executed": 54,
        "sim_ok": 0,
        "failed": 54,
        "planned": 0,
        "encode_failed": 0,
        "insolvent_zero_capital": 0,
        "exceptions": 0,
        "missing": 0,
    }
    assert len(report["paths"]) == 54
    assert {row["capital"] for row in report["paths"]} == {"self_fund"}, (
        "the report keeps the posture each probe ran under"
    )


def test_probe_report_keeps_a_typed_zero_capital_refusal_distinct() -> None:
    """A capital-policy refusal is neither an execution nor an encoder defect.

    The row keeps the wire's typed deficit (asset / required / available), so the
    report states WHAT the stream could not fund; the summary counts it under its
    own key rather than folding it into `encode_failed` (a grammar gap) or into
    the executed economic failures (a stream that really ran and lost).
    """
    responses = [
        {
            "permutation": "V2-V3-V4",
            "direction": "forward",
            "path_id": 5,
            "response": {
                "ok": True,
                "outcome": "failed",
                "capital": "zero_balance",
                "bucket": matrix.INSOLVENT_ZERO_CAPITAL_BUCKET,
                "zero_capital": {
                    "asset": "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
                    "required": 1_000,
                    "available": 400,
                },
            },
        }
    ]

    report = matrix.build_probe_report(responses)

    assert report["summary"] == {
        "executed": 0,
        "sim_ok": 0,
        "failed": 0,
        "planned": 0,
        "encode_failed": 0,
        "insolvent_zero_capital": 1,
        "exceptions": 0,
        "missing": 0,
    }
    (row,) = report["paths"]
    assert row["bucket"] == "insolvent-zero-capital"
    assert row["zero_capital"] == {
        "asset": "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        "required": 1_000,
        "available": 400,
    }
    assert row["zero_capital"]["required"] > row["zero_capital"]["available"]


def test_probe_report_does_not_count_an_encode_refusal_as_execution() -> None:
    responses = [
        {
            "permutation": "V2-V2-V4",
            "direction": "forward",
            "path_id": 3,
            "response": {
                "ok": True,
                "outcome": "failed",
                "capital": "zero_balance",
                "bucket": "encode-failed",
            },
        }
    ]

    assert matrix.build_probe_report(responses)["summary"] == {
        "executed": 0,
        "sim_ok": 0,
        "failed": 0,
        "planned": 0,
        "encode_failed": 1,
        "insolvent_zero_capital": 0,
        "exceptions": 0,
        "missing": 0,
    }


def test_probe_report_counts_a_planned_execution_separately() -> None:
    """A probe that really produced an encoder plan is counted as `planned`.

    The typed probe report carries the production encoder's joined row; the wire
    response carries a summary of it, so the matrix can tell an executed plan
    from a simulation that composed neither a plan nor a submission (the probe
    executor has no submit lane at all — there is no submission record to look
    for).
    """
    responses = [
        {
            "permutation": "V2-V3-V4",
            "direction": "forward",
            "path_id": 7,
            "response": {
                "ok": True,
                "outcome": "sim_ok",
                "capital": "production",
                "planning": {
                    "execution_target": "0x00000000000000000000000000000000000000C0",
                    "value": 0,
                    "calldata_len": 68,
                },
            },
        }
    ]

    report = matrix.build_probe_report(responses)

    assert report["summary"] == {
        "executed": 1,
        "sim_ok": 1,
        "failed": 0,
        "planned": 1,
        "encode_failed": 0,
        "insolvent_zero_capital": 0,
        "exceptions": 0,
        "missing": 0,
    }
    assert report["paths"][0]["planned"] is True
    assert report["paths"][0]["capital"] == "production"


def test_probe_requests_carry_one_capital_mode_in_the_shared_vocabulary() -> None:
    """Every probe request names its capital posture, in the host's vocabulary.

    The matrix script is a wire CLIENT: it must send the same spellings the host
    validates, so its mode list is checked against the operator channel's parser
    rather than trusted.
    """
    from degenbot.operator.operator_channel import capital_from_wire
    from degenbot.runner._sim_submit import ProbeCapital

    assert matrix.probe_request(7) == {
        "op": "probe_path",
        "payload": {"path_id": 7, "capital": "self_fund"},
    }, "the default probe is the self-funded one the wire has always carried"

    assert set(matrix.CAPITAL_MODES) == {mode.value for mode in ProbeCapital}, (
        "the script's CLI vocabulary and the host's must be the same set"
    )
    for capital in matrix.CAPITAL_MODES:
        request = matrix.probe_request(9, capital=capital)
        assert request["payload"] == {"path_id": 9, "capital": capital}
        assert capital_from_wire(request["payload"]).value == capital


def test_a_probe_request_refuses_an_unknown_capital_spelling() -> None:
    """The client-side refusal mirrors the host's, so a typo never reaches a bot."""
    with pytest.raises(ValueError, match="unknown capital"):
        matrix.probe_request(7, capital="zero-balance")
