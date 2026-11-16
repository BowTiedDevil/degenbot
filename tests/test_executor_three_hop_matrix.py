from __future__ import annotations

import itertools
import json
import sys
from pathlib import Path
from typing import TYPE_CHECKING, Any

import pytest

from degenbot.operator.operator_channel import PROBE_SUBMISSION_EVIDENCE, capital_from_wire
from degenbot.runner._sim_submit import ProbeCapital
from tests.repo_script import load_script

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts" / "executor_three_hop_matrix.py"
matrix = load_script("executor_three_hop_matrix", MODULE_PATH)

if TYPE_CHECKING:
    from collections.abc import Callable

WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"

#: The executor's own ``InsufficientProfit(uint256,uint256)`` revert: the selector
#: plus its two ABI words.
TERMINAL_REVERT = "0x4e88422a" + "00" * 64


def _registrations(*, bidirectional: bool = False) -> list[dict[str, Any]]:
    """The ``inject`` command's registration file for one complete matrix."""
    return [
        {
            "permutation": request["permutation"],
            "direction": request["direction"],
            "request": request["request"],
            "response": {"ok": True, "path_id": index},
        }
        for index, request in enumerate(matrix.path_requests(bidirectional=bidirectional), start=1)
    ]


def _response(**overrides: Any) -> dict[str, Any]:
    """One profitable probe response — the shape the operator projects — overridden."""
    response: dict[str, Any] = {
        "ok": True,
        "path_id": 1,
        "permutation": "V2-V3-V4",
        "capital": "self_fund",
        "current_block": 21_000_000,
        "assembly": "assembled",
        "outcome": "profitable",
        "receipt": {"gross_profit": 10**15, "net_profit": 10**13, "gas_used": 120_000},
        "submission": dict(matrix.SUBMISSION_EVIDENCE),
    }
    response.update(overrides)
    return response


def _insolvent_response(**overrides: Any) -> dict[str, Any]:
    """One typed capital-policy refusal: the insolvent run's expected row."""
    response = _response(**{
        "outcome": "failed",
        "bucket": matrix.INSOLVENT_ZERO_CAPITAL_BUCKET,
        "capital": "zero_balance",
        "revert": "0x",
        "zero_capital": {"asset": WETH, "required": 1_000_000, "available": 250_000},
        **overrides,
    })
    response.pop("receipt", None)
    return response


def _every_row(
    respond: Callable[..., dict[str, Any]], **overrides: Any
) -> dict[int, dict[str, Any]]:
    """One response per row, each naming the engine path that row's probe requested."""
    return {
        index: respond(**{"path_id": registration["response"]["path_id"], **overrides})
        for index, registration in enumerate(_registrations())
    }


def _completed_report(
    responses: dict[int, dict[str, Any]] | None = None,
    *,
    expect: str = matrix.EXPECT_EXECUTED,
    capital: str = matrix.DEFAULT_CAPITAL,
) -> dict[str, Any]:
    """Fill a full 27-row report, replacing the rows named in ``responses``.

    ``capital`` is the posture the run asked for, so an unnamed row gets the
    profitable response that names its own path in that posture and the named rows
    are the only ones the test is about.
    """
    registrations = _registrations()
    report = matrix.build_probe_report(registrations, expect=expect, capital=capital)
    for index, row in enumerate(report["paths"]):
        response = (responses or {}).get(
            index,
            _response(path_id=registrations[index]["response"]["path_id"], capital=capital),
        )
        row.update(matrix.probe_row(registrations[index], response, expect=expect, capital=capital))
    matrix.refresh_probe_report(report, interrupted=False)
    return report


# ── the matrix and the legacy log report ────────────────────────────────


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
    registrations = _registrations(bidirectional=True)
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


# ── the probe report's row categories ───────────────────────────────────


def test_a_profitable_simulation_is_execution_coverage() -> None:
    report = _completed_report()

    assert report["schema_version"] == matrix.PROBE_REPORT_SCHEMA_VERSION
    assert report["summary"] == {
        "expect": matrix.EXPECT_EXECUTED,
        "expected": 27,
        "completed": 27,
        "missing": 0,
        "matched": 27,
        "unexpected": 0,
        "complete": True,
        "interrupted": False,
        "categories": {
            matrix.CATEGORY_PLANNING_REFUSED: 0,
            matrix.CATEGORY_EXECUTED_SUCCESS: 27,
            matrix.CATEGORY_ECONOMIC_REJECT: 0,
            matrix.CATEGORY_UNEXPECTED_REVERT: 0,
            matrix.CATEGORY_ASSEMBLY_MISSING: 0,
            matrix.CATEGORY_EXCEPTION: 0,
        },
    }
    (row, *rest) = report["paths"]
    assert rest, "the report keeps one row per expected identity"
    assert row["category"] == matrix.CATEGORY_EXECUTED_SUCCESS
    assert row["reason"] == "profitable-simulation"
    assert row["matches_policy"] is True
    assert row["expected"] == matrix.EXPECT_EXECUTED
    assert row["request_block"] == 21_000_000
    assert row["submission"] == matrix.SUBMISSION_EVIDENCE
    assert matrix.probe_exit_code(report) == matrix.PROBE_EXIT_COMPLETE


