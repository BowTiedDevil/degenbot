#!/usr/bin/env python3
"""Inject and audit the fixed WETH/USDC/WBTC three-hop protocol matrix.

The injection command proves registration only. The report command correlates
returned engine path IDs with production solver/simulation logs and fails until
all 27 permutations have a valid execution receipt. It also fails if a submission
record appears in what is required to be a dry-run log.

The ``probe`` command drives the operator's explicit execution probe and writes a
versioned, fail-closed report. Every expected row is in the first write (as
``pending``) and the report is replaced after every response, so an interrupted run
leaves an auditable report instead of a green fragment. Each completed row keeps
the probe's own evidence — the probed block, the simulate verdict's typed label, the
raw failure bucket and revert bytes, the typed ``zero_capital`` deficit, the
encoder's ``planning`` summary, and the structural ``submission`` object — and is
classified into exactly one category: ``planning_refused`` (no bytecode was ever
simulated), ``executed_success`` (a committed profitable simulation),
``economic_reject`` (a receipt that lost money — a gas-unprofitable simulation or
the executor's terminal ``InsufficientProfit`` revert, proven by its selector), or
one of the failure categories ``unexpected_revert``, ``assembly_missing``, and
``exception``. The run declares what it expects with ``--expect``: ``executed``
(every row must be execution coverage) or ``insolvent`` (every row must be the
typed capital-policy refusal). Any row that does not match fails the command.
"""

from __future__ import annotations

import argparse
import itertools
import json
import re
import socket
import string
import sys
import time
from functools import partial
from pathlib import Path
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from collections.abc import Callable, Iterable

MANIFEST_PATH = Path(__file__).with_suffix(".json")
FAMILIES = ("V2", "V3", "V4")
EDGE_ORDER = ("A", "B", "C")
PERMUTATION_COUNT = 27
DIRECTED_PATH_COUNT = 27

#: The probe payload's capital vocabulary — the operator wire's spellings, which
#: the host validates (`degenbot.operator.operator_channel.capital_from_wire`).
CAPITAL_MODES = ("production", "self_fund", "zero_balance")
DEFAULT_CAPITAL = "self_fund"

#: The failure bucket of a **capital-policy refusal** — the core ledger answered
#: that the stream cannot be funded under the probed posture
#: (`degenbot_arbitrage::INSOLVENT_ZERO_CAPITAL_BUCKET`).
#:
#: Distinct from ``encode-failed``, which means "the encoder has no representation
#: for this path". A zero-capital probe over a losing permutation produces THIS
#: bucket, and the row keeps the deficit's typed ``zero_capital`` details
#: (``asset`` / ``required`` / ``available``) rather than only a label.
INSOLVENT_ZERO_CAPITAL_BUCKET = "insolvent-zero-capital"

#: The bucket of an encoder grammar gap: the production encoder has no
#: representation for the path, so no bytecode was ever simulated.
ENCODE_FAILED_BUCKET = "encode-failed"

#: The bucket of the executor's terminal economic refusal — the on-chain profit
#: floor answered "no" AFTER the swaps ran.
INSUFFICIENT_PROFIT_BUCKET = "InsufficientProfit"

#: That refusal's own raw selector: ``keccak256("InsufficientProfit(uint256,uint256)")[:4]``,
#: the row `degenbot-decoders::revert::EXECUTOR_REVERT_SELECTORS` pins against the
#: executor release's declared signature.
INSUFFICIENT_PROFIT_SELECTOR = "0x4e88422a"

#: The hex length of that error's canonical payload: the selector plus exactly its
#: two ABI words (``actual``, ``expected``). A shorter payload is truncated, a
#: longer one carries bytes the error does not encode, and either way it is not
#: evidence of the terminal refusal.
INSUFFICIENT_PROFIT_HEX_LEN = len("0x") + 8 + 2 * 64

#: The row categories a probe outcome is classified into. Only
#: :data:`CATEGORY_EXECUTED_SUCCESS` and :data:`CATEGORY_ECONOMIC_REJECT` are EVM
#: execution coverage: the dedicated probe executor has no submit lane, so nothing
#: it reports was broadcast, and an outcome the report cannot account for is never
#: counted as coverage.
CATEGORY_PLANNING_REFUSED = "planning_refused"
CATEGORY_EXECUTED_SUCCESS = "executed_success"
CATEGORY_ECONOMIC_REJECT = "economic_reject"
CATEGORY_UNEXPECTED_REVERT = "unexpected_revert"
CATEGORY_ASSEMBLY_MISSING = "assembly_missing"
CATEGORY_EXCEPTION = "exception"

CATEGORIES = (
    CATEGORY_PLANNING_REFUSED,
    CATEGORY_EXECUTED_SUCCESS,
    CATEGORY_ECONOMIC_REJECT,
    CATEGORY_UNEXPECTED_REVERT,
    CATEGORY_ASSEMBLY_MISSING,
    CATEGORY_EXCEPTION,
)

