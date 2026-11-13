#!/usr/bin/env python3
"""Inject and audit the fixed WETH/USDC/WBTC three-hop protocol matrix.

The injection command proves registration only. The report command correlates
returned engine path IDs with production solver/simulation logs and fails until
all 27 permutations have a valid execution receipt. It also fails if a submission
record appears in what is required to be a dry-run log.
"""

from __future__ import annotations

import argparse
import itertools
import json
import re
import socket
import sys
import time
from pathlib import Path
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from collections.abc import Iterable

MANIFEST_PATH = Path(__file__).with_suffix(".json")
FAMILIES = ("V2", "V3", "V4")
EDGE_ORDER = ("A", "B", "C")
PERMUTATION_COUNT = 27
DIRECTED_PATH_COUNT = 27

#: The probe payload's capital vocabulary — the operator wire's spellings, which
#: the host validates (`degenbot.operator.operator_channel.capital_from_wire`).
CAPITAL_MODES = ("production", "self_fund", "zero_balance")
DEFAULT_CAPITAL = "self_fund"


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


def probe_paths(
    socket_path: Path,
    registrations: list[dict[str, Any]],
    output_path: Path,
    *,
    capital: str = DEFAULT_CAPITAL,
) -> dict[str, Any]:
    """Execute every registered path with an explicit small WETH input.

    Args:
        socket_path: The operator socket.
        registrations: The ``inject`` command's registrations (path IDs).
        output_path: Where the execution report is written.
        capital: The capital posture every probe asks for (:data:`CAPITAL_MODES`).

    Returns:
        A per-directed-path execution report.

    Raises:
        RuntimeError: If the operator refuses a probe.

    """
    responses = []
    for registration in registrations:
        path_id = registration["response"]["path_id"]
        try:
            response = send_command(socket_path, probe_request(path_id, capital=capital))
            if not response.get("ok"):
                response = {
                    "outcome": "exception",
                    "bucket": "operator-error",
                    "error": str(response.get("error", response)),
                }
        except (OSError, RuntimeError, TimeoutError) as exc:
            response = {"outcome": "exception", "bucket": type(exc).__name__, "error": str(exc)}
        responses.append({
            "permutation": registration["permutation"],
            "direction": registration["direction"],
            "path_id": path_id,
            "response": response,
        })
        sys.stdout.write(
            f"{registration['permutation']} {registration['direction']} "
            f"{response.get('outcome')} {response.get('bucket', '')}\n"
        )
        sys.stdout.flush()
    report = build_probe_report(responses)
    output_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    return report


def build_probe_report(responses: list[dict[str, Any]]) -> dict[str, Any]:
    """Count economic reverts as executions, not as missing or exceptional probes.

    Returns:
        A summary and one row per directed execution probe.

    """
    paths = []
    for item in responses:
        response = item["response"]
        outcome = response["outcome"]
        paths.append({
            "permutation": item["permutation"],
            "direction": item["direction"],
            "path_id": item["path_id"],
            "outcome": outcome,
            "bucket": response.get("bucket"),
            **({"error": response["error"]} if "error" in response else {}),
        })
    economic_failures = sum(
        row["outcome"] == "failed" and row["bucket"] != "encode-failed" for row in paths
    )
    encode_failures = sum(row["bucket"] == "encode-failed" for row in paths)
    sim_ok = sum(row["outcome"] == "sim_ok" for row in paths)
    return {
        "summary": {
            "executed": sim_ok + economic_failures,
            "sim_ok": sim_ok,
            "failed": economic_failures,
            "encode_failed": encode_failures,
            "exceptions": sum(row["outcome"] == "exception" for row in paths),
            "missing": sum(
                row["outcome"] not in {"sim_ok", "failed", "exception"} for row in paths
            ),
        },
        "paths": paths,
    }


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
    """Run the requested injection or reporting command.

    Returns:
        Zero only for ``print-requests``, successful injection, or a fully valid report.

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
        result = probe_paths(args.socket, registrations, args.output, capital=args.capital)
        return 0 if result["summary"]["executed"] == DIRECTED_PATH_COUNT else 1

    registrations = json.loads(args.registrations.read_text(encoding="utf-8"))
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