def test_a_gas_unprofitable_receipt_is_an_economic_reject_not_a_success() -> None:
    """A receipt below the profit floor executed — it is not a winning arbitrage."""
    report = _completed_report({0: _response(outcome="gas-unprofitable")})

    row = report["paths"][0]
    assert row["category"] == matrix.CATEGORY_ECONOMIC_REJECT
    assert row["reason"] == "gas-unprofitable-receipt"
    assert row["matches_policy"] is True, "execution coverage under --expect executed"
    assert report["summary"]["matched"] == 27
    assert matrix.probe_exit_code(report) == matrix.PROBE_EXIT_COMPLETE


def test_the_terminal_profit_revert_is_an_economic_reject() -> None:
    """The executor's own terminal refusal is proven by its raw selector."""
    report = _completed_report({
        0: _response(
            outcome="failed",
            bucket=matrix.INSUFFICIENT_PROFIT_BUCKET,
            revert=TERMINAL_REVERT,
        )
    })

    row = report["paths"][0]
    assert row["category"] == matrix.CATEGORY_ECONOMIC_REJECT
    assert row["reason"] == "insufficient-profit-revert"
    assert row["bucket"] == matrix.INSUFFICIENT_PROFIT_BUCKET
    assert row["revert"] == TERMINAL_REVERT
    assert row["matches_policy"] is True


def test_a_failed_row_without_revert_bytes_is_an_unexpected_revert() -> None:
    """The bucket label alone is not evidence: no bytes, no economic verdict."""
    report = _completed_report({
        0: _response(
            outcome="failed",
            bucket=matrix.INSUFFICIENT_PROFIT_BUCKET,
            revert="0x",
        )
    })

    row = report["paths"][0]
    assert row["category"] == matrix.CATEGORY_UNEXPECTED_REVERT
    assert row["reason"] == "revert-evidence"
    assert row["matches_policy"] is False
    assert report["summary"]["unexpected"] == 1
    assert matrix.probe_exit_code(report) == matrix.PROBE_EXIT_FAILED


def test_a_truncated_terminal_revert_is_not_economic_evidence() -> None:
    report = _completed_report({
        0: _response(
            outcome="failed",
            bucket=matrix.INSUFFICIENT_PROFIT_BUCKET,
            revert=matrix.INSUFFICIENT_PROFIT_SELECTOR,
        )
    })

    assert report["paths"][0]["category"] == matrix.CATEGORY_UNEXPECTED_REVERT


def test_an_unknown_bucket_is_an_unexpected_revert() -> None:
    report = _completed_report({
        0: _response(outcome="failed", bucket="mystery-bucket", revert=TERMINAL_REVERT)
    })

    row = report["paths"][0]
    assert row["category"] == matrix.CATEGORY_UNEXPECTED_REVERT
    assert row["reason"] == "unknown-bucket"


def test_an_unknown_selector_is_an_unexpected_revert() -> None:
    report = _completed_report({
        0: _response(
            outcome="failed",
            bucket=matrix.INSUFFICIENT_PROFIT_BUCKET,
            revert="0xdeadbeef" + "00" * 64,
        )
    })

    row = report["paths"][0]
    assert row["category"] == matrix.CATEGORY_UNEXPECTED_REVERT
    assert row["reason"] == "revert-evidence"


def test_a_retired_outcome_spelling_is_not_evidence() -> None:
    """The wire's typed labels are the contract; an ``ok``-flavored label is not."""
    report = _completed_report({0: _response(outcome="sim_ok")})

    row = report["paths"][0]
    assert row["category"] == matrix.CATEGORY_EXCEPTION
    assert row["reason"] == "unrecognized-outcome"


def test_an_assembly_skip_is_assembly_missing() -> None:
    report = _completed_report({
        0: _response(outcome="skip-thin-margin", assembly="skip-thin-margin")
    })

    row = report["paths"][0]
    assert row["category"] == matrix.CATEGORY_ASSEMBLY_MISSING
    assert row["reason"] == "assembly-skip"
    assert row["matches_policy"] is False
    assert matrix.probe_exit_code(report) == matrix.PROBE_EXIT_FAILED


def test_an_exception_row_fails_the_run() -> None:
    report = _completed_report({0: {"outcome": "exception", "error": "OSError: boom"}})

    row = report["paths"][0]
    assert row["category"] == matrix.CATEGORY_EXCEPTION
    assert row["reason"] == "probe-exception"
    assert report["summary"]["unexpected"] == 1
    assert matrix.probe_exit_code(report) == matrix.PROBE_EXIT_FAILED


# ── the expected-outcome policy ─────────────────────────────────────────


def test_expected_insolvency_matches_only_the_typed_refusal() -> None:
    report = _completed_report(
        _every_row(_insolvent_response),
        expect=matrix.EXPECT_INSOLVENT,
        capital="zero_balance",
    )

    row = report["paths"][0]
    assert row["category"] == matrix.CATEGORY_PLANNING_REFUSED
    assert row["reason"] == "typed-insolvency"
    assert row["bucket"] == matrix.INSOLVENT_ZERO_CAPITAL_BUCKET
    assert row["zero_capital"] == {"asset": WETH, "required": 1_000_000, "available": 250_000}
    assert row["matches_policy"] is True
    assert report["summary"]["matched"] == 27
    assert report["summary"]["unexpected"] == 0
    assert matrix.probe_exit_code(report) == matrix.PROBE_EXIT_COMPLETE