#: The categories that mean "the vector really reached the EVM": a committed
#: simulation (profitable) or a receipt-bearing one (a gas-unprofitable receipt, or
#: the terminal profit-floor revert). Nothing else is execution coverage.
EXECUTION_CATEGORIES = frozenset({CATEGORY_EXECUTED_SUCCESS, CATEGORY_ECONOMIC_REJECT})

#: The simulate verdict labels that carry a receipt — the only outcomes that can be
#: execution coverage, and only when the receipt's own numbers back them.
EXECUTION_OUTCOMES = ("profitable", "gas-unprofitable")

#: The expected-outcome policies a probe run declares with ``--expect``.
EXPECT_EXECUTED = "executed"
EXPECT_INSOLVENT = "insolvent"
EXPECTATIONS = (EXPECT_EXECUTED, EXPECT_INSOLVENT)

#: The directed-row counts a complete matrix can have: the 27-permutation forward
#: cycle, or that cycle and its reverse (54).
EXPECTED_DIRECTED_COUNTS = (DIRECTED_PATH_COUNT, DIRECTED_PATH_COUNT * 2)

#: The character length of a ``0x``-prefixed 20-byte address — the only shape a
#: typed deficit may name as the asset it could not fund.
ADDRESS_HEX_LEN = len("0x") + 40

#: The version of the report this script writes. Version 2 states every row's raw
#: evidence and its classification instead of counting bucket labels.
PROBE_REPORT_SCHEMA_VERSION = 2

#: The structural non-submission statement a probe response must carry: the
#: executor's construction has no submit lane to reach
#: (`degenbot.operator.operator_channel.PROBE_SUBMISSION_EVIDENCE`), so this is a
#: fact about its shape, never an inference from the absence of a log record.
SUBMISSION_EVIDENCE_TEXT = "dedicated ProbeExecutor has no submit lane"

SUBMISSION_EVIDENCE = {
    "possible": False,
    "observed": False,
    "evidence": SUBMISSION_EVIDENCE_TEXT,
}

#: Exit codes of the ``probe`` command (argparse reserves 2 for a bad invocation,
#: and the ``report`` command keeps 0/1).
PROBE_EXIT_COMPLETE = 0
PROBE_EXIT_FAILED = 1
PROBE_EXIT_REFUSED = 2


def load_manifest() -> dict[str, Any]:
    """Load the chain, token, and nine-pool manifest.

    Returns:
        The decoded manifest.

    """
    return json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))


def path_requests(*, bidirectional: bool = False) -> list[dict[str, Any]]:
    """Return operator requests for every family sequence and token-cycle direction.

    Returns:
        One request per permutation, or two when ``bidirectional`` is true.

    """
    edges = load_manifest()["edges"]
    requests = []
    directions = (("forward", EDGE_ORDER), ("reverse", EDGE_ORDER[::-1]))
    for families in itertools.product(FAMILIES, repeat=3):
        for direction, edge_order in directions[: 2 if bidirectional else 1]:
            steps = []
            for edge, family in zip(edge_order, families, strict=True):
                step = {"family": family, **edges[edge]["pools"][family]}
                steps.append(step)
            requests.append({
                "permutation": "-".join(families),
                "direction": direction,
                "request": {"op": "add_path", "payload": {"steps": steps}},
            })
    return requests


def send_command(
    socket_path: Path, request: dict[str, Any], timeout: float = 30.0
) -> dict[str, Any]:
    """Send one JSON-lines operator request and read its one response.

    Returns:
        The decoded operator response.

    Raises:
        RuntimeError: If the socket closes before a response arrives.

    """
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(timeout)
        connection.connect(str(socket_path))
        connection.sendall(json.dumps(request).encode() + b"\n")
        data = b""
        while not data.endswith(b"\n"):
            chunk = connection.recv(65536)
            if not chunk:
                msg = "operator socket closed without a response"
                raise RuntimeError(msg)
            data += chunk
    return json.loads(data)


def inject_paths(
    socket_path: Path, output_path: Path, ready_timeout: float = 120.0
) -> list[dict[str, Any]]:
    """Register all 27 forward paths, retrying only the crawl's pre-boot refusal.

    Returns:
        The forward requests and successful registration responses, including path IDs.

    Raises:
        TimeoutError: If the operator socket or crawl is not ready in time.
        RuntimeError: If the operator refuses a registration.
        TypeError: If a successful response lacks an integer path ID.

    """
    deadline = time.monotonic() + ready_timeout
    while not socket_path.exists():
        if time.monotonic() >= deadline:
            msg = f"operator socket did not appear: {socket_path}"
            raise TimeoutError(msg)
        time.sleep(0.1)

    results = []
    for item in path_requests():
        while True:
            response = send_command(socket_path, item["request"])
            transient = "no live crawl session" in response.get("error", "")
            if not transient or time.monotonic() >= deadline:
                break
            time.sleep(0.25)
        if not response.get("ok"):
            msg = f"{item['permutation']} registration failed: {response}"
            raise RuntimeError(msg)
        if not isinstance(response.get("path_id"), int):
            msg = f"{item['permutation']} returned no engine path_id: {response}"
            raise TypeError(msg)
        result = {**item, "response": response}
        results.append(result)
        sys.stdout.write(
            f"{item['permutation']} {item['direction']} path_id={response['path_id']}\n"
        )
        sys.stdout.flush()
    output_path.write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
    return results


