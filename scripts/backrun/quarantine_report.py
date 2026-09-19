#!/usr/bin/env python3
"""Post-hoc outcome report for the backrun quarantine journal and archive.

The sidecar parks nonce-gap-blocked bundles in a JSON Lines journal and appends
resolved outcomes to a sibling, never-compacted archive. This script folds both
surfaces into one row per distinct frame: archived outcomes are authoritative,
and every other frame is probed read-only against the node. It writes nothing.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

JOURNAL_NAME = "backrun-quarantine.jsonl"
ARCHIVE_NAME = "backrun-quarantine-resolved.jsonl"
NODE_ENV = "DEGENBOT_RPC_HTTP_CHAINID_1"
RPC_TIMEOUT_S = 30
ROW_FMT = "{:<12} {:<8} {:<20} {:<12} {}"


class _RpcError(Exception):
    """A JSON-RPC call failed in transport or returned an error object."""


def _out(text: str) -> None:
    sys.stdout.write(f"{text}\n")


def _warn(text: str) -> None:
    sys.stderr.write(f"warning: {text}\n")


def _state_dir() -> Path:
    xdg = os.environ.get("XDG_STATE_HOME")
    if xdg and Path(xdg).is_absolute():
        return Path(xdg) / "degenbot" / "state"
    return Path.home() / ".local" / "state" / "degenbot" / "state"


def _as_int(value: object) -> int | None:
    if isinstance(value, bool):
        return None
    if isinstance(value, int):
        return value
    if isinstance(value, str):
        try:
            return int(value, 0)
        except ValueError:
            return None
    return None


def _sort_ms(value: object) -> int:
    ms = _as_int(value)
    return ms if ms is not None else -1


def _fmt_ms(value: object) -> str:
    ms = _as_int(value)
    if ms is None:
        return "-"
    return time.strftime("%Y-%m-%d %H:%M:%S UTC", time.gmtime(ms / 1000))


def _read_lines(path: Path) -> list[str] | None:
    if not path.exists():
        return None
    return path.read_text(encoding="utf-8").splitlines()


def _last_nonempty(lines: list[str]) -> int:
    for index in range(len(lines) - 1, -1, -1):
        if lines[index].strip():
            return index
    return -1


def _load_archive(path: Path) -> tuple[list[str], dict[str, dict[str, object]], list[str]]:
    order: list[str] = []
    records: dict[str, dict[str, object]] = {}
    errors: list[str] = []
    lines = _read_lines(path)
    if lines is None:
        return order, records, errors
    last = _last_nonempty(lines)
    for index, line in enumerate(lines):
        text = line.strip()
        if not text:
            continue
        try:
            parsed = json.loads(text)
        except json.JSONDecodeError:
            if index != last:
                errors.append(f"{path}:{index + 1}: unparseable archive line")
            continue
        if not isinstance(parsed, dict) or not isinstance(parsed.get("frame_hash"), str):
            if index != last:
                errors.append(f"{path}:{index + 1}: archive line lacks frame_hash")
            continue
        key = str(parsed["frame_hash"]).lower()
        previous = records.get(key)
        if previous is None:
            order.append(key)
        elif _sort_ms(parsed.get("resolved_unix_ms")) < _sort_ms(previous.get("resolved_unix_ms")):
            continue
        records[key] = parsed
    return order, records, errors


def _load_parks(path: Path) -> tuple[list[str], dict[str, dict[str, object]], list[str]]:
    order: list[str] = []
    parks: dict[str, dict[str, object]] = {}
    errors: list[str] = []
    lines = _read_lines(path)
    if lines is None:
        return order, parks, errors
    last = _last_nonempty(lines)
    for index, line in enumerate(lines):
        text = line.strip()
        if not text:
            continue
        try:
            parsed = json.loads(text)
        except json.JSONDecodeError:
            if index != last:
                errors.append(f"{path}:{index + 1}: unparseable journal line")
            continue
        if not isinstance(parsed, dict) or parsed.get("kind") != "park":
            continue
        frame = parsed.get("frame")
        if not isinstance(frame, dict) or not isinstance(frame.get("hash"), str):
            continue
        key = str(frame["hash"]).lower()
        if key not in parks:
            order.append(key)
        parks[key] = {
            "frame_hash": frame["hash"],
            "sender": parsed.get("sender", frame.get("from")),
            "claimed_nonce": parsed.get("claimed_nonce", frame.get("nonce")),
            "received_unix_ms": frame.get("received_unix_ms"),
        }
    return order, parks, errors


def _rpc(url: str, method: str, params: list[object]) -> object:
    payload = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
    request = urllib.request.Request(  # ruff: ignore[suspicious-url-open-usage]
        url, data=payload.encode("utf-8"), headers={"Content-Type": "application/json"}
    )
    try:
        # The node URL comes from the operator's own environment/flag.
        with urllib.request.urlopen(  # ruff: ignore[suspicious-url-open-usage]
            request, timeout=RPC_TIMEOUT_S
        ) as response:
            body = json.load(response)
    except (urllib.error.URLError, OSError) as exc:
        msg = f"{method}: transport failure: {exc}"
        raise _RpcError(msg) from exc
    if not isinstance(body, dict):
        msg = f"{method}: malformed JSON-RPC response"
        raise _RpcError(msg)
    if body.get("error"):
        msg = f"{method}: node error: {body['error']}"
        raise _RpcError(msg)
    return body.get("result")


def _consumed_details(url: str, consumer: dict[str, object]) -> dict[str, object]:
    by = consumer.get("hash")
    tx = consumer
    if isinstance(by, str):
        try:
            fetched = _rpc(url, "eth_getTransactionByHash", [by])
        except _RpcError as exc:
            return {"outcome": "probe_failed", "error": str(exc)}
        if isinstance(fetched, dict):
            tx = fetched
    return {
        "outcome": "slot_taken",
        "by": by,
        "block": _as_int(tx.get("blockNumber")),
        "from": tx.get("from"),
        "to": tx.get("to"),
    }


def _probe(url: str, frame_hash: str, sender: object, nonce: object) -> dict[str, object]:
    try:
        receipt = _rpc(url, "eth_getTransactionReceipt", [frame_hash])
        if isinstance(receipt, dict):
            return {
                "outcome": "mined",
                "block": _as_int(receipt.get("blockNumber")),
                "block_hash": receipt.get("blockHash"),
            }
        claimed = _as_int(nonce)
        if not isinstance(sender, str) or claimed is None:
            return {"outcome": "still_pending"}
        consumer = _rpc(url, "eth_getTransactionBySenderAndNonce", [sender, hex(claimed)])
    except _RpcError as exc:
        return {"outcome": "probe_failed", "error": str(exc)}
    if not isinstance(consumer, dict):
        return {"outcome": "still_pending"}
    return _consumed_details(url, consumer)


def _enrich_slot_taken(url: str, info: dict[str, object]) -> None:
    by = info.get("by")
    if not isinstance(by, str):
        return
    try:
        tx = _rpc(url, "eth_getTransactionByHash", [by])
    except _RpcError:
        return
    if isinstance(tx, dict):
        info["from"] = tx.get("from")
        info["to"] = tx.get("to")
        if info.get("block") is None:
            info["block"] = _as_int(tx.get("blockNumber"))


def _archived(record: dict[str, object]) -> dict[str, object]:
    return {
        "outcome": record.get("resolution"),
        "block": _as_int(record.get("block")),
        "block_hash": record.get("block_hash"),
        "by": record.get("by"),
        "finalized_block": _as_int(record.get("finalized_block")),
        "resolved_unix_ms": _as_int(record.get("resolved_unix_ms")),
    }


def _describe(info: dict[str, object]) -> str:
    kind = str(info.get("outcome"))
    if kind == "probe_failed":
        return f"probe-failed: {info.get('error')}"
    if kind == "still_pending":
        return "still-pending"
    parts = [kind]
    if kind == "slot_taken" and info.get("by"):
        parts.append(f"by {info['by']}")
    if info.get("block"):
        parts.append(f"block {info['block']}")
    if kind == "slot_taken" and (info.get("from") or info.get("to")):
        parts.append(f"consumer {info.get('from')} -> {info.get('to')}")
    if info.get("finalized_block"):
        parts.append(f"finalized_at {info['finalized_block']}")
    if info.get("resolved_unix_ms"):
        parts.append(f"resolved {_fmt_ms(info['resolved_unix_ms'])}")
    return " ".join(parts)


def _tally(counts: dict[str, int], info: dict[str, object]) -> None:
    kind = str(info.get("outcome"))
    if kind in counts:
        counts[kind] += 1
    if kind == "slot_taken":
        counts["slot_taken_by" if info.get("by") else "slot_taken_without_by"] += 1


def _collect(
    journal: Path, archive: Path
) -> tuple[list[str], dict[str, dict[str, object]], dict[str, dict[str, object]], list[str]]:
    archive_order, archive_records, archive_errors = _load_archive(archive)
    park_order, parks, park_errors = _load_parks(journal)
    keys = list(archive_order)
    known = set(keys)
    for key in park_order:
        if key not in known:
            keys.append(key)
            known.add(key)
    return keys, archive_records, parks, archive_errors + park_errors


def _rows(
    keys: list[str],
    archive_records: dict[str, dict[str, object]],
    parks: dict[str, dict[str, object]],
    node: str,
) -> tuple[list[str], dict[str, int]]:
    rows: list[str] = []
    counts = {
        "mined": 0,
        "slot_taken": 0,
        "slot_taken_by": 0,
        "slot_taken_without_by": 0,
        "still_pending": 0,
        "probe_failed": 0,
    }
    for key in keys:
        record = archive_records.get(key)
        park = parks.get(key)
        source = record if record is not None else park
        if source is None:
            continue
        frame_hash = str(source.get("frame_hash", ""))
        nonce = source.get("claimed_nonce")
        sender = source.get("sender")
        received = park.get("received_unix_ms") if park else None
        if record is not None:
            info = _archived(record)
            if info.get("outcome") == "slot_taken":
                _enrich_slot_taken(node, info)
        else:
            info = _probe(node, frame_hash, sender, nonce)
        _tally(counts, info)
        prefix = frame_hash[:10]
        sender_text = str(sender)[:10] if sender else "-"
        nonce_text = str(nonce) if nonce is not None else "-"
        outcome = _describe(info)
        rows.append(
            ROW_FMT.format(prefix, nonce_text, _fmt_ms(received), sender_text, outcome)
        )
    return rows, counts


def _print_report(
    journal: Path,
    archive: Path,
    node: str,
    rows: list[str],
    outcome: tuple[dict[str, int], list[str]],
) -> None:
    counts, errors = outcome
    for error in errors:
        _warn(error)
    _out(f"journal: {journal}")
    _out(f"archive: {archive}")
    _out(f"node:    {node}")
    _out("")
    _out(ROW_FMT.format("FRAME", "NONCE", "SURFACED (UTC)", "SENDER", "OUTCOME"))
    for row in rows:
        _out(row)
    _out("")
    _out("summary")
    _out(f"  total frames : {len(rows)}")
    _out(f"  mined        : {counts['mined']}")
    _out(
        f"  slot_taken   : {counts['slot_taken']} "
        f"(by={counts['slot_taken_by']}, without_by={counts['slot_taken_without_by']})"
    )
    _out(f"  still pending: {counts['still_pending']}")
    _out(f"  probe failed : {counts['probe_failed']}")


def main() -> int:
    """Print the per-frame quarantine outcome report and return the exit code.

    Returns:
        The process exit code: 0 when every frame resolved, 1 on any probe failure.

    """
    parser = argparse.ArgumentParser(
        description="Report backrun quarantine frames and their on-chain outcomes."
    )
    parser.add_argument("--journal", type=Path, default=None)
    parser.add_argument("--archive", type=Path, default=None)
    parser.add_argument("--node", default=None)
    args = parser.parse_args()

    journal = args.journal if args.journal is not None else _state_dir() / JOURNAL_NAME
    archive = args.archive if args.archive is not None else journal.with_name(ARCHIVE_NAME)
    node = args.node or os.environ.get(NODE_ENV)
    if not node:
        sys.stderr.write(f"error: no node RPC URL; pass --node or set {NODE_ENV}\n")
        return 2

    keys, archive_records, parks, errors = _collect(journal, archive)
    rows, counts = _rows(keys, archive_records, parks, node)
    _print_report(journal, archive, node, rows, (counts, errors))
    return 1 if counts["probe_failed"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