def test_an_insolvent_bucket_without_the_typed_deficit_is_an_unexpected_revert() -> None:
    for deficit in (None, {"asset": WETH, "required": 1_000_000}, {"asset": WETH}):
        response = _insolvent_response()
        if deficit is None:
            response.pop("zero_capital")
        else:
            response["zero_capital"] = deficit
        report = _completed_report(
            {0: response}, expect=matrix.EXPECT_INSOLVENT, capital="zero_balance"
        )

        row = report["paths"][0]
        assert row["category"] == matrix.CATEGORY_UNEXPECTED_REVERT, deficit
        assert row["reason"] == "incomplete-typed-deficit", deficit
        assert row["matches_policy"] is False, deficit


def test_a_deficit_that_is_not_a_shortfall_is_not_typed_evidence() -> None:
    report = _completed_report(
        {0: _insolvent_response(zero_capital={"asset": WETH, "required": 1, "available": 1})},
        expect=matrix.EXPECT_INSOLVENT,
        capital="zero_balance",
    )

    assert report["paths"][0]["category"] == matrix.CATEGORY_UNEXPECTED_REVERT


@pytest.mark.parametrize(
    "asset",
    [
        pytest.param(WETH[:-1], id="39-hex-digits"),
        pytest.param(WETH + "0", id="41-hex-digits"),
        pytest.param("0x" + "0_" * 20, id="underscore"),
        pytest.param("0x" + "0z" * 20, id="non-hex"),
        pytest.param(WETH.removeprefix("0x"), id="no-prefix"),
        pytest.param("0X" + WETH[len("0x") :], id="upper-case-prefix"),
        pytest.param(True, id="not-a-string"),
    ],
)
def test_a_deficit_asset_must_be_twenty_bytes_of_hex(asset: object) -> None:
    """The deficit names the asset as an address, so a padded or non-hex string is no asset."""
    response = _insolvent_response(
        zero_capital={"asset": asset, "required": 1_000_000, "available": 250_000}
    )
    report = _completed_report(
        {0: response}, expect=matrix.EXPECT_INSOLVENT, capital="zero_balance"
    )

    assert matrix.typed_deficit(response) is None, asset
    row = report["paths"][0]
    assert row["category"] == matrix.CATEGORY_UNEXPECTED_REVERT, asset
    assert row["reason"] == "incomplete-typed-deficit", asset
    assert row["matches_policy"] is False, asset


def test_a_deficit_asset_is_read_in_either_hex_case() -> None:
    """Checksummed and lower-case spellings are both the same 20 bytes."""
    for asset in (WETH, WETH.lower()):
        response = _insolvent_response(
            zero_capital={"asset": asset, "required": 1_000_000, "available": 250_000}
        )
        assert matrix.typed_deficit(response) == {
            "asset": asset,
            "required": 1_000_000,
            "available": 250_000,
        }, asset


def test_an_execution_where_insolvency_was_expected_is_unexpected() -> None:
    report = _completed_report(
        _every_row(_insolvent_response) | {0: _response(capital="zero_balance")},
        expect=matrix.EXPECT_INSOLVENT,
        capital="zero_balance",
    )

    assert report["paths"][0]["category"] == matrix.CATEGORY_EXECUTED_SUCCESS
    assert report["paths"][0]["matches_policy"] is False
    assert report["summary"]["matched"] == 26
    assert report["summary"]["unexpected"] == 1
    assert matrix.probe_exit_code(report) == matrix.PROBE_EXIT_FAILED


def test_a_typed_insolvency_under_an_executed_policy_is_unexpected() -> None:
    report = _completed_report(
        {0: _insolvent_response()}, expect=matrix.EXPECT_EXECUTED, capital="zero_balance"
    )

    row = report["paths"][0]
    assert row["category"] == matrix.CATEGORY_PLANNING_REFUSED
    assert row["matches_policy"] is False
    assert matrix.probe_exit_code(report) == matrix.PROBE_EXIT_FAILED


def test_an_encode_refusal_is_a_planning_refusal_under_both_policies() -> None:
    responses = {0: _response(outcome="failed", bucket=matrix.ENCODE_FAILED_BUCKET, revert="0x")}

    executed = _completed_report(responses)
    insolvent = _completed_report(responses, expect=matrix.EXPECT_INSOLVENT)

    for report in (executed, insolvent):
        row = report["paths"][0]
        assert row["category"] == matrix.CATEGORY_PLANNING_REFUSED
        assert row["reason"] == "encode-refusal"
        assert row["matches_policy"] is False, "an encode refusal never executed"
        assert matrix.probe_exit_code(report) == matrix.PROBE_EXIT_FAILED


# ── submission evidence ─────────────────────────────────────────────────


def test_every_row_and_the_report_state_the_structural_submission_evidence() -> None:
    report = _completed_report()

    assert report["submission"] == matrix.SUBMISSION_EVIDENCE
    assert matrix.SUBMISSION_EVIDENCE == PROBE_SUBMISSION_EVIDENCE, (
        "the client's structural claim must be the host's own evidence object"
    )
    assert {json.dumps(row["submission"]) for row in report["paths"]} == {
        json.dumps(matrix.SUBMISSION_EVIDENCE)
    }


@pytest.mark.parametrize("claim", [{"possible": True}, {"observed": True}])
def test_a_response_claiming_a_submission_fails_the_run(claim: dict[str, bool]) -> None:
    report = _completed_report({0: _response(submission={**matrix.SUBMISSION_EVIDENCE, **claim})})

    row = report["paths"][0]
    assert row["category"] == matrix.CATEGORY_EXCEPTION
    assert row["reason"] == "submission-evidence"
    assert report["submission"]["possible"] or report["submission"]["observed"]
    assert matrix.probe_exit_code(report) == matrix.PROBE_EXIT_FAILED