def probe_request(path_id: int, *, capital: str = DEFAULT_CAPITAL) -> dict[str, Any]:
    """Build the operator request that probes one registered path.

    Args:
        path_id: The engine path ID the registration returned.
        capital: The capital posture spelling the probe asks for
            (:data:`CAPITAL_MODES`); ``self_fund`` is the default the wire has
            always carried.

    Returns:
        The JSON-lines request object.

    Raises:
        ValueError: if ``capital`` is not one of :data:`CAPITAL_MODES`.

    """
    if capital not in CAPITAL_MODES:
        expected = "|".join(CAPITAL_MODES)
        msg = f"unknown capital {capital!r} (expected {expected})"
        raise ValueError(msg)
    return {"op": "probe_path", "payload": {"path_id": path_id, "capital": capital}}


def validate_registrations(registrations: object) -> None:
    """Refuse a registration set that is not one complete directed matrix.

    A registration's identity is the only thing tying a probe row to a permutation
    and a direction: a repeated identity would let one path answer for two rows, a
    repeated engine path ID would probe one path twice under two names, and an
    omitted identity would shrink the matrix without saying so. All three fail
    closed here, before a single probe is issued. A row's direction is stated,
    never assumed — an absent ``direction`` is refused rather than read as
    forward, because a default would silently double a forward row into a
    direction the registration never claimed.

    Args:
        registrations: The ``inject`` command's JSON — one entry per registered
            directed row (``permutation``, ``direction``, and the registration
            ``response`` carrying the engine ``path_id``).

    Raises:
        TypeError: If the set is not a JSON array of registration objects, or a row
            carries no integer engine path ID.
        ValueError: If the set is not exactly 27 or 54 rows, omits a row's
            ``direction``, names an identity the matrix does not contain, carries a
            negative engine path ID, repeats an identity or an engine path ID, or
            omits an expected identity.

    """
    if not isinstance(registrations, list):
        msg = f"registrations must be a JSON array, not {type(registrations).__name__}"
        raise TypeError(msg)
    count = len(registrations)
    if count not in EXPECTED_DIRECTED_COUNTS:
        sizes = " or ".join(str(size) for size in EXPECTED_DIRECTED_COUNTS)
        msg = f"registrations must cover exactly {sizes} directed rows, got {count}"
        raise ValueError(msg)
    expected_identities = {
        (request["permutation"], request["direction"])
        for request in path_requests(bidirectional=count == DIRECTED_PATH_COUNT * 2)
    }
    seen_identities: set[tuple[object, object]] = set()
    seen_path_ids: set[int] = set()
    for index, registration in enumerate(registrations):
        if not isinstance(registration, dict):
            msg = f"registration {index} is not a JSON object"
            raise TypeError(msg)
        if "direction" not in registration:
            msg = (
                f"registration {index} ({registration.get('permutation')}) omits its "
                f"direction; a directed row states forward or reverse explicitly"
            )
            raise ValueError(msg)
        identity = (registration.get("permutation"), registration["direction"])
        if identity not in expected_identities:
            msg = f"registration {index} names an identity outside the matrix: {identity}"
            raise ValueError(msg)
        if identity in seen_identities:
            msg = f"registration {index} repeats identity {identity}"
            raise ValueError(msg)
        seen_identities.add(identity)
        response = registration.get("response")
        path_id = response.get("path_id") if isinstance(response, dict) else None
        if not isinstance(path_id, int) or isinstance(path_id, bool):
            msg = f"registration {index} ({identity}) carries no integer engine path_id"
            raise TypeError(msg)
        if path_id < 0:
            msg = f"registration {index} ({identity}) carries a negative engine path_id {path_id}"
            raise ValueError(msg)
        if path_id in seen_path_ids:
            msg = f"registration {index} repeats engine path_id {path_id}"
            raise ValueError(msg)
        seen_path_ids.add(path_id)
    missing = expected_identities - seen_identities
    if missing:
        msg = f"registrations omit {len(missing)} expected identities: {sorted(missing)[:4]}"
        raise ValueError(msg)


def _is_integer(value: object) -> bool:
    """Whether ``value`` is a JSON integer.

    Args:
        value: A decoded JSON value.

    Returns:
        True for an ``int`` that is not a ``bool`` — a JSON ``true`` is not a count.

    """
    return isinstance(value, int) and not isinstance(value, bool)


def _is_amount(value: object) -> bool:
    """Whether ``value`` is a non-negative JSON integer.

    Args:
        value: A decoded JSON value.

    Returns:
        True for an ``int`` that is not a ``bool`` and is not negative.

    """
    return _is_integer(value) and value >= 0


def _is_hex(value: str) -> bool:
    """Whether ``value`` is a non-empty run of hex digits.

    Args:
        value: The hex payload, without its ``0x`` prefix.

    Returns:
        True when every character is a hex digit (``int(value, 16)`` would accept
        underscores, which no revert payload can contain).

    """
    return bool(value) and all(character in string.hexdigits for character in value)


def typed_deficit(response: dict[str, Any]) -> dict[str, Any] | None:
    """Return a response's typed zero-capital deficit when it is COMPLETE.

    The capital-policy refusal's data half is the wire's ``zero_capital`` object:
    the asset the stream could not fund, the amount it needed, and the
    path-generated credit it had. A bucket label cannot say any of that, so a row
    is read as the typed refusal only when all three fields are present, the asset
    is exactly 20 bytes of hex behind its ``0x`` prefix, and the demand really
    exceeds the credit. Anything less is a label without evidence.

    Args:
        response: One operator probe response.

    Returns:
        The validated ``{"asset", "required", "available"}`` object, or ``None``.

    """
    deficit = response.get("zero_capital")
    if not isinstance(deficit, dict):
        return None
    asset = deficit.get("asset")
    required = deficit.get("required")
    available = deficit.get("available")
    if not (isinstance(asset, str) and asset.startswith("0x") and len(asset) == ADDRESS_HEX_LEN):
        return None
    if not _is_hex(asset[len("0x") :]):
        return None
    if not _is_amount(required) or not _is_amount(available):
        return None
    if required <= available:
        return None
    return {"asset": asset, "required": required, "available": available}


def is_terminal_economic_revert(revert: object) -> bool:
    """Whether raw revert hex is the executor's terminal ``InsufficientProfit`` error.

    The economic refusal is evidence only when the BYTES say so: the selector is
    ``keccak256("InsufficientProfit(uint256,uint256)")[:4]`` and the payload is
    exactly that selector plus both ABI words, in hex. A bucket label with empty,
    truncated, padded, or non-hex revert data cannot carry the verdict.

    Args:
        revert: The probe response's ``revert`` hex string.

    Returns:
        True only for the executor's own canonical two-word economic refusal.

    """
    if not isinstance(revert, str) or not revert.startswith("0x"):
        return False
    if len(revert) != INSUFFICIENT_PROFIT_HEX_LEN:
        return False
    payload = revert[len("0x") :]
    if not _is_hex(payload):
        return False
    selector = payload[: len(INSUFFICIENT_PROFIT_SELECTOR.removeprefix("0x"))]
    return selector.lower() == INSUFFICIENT_PROFIT_SELECTOR.removeprefix("0x")


def typed_receipt(response: dict[str, Any]) -> dict[str, int] | None:
    """Return a response's typed receipt when it is COMPLETE.

    A receipt-bearing outcome is execution coverage because a receipt exists, so
    the numbers are the evidence: gross profit, net profit, and the simulated gas.
    A row that says ``profitable`` without them states a conclusion and withholds
    its basis, which is indistinguishable from a short-circuited surface.

    Args:
        response: One operator probe response.

    Returns:
        The validated ``{"gross_profit", "net_profit", "gas_used"}`` object, or
        ``None`` when any field is missing or is not an integer (a JSON ``true``
        is not a count; a net profit may be negative and still be a receipt).

    """
    receipt = response.get("receipt")
    if not isinstance(receipt, dict):
        return None
    gross_profit = receipt.get("gross_profit")
    net_profit = receipt.get("net_profit")
    gas_used = receipt.get("gas_used")
    if not _is_integer(gross_profit) or not _is_integer(net_profit):
        return None
    if not _is_amount(gas_used) or gross_profit < 0:
        return None
    return {"gross_profit": gross_profit, "net_profit": net_profit, "gas_used": gas_used}


def submission_defect(response: dict[str, Any]) -> str | None:
    """Return why one response's submission evidence is unacceptable, or ``None``.

    The evidence is structural: a probe cannot submit, so a response must say so —
    ``possible: false``, ``observed: false``, and the executor-shape sentence. A
    missing object, a missing key, a truthy claim, a non-boolean, or a foreign
    sentence is a surface that cannot be audited from the row.

    Args:
        response: One operator probe response.

    Returns:
        A stable reason token for the defect, or ``None`` when the evidence is the
        exact structural statement this report contract requires.

    """
    submission = response.get("submission")
    if not isinstance(submission, dict):
        return "submission-evidence-missing"
    if submission.get("possible") is not False or submission.get("observed") is not False:
        return "submission-evidence"
    evidence = submission.get("evidence")
    if not isinstance(evidence, str) or not evidence:
        return "submission-evidence-missing"
    if evidence != SUBMISSION_EVIDENCE_TEXT:
        return "submission-evidence-unexpected"
    return None