def test_a_row_without_submission_evidence_is_an_exception() -> None:
    response = _response()
    response.pop("submission")

    report = _completed_report({0: response})

    row = report["paths"][0]
    assert row["category"] == matrix.CATEGORY_EXCEPTION
    assert row["reason"] == "submission-evidence-missing"


def test_a_padded_or_non_hex_terminal_revert_is_not_economic_evidence() -> None:
    """The economic verdict is read from exact canonical bytes, not a selector prefix."""
    for revert in (
        TERMINAL_REVERT + "00",
        TERMINAL_REVERT + "deadbeef",
        matrix.INSUFFICIENT_PROFIT_SELECTOR + "zz" * 64,
        matrix.INSUFFICIENT_PROFIT_SELECTOR + "0x" + "00" * 64,
    ):
        report = _completed_report({
            0: _response(
                outcome="failed",
                bucket=matrix.INSUFFICIENT_PROFIT_BUCKET,
                revert=revert,
            )
        })

        row = report["paths"][0]
        assert row["category"] == matrix.CATEGORY_UNEXPECTED_REVERT, revert
        assert row["reason"] == "revert-evidence", revert
        assert row["matches_policy"] is False, revert


def test_a_receipt_bearing_outcome_without_a_receipt_is_not_coverage() -> None:
    """`profitable` without the numbers is a claim, not an executed outcome."""
    for outcome in ("profitable", "gas-unprofitable"):
        response = _response(outcome=outcome)
        response.pop("receipt")
        report = _completed_report({0: response})

        row = report["paths"][0]
        assert row["category"] == matrix.CATEGORY_EXCEPTION, outcome
        assert row["reason"] == "receipt-evidence-missing", outcome
        assert row["matches_policy"] is False, outcome
        assert matrix.probe_exit_code(report) == matrix.PROBE_EXIT_FAILED


@pytest.mark.parametrize(
    "receipt",
    [
        pytest.param({"gross_profit": True, "net_profit": 1, "gas_used": 1}, id="bool-gross"),
        pytest.param({"gross_profit": 1, "net_profit": 1}, id="missing-gas-used"),
        pytest.param({"gross_profit": 1, "net_profit": 1, "gas_used": -1}, id="negative-gas-used"),
        pytest.param({"gross_profit": -1, "net_profit": 1, "gas_used": 1}, id="negative-gross"),
        pytest.param({"gross_profit": 1, "net_profit": "1", "gas_used": 1}, id="string-net-profit"),
        pytest.param("profitable", id="not-an-object"),
    ],
)
def test_an_incomplete_receipt_is_an_exception(receipt: object) -> None:
    report = _completed_report({0: _response(receipt=receipt)})

    row = report["paths"][0]
    assert row["category"] == matrix.CATEGORY_EXCEPTION
    assert row["reason"] == "receipt-evidence-missing"


def test_a_negative_net_profit_is_still_a_complete_receipt() -> None:
    """A gas-unprofitable receipt may lose money on gas and still be a receipt."""
    report = _completed_report({
        0: _response(
            outcome="gas-unprofitable",
            receipt={"gross_profit": 10, "net_profit": -5, "gas_used": 100},
        )
    })

    row = report["paths"][0]
    assert row["category"] == matrix.CATEGORY_ECONOMIC_REJECT
    assert row["matches_policy"] is True


@pytest.mark.parametrize(
    "submission",
    [
        pytest.param({}, id="empty-object"),
        pytest.param({"possible": False, "observed": False}, id="no-evidence-text"),
        pytest.param({"possible": False, "observed": False, "evidence": ""}, id="empty-evidence"),
        pytest.param(
            {"possible": False, "observed": False, "evidence": "we grepped a log"},
            id="foreign-evidence",
        ),
        pytest.param(
            {"possible": 0, "observed": False, "evidence": matrix.SUBMISSION_EVIDENCE_TEXT},
            id="non-boolean-possible",
        ),
        pytest.param(
            {"observed": False, "evidence": matrix.SUBMISSION_EVIDENCE_TEXT},
            id="missing-possible",
        ),
        pytest.param("no submit lane", id="not-an-object"),
    ],
)
def test_submission_evidence_must_be_the_exact_structural_statement(submission: object) -> None:
    report = _completed_report({0: _response(submission=submission)})

    row = report["paths"][0]
    assert row["category"] == matrix.CATEGORY_EXCEPTION
    assert row["reason"].startswith("submission-evidence")
    assert row["matches_policy"] is False


# ── response identity correlation ─────────────────────────────────────


_IDENTITY_DEFECTS = [
    pytest.param({"path_id": 99}, "response-path-id", id="foreign-path-id"),
    pytest.param({"path_id": None}, "response-path-id", id="missing-path-id"),
    pytest.param({"path_id": True}, "response-path-id", id="boolean-path-id"),
    pytest.param({"path_id": "1"}, "response-path-id", id="string-path-id"),
    pytest.param({"capital": "production"}, "response-capital", id="foreign-capital"),
    pytest.param({"capital": None}, "response-capital", id="missing-capital"),
    pytest.param({"current_block": -1}, "response-block", id="negative-block"),
    pytest.param({"current_block": True}, "response-block", id="boolean-block"),
    pytest.param({"current_block": "21000000"}, "response-block", id="string-block"),
    pytest.param({"current_block": None}, "response-block", id="missing-block"),
]