def classify_probe_response(response: dict[str, Any]) -> tuple[str, str]:
    """Classify one probe response into a row category and the reason for it.

    Only a receipt-bearing outcome is execution coverage, and the receipt is the
    response's own simulate verdict: an assembly skip, an operator refusal, a
    transport failure, a submission claim, or a bucket label with no bytes behind it
    is something else. The categories are mutually exclusive and total, so a
    summary over them always adds up.

    Args:
        response: One operator probe response, or the exception shape this script
            synthesizes for a transport failure or a refused command.

    Returns:
        ``(category, reason)`` — the row's category and a short stable token naming
        the evidence it was chosen on.

    """
    outcome = response.get("outcome")
    defect = submission_defect(response)
    if outcome == "exception":
        category, reason = CATEGORY_EXCEPTION, "probe-exception"
    elif defect is not None:
        category, reason = CATEGORY_EXCEPTION, defect
    elif outcome in EXECUTION_OUTCOMES:
        category, reason = _classify_execution_response(response, outcome=outcome)
    elif isinstance(outcome, str) and outcome.startswith("skip-"):
        category, reason = CATEGORY_ASSEMBLY_MISSING, "assembly-skip"
    elif outcome != "failed":
        category, reason = CATEGORY_EXCEPTION, "unrecognized-outcome"
    else:
        category, reason = _classify_failed_response(response)
    return category, reason


def _classify_execution_response(response: dict[str, Any], *, outcome: str) -> tuple[str, str]:
    """Classify a receipt-bearing outcome, which only a complete receipt supports.

    Args:
        response: One probe response whose ``outcome`` is receipt-bearing.
        outcome: The response's own typed outcome label.

    Returns:
        ``(category, reason)``: ``executed_success`` for a profitable simulation,
        ``economic_reject`` for a gas-unprofitable one, and ``exception`` when the
        row states the label without the receipt numbers behind it.

    """
    if typed_receipt(response) is None:
        return CATEGORY_EXCEPTION, "receipt-evidence-missing"
    if outcome == "profitable":
        return CATEGORY_EXECUTED_SUCCESS, "profitable-simulation"
    return CATEGORY_ECONOMIC_REJECT, "gas-unprofitable-receipt"


def _classify_failed_response(response: dict[str, Any]) -> tuple[str, str]:
    """Classify a ``failed`` outcome by its bucket and its raw revert evidence.

    Args:
        response: One probe response whose ``outcome`` is ``"failed"``.

    Returns:
        ``(category, reason)`` for the failure: the two typed planning refusals,
        the terminal economic revert, or an unexplained revert.

    """
    bucket = response.get("bucket")
    if bucket == ENCODE_FAILED_BUCKET:
        return CATEGORY_PLANNING_REFUSED, "encode-refusal"
    if bucket == INSOLVENT_ZERO_CAPITAL_BUCKET:
        if typed_deficit(response) is None:
            return CATEGORY_UNEXPECTED_REVERT, "incomplete-typed-deficit"
        return CATEGORY_PLANNING_REFUSED, "typed-insolvency"
    if bucket != INSUFFICIENT_PROFIT_BUCKET:
        return CATEGORY_UNEXPECTED_REVERT, "unknown-bucket"
    if not is_terminal_economic_revert(response.get("revert")):
        return CATEGORY_UNEXPECTED_REVERT, "revert-evidence"
    return CATEGORY_ECONOMIC_REJECT, "insufficient-profit-revert"


def probe_matches_policy(response: dict[str, Any], *, category: str, expect: str) -> bool:
    """Whether one classified row satisfies the run's expected-outcome policy.

    Args:
        response: The row's operator probe response.
        category: The row's category (:data:`CATEGORIES`).
        expect: The run's declared policy (:data:`EXPECTATIONS`).

    Returns:
        True when ``--expect`` accepts this row. Under ``executed`` only the two
        execution categories match; under ``insolvent`` only the typed
        capital-policy refusal does. An execution where a refusal was expected, a
        refusal where execution was expected, and any unexplained revert are all
        unexpected.

    """
    if expect == EXPECT_EXECUTED:
        return category in EXECUTION_CATEGORIES
    return (
        category == CATEGORY_PLANNING_REFUSED
        and response.get("bucket") == INSOLVENT_ZERO_CAPITAL_BUCKET
        and typed_deficit(response) is not None
    )


def response_identity_defect(response: dict[str, Any], *, path_id: int, capital: str) -> str | None:
    """Return why a response cannot be the answer to the request this row issued.

    The wire is a request/response pair, but a reply is evidence only when it names
    its request: a profitable verdict for another path, a receipt for a posture
    nobody asked for, or a block the call cannot have run at is an unaccounted
    outcome, whatever its verdict says. Auditing the claim here keeps the report
    from resting on the transport having correlated the reply.

    Args:
        response: One operator probe response.
        path_id: The engine path ID this row's probe requested.
        capital: The capital posture this row's probe requested
            (:data:`CAPITAL_MODES`).

    Returns:
        A stable reason token for the first disagreement — ``response-path-id`` (not
        a non-bool integer equal to the requested path), ``response-capital`` (not
        exactly the requested spelling), ``response-block`` (not a non-negative
        non-bool integer) — or ``None`` when the response names this row's request.

    """
    reported_path_id = response.get("path_id")
    if not _is_integer(reported_path_id) or reported_path_id != path_id:
        return "response-path-id"
    if response.get("capital") != capital:
        return "response-capital"
    if not _is_amount(response.get("current_block")):
        return "response-block"
    return None