@pytest.mark.parametrize(("overrides", "reason"), _IDENTITY_DEFECTS)
@pytest.mark.parametrize(
    ("expect", "respond", "capital"),
    [
        pytest.param(matrix.EXPECT_EXECUTED, _response, matrix.DEFAULT_CAPITAL, id="executed"),
        pytest.param(matrix.EXPECT_INSOLVENT, _insolvent_response, "zero_balance", id="insolvent"),
    ],
)
def test_a_response_that_does_not_name_its_request_matches_no_policy(
    overrides: dict[str, Any],
    reason: str,
    expect: str,
    respond: Any,
    capital: str,
) -> None:
    """The row audits the response's own identity; transport correlation is not evidence."""
    report = _completed_report(_every_row(respond, **overrides), expect=expect, capital=capital)

    row = report["paths"][0]
    assert row["category"] == matrix.CATEGORY_EXCEPTION, reason
    assert row["reason"] == reason, reason
    assert row["matches_policy"] is False, reason
    assert row["response"].get("path_id") == overrides.get("path_id", 1), (
        "the raw response is kept beside the verdict, never normalized away"
    )
    assert {other["reason"] for other in report["paths"]} == {reason}, "every row audits itself"
    assert report["summary"]["matched"] == 0
    assert matrix.probe_exit_code(report) == matrix.PROBE_EXIT_FAILED


def test_an_uncorrelated_row_states_what_was_requested_and_what_answered() -> None:
    """The row keeps the requested identity AND the raw response's own claims."""
    report = _completed_report(
        _every_row(_response, path_id=9, capital="production", current_block=21_000_001)
    )

    row = report["paths"][0]
    assert (row["path_id"], row["capital"], row["request_block"]) == (1, "production", 21_000_001)
    assert row["reason"] == "response-path-id", "the first disagreement is the reason"
    assert row["response"]["path_id"] == 9


def test_a_transport_that_always_answers_for_one_path_fails_the_run(tmp_path: Path) -> None:
    """A reply for another path is not coverage, whatever verdict it carries."""
    output = tmp_path / "report.json"

    def send(_request: dict[str, Any]) -> dict[str, Any]:
        return _response(path_id=1)

    report = matrix.probe_paths(_registrations(), output, expect=matrix.EXPECT_EXECUTED, send=send)

    (first, *rest) = report["paths"]
    assert first["matches_policy"] is True, "the row that asked for path 1 did match"
    assert {row["reason"] for row in rest} == {"response-path-id"}
    assert report["summary"]["matched"] == 1
    assert report["summary"]["unexpected"] == 26
    assert matrix.probe_exit_code(report) == matrix.PROBE_EXIT_FAILED


# ── registration validation ─────────────────────────────────────────────


def test_the_two_complete_matrix_shapes_are_accepted() -> None:
    matrix.validate_registrations(_registrations())
    matrix.validate_registrations(_registrations(bidirectional=True))


def _registrations_without_the_first_direction() -> list[dict[str, Any]]:
    """The forward matrix with its first row's ``direction`` omitted."""
    (first, *rest) = _registrations()
    return [{key: value for key, value in first.items() if key != "direction"}, *rest]


def _invalid_registration_sets() -> list[tuple[str, list[dict[str, Any]]]]:
    """Registration sets that cannot be one complete directed matrix."""
    forward = _registrations()
    bidirectional = _registrations(bidirectional=True)
    (first, *rest) = forward
    return [
        ("26-forward-rows", forward[:-1]),
        ("53-bidirectional-rows", bidirectional[:-1]),
        ("28-rows-with-a-duplicate", [*forward, forward[-1]]),
        ("one-path-id-twice", [{**row, "response": {"path_id": 1}} for row in forward]),
        (
            "direction-outside-the-matrix",
            [
                {**row, "direction": "reverse"} if index == 0 else row
                for index, row in enumerate(forward)
            ],
        ),
        ("permutation-outside-the-matrix", [{**row, "permutation": "V5-V5-V5"} for row in forward]),
        ("one-identity-repeated", [{**row, "permutation": "V2-V2-V2"} for row in forward]),
        ("path-id-is-not-an-integer", [{**row, "response": {"path_id": "7"}} for row in forward]),
        ("path-id-is-negative", [{**first, "response": {"path_id": -1}}, *rest]),
        ("direction-is-omitted", _registrations_without_the_first_direction()),
        ("row-is-not-an-object", ["not-a-row", *forward[1:]]),
    ]


@pytest.mark.parametrize(
    "registrations",
    [registrations for _, registrations in _invalid_registration_sets()],
    ids=[case for case, _ in _invalid_registration_sets()],
)
def test_an_invalid_registration_set_is_refused(registrations: list[dict[str, Any]]) -> None:
    with pytest.raises((TypeError, ValueError)):
        matrix.validate_registrations(registrations)


def test_a_registration_without_a_direction_is_refused_not_defaulted() -> None:
    """A missing direction is never read as forward: the matrix states both sides."""
    with pytest.raises(ValueError, match="omits its direction"):
        matrix.validate_registrations(_registrations_without_the_first_direction())


def test_a_negative_engine_path_id_is_refused() -> None:
    (first, *rest) = _registrations()
    with pytest.raises(ValueError, match="negative engine path_id"):
        matrix.validate_registrations([{**first, "response": {"path_id": -1}}, *rest])