def probe_row(
    registration: dict[str, Any], response: dict[str, Any], *, expect: str, capital: str
) -> dict[str, Any]:
    """State one completed probe row: its identity, its raw evidence, its verdict.

    Args:
        registration: The directed row's registration (permutation, direction, and
            the engine ``path_id`` the registration returned).
        response: The operator's probe response for that row.
        expect: The run's declared expected-outcome policy.
        capital: The capital posture the probe requested (:data:`CAPITAL_MODES`),
            audited against the response's own ``capital``.

    Returns:
        The row: what was probed, the posture and block the call ran under, the raw
        outcome/bucket/revert, the typed refusal details, the category and the
        policy match, the structural submission evidence, and the whole raw
        response — so a later audit reads the evidence, never a normalized label.
        A response that does not name this row's request is an ``exception`` row
        whose reason is the identity defect, so it can satisfy no expected
        outcome while the disagreement stays visible beside the raw response.

    """
    category, reason = classify_probe_response(response)
    defect = response_identity_defect(
        response, path_id=registration["response"]["path_id"], capital=capital
    )
    # An already-exception row keeps its own reason: a transport failure, a refused
    # command, or a submission claim is the more specific answer, and the row is no
    # more a policy match either way.
    if defect is not None and category != CATEGORY_EXCEPTION:
        category, reason = CATEGORY_EXCEPTION, defect
    return {
        "state": "completed",
        "permutation": registration["permutation"],
        "direction": registration["direction"],
        "path_id": registration["response"]["path_id"],
        "capital": response.get("capital"),
        "request_block": response.get("current_block"),
        "outcome": response.get("outcome"),
        "bucket": response.get("bucket"),
        "revert": response.get("revert"),
        "zero_capital": typed_deficit(response),
        "category": category,
        "reason": reason,
        "expected": expect,
        "matches_policy": probe_matches_policy(response, category=category, expect=expect),
        "submission": response.get("submission"),
        "response": response,
    }


def pending_row(registration: dict[str, Any], *, expect: str) -> dict[str, Any]:
    """State one expected row that has not been probed yet.

    Args:
        registration: The directed row's registration.
        expect: The run's declared expected-outcome policy.

    Returns:
        The row with no evidence in it: pending, unclassified, and NOT a policy
        match, so a report that stops early can never read as coverage.

    """
    return {
        "state": "pending",
        "permutation": registration["permutation"],
        "direction": registration["direction"],
        "path_id": registration["response"]["path_id"],
        "capital": None,
        "request_block": None,
        "outcome": None,
        "bucket": None,
        "revert": None,
        "zero_capital": None,
        "category": None,
        "reason": None,
        "expected": expect,
        "matches_policy": False,
        "submission": None,
        "response": None,
    }


def refresh_probe_report(report: dict[str, Any], *, interrupted: bool) -> None:
    """Recompute a report's summary and submission evidence from its rows.

    Args:
        report: The report being built (mutated in place).
        interrupted: Whether the run stopped before its last expected row.

    """
    rows = report["paths"]
    categories: dict[str, int] = dict.fromkeys(CATEGORIES, 0)
    completed = 0
    for row in rows:
        if row["state"] == "completed":
            completed += 1
            categories[row["category"]] += 1
    matched = sum(bool(row["matches_policy"]) for row in rows)
    report["summary"] = {
        "expect": report["expect"],
        "expected": len(rows),
        "completed": completed,
        "missing": len(rows) - completed,
        "matched": matched,
        "unexpected": completed - matched,
        "complete": completed == len(rows),
        "interrupted": interrupted,
        "categories": categories,
    }
    claims = [row["submission"] for row in rows if isinstance(row["submission"], dict)]
    report["submission"] = {
        **SUBMISSION_EVIDENCE,
        "possible": any(bool(claim.get("possible")) for claim in claims),
        "observed": any(bool(claim.get("observed")) for claim in claims),
    }


def build_probe_report(
    registrations: list[dict[str, Any]],
    *,
    expect: str,
    capital: str = DEFAULT_CAPITAL,
) -> dict[str, Any]:
    """Start a report whose every expected row is pending.

    Args:
        registrations: The validated directed registrations.
        expect: The run's declared expected-outcome policy (:data:`EXPECTATIONS`).
        capital: The capital posture every probe asks for (:data:`CAPITAL_MODES`).

    Returns:
        The report skeleton: the schema version, the policy it will be read under,
        the structural submission evidence, a summary that claims no coverage yet,
        and one pending row per expected identity.

    Raises:
        ValueError: If ``expect`` is not one of :data:`EXPECTATIONS`.

    """
    if expect not in EXPECTATIONS:
        expected = "|".join(EXPECTATIONS)
        msg = f"unknown expected outcome {expect!r} (expected {expected})"
        raise ValueError(msg)
    report: dict[str, Any] = {
        "schema_version": PROBE_REPORT_SCHEMA_VERSION,
        "expect": expect,
        "capital": capital,
        "paths": [pending_row(registration, expect=expect) for registration in registrations],
    }
    refresh_probe_report(report, interrupted=False)
    return report