def test_a_non_array_registration_file_is_refused() -> None:
    with pytest.raises(TypeError, match="JSON array"):
        matrix.validate_registrations({"paths": []})


# ── incremental persistence and exit codes ──────────────────────────────


def test_the_report_is_pending_before_the_first_probe_and_replaced_after_each(
    tmp_path: Path,
) -> None:
    output = tmp_path / "report.json"
    seen: list[dict[str, Any]] = []

    def send(request: dict[str, Any]) -> dict[str, Any]:
        seen.append(json.loads(output.read_text(encoding="utf-8")))
        return _response(path_id=request["payload"]["path_id"])

    report = matrix.probe_paths(_registrations(), output, expect=matrix.EXPECT_EXECUTED, send=send)

    assert len(seen) == 27
    first = seen[0]
    assert first["paths"][0]["state"] == "pending"
    assert first["summary"]["completed"] == 0
    assert first["summary"]["missing"] == 27
    assert first["summary"]["matched"] == 0
    second = seen[1]
    assert second["paths"][0]["state"] == "completed"
    assert second["paths"][0]["category"] == matrix.CATEGORY_EXECUTED_SUCCESS
    assert second["paths"][1]["state"] == "pending"
    assert json.loads(output.read_text(encoding="utf-8")) == report
    assert report["summary"]["complete"] is True


def test_an_interrupted_run_leaves_a_parseable_incomplete_report(tmp_path: Path) -> None:
    output = tmp_path / "report.json"
    calls = 0

    def send(request: dict[str, Any]) -> dict[str, Any]:
        nonlocal calls
        if calls == 3:
            raise KeyboardInterrupt
        calls += 1
        return _response(path_id=request["payload"]["path_id"])

    report = matrix.probe_paths(_registrations(), output, expect=matrix.EXPECT_EXECUTED, send=send)

    on_disk = json.loads(output.read_text(encoding="utf-8"))
    assert on_disk == report, "the interrupted report is the file on disk"
    assert report["summary"]["interrupted"] is True
    assert report["summary"]["complete"] is False
    assert report["summary"]["completed"] == 3
    assert report["summary"]["missing"] == 24
    assert [row["state"] for row in report["paths"]][:4] == [
        "completed",
        "completed",
        "completed",
        "pending",
    ]
    assert report["summary"]["matched"] == 3
    assert matrix.probe_exit_code(report) == matrix.PROBE_EXIT_FAILED


def test_an_operator_refusal_and_a_transport_failure_are_exception_rows(
    tmp_path: Path,
) -> None:
    output = tmp_path / "report.json"

    def refusing(_request: dict[str, Any]) -> dict[str, Any]:
        return {"ok": False, "error": "unknown op 'probe_path'"}

    refused = matrix.probe_paths(
        _registrations(), output, expect=matrix.EXPECT_EXECUTED, send=refusing
    )
    row = refused["paths"][0]
    assert row["category"] == matrix.CATEGORY_EXCEPTION
    assert row["response"]["operator_response"] == {
        "ok": False,
        "error": "unknown op 'probe_path'",
    }

    def broken(_request: dict[str, Any]) -> dict[str, Any]:
        msg = "operator socket closed without a response"
        raise RuntimeError(msg)

    failed = matrix.probe_paths(
        _registrations(), output, expect=matrix.EXPECT_EXECUTED, send=broken
    )
    (only,) = {row["category"] for row in failed["paths"]}
    assert only == matrix.CATEGORY_EXCEPTION
    assert "without a response" in failed["paths"][0]["response"]["error"]
    assert matrix.probe_exit_code(failed) == matrix.PROBE_EXIT_FAILED


def _probe_argv(
    tmp_path: Path,
    registrations: list[dict[str, Any]],
    expect: str,
    *,
    capital: str = matrix.DEFAULT_CAPITAL,
) -> list[str]:
    path = tmp_path / "registrations.json"
    path.write_text(json.dumps(registrations), encoding="utf-8")
    return [
        "executor_three_hop_matrix.py",
        "probe",
        "--socket",
        str(tmp_path / "operator.sock"),
        "--registrations",
        str(path),
        "--output",
        str(tmp_path / "report.json"),
        "--expect",
        expect,
        "--capital",
        capital,
    ]


@pytest.mark.parametrize(
    ("bidirectional", "expect", "capital", "respond", "expected_code"),
    [
        (False, matrix.EXPECT_EXECUTED, matrix.DEFAULT_CAPITAL, _response, 0),
        (True, matrix.EXPECT_EXECUTED, matrix.DEFAULT_CAPITAL, _response, 0),
        (True, matrix.EXPECT_INSOLVENT, "zero_balance", _insolvent_response, 0),
        (True, matrix.EXPECT_INSOLVENT, matrix.DEFAULT_CAPITAL, _response, 1),
        (True, matrix.EXPECT_EXECUTED, "zero_balance", _insolvent_response, 1),
        (True, matrix.EXPECT_EXECUTED, "zero_balance", _response, 1),
    ],
)
def test_the_probe_command_exits_with_the_matrix(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    *,
    bidirectional: bool,
    expect: str,
    capital: str,
    respond: Any,
    expected_code: int,
) -> None:
    """A correct run needs no ``|| true``; an unexpected outcome fails the command."""

    def handler(_socket: Path, request: dict[str, Any]) -> dict[str, Any]:
        return respond(path_id=request["payload"]["path_id"])

    monkeypatch.setattr(matrix, "send_command", handler)
    registrations = _registrations(bidirectional=bidirectional)
    monkeypatch.setattr(sys, "argv", _probe_argv(tmp_path, registrations, expect, capital=capital))

    assert matrix.main() == expected_code