def write_probe_report(output_path: Path, report: dict[str, Any]) -> None:
    """Write the report to ``output_path`` by atomic replace.

    A temporary file in the destination directory is written and then renamed over
    the target, so a reader — or the post-mortem of a crashed run — always finds a
    complete, parseable report: either the previous one or the new one, never half
    of either.

    Args:
        output_path: Where the report belongs.
        report: The report to write.

    """
    temporary = output_path.with_name(f"{output_path.name}.tmp")
    temporary.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    temporary.replace(output_path)


def probe_exit_code(report: dict[str, Any]) -> int:
    """Return the ``probe`` command's exit code for one finished report.

    Args:
        report: The report the run wrote.

    Returns:
        :data:`PROBE_EXIT_COMPLETE` only for a run that probed every expected row
        and matched the declared policy on all of them;
        :data:`PROBE_EXIT_FAILED` for an interrupted, incomplete, or mismatched
        run — which is why a correct run needs no ``|| true``.

    """
    summary = report["summary"]
    if summary["interrupted"] or not summary["complete"] or summary["unexpected"]:
        return PROBE_EXIT_FAILED
    return PROBE_EXIT_COMPLETE


def _probe_once(
    transport: Callable[[dict[str, Any]], dict[str, Any]],
    registration: dict[str, Any],
    *,
    capital: str,
) -> dict[str, Any]:
    """Issue one probe and return its response, or the exception shape for it.

    Args:
        transport: The function that sends one request and returns its response
            (``send_command`` bound to the socket, or a test's stub).
        registration: The row's registration (its engine ``path_id``).
        capital: The capital posture to ask for.

    Returns:
        The operator's response verbatim when the command succeeded, else the
        shaped failure: a transport failure, a non-object response, and a refused
        command all report ``outcome: "exception"`` with the raw refusal kept beside
        it, so a row can never read as execution when no execution was observed.

    Raises:
        KeyboardInterrupt: If the operator interrupted the run; the caller marks the
            report incomplete and still writes it.

    """
    path_id = registration["response"]["path_id"]
    try:
        response = transport(probe_request(path_id, capital=capital))
    except KeyboardInterrupt:
        raise
    except Exception as exc:  # ruff: ignore[blind-except] - a failed probe is an outcome
        return {"outcome": "exception", "error": f"{type(exc).__name__}: {exc}"}
    if not isinstance(response, dict):
        msg = f"operator response is not an object: {response!r}"
        return {"outcome": "exception", "error": msg}
    if not response.get("ok"):
        refusal = str(response.get("error", response))
        return {"outcome": "exception", "error": refusal, "operator_response": response}
    return response


def _probe_rows(
    send: Callable[[dict[str, Any]], dict[str, Any]],
    registrations: list[dict[str, Any]],
    report: dict[str, Any],
    output_path: Path,
    *,
    capital: str,
) -> None:
    """Probe every expected row in order, replacing the report after each response.

    A ``KeyboardInterrupt`` propagates: the caller marks the report interrupted and
    still writes it, so the rows that never ran stay visible as pending.

    Args:
        send: The request/response function (one probe request in, one response out).
        registrations: The validated directed registrations.
        report: The report skeleton to fill (mutated in place).
        output_path: Where the report is replaced after every response.
        capital: The capital posture every probe asks for (:data:`CAPITAL_MODES`).

    """
    expect = report["expect"]
    for registration, row in zip(registrations, report["paths"], strict=True):
        response = _probe_once(send, registration, capital=capital)
        row.update(probe_row(registration, response, expect=expect, capital=capital))
        refresh_probe_report(report, interrupted=False)
        write_probe_report(output_path, report)
        sys.stdout.write(
            f"{row['permutation']} {row['direction']} {row['category']} "
            f"{row['outcome']} {row['bucket'] or ''}\n"
        )
        sys.stdout.flush()


def probe_paths(
    registrations: list[dict[str, Any]],
    output_path: Path,
    *,
    expect: str,
    send: Callable[[dict[str, Any]], dict[str, Any]],
    capital: str = DEFAULT_CAPITAL,
) -> dict[str, Any]:
    """Probe every registered path, keeping the report complete at every step.

    The report is written BEFORE the first probe and re-written after every
    response, each time by atomic replace, so an interrupted run leaves a parseable
    report whose remaining rows are still pending — never a green summary of the
    rows that happened to finish. A ``KeyboardInterrupt`` ends the loop, marks the
    report interrupted, and returns it, leaving the caller's exit code non-zero
    because the matrix is not complete.

    A refused registration set, or an ``expect`` that is not a declared policy,
    raises before the first write, so a run that could never be audited leaves no
    report behind at all.

    Args:
        registrations: The directed registrations to probe.
        output_path: Where the report is written after every response.
        expect: The run's declared expected-outcome policy (:data:`EXPECTATIONS`).
        send: The request/response function — :func:`send_command` bound to the
            operator socket, or a stub.
        capital: The capital posture every probe asks for (:data:`CAPITAL_MODES`).

    Returns:
        The finished, possibly incomplete, report.

    """
    validate_registrations(registrations)
    report = build_probe_report(registrations, expect=expect, capital=capital)
    write_probe_report(output_path, report)
    interrupted = False
    try:
        _probe_rows(send, registrations, report, output_path, capital=capital)
    except KeyboardInterrupt:
        interrupted = True
    refresh_probe_report(report, interrupted=interrupted)
    write_probe_report(output_path, report)
    return report