def test_the_probe_command_refuses_an_invalid_matrix_without_writing(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(sys, "argv", _probe_argv(tmp_path, _registrations()[:-1], "executed"))

    assert matrix.main() == matrix.PROBE_EXIT_REFUSED
    assert not (tmp_path / "report.json").exists(), "a refused run leaves no report"


def test_the_probe_command_requires_an_expected_outcome(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    argv = _probe_argv(tmp_path, _registrations(), "executed")
    argv = [item for item in argv if item not in {"--expect", "executed"}]
    monkeypatch.setattr(sys, "argv", argv)

    with pytest.raises(SystemExit):
        matrix.main()


def test_probe_requests_carry_one_capital_mode_in_the_shared_vocabulary() -> None:
    """Every probe request names its capital posture, in the host's vocabulary.

    The matrix script is a wire CLIENT: it must send the same spellings the host
    validates, so its mode list is checked against the operator channel's parser
    rather than trusted.
    """
    assert matrix.probe_request(7) == {
        "op": "probe_path",
        "payload": {
            "path_id": 7,
            "optimal_input": matrix.DEFAULT_FIXED_INPUT,
            "capital": "self_fund",
        },
    }, "the default probe is the self-funded one the wire has always carried"

    assert set(matrix.CAPITAL_MODES) == {mode.value for mode in ProbeCapital}, (
        "the script's CLI vocabulary and the host's must be the same set"
    )
    for capital in matrix.CAPITAL_MODES:
        request = matrix.probe_request(9, capital=capital)
        assert request["payload"] == {
            "path_id": 9,
            "optimal_input": matrix.DEFAULT_FIXED_INPUT,
            "capital": capital,
        }
        assert capital_from_wire(request["payload"]).value == capital


def test_a_probe_request_refuses_an_unknown_capital_spelling() -> None:
    """The client-side refusal mirrors the host's, so a typo never reaches a bot."""
    with pytest.raises(ValueError, match="unknown capital"):
        matrix.probe_request(7, capital="zero-balance")


def _never_called(request: dict[str, Any]) -> dict[str, Any]:
    """A transport that must not be reached: a refused run issues no probe."""
    msg = f"the probe was issued despite the refusal: {request}"
    raise AssertionError(msg)


# ── the fixed input and the run's metadata ─────────────────────────────


def test_a_probe_request_carries_the_fixed_input_it_was_given() -> None:
    """The amount vector is stated on the wire, never left to the host's default."""
    assert matrix.probe_request(7)["payload"]["optimal_input"] == matrix.DEFAULT_FIXED_INPUT
    request = matrix.probe_request(7, fixed_input=10**18)
    assert request["payload"] == {
        "path_id": 7,
        "optimal_input": 10**18,
        "capital": "self_fund",
    }


@pytest.mark.parametrize("fixed_input", [0, -1, True, False, 1.5, "1000000000000000", None])
def test_an_invalid_fixed_input_is_refused_before_a_probe(
    tmp_path: Path, *, fixed_input: object
) -> None:
    """A typo in the one shared amount fails closed, and writes no report."""
    with pytest.raises(ValueError, match="fixed input"):
        matrix.probe_request(7, fixed_input=fixed_input)  # type: ignore[arg-type]

    output = tmp_path / "report.json"
    with pytest.raises(ValueError, match="fixed input"):
        matrix.probe_paths(
            _registrations(),
            output,
            expect=matrix.EXPECT_EXECUTED,
            send=_never_called,
            fixed_input=fixed_input,  # type: ignore[arg-type]
        )
    assert not output.exists(), "a refused run writes no report"


def test_the_report_states_the_fixed_input_and_metadata_before_the_first_probe(
    tmp_path: Path,
) -> None:
    """The run's conditions are in the report from its first write, and in every row.

    The metadata is what makes a report reproducible, so an interrupted run must
    still carry it: it is written before the first probe, not patched in at the end.
    """
    output = tmp_path / "report.json"
    fixed_input = 10**16
    metadata = {
        "schema_version": 1,
        "chain_id": 1,
        "block_number": 21_000_000,
        "release_id": "cmd_executor/2",
        "fixed_input": fixed_input,
        "capitals": ["self_fund"],
    }
    witnesses: list[dict[str, Any]] = []
    requests: list[dict[str, Any]] = []

    def send(request: dict[str, Any]) -> dict[str, Any]:
        requests.append(request)
        witnesses.append(json.loads(output.read_text(encoding="utf-8")))
        return _response(path_id=request["payload"]["path_id"])

    report = matrix.probe_paths(
        _registrations(),
        output,
        expect=matrix.EXPECT_EXECUTED,
        send=send,
        fixed_input=fixed_input,
        metadata=metadata,
    )

    first = witnesses[0]
    assert first["metadata"] == metadata, "the metadata is in the report BEFORE the first probe"
    assert first["fixed_input"] == fixed_input
    assert first["paths"][0]["state"] == "pending"
    assert {row["fixed_input"] for row in first["paths"]} == {fixed_input}
    assert all(witness["metadata"] == metadata for witness in witnesses), (
        "every incremental write carries the same metadata"
    )
    assert {request["payload"]["optimal_input"] for request in requests} == {fixed_input}
    assert report["metadata"] == metadata
    assert report["fixed_input"] == fixed_input
    assert report["paths"][0]["request_block"] == 21_000_000, "the row keeps its own block"
    assert json.loads(output.read_text(encoding="utf-8")) == report


def test_an_interrupted_run_still_carries_its_metadata(tmp_path: Path) -> None:
    """A stop mid-matrix leaves a report that states the conditions, not a fragment."""
    output = tmp_path / "report.json"
    metadata = {"chain_id": 1, "fixed_input": matrix.DEFAULT_FIXED_INPUT}
    calls = 0

    def send(request: dict[str, Any]) -> dict[str, Any]:
        nonlocal calls
        if calls == 2:
            raise KeyboardInterrupt
        calls += 1
        return _response(path_id=request["payload"]["path_id"])

    report = matrix.probe_paths(
        _registrations(),
        output,
        expect=matrix.EXPECT_EXECUTED,
        send=send,
        metadata=metadata,
    )

    assert report["summary"]["interrupted"] is True
    assert report["metadata"] == metadata
    assert json.loads(output.read_text(encoding="utf-8"))["metadata"] == metadata
    assert matrix.probe_exit_code(report) == matrix.PROBE_EXIT_FAILED


@pytest.mark.parametrize("document", [[], ["chain_id"], "metadata", 7, None, {}])
def test_an_invalid_metadata_document_is_refused(tmp_path: Path, *, document: object) -> None:
    """Only a non-empty JSON object can state a run's conditions."""
    path = tmp_path / "metadata.json"
    path.write_text(json.dumps(document), encoding="utf-8")

    with pytest.raises((TypeError, ValueError)):
        matrix.load_metadata(path, fixed_input=matrix.DEFAULT_FIXED_INPUT)


def test_a_missing_or_unparseable_metadata_file_is_refused(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="missing"):
        matrix.load_metadata(tmp_path / "absent.json", fixed_input=matrix.DEFAULT_FIXED_INPUT)
    broken = tmp_path / "broken.json"
    broken.write_text("{not json", encoding="utf-8")
    with pytest.raises(ValueError, match="not readable JSON"):
        matrix.load_metadata(broken, fixed_input=matrix.DEFAULT_FIXED_INPUT)


def test_metadata_fixed_input_must_agree_with_the_requested_one() -> None:
    """A report whose metadata names another vector is not reproducible."""
    with pytest.raises(ValueError, match="disagrees"):
        matrix.validate_metadata({"fixed_input": 10**16}, fixed_input=10**15)
    with pytest.raises(ValueError, match="positive"):
        matrix.validate_metadata({"fixed_input": 0}, fixed_input=matrix.DEFAULT_FIXED_INPUT)
    assert matrix.validate_metadata({"chain_id": 1}, fixed_input=10**15) == {"chain_id": 1}


def test_an_invalid_metadata_document_refuses_the_matrix_before_any_probe(
    tmp_path: Path,
) -> None:
    output = tmp_path / "report.json"

    with pytest.raises(TypeError, match="JSON object"):
        matrix.probe_paths(
            _registrations(),
            output,
            expect=matrix.EXPECT_EXECUTED,
            send=_never_called,
            metadata=["chain_id"],  # type: ignore[arg-type]
        )
    assert not output.exists(), "a refused run writes no report"


def test_the_probe_command_threads_the_fixed_input_and_metadata(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    metadata = {"chain_id": 1, "source_commit": "a" * 40, "fixed_input": 10**16}
    metadata_path = tmp_path / "metadata.json"
    metadata_path.write_text(json.dumps(metadata), encoding="utf-8")
    requests: list[dict[str, Any]] = []

    def handler(_socket: Path, request: dict[str, Any]) -> dict[str, Any]:
        requests.append(request)
        return _response(path_id=request["payload"]["path_id"])

    monkeypatch.setattr(matrix, "send_command", handler)
    argv = [
        *_probe_argv(tmp_path, _registrations(), matrix.EXPECT_EXECUTED),
        "--fixed-input",
        str(10**16),
        "--metadata-file",
        str(metadata_path),
    ]
    monkeypatch.setattr(sys, "argv", argv)

    assert matrix.main() == matrix.PROBE_EXIT_COMPLETE
    report = json.loads((tmp_path / "report.json").read_text(encoding="utf-8"))
    assert report["metadata"] == metadata
    assert report["fixed_input"] == 10**16
    assert {request["payload"]["optimal_input"] for request in requests} == {10**16}


@pytest.mark.parametrize(
    ("extra", "expected"),
    [
        (["--fixed-input", "0"], matrix.PROBE_EXIT_REFUSED),
        (["--metadata-file", "absent.json"], matrix.PROBE_EXIT_REFUSED),
    ],
)
def test_the_probe_command_refuses_bad_run_conditions_without_writing(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    *,
    extra: list[str],
    expected: int,
) -> None:
    argv = _probe_argv(tmp_path, _registrations(), matrix.EXPECT_EXECUTED)
    argv += [item.replace("absent.json", str(tmp_path / "absent.json")) for item in extra]
    monkeypatch.setattr(sys, "argv", argv)

    assert matrix.main() == expected
    assert not (tmp_path / "report.json").exists(), "a refused run writes no report"