def build_report(lines: Iterable[str], registrations: list[dict[str, Any]]) -> dict[str, Any]:
    """Fold only the injected path IDs into per-permutation simulation outcomes.

    Returns:
        A summary and one outcome row per protocol permutation.

    """
    paths = []
    by_permutation = {}
    by_path_id = {}
    for registration in registrations:
        permutation = registration["permutation"]
        row = by_permutation.get(permutation)
        if row is None:
            row = {
                "permutation": permutation,
                "path_ids": [],
                "sim_ok_count": 0,
                "failure_count": 0,
                "exception_count": 0,
                "outcome": "missing",
            }
            paths.append(row)
            by_permutation[permutation] = row
        path_id = registration["response"]["path_id"]
        row["path_ids"].append(path_id)
        by_path_id[path_id] = row

    broadcast_observed = False
    for line in lines:
        if "Submitted path " in line:
            broadcast_observed = True
        success = re.search(r"\[(?:sim-ok|profit)\] path=(\d+)", line)
        failure = re.search(r"\[sim-fail\] path=(\d+)", line)
        exception = re.search(r"candidate simulation raised an exception \{path_id=(\d+)", line)
        if success and (row := by_path_id.get(int(success.group(1)))) is not None:
            row["sim_ok_count"] += 1
        if failure and (row := by_path_id.get(int(failure.group(1)))) is not None:
            row["failure_count"] += 1
        if exception and (row := by_path_id.get(int(exception.group(1)))) is not None:
            row["exception_count"] += 1

    for row in paths:
        if row["sim_ok_count"]:
            row["outcome"] = "sim_ok"
        elif row["exception_count"]:
            row["outcome"] = "exception"
        elif row["failure_count"]:
            row["outcome"] = "failed"

    return {
        "summary": {
            "valid": sum(row["outcome"] == "sim_ok" for row in paths),
            "failed": sum(row["outcome"] == "failed" for row in paths),
            "exceptions": sum(row["outcome"] == "exception" for row in paths),
            "missing": sum(row["outcome"] == "missing" for row in paths),
            "broadcast_observed": broadcast_observed,
        },
        "paths": paths,
    }


def main() -> int:
    """Run the requested injection, probing, or log-reporting command.

    Returns:
        :data:`PROBE_EXIT_COMPLETE` only for ``print-requests``, successful
        injection, a probe run that covered every expected row under its declared
        policy, or a legacy log report with full valid coverage and no broadcast
        record; :data:`PROBE_EXIT_FAILED` for a probe or report run that did not;
        :data:`PROBE_EXIT_REFUSED` when a probe refused its registration set before
        issuing a single probe.

    """
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest="command", required=True)
    subcommands.add_parser("print-requests")
    inject = subcommands.add_parser("inject")
    inject.add_argument("--socket", type=Path, required=True)
    inject.add_argument("--output", type=Path, required=True)
    inject.add_argument("--ready-timeout", type=float, default=120.0)
    probe = subcommands.add_parser("probe")
    probe.add_argument("--socket", type=Path, required=True)
    probe.add_argument("--registrations", type=Path, required=True)
    probe.add_argument("--output", type=Path, required=True)
    probe.add_argument("--capital", choices=CAPITAL_MODES, default=DEFAULT_CAPITAL)
    probe.add_argument("--expect", choices=EXPECTATIONS, required=True)
    report = subcommands.add_parser("report")
    report.add_argument("--log", type=Path, required=True)
    report.add_argument("--registrations", type=Path, required=True)
    report.add_argument("--output", type=Path)
    args = parser.parse_args()

    if args.command == "print-requests":
        sys.stdout.write(json.dumps(path_requests(), indent=2) + "\n")
        return 0
    if args.command == "inject":
        inject_paths(args.socket, args.output, args.ready_timeout)
        return 0

    registrations = json.loads(args.registrations.read_text(encoding="utf-8"))
    if args.command == "probe":
        try:
            result = probe_paths(
                registrations,
                args.output,
                expect=args.expect,
                send=partial(send_command, args.socket),
                capital=args.capital,
            )
        except (TypeError, ValueError) as exc:
            sys.stderr.write(f"probe refused: {exc}\n")
            return PROBE_EXIT_REFUSED
        return probe_exit_code(result)

    result = build_report(
        args.log.read_text(encoding="utf-8", errors="replace").splitlines(), registrations
    )
    rendered = json.dumps(result, indent=2) + "\n"
    if args.output:
        args.output.write_text(rendered, encoding="utf-8")
    sys.stdout.write(rendered)
    return (
        0
        if result["summary"]["valid"] == PERMUTATION_COUNT
        and not result["summary"]["broadcast_observed"]
        else 1
    )


if __name__ == "__main__":
    raise SystemExit(main())
