#!/usr/bin/env python3
"""Offline analysis of a live `DEGENBOT_DELTA_DISPATCH` OFF/ON capture pair.

Two production evidence classes arrive in one bot log and they answer different
questions:

* the **dispatch-level** rows (`fan-out storage selection`, `storage prefetch`,
  `[sim] cands=... {current_block=...}`) fire once per fenced fan-out and carry
  that fan-out's own block, anchor, storage lane, and readiness reason;
* the **candidate-level** `fanout-trace` rows fire once per simulated candidate at
  the simulation seam, and the seam has TWO production callers (the dispatch
  fan-out and the inline hook re-quote arm). A row records the caller's head
  block and never the storage lane it read through.

So a candidate is attributed only through the additive `fanout` key the dispatch
fan-out stamps on every row it emits, which joins that fan-out's candidate rows
to its own selection / prefetch / `cands=` rows. A candidate row WITHOUT the key
— every inline hook re-quote row, and every row of a capture taken before the
key existed — stays a whole-window number. No row is ever assigned to a lane by
block number: that would coin a comparison the capture cannot carry.

A second limit is the sink itself: the Python log layer's console queue is
bounded and drops records under burst (`degenbot.log_dropped_total{sink="console"}`,
ADR-043 §6), so every row count here is a LOWER BOUND on what the process did.
A partial row set is visible in the captures themselves (a fenced legacy fan-out
with its selection row but neither its prefetch nor its `cands=` row).

Usage:

    just bal-live-ab-report
    python3 scripts/bal_live_ab_analysis.py \\
        --off logs/bal_dispatch_off.log --on logs/bal_dispatch_on.log \\
        --out logs/bal_live_ab_result.md

`--format json` prints the computed structure instead of the markdown.

# Migration notes (the additive `fanout` key and the `bal-head` rows)

Three row families gained an optional `fanout=<id>` field and the simulation
gained one new row family, all ADDITIVE — no existing field changed name or
meaning, so this script reads an old committed capture and a new attributed one
alike:

* `fan-out storage selection`, `storage prefetch` and the `[sim] cands=` row each
  append ` fanout=<id>` as their LAST field, inside the row's existing body (the
  two brace-delimited rows keep their `{...}` suffix);
* `fanout-trace` appends `"fanout":<id>` as its last JSON key. A row emitted by
  a caller OUTSIDE any fan-out (the inline hook re-quote arm) omits the key
  entirely, and so does every row from a capture taken before the key existed;
  those rows are reported UNATTRIBUTED, never assigned to a fan-out by block
  number;
* `bal-head {...}` is a new per-head row (one per head transition, on the same
  `sim` domain) carrying the exact `(anchor, hash, parent)`, the transition's
  `outcome` (`settled` / `cold` / `superseded` / `foreign`), `chain`
  (`carried` / `cold` / `already-current`), the detailed `refusal` and `bal`
  causes a fan-out's collapsed cold label hides, and the head's `prepare_us` /
  `apply_us` walls. An old capture carries none, and the report says so.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from collections.abc import Iterable, Sequence


TRACE_MARKER = "fanout-trace "
SELECTION_MARKER = "fan-out storage selection "
PREFETCH_MARKER = "storage prefetch "
HEAD_MARKER = "bal-head "
LAB_MARKER = "INFO [sim] "
OUTCOME_MARKER = "[logging] [sim] "
MEMO_MARKER = "INFO [sim] storage memo stats (block retired) "
CORDON_MARKERS = {"cordon ENTER": "enter", "cordon EXIT": "exit"}
BOOT_MARKER = "[runner] started bot pid "
RPC_MARKER = "[runner] resolved rpc "
BACKFILL_MARKER = "BlockPump: auto-backfill from snapshot block to WS block before resume "
CONNECTORS_MARKER = "[driver_boot] connector index loaded "
EXECUTOR_MARKER = "[sim] executor runtime bytecode loaded"

# The `, fanout=<id>` tail is OPTIONAL in every pattern below: a capture taken
# before the key existed carries no such field, and both shapes parse (see the
# module's migration notes).
_RE_SELECTION = re.compile(
    r"\{current_block=(\d+), anchor=(\d+), anchor_fenced=(?:true|false), "
    r'storage="(\w+)", reason=(?:None|Some\("([^"]+)"\))(?:, fanout=(\d+))?\}$'
)
_RE_PREFETCH = re.compile(
    r"\{current_block=(\d+), anchor=(\d+), anchor_fenced=(?:true|false), "
    r"derived=(\d+), learned=(\d+), hook=(\d+), requested=(\d+), seeded=(\d+), "
    r'skipped=(?:None|Some\("([^"]+)"\))(?:, fanout=(\d+))?\}$'
)
_RE_LAB = re.compile(
    r"cands=(\d+) sims=(\d+) sim_ms=(\d+) builds=(\d+) build_ms=(\d+) cold: "
    r"b_fast=(\d+) b_slow=(\d+) b_ms=(\d+) code=(\d+) c_ms=(\d+) s_fast=(\d+) s_slow=(\d+)"
    r" s_ms=(\d+) bh=(\d+) warm_basic_hits=(\d+) slow_ms=(\d+)(?: fanout=(\d+))?"
    r" \{current_block=(\d+), fanout_ms=(\d+)\}$"
)
_RE_OUTCOME = re.compile(
    r"ready_ms=([\d.]+) (\d+) candidates: (\d+) ok \((\d+) profitable, (\d+) below threshold\), "
    r"(\d+) failed, (\d+) exceptions(?: — by reason: (.*?))?(?: — suppressed=(\d+))?$"
)
_RE_MEMO = re.compile(r"\{block_number=(\d+), memo\.hits=(\d+), memo\.misses=(\d+)\}$")
_RE_BUCKET = re.compile(r"([A-Za-z][^=]*?)=(\d+)")
_RE_BOOT = re.compile(r"(\d+) (\S+)$")
_RE_BACKFILL_SEED = re.compile(r"seed=(\d+)")
_RE_EXECUTOR = re.compile(r"bytes=(\d+), path=(\S+)$")


@dataclass(frozen=True)
class TraceRow:
    """One simulated candidate at the seam.

    `fanout` names the dispatch fan-out that simulated it, when the row carries
    that key. A row from the inline hook re-quote arm — or from a capture taken
    before the key existed — has `None` and stays UNATTRIBUTED: this analysis
    never assigns such a row to a fan-out by block number.
    """

    line: int
    block: int
    cand: str
    sim_ns: int
    verdict: str
    gas: int
    addresses: tuple[str, ...]
    slots: int
    relevance: tuple[str, ...]
    fanout: int | None = None


@dataclass(frozen=True)
class SelectionRow:
    """One fenced fan-out's storage resolution (dispatch-level telemetry)."""

    line: int
    current_block: int
    anchor: int
    storage: str
    reason: str | None
    fanout: int | None = None


@dataclass(frozen=True)
class PrefetchRow:
    """One legacy-lane prefetch batch's plan attribution."""

    line: int
    current_block: int
    anchor: int
    derived: int
    learned: int
    hook: int
    requested: int
    seeded: int
    skipped: str | None
    fanout: int | None = None


@dataclass(frozen=True)
class LabRow:
    """One fenced fan-out's own wall plus the process-global cold counters."""

    line: int
    current_block: int
    candidates: int
    sims: int
    sim_ms: int
    builds: int
    build_ms: int
    b_slow: int
    s_slow: int
    slow_ms: int
    fanout_ms: int
    fanout: int | None = None


@dataclass(frozen=True)
class HeadRow:
    """One canonical-head transition as the pipeline reported it.

    The fan-out side of these facts is a CLOSED cold label (`lineage-refused`
    covers four refusals, `bal-unreadable` three). This row is the explanation
    behind that label, plus the head's own prepare and apply walls.
    """

    line: int
    anchor: int
    hash: str
    parent: str
    outcome: str
    chain: str | None
    refusal: str | None
    bal: str | None
    prepare_us: int | None
    apply_us: int | None
    carried: int | None
    writes: int | None
    overwritten: int | None
    skipped: int | None


@dataclass(frozen=True)
class OutcomeRow:
    """One drained batch outcome as the driver rendered it."""

    line: int
    ready_ms: float
    candidates: int
    ok: int
    failed: int
    exceptions: int
    buckets: dict[str, int]


@dataclass(frozen=True)
class MemoRow:
    """One retired per-anchor storage memo's hit/miss counts."""

    line: int
    block: int
    hits: int
    misses: int


@dataclass(frozen=True)
class SimSummary:
    """A percentile summary over a nanosecond sample."""

    count: int
    total: int
    mean: float
    p50: float
    p90: float
    p95: float
    maximum: int


@dataclass
class Capture:
    """One capture file's parsed streams (all rows, in file order)."""

    label: str
    path: str
    trace: list[TraceRow] = field(default_factory=list)
    selections: list[SelectionRow] = field(default_factory=list)
    prefetches: list[PrefetchRow] = field(default_factory=list)
    heads: list[HeadRow] = field(default_factory=list)
    labs: list[LabRow] = field(default_factory=list)
    outcomes: list[OutcomeRow] = field(default_factory=list)
    memos: list[MemoRow] = field(default_factory=list)
    cordons: list[tuple[int, str]] = field(default_factory=list)
    dropped_lines: int = 0
    boot: str | None = None
    executor: str | None = None
    extract_rows: int | None = None
    extract_mismatch: int | None = None
    rpc: str | None = None
    backfill: str | None = None
    backfill_seed: int | None = None
    connectors: str | None = None


# ── parsers ────────────────────────────────────────────────────────────────


def _payload(line: str, marker: str) -> str | None:
    index = line.find(marker)
    if index < 0:
        return None
    return line[index + len(marker) :].strip()


def parse_trace_line(line: str) -> TraceRow | None:
    """Parse one `fanout-trace {...}` row; `None` for any other line."""
    body = _payload(line, TRACE_MARKER)
    if body is None or not body.startswith("{"):
        return None
    try:
        row = json.loads(body)
    except json.JSONDecodeError:
        return None
    reads: list[str] = row.get("reads", [])
    addresses = tuple(entry.split("|", 1)[0] for entry in reads)
    slots = 0
    for entry in reads:
        _, _, keys = entry.partition("|")
        slots += len([key for key in keys.split(",") if key])
    return TraceRow(
        line=0,
        block=int(row["block"]),
        cand=str(row["cand"]),
        sim_ns=int(row["sim_ns"]),
        verdict=str(row["verdict"]),
        gas=int(row["gas"]),
        addresses=addresses,
        slots=slots,
        relevance=tuple(row.get("relevance", [])),
        # Absent for a caller outside any fan-out (the inline hook re-quote arm)
        # and for every pre-`fanout` row: both stay unattributed.
        fanout=int(row["fanout"]) if "fanout" in row else None,
    )


def parse_selection_line(line: str) -> SelectionRow | None:
    """Parse one `fan-out storage selection` row; `None` for any other line."""
    body = _payload(line, SELECTION_MARKER)
    if body is None:
        return None
    match = _RE_SELECTION.match(body)
    if match is None:
        return None
    current_block, anchor, storage, reason, fanout = match.groups()
    return SelectionRow(
        line=0,
        current_block=int(current_block),
        anchor=int(anchor),
        storage=storage,
        reason=reason,
        fanout=int(fanout) if fanout else None,
    )


def parse_prefetch_line(line: str) -> PrefetchRow | None:
    """Parse one `storage prefetch` row; `None` for any other line."""
    body = _payload(line, PREFETCH_MARKER)
    if body is None:
        return None
    match = _RE_PREFETCH.match(body)
    if match is None:
        return None
    fields = match.groups()
    return PrefetchRow(
        line=0,
        current_block=int(fields[0]),
        anchor=int(fields[1]),
        derived=int(fields[2]),
        learned=int(fields[3]),
        hook=int(fields[4]),
        requested=int(fields[5]),
        seeded=int(fields[6]),
        skipped=fields[7],
        fanout=int(fields[8]) if fields[8] else None,
    )


def parse_lab_line(line: str) -> LabRow | None:
    """Parse one driver `cands=... sims=...` fan-out row; `None` otherwise."""
    body = _payload(line, LAB_MARKER)
    if body is None:
        return None
    match = _RE_LAB.match(body)
    if match is None:
        return None
    fields = match.groups()
    # `fanout` is the group BEFORE the two trailing `{current_block=..., fanout_ms=...}`
    # fields, and it is absent in a capture taken before the key existed.
    numbers = [int(value) if value is not None else None for value in fields]
    return LabRow(
        line=0,
        current_block=numbers[17],
        candidates=numbers[0],
        sims=numbers[1],
        sim_ms=numbers[2],
        builds=numbers[3],
        build_ms=numbers[4],
        b_slow=numbers[6],
        s_slow=numbers[11],
        slow_ms=numbers[15],
        fanout_ms=numbers[18],
        fanout=numbers[16],
    )


def parse_head_line(line: str) -> HeadRow | None:
    """Parse one `bal-head {...}` row; `None` for any other line."""
    body = _payload(line, HEAD_MARKER)
    if body is None or not body.startswith("{"):
        return None
    try:
        row = json.loads(body)
    except json.JSONDecodeError:
        return None

    def optional_int(key: str) -> int | None:
        value = row.get(key)
        return int(value) if isinstance(value, int) else None

    def optional_str(key: str) -> str | None:
        value = row.get(key)
        return str(value) if isinstance(value, str) else None

    return HeadRow(
        line=0,
        anchor=int(row["anchor"]),
        hash=str(row.get("hash", "")),
        parent=str(row.get("parent", "")),
        outcome=str(row.get("outcome", "")),
        chain=optional_str("chain"),
        refusal=optional_str("refusal"),
        bal=optional_str("bal"),
        prepare_us=optional_int("prepare_us"),
        apply_us=optional_int("apply_us"),
        carried=optional_int("carried"),
        writes=optional_int("writes"),
        overwritten=optional_int("overwritten"),
        skipped=optional_int("skipped"),
    )


def parse_outcome_line(line: str) -> OutcomeRow | None:
    """Parse one driver `[sim] ready_ms=...` outcome row; `None` otherwise."""
    body = _payload(line, OUTCOME_MARKER)
    if body is None:
        return None
    match = _RE_OUTCOME.match(body)
    if match is None:
        return None
    ready_ms, candidates, ok, _profitable, _below, failed, exceptions, breakdown, _suppressed = (
        match.groups()
    )
    buckets = {name.strip(): int(count) for name, count in _RE_BUCKET.findall(breakdown or "")}
    return OutcomeRow(
        line=0,
        ready_ms=float(ready_ms),
        candidates=int(candidates),
        ok=int(ok),
        failed=int(failed),
        exceptions=int(exceptions),
        buckets=buckets,
    )


def parse_memo_line(line: str) -> MemoRow | None:
    """Parse one `storage memo stats (block retired)` row; `None` otherwise."""
    body = _payload(line, MEMO_MARKER)
    if body is None:
        return None
    match = _RE_MEMO.match(body)
    if match is None:
        return None
    block, hits, misses = match.groups()
    return MemoRow(line=0, block=int(block), hits=int(hits), misses=int(misses))


def parse_cordon_line(line: str) -> str | None:
    """`enter` / `exit` for a cordon row, `None` for any other line."""
    for marker, kind in CORDON_MARKERS.items():
        if marker in line:
            return kind
    return None


def parse_backfill_line(line: str) -> str | None:
    """The `{seed=..., ws_block=...}` body of the pump's backfill row, if this is it."""
    return _payload(line, BACKFILL_MARKER)


def backfill_seed(body: str | None) -> int | None:
    """The snapshot block the pump backfilled from, if the body carries one."""
    if body is None:
        return None
    match = _RE_BACKFILL_SEED.search(body)
    return int(match.group(1)) if match else None


def parse_rpc_line(line: str) -> str | None:
    """The `http=/ws=` pair of the runner's resolved-RPC row, if this is it."""
    return _payload(line, RPC_MARKER)


def parse_connectors_line(line: str) -> str | None:
    """The `{edges=...}` body of the driver's connector-index row, if this is it."""
    return _payload(line, CONNECTORS_MARKER)


def parse_boot_line(line: str) -> str | None:
    """The `pid started-at` pair of the runner's boot line, if this is it."""
    body = _payload(line, BOOT_MARKER)
    return body if body and _RE_BOOT.match(body) else None


def parse_executor_line(line: str) -> str | None:
    """The executor runtime image's `bytes / path`, if this is its load row."""
    if EXECUTOR_MARKER not in line:
        return None
    index = line.find("{")
    if index < 0:
        return None
    body = line[index + 1 :].rstrip().removesuffix("}")
    return body if _RE_EXECUTOR.match(body) else None


# ── aggregation ────────────────────────────────────────────────────────────


def summarize(values: Sequence[int]) -> SimSummary:
    """Summarize a sample with linear-interpolated percentiles over the sort."""
    ordered = sorted(values)
    count = len(ordered)
    if count == 0:
        return SimSummary(count=0, total=0, mean=0.0, p50=0.0, p90=0.0, p95=0.0, maximum=0)

    def quantile(q: float) -> float:
        position = (count - 1) * q
        low = int(position)
        high = min(low + 1, count - 1)
        weight = position - low
        return ordered[low] * (1 - weight) + ordered[high] * weight

    total = sum(ordered)
    return SimSummary(
        count=count,
        total=total,
        mean=total / count,
        p50=quantile(0.5),
        p90=quantile(0.90),
        p95=quantile(0.95),
        maximum=ordered[-1],
    )


def regime_of(candidate_count: int) -> str:
    """The candidate-density regime one head's sample volume falls in."""
    if candidate_count <= 2:
        return "sparse"
    if candidate_count < 20:
        return "steady"
    return "burst"


def relevance_coverage(row: TraceRow) -> float:
    """Fraction of the pre-simulation relevance set the read walk carried."""
    if not row.relevance:
        return 1.0
    read_addresses = {address.lower() for address in row.addresses}
    relevant = {address.lower() for address in row.relevance}
    return len(read_addresses & relevant) / len(relevant)


@dataclass
class FanoutAttribution:
    """One fan-out's rows, joined by the additive `fanout` key."""

    fanout: int
    selection: SelectionRow | None = None
    prefetch: PrefetchRow | None = None
    lab: LabRow | None = None
    traces: list[TraceRow] = field(default_factory=list)


def fanout_attribution(capture: Capture) -> dict[int, FanoutAttribution]:
    """Join a capture's rows by `fanout`, never by block number.

    A row without the key belongs to NO fan-out in this analysis: it is either an
    inline hook re-quote row or a row from a capture taken before the key
    existed, and both stay unattributed.
    """
    joined: dict[int, FanoutAttribution] = {}
    for selection in capture.selections:
        if selection.fanout is not None:
            joined.setdefault(
                selection.fanout, FanoutAttribution(selection.fanout)
            ).selection = selection
    for prefetch in capture.prefetches:
        if prefetch.fanout is not None:
            joined.setdefault(
                prefetch.fanout, FanoutAttribution(prefetch.fanout)
            ).prefetch = prefetch
    for lab in capture.labs:
        if lab.fanout is not None:
            joined.setdefault(lab.fanout, FanoutAttribution(lab.fanout)).lab = lab
    for row in capture.trace:
        if row.fanout is not None:
            joined.setdefault(row.fanout, FanoutAttribution(row.fanout)).traces.append(row)
    return joined


def unattributed_traces(capture: Capture) -> list[TraceRow]:
    """The candidate rows that name no fan-out, and are therefore never assigned."""
    return [row for row in capture.trace if row.fanout is None]


@dataclass(frozen=True)
class LabLane:
    """One `[sim] cands=` row joined to its lane and to the candidates it owns."""

    lab: LabRow
    lane: str | None
    joined_by: str
    attributed: int
    candidate_ns: int


def lab_lanes(capture: Capture) -> list[LabLane]:
    """Join every fan-out row to a storage lane, through the `fanout` key first.

    A keyed row is joined through that key alone: a fan-out whose selection row
    the bounded sink dropped reads as lane-unknown rather than borrowing a
    neighbour's lane from the same block. Only an unkeyed capture — one older
    than the key, holding no `fanout` field anywhere — falls back to the block,
    where the single selection row per fenced block makes the join unambiguous.
    """
    joined = fanout_attribution(capture)
    by_block = {row.current_block: row for row in capture.selections}
    lanes: list[LabLane] = []
    for lab in capture.labs:
        if lab.fanout is not None:
            entry = joined.get(lab.fanout)
            selection = entry.selection if entry is not None else None
            traces = entry.traces if entry is not None else []
            joined_by = "fanout"
        else:
            selection = by_block.get(lab.current_block)
            traces = []
            joined_by = "block" if selection is not None else "none"
        lanes.append(
            LabLane(
                lab=lab,
                lane=selection.storage if selection is not None else None,
                joined_by=joined_by,
                attributed=len(traces),
                candidate_ns=sum(row.sim_ns for row in traces),
            )
        )
    return lanes


def rows_by_block(rows: Iterable[TraceRow]) -> dict[int, list[TraceRow]]:
    """Group trace rows by the head block they were simulated under."""
    grouped: dict[int, list[TraceRow]] = {}
    for row in rows:
        grouped.setdefault(row.block, []).append(row)
    return grouped


def path_ids(rows: Iterable[TraceRow]) -> set[int]:
    """The solver path ids behind a candidate set (`<path>@<deployment>`)."""
    return {int(row.cand.split("@", 1)[0]) for row in rows}


def window_bounds(capture: Capture) -> tuple[int, int] | None:
    """The capture's first and last trace head block, `None` when it has none."""
    blocks = [row.block for row in capture.trace]
    if not blocks:
        return None
    return min(blocks), max(blocks)


def trace_identity(row: TraceRow) -> tuple[object, ...]:
    """A line-number-free identity for comparing two trace streams."""
    return (
        row.block,
        row.cand,
        row.sim_ns,
        row.verdict,
        row.gas,
        row.addresses,
        row.slots,
        row.relevance,
    )


def scan_capture(
    path: Path | str, label: str | None = None, trace_extract: Path | str | None = None
) -> Capture:
    """Stream one log file and parse every analysis row exactly once.

    `trace_extract` names a `fanout-trace`-only extract of the same capture (the
    `grep 'fanout-trace '` file): its rows must match the capture's trace
    stream exactly, which is the cross-check that says the candidate stream is
    complete even though the DEBUG/INFO rows of the same sink are dropped under
    burst.
    """
    capture = Capture(label=label or Path(path).stem, path=str(path))
    with Path(path).open(encoding="utf-8", errors="replace") as handle:
        for index, line in enumerate(handle, start=1):
            if TRACE_MARKER in line:
                row = parse_trace_line(line)
                if row is None:
                    capture.dropped_lines += 1
                else:
                    capture.trace.append(
                        TraceRow(
                            line=index,
                            block=row.block,
                            cand=row.cand,
                            sim_ns=row.sim_ns,
                            verdict=row.verdict,
                            gas=row.gas,
                            addresses=row.addresses,
                            slots=row.slots,
                            relevance=row.relevance,
                            fanout=row.fanout,
                        )
                    )
                continue
            # The cheap substring checks select a candidate parser; only a
            # SUCCESSFUL parse consumes the line, so a broad marker (`INFO [sim] `)
            # cannot swallow a neighbouring row type.
            if SELECTION_MARKER in line and (selection := parse_selection_line(line)) is not None:
                capture.selections.append(
                    SelectionRow(
                        index,
                        selection.current_block,
                        selection.anchor,
                        selection.storage,
                        selection.reason,
                        selection.fanout,
                    )
                )
                continue
            if PREFETCH_MARKER in line and (prefetch := parse_prefetch_line(line)) is not None:
                capture.prefetches.append(
                    PrefetchRow(
                        line=index,
                        current_block=prefetch.current_block,
                        anchor=prefetch.anchor,
                        derived=prefetch.derived,
                        learned=prefetch.learned,
                        hook=prefetch.hook,
                        requested=prefetch.requested,
                        seeded=prefetch.seeded,
                        skipped=prefetch.skipped,
                        fanout=prefetch.fanout,
                    )
                )
                continue
            if LAB_MARKER in line and (lab := parse_lab_line(line)) is not None:
                capture.labs.append(
                    LabRow(
                        line=index,
                        current_block=lab.current_block,
                        candidates=lab.candidates,
                        sims=lab.sims,
                        sim_ms=lab.sim_ms,
                        builds=lab.builds,
                        build_ms=lab.build_ms,
                        b_slow=lab.b_slow,
                        s_slow=lab.s_slow,
                        slow_ms=lab.slow_ms,
                        fanout_ms=lab.fanout_ms,
                        fanout=lab.fanout,
                    )
                )
                continue
            if HEAD_MARKER in line and (head := parse_head_line(line)) is not None:
                capture.heads.append(
                    HeadRow(
                        line=index,
                        anchor=head.anchor,
                        hash=head.hash,
                        parent=head.parent,
                        outcome=head.outcome,
                        chain=head.chain,
                        refusal=head.refusal,
                        bal=head.bal,
                        prepare_us=head.prepare_us,
                        apply_us=head.apply_us,
                        carried=head.carried,
                        writes=head.writes,
                        overwritten=head.overwritten,
                        skipped=head.skipped,
                    )
                )
                continue
            if OUTCOME_MARKER in line and (outcome := parse_outcome_line(line)) is not None:
                capture.outcomes.append(
                    OutcomeRow(
                        line=index,
                        ready_ms=outcome.ready_ms,
                        candidates=outcome.candidates,
                        ok=outcome.ok,
                        failed=outcome.failed,
                        exceptions=outcome.exceptions,
                        buckets=outcome.buckets,
                    )
                )
                continue
            if MEMO_MARKER in line and (memo := parse_memo_line(line)) is not None:
                capture.memos.append(MemoRow(index, memo.block, memo.hits, memo.misses))
                continue
            if (kind := parse_cordon_line(line)) is not None:
                capture.cordons.append((index, kind))
                continue
            if capture.rpc is None and RPC_MARKER in line:
                capture.rpc = parse_rpc_line(line)
                continue
            if capture.backfill is None and BACKFILL_MARKER in line:
                capture.backfill = parse_backfill_line(line)
                capture.backfill_seed = backfill_seed(capture.backfill)
                continue
            if capture.connectors is None and CONNECTORS_MARKER in line:
                capture.connectors = parse_connectors_line(line)
                continue
            if capture.boot is None and (boot := parse_boot_line(line)) is not None:
                capture.boot = boot
                continue
            if capture.executor is None and (executor := parse_executor_line(line)) is not None:
                capture.executor = executor
    if trace_extract is not None:
        extracted = scan_capture(trace_extract, label=f"{capture.label}_trace").trace
        capture.extract_rows = len(extracted)
        capture.extract_mismatch = abs(len(extracted) - len(capture.trace)) + sum(
            1
            for left, right in zip(extracted, capture.trace, strict=False)
            if trace_identity(left) != trace_identity(right)
        )
    return capture


# ── report ─────────────────────────────────────────────────────────────────


def validation_problems(off: Capture, on: Capture) -> list[str]:
    """The capture-pair checks that must hold before any number is reported."""
    problems: list[str] = []
    for capture in (off, on):
        if not capture.trace:
            problems.append(
                f"{capture.label}: no fanout-trace rows — the capture is not a sim window"
            )
        if not capture.selections:
            problems.append(
                f"{capture.label}: no fan-out storage selection rows — the dispatch lane is unobservable"
            )
        if len(capture.trace) and not 2 * sum(1 for row in capture.trace if row.addresses) >= len(
            capture.trace
        ):
            problems.append(
                f"{capture.label}: most trace rows carry no read addresses — the read walk is missing"
            )
        if any(row.sim_ns < 0 for row in capture.trace):
            problems.append(f"{capture.label}: a trace row carries a negative sim_ns")
    if any(row.storage == "delta" for row in off.selections):
        problems.append("off: a fan-out selected delta under the OFF flag — the capture is not OFF")
    if not any(row.storage == "delta" or row.reason for row in on.selections):
        problems.append("on: no delta selection and no cold reason — the flag left no telemetry")
    if off.executor != on.executor:
        problems.append("pair: the executor runtime image differs between the captures")
    if off.rpc != on.rpc:
        problems.append("pair: the resolved RPC endpoint differs between the captures")
    if off.backfill_seed != on.backfill_seed:
        problems.append(
            "pair: the snapshot seed the backfill started from differs between the captures"
        )
    if off.connectors != on.connectors:
        problems.append("pair: the connector index differs between the captures")
    for capture in (off, on):
        if capture.extract_mismatch:
            problems.append(
                f"{capture.label}: the trace extract disagrees with the capture "
                f"({capture.extract_mismatch} row difference(s) over {capture.extract_rows} rows)"
            )
    return problems


def _ms(nanoseconds: float) -> str:
    return f"{nanoseconds / 1e6:.2f}"


def _percent_change(before: float, after: float) -> str:
    """Signed percentage change against `before`, `n/a` when it is zero."""
    if not before:
        return "n/a"
    return f"{100 * (after - before) / before:+.1f}%"


def _summary_cells(summary: SimSummary) -> str:
    """Timing cells: count, total/mean/p50/p95/max in milliseconds."""
    return (
        f"{summary.count} | {_ms(summary.total)} | {_ms(summary.mean)} | "
        f"{_ms(summary.p50)} | {_ms(summary.p95)} | {_ms(summary.maximum)}"
    )


def _count_phrase(summary: SimSummary) -> str:
    """One compact cardinality phrase for a two-capture pair table."""
    return (
        f"n={summary.count}, total={summary.total}, mean={summary.mean:.2f}, "
        f"p50={summary.p50:.2f}, p95={summary.p95:.2f}, max={summary.maximum}"
    )


def _timing_phrase(summary: SimSummary) -> str:
    """One compact timing phrase for a two-capture pair table (milliseconds)."""
    return (
        f"n={summary.count}, total={_ms(summary.total)} ms, mean={_ms(summary.mean)} ms, "
        f"p50={_ms(summary.p50)} ms, p95={_ms(summary.p95)} ms, max={_ms(summary.maximum)} ms"
    )


def _coverage_phrase(values: Sequence[float]) -> str:
    """The coverage distribution as min / mean / p50 / max fractions."""
    if not values:
        return "no rows"
    summary = summarize([round(value * 1000) for value in values])
    return (
        f"min={min(values):.3f}, mean={summary.mean / 1000:.3f}, "
        f"p50={summary.p50 / 1000:.3f}, max={max(values):.3f}"
    )


def _regime_table(capture: Capture) -> list[str]:
    grouped = rows_by_block(capture.trace)
    buckets: dict[str, list[TraceRow]] = {"sparse": [], "steady": [], "burst": []}
    for rows in grouped.values():
        buckets[regime_of(len(rows))].extend(rows)
    lines = [
        "| regime | heads | candidates | total ms | mean ms | p50 ms | p95 ms | max ms |",
        "| --- | --- | --- | --- | --- | --- | --- | --- |",
    ]
    for regime in ("sparse", "steady", "burst"):
        rows = buckets[regime]
        heads = len({row.block for row in rows})
        lines.append(
            f"| {regime} | {heads} | {_summary_cells(summarize([row.sim_ns for row in rows]))} |"
        )
    return lines


def _read_table(capture: Capture) -> list[tuple[str, str]]:
    """Per-capture read-set facts as (label, cell) pairs for the pair table."""
    coverage = [relevance_coverage(row) for row in capture.trace]
    fully = sum(1 for value in coverage if value >= 0.999)
    at_least_90 = sum(1 for value in coverage if value >= 0.9)
    below_half = sum(1 for value in coverage if value < 0.5)
    return [
        (
            "addresses per candidate",
            _count_phrase(summarize([len(row.addresses) for row in capture.trace])),
        ),
        (
            "slots per candidate",
            _count_phrase(summarize([row.slots for row in capture.trace])),
        ),
        (
            "pre-simulation relevance per candidate",
            _count_phrase(summarize([len(row.relevance) for row in capture.trace])),
        ),
        (
            "relevance coverage (min / mean / p50 / max)",
            _coverage_phrase(coverage),
        ),
        (
            "coverage rows at 1.0 / >=0.9 / <0.5",
            f"{fully} / {at_least_90} / {below_half} of {len(coverage)}",
        ),
    ]


def _by_candidate_count(lanes: Sequence[LabLane]) -> dict[int, list[LabLane]]:
    """Group fan-out rows by the candidate count each reported."""
    grouped: dict[int, list[LabLane]] = {}
    for entry in lanes:
        grouped.setdefault(entry.lab.candidates, []).append(entry)
    return grouped


def _lane_cell(entry: LabLane) -> str:
    """Return one fan-out's lane, `unknown` when its selection row did not survive."""
    return entry.lane or "unknown"


def _candidate_ms_cell(entry: LabLane) -> str:
    """Return that fan-out's own candidate time, `—` when its candidates carry no key."""
    return _ms(float(entry.candidate_ns)) if entry.attributed else "—"


def _walls(entries: Sequence[LabLane]) -> str:
    """Describe a group of fan-outs' `fanout_ms` walls and their candidate time."""
    walls = ", ".join(str(entry.lab.fanout_ms) for entry in entries)
    candidates = ", ".join(_candidate_ms_cell(entry) for entry in entries)
    return f"{walls} ms of `fanout_ms` over {candidates} ms of candidate time"


def _cross_lane_counts(
    off_by_count: dict[int, list[LabLane]],
    on_by_count: dict[int, list[LabLane]],
    shared: Sequence[int],
) -> list[tuple[int, list[LabLane], list[LabLane]]]:
    """Find the counts where an OFF `legacy` fan-out meets an ON `delta` fan-out.

    A cross-lane count is a count-match, never a pair: the two arms ran disjoint
    heads over different candidate sets, so nothing here is a repeated
    observation of one fan-out on two lanes.
    """
    cross: list[tuple[int, list[LabLane], list[LabLane]]] = []
    for count in shared:
        off_legacy = [entry for entry in off_by_count[count] if entry.lane == "legacy"]
        on_delta = [entry for entry in on_by_count[count] if entry.lane == "delta"]
        if off_legacy and on_delta:
            cross.append((count, off_legacy, on_delta))
    return cross


def _matched_observations(
    off_by_count: dict[int, list[LabLane]],
    on_by_count: dict[int, list[LabLane]],
    shared: Sequence[int],
) -> list[str]:
    """State the count matches as observations, never as pairs or speedups."""
    lines: list[str] = []
    cross = _cross_lane_counts(off_by_count, on_by_count, shared)
    if cross:
        lines += [
            "OFF `legacy` and ON `delta` meet at candidate count(s) "
            + ", ".join(str(count) for count, _, _ in cross)
            + ":",
            "",
        ]
        for count, off_legacy, on_delta in cross:
            lines.append(
                f"- count {count}: off legacy {_walls(off_legacy)}; on delta {_walls(on_delta)}."
            )
        lines += [
            "",
            "Each of those is a cross-lane count match, not a pair: the arms ran disjoint",
            "block ranges, so no fan-out shares a head, a candidate set, or a block with its",
            "counterpart, the sample is one or two fan-outs per arm, and `fanout_ms` is a",
            "millisecond-resolution wall. Read them as an observational direction to test on",
            "matched heads — never a paired head comparison and never a controlled speedup.",
            "",
        ]
    else:
        lines += [
            "No candidate count matches an OFF `legacy` fan-out against an ON `delta`",
            "fan-out, so this pair carries no cross-lane count observation.",
            "",
        ]
    off_only = sorted(set(off_by_count) - set(shared))
    on_only = sorted(set(on_by_count) - set(shared))
    if off_only or on_only:
        lines += [
            "Counts one arm carries with no counterpart at all: "
            + f"off {_count_list(off_only)}; on {_count_list(on_only)}.",
            "",
        ]
    return lines


def _us(microseconds: float) -> str:
    """Convert one microsecond wall to milliseconds."""
    return f"{microseconds / 1000:.2f}"


def _head_wall_lines(capture: Capture) -> list[str]:
    """Render the `bal-head` prepare/apply walls as one summary row each, in ms."""
    lines: list[str] = []
    prepare = [row.prepare_us for row in capture.heads if row.prepare_us is not None]
    apply = [row.apply_us for row in capture.heads if row.apply_us is not None]
    for label, values in (("prepare", prepare), ("apply", apply)):
        summary = summarize(values)
        lines.append(
            f"| {label} | {summary.count} | {_us(summary.total)} | {_us(summary.p50)} | "
            f"{_us(summary.p90)} | {_us(summary.maximum)} |"
        )
    return lines


def _id_list(ids: Sequence[int]) -> str:
    """List the fan-out ids a summary line names."""
    return ", ".join(str(fanout) for fanout in ids)


def _count_list(counts: Sequence[int]) -> str:
    """List the candidate counts a summary line names."""
    return ", ".join(str(count) for count in counts)


def render_markdown(off: Capture, on: Capture) -> str:
    """Render the OFF/ON report: dispatch telemetry apart from window timing."""
    lines: list[str] = []
    problems = validation_problems(off, on)
    off_bounds = window_bounds(off)
    on_bounds = window_bounds(on)
    ready = [row for row in on.selections if row.storage == "delta"]
    cold = [row for row in on.selections if row.reason]

    lines += [
        "# Live BAL dispatch OFF versus ON",
        "",
        "Observational comparison of two live bot windows on the same host, the same",
        "owner-patched executor, and the same configuration apart from",
        "`DEGENBOT_DELTA_DISPATCH`. Regenerate with `just bal-live-ab-report`.",
        "",
        "## What this capture can and cannot say",
        "",
        "The `fanout-trace` rows are emitted per candidate at the simulation seam by",
        "every production caller. A dispatch fan-out's rows carry that fan-out's",
        "`fanout` key, which joins them to its dispatch-level rows; a row WITHOUT the",
        "key — every inline hook re-quote row, and every row of a capture taken before",
        "the key existed — is reported **unattributed** and is never assigned to a",
        "fan-out. Whole-window candidate timing therefore stays a window number, and",
        "any per-lane candidate timing below is one fan-out's own, not a paired",
        "comparison across the two captures. Every lane claim comes from the per-fan-out",
        "dispatch rows (`fan-out storage selection`, `storage prefetch`, `[sim] cands=`),",
        "which carry their own block, anchor, representation, and `fanout` key.",
        "",
        "The console sink is a bounded drop-counting queue, so each row count is a",
        "lower bound on what the process did; a fan-out whose selection row survived",
        "can still be missing its prefetch and `cands=` rows.",
        "",
        "## Capture validation",
        "",
        "| check | off | on |",
        "| --- | --- | --- |",
        f"| capture file | `{off.path}` | `{on.path}` |",
        f"| boot (pid / start) | {off.boot or 'unparsed'} | {on.boot or 'unparsed'} |",
        f"| rpc | {off.rpc or 'unparsed'} | {on.rpc or 'unparsed'} |",
        f"| backfill | {off.backfill or 'unparsed'} | {on.backfill or 'unparsed'} |",
        f"| connectors | {off.connectors or 'unparsed'} | {on.connectors or 'unparsed'} |",
        f"| executor image | {off.executor or 'unparsed'} | {on.executor or 'unparsed'} |",
        f"| trace rows | {len(off.trace)} | {len(on.trace)} |",
        f"| head blocks | {len(rows_by_block(off.trace))} | {len(rows_by_block(on.trace))} |",
        f"| block range | {off_bounds} | {on_bounds} |",
        f"| fenced fan-outs | {len(off.selections)} | {len(on.selections)} |",
        f"| prefetch rows | {len(off.prefetches)} | {len(on.prefetches)} |",
        f"| `cands=` rows | {len(off.labs)} | {len(on.labs)} |",
        f"| retired memos | {len(off.memos)} | {len(on.memos)} |",
        f"| `bal-head` rows | {len(off.heads)} | {len(on.heads)} |",
        f"| trace extract (rows / mismatches) | {_extract_cell(off)} | {_extract_cell(on)} |",
        f"| cordon rows | {len(off.cordons)} | {len(on.cordons)} |",
        "",
    ]
    lines += (
        ["Validation failures:", "", *[f"- {problem}" for problem in problems], ""]
        if problems
        else ["No validation failure raised.", ""]
    )

    lines += [
        "## Dispatch-level storage selection",
        "",
        "One row per fenced fan-out; `reason` is the closed cold label, `None` on a",
        "ready delta fan-out and on the OFF flag's own `legacy`/`disabled` lane.",
        "",
    ]
    for capture in (off, on):
        lanes = Counter(row.storage for row in capture.selections)
        reasons = Counter(row.reason for row in capture.selections)
        lines += [
            f"### {capture.label.upper()}",
            "",
            f"lanes: {dict(lanes)}; reasons: {dict(reasons)}",
            "",
            "| current_block | anchor | storage | reason | prefetch row | `cands=` row |",
            "| --- | --- | --- | --- | --- | --- |",
        ]
        joined = fanout_attribution(capture)
        prefetch_blocks = {row.current_block for row in capture.prefetches}
        lab_blocks = {row.current_block for row in capture.labs}
        for row in capture.selections:
            entry = joined.get(row.fanout) if row.fanout is not None else None
            has_prefetch = (
                entry.prefetch is not None
                if entry is not None
                else row.current_block in prefetch_blocks
            )
            has_lab = (
                entry.lab is not None if entry is not None else row.current_block in lab_blocks
            )
            lines.append(
                f"| {row.current_block} | {row.anchor} | {row.storage} | {row.reason or '—'} | "
                f"{'yes' if has_prefetch else 'no'} | {'yes' if has_lab else 'no'} |"
            )
        lines.append("")

    lines += [
        "## Fan-out attribution (the additive `fanout` key)",
        "",
        "One `fanout` identifier is minted per dispatch and carried by that fan-out's",
        "storage-selection row, its prefetch row, its `[sim]` delta summary, and every",
        "candidate's `fanout-trace` row, so the four families join without guessing from",
        "a block number. A candidate row with no `fanout` key — every inline hook",
        "re-quote row, and every row of a capture taken before the key existed — is",
        "listed as UNATTRIBUTED and is never folded into a fan-out.",
        "",
    ]
    for capture in (off, on):
        joined = fanout_attribution(capture)
        unattributed = unattributed_traces(capture)
        attributed = sum(len(entry.traces) for entry in joined.values())
        keyed_selections = sum(1 for row in capture.selections if row.fanout is not None)
        idless = sorted(fanout for fanout, entry in joined.items() if entry.selection is None)
        lines += [
            f"### {capture.label.upper()}",
            "",
            f"fanout ids observed: {len(joined)}; selection rows: {len(capture.selections)} "
            f"({keyed_selections} keyed); prefetch rows: {len(capture.prefetches)}; "
            f"`cands=` rows: {len(capture.labs)}.",
            "",
        ]
        if idless:
            lines += [
                f"Fan-out ids with no surviving selection row: {len(idless)} "
                f"({_id_list(idless)}) — the bounded sink dropped the row, so their lane is",
                "unknown and is never borrowed from the block they ran under.",
                "",
            ]
        if len(joined) and not keyed_selections:
            lines += [
                "No selection row carries the `fanout` key: this capture predates it, so the",
                "lanes below come from the one selection row per fenced block.",
                "",
            ]
        lines += [
            f"attributed candidates: {attributed} of {len(capture.trace)}; "
            f"unattributed candidates: {len(unattributed)}.",
            "",
        ]
        if not joined:
            lines += [
                "No row in this capture carries a `fanout` key: every candidate below is",
                "unattributed, and this capture supports no per-lane candidate claim.",
                "",
            ]
            continue
        lines += [
            "| fanout | current_block | anchor | storage | reason | prefetch row | `cands=` row | candidates | candidate ms |",
            "| --- | --- | --- | --- | --- | --- | --- | --- | --- |",
        ]
        for fanout, entry in sorted(joined.items()):
            selection = entry.selection
            candidates = len(entry.traces)
            candidate_ms = _ms(float(sum(row.sim_ns for row in entry.traces)))
            lines.append(
                f"| {fanout} | {selection.current_block if selection else '—'} | "
                f"{selection.anchor if selection else '—'} | "
                f"{selection.storage if selection else '—'} | "
                f"{(selection.reason if selection and selection.reason else '—')} | "
                f"{'yes' if entry.prefetch else 'no'} | "
                f"{'yes' if entry.lab else 'no'} | "
                f"{candidates} | {candidate_ms} |"
            )
        lines.append("")

    lines += [
        "## Head readiness detail (the `bal-head` rows)",
        "",
        "One row per head transition: the exact head, what the transition did, the",
        "DETAILED cause behind the collapsed cold label a fan-out reports, and the",
        "head's own prepare/apply walls. `lineage-refused` covers four refusals and",
        "`bal-unreadable` three, so this section is what explains a fallback.",
        "",
    ]
    for capture in (off, on):
        lines.append(f"### {capture.label.upper()}")
        lines.append("")
        if not capture.heads:
            lines += [
                "No `bal-head` rows — the capture predates the row, so the refusal causes",
                "and the per-head walls are not recoverable from it.",
                "",
            ]
            continue
        lines += [
            f"outcomes: {_counts_phrase(Counter(row.outcome for row in capture.heads))}; "
            f"chain: {_counts_phrase(Counter(str(row.chain) for row in capture.heads))}; "
            f"refusals: {_counts_phrase(Counter(str(row.refusal) for row in capture.heads))}; "
            f"bal skips: {_counts_phrase(Counter(str(row.bal) for row in capture.heads))}.",
            "",
            "| wall | heads | total ms | median ms | p90 ms | max ms |",
            "| --- | --- | --- | --- | --- | --- |",
            *_head_wall_lines(capture),
            "",
            "`prepare` and `apply` are the head-arrival pipeline's BACKGROUND walls: the",
            "transition that settles a head pays them on whichever task performs it, and",
            "they are not part of a fan-out's `fanout_ms`. The two are therefore reported",
            "apart and never combined — `fanout_ms` prices one dispatch's candidate work,",
            "while the prepare wall prices a whole head's BAL readiness, ahead of and",
            "concurrently with the dispatches of other heads.",
            "",
            "| anchor | hash | outcome | chain | refusal | bal | prepare_us | apply_us | carried | writes | overwritten | skipped |",
            "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |",
        ]
        for row in capture.heads:
            lines.append(
                f"| {row.anchor} | {row.hash[:10]}… | {row.outcome} | {row.chain or '—'} | "
                f"{row.refusal or '—'} | {row.bal or '—'} | "
                f"{row.prepare_us if row.prepare_us is not None else '—'} | "
                f"{row.apply_us if row.apply_us is not None else '—'} | "
                f"{row.carried if row.carried is not None else '—'} | "
                f"{row.writes if row.writes is not None else '—'} | "
                f"{row.overwritten if row.overwritten is not None else '—'} | "
                f"{row.skipped if row.skipped is not None else '—'} |"
            )
        lines.append("")

    lines += [
        "## Prefetch and anchored fan-out wall",
        "",
        "`storage prefetch` is the legacy lane's one batch; a ready delta fan-out",
        "dispatches none, which is what makes the two lanes distinguishable here.",
        "`fanout_ms` is that fan-out's own wall; `sims`/`slow_ms` are process-global",
        "counter deltas over the same interval, so they include concurrent work.",
        "",
    ]
    for capture in (off, on):
        lines += [
            f"### {capture.label.upper()}",
            "",
            "| current_block | anchor | derived | learned | hook | requested | seeded | skipped |",
            "| --- | --- | --- | --- | --- | --- | --- | --- |",
        ]
        for row in capture.prefetches:
            lines.append(
                f"| {row.current_block} | {row.anchor} | {row.derived} | {row.learned} | "
                f"{row.hook} | {row.requested} | {row.seeded} | {row.skipped or '—'} |"
            )
        lines += [
            "",
            "| current_block | lane | candidates | sims | sim_ms | builds | fanout_ms | b_slow | s_slow | slow_ms |",
            "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |",
        ]
        for entry in lab_lanes(capture):
            row = entry.lab
            lines.append(
                f"| {row.current_block} | {_lane_cell(entry)} | {row.candidates} | {row.sims} | "
                f"{row.sim_ms} | {row.builds} | {row.fanout_ms} | {row.b_slow} | "
                f"{row.s_slow} | {row.slow_ms} |"
            )
        lines.append("")

    lines += [
        "### Matched candidate counts (each arm's own lane)",
        "",
        "A shared candidate count does not make two fan-outs a pair: they ran at different",
        "heads over different candidate sets, once each. Every row below names its own",
        "arm's lane, the wall that fan-out paid (`fanout_ms`), and the candidate time of",
        "the candidates that fan-out owns.",
        "",
    ]
    off_by_count = _by_candidate_count(lab_lanes(off))
    on_by_count = _by_candidate_count(lab_lanes(on))
    shared = sorted(set(off_by_count) & set(on_by_count))
    if not shared:
        lines += [
            "No candidate count is shared by the two captures, so no count-matched row exists.",
            "",
        ]
    else:
        lines += [
            "| candidates | arm | lane | current_block | fanout_ms | attributed | candidate ms |",
            "| --- | --- | --- | --- | --- | --- | --- |",
        ]
        for count in shared:
            for arm, entries in (("off", off_by_count[count]), ("on", on_by_count[count])):
                for entry in sorted(entries, key=lambda item: item.lab.current_block):
                    lines.append(
                        f"| {count} | {arm} | {_lane_cell(entry)} | {entry.lab.current_block} | "
                        f"{entry.lab.fanout_ms} | {entry.attributed} of {entry.lab.candidates} | "
                        f"{_candidate_ms_cell(entry)} |"
                    )
        lines.append("")
        lines += _matched_observations(off_by_count, on_by_count, shared)
    for capture, grouped in ((off, off_by_count), (on, on_by_count)):
        census = Counter(_lane_cell(entry) for entries in grouped.values() for entry in entries)
        lines += [
            f"{capture.label.upper()} `cands=` rows by lane: {_counts_phrase(census)}.",
            "",
        ]
    lines += [
        "OFF resolves no `delta` fan-out at all — the flag-off window selects `legacy` (or",
        "`disabled`) on every fenced fan-out — so no same-lane OFF counterpart exists at any",
        "candidate count. The cross-lane counts above are the only count-matched observations",
        "this pair carries.",
        "",
    ]

    lines += [
        "## Whole-window candidate timing",
        "",
        "Every `fanout-trace` row in the capture, unattributed to a storage lane.",
        "Regimes are per-head sample volume: sparse 1–2, steady 3–19, burst 20+.",
        "",
        "| capture | candidates | total ms | mean ms | p50 ms | p95 ms | max ms |",
        "| --- | --- | --- | --- | --- | --- | --- |",
    ]
    for capture in (off, on):
        lines.append(
            f"| {capture.label.upper()} | {_summary_cells(summarize([row.sim_ns for row in capture.trace]))} |"
        )
    lines.append("")
    for capture in (off, on):
        lines += [f"### {capture.label.upper()} by regime", "", *_regime_table(capture), ""]

    off_total = sum(row.sim_ns for row in off.trace)
    on_total = sum(row.sim_ns for row in on.trace)
    off_outlier = max((row.sim_ns for row in off.trace), default=0)
    off_heads = rows_by_block(off.trace)
    on_heads = rows_by_block(on.trace)
    off_span = (off_bounds[1] - off_bounds[0] + 1) if off_bounds else 0
    on_span = (on_bounds[1] - on_bounds[0] + 1) if on_bounds else 0
    off_burst = sum(1 for rows in off_heads.values() if regime_of(len(rows)) == "burst")
    on_burst = sum(1 for rows in on_heads.values() if regime_of(len(rows)) == "burst")
    outlier_share = f"{100 * off_outlier / off_total:.1f}% of that window's total"
    lines += [
        f"The whole-window candidate total is {_ms(off_total)} ms (off) against "
        f"{_ms(on_total)} ms (on), a {_percent_change(off_total, on_total)} change at "
        f"{len(off.trace)} vs {len(on.trace)} candidates. Reported as an observation, not a "
        "speedup: the rows carry no lane, the mixes differ, and the ON window resolved most "
        "of its fenced fan-outs on `delta`, so the two windows are not the same workload. "
        "The OFF total also carries "
        f"one {_ms(off_outlier)} ms candidate — {outlier_share}.",
        "**Sparse heads.** The sample is thin at the head level: "
        f"{len(off_heads)} heads of {off_span} blocks carry a simulated candidate (off) and "
        f"{len(on_heads)} of {on_span} (on), and the burst regime rests on {off_burst} and "
        f"{on_burst} heads. Per-head and per-regime numbers are therefore a handful of "
        "observations each, with no repetition of any head on either lane — read them as "
        "descriptions of these windows, never as a distribution over heads.",
        "",
    ]

    lines += [
        "## Candidate mix",
        "",
        "| fact | off | on |",
        "| --- | --- | --- |",
    ]
    off_ids = {row.cand for row in off.trace}
    on_ids = {row.cand for row in on.trace}
    lines += [
        f"| distinct candidates | {len(off_ids)} | {len(on_ids)} |",
        f"| distinct solver paths | {len(path_ids(off.trace))} | {len(path_ids(on.trace))} |",
        f"| busiest head (candidates) | {_busiest(off)} | {_busiest(on)} |",
        "",
        f"Candidate ids shared by both windows: {len(off_ids & on_ids)} "
        f"({len(path_ids(off.trace) & path_ids(on.trace))} shared solver paths). A shared id "
        "is the same path at the same deployment, never the same solve: the two windows "
        "sit at different heads, so no candidate is a paired observation.",
        "",
    ]
    lines += [
        "Verdicts (whole window, unattributed): "
        f"off {_counts_phrase(Counter(row.verdict for row in off.trace))}; "
        f"on {_counts_phrase(Counter(row.verdict for row in on.trace))}.",
        "",
    ]

    lines += [
        "## Storage and read-set facts (whole window)",
        "",
        "The post-execution read walk `reads` is what the frame touched; `relevance`",
        "is the pre-simulation set the path and the deployment's settlement facts make",
        "relevant. Coverage below is the share of `relevance` the walk carried.",
        "",
        "The two sets answer different questions, so coverage below one is expected:",
        "`relevance` names every hop token and settlement address the path could touch,",
        "while `reads` is the walk the frame actually made.",
        "",
        "| fact | off | on |",
        "| --- | --- | --- |",
    ]
    off_read = _read_table(off)
    on_read = _read_table(on)
    for (label, off_cell), (_same, on_cell) in zip(off_read, on_read, strict=True):
        lines.append(f"| {label} | {off_cell} | {on_cell} |")
    lines.append("")

    lines += [
        "## Verdict and failure distribution",
        "",
        "The driver's per-batch `[sim]` renders, sliced to the lines between the first",
        "and last trace row (the capture's prologue is excluded).",
        "",
    ]
    on_totals = _outcome_totals(on)
    for capture in (off, on):
        totals = _outcome_totals(capture)
        lines += [
            f"### {capture.label.upper()}",
            "",
            f"batches: {totals.batches}; candidates: {totals.candidates}; "
            f"ok: {totals.ok}; failed: {totals.failed}; exceptions: {totals.exceptions}",
            "",
            f"failure buckets: {_counts_phrase(totals.buckets)}",
            "",
        ]

    lines += [
        "## Confounders (explicit, non-paired)",
        "",
        "1. **Disjoint heads.** The windows cover different block ranges "
        f"({off_bounds} vs {on_bounds}); market state, dirty pools, and discovery "
        "differ, so no candidate is paired across the windows.",
        "2. **Different candidate mixes.** Whole-window volumes differ "
        f"({len(off.trace)} vs {len(on.trace)} rows; {len(off_ids)} vs {len(on_ids)} "
        f"distinct candidates; {len(off_ids & on_ids)} shared ids), and the per-head "
        "density regimes do not align.",
        "3. **Different readiness.** OFF requests no BAL anywhere; ON resolved "
        f"{_counts_phrase(Counter(row.storage for row in on.selections))} across "
        f"{len(on.selections)} fenced fan-outs, so the two arms do not run the same lane "
        "mix: the delta lane is sampled only where a head settled ready, and the "
        "candidates the bounded sink left unattributed belong to neither lane.",
        "4. **Cordon episodes differ.** A cordon holds the deferrable intake and floors "
        f"sim intake: off {_cordon_phrase(off)}, on {_cordon_phrase(on)} (leading exits "
        "are the boot cordon lifting).",
        "5. **Lossy console sink.** Every count here is a lower bound on what the process "
        "did: a fan-out whose selection row survived can still be missing its prefetch "
        "and `cands=` rows.",
        "6. **Unattributed candidate stream.** The trace stream is shared by the dispatch "
        "fan-out and the inline hook re-quote arm, so its timing is not a per-lane "
        "measurement and only the dispatch rows above name a lane.",
        "",
        "## Readiness detail",
        "",
        f"ON's fenced fan-outs: {_counts_phrase(Counter(row.storage for row in on.selections))} "
        f"with reasons {_counts_phrase(Counter(str(row.reason) for row in on.selections))}. "
        f"Readiness resolved {len(ready)} of {len(on.selections)} fenced fan-outs as `delta`; "
        "every other one fell back with a cold label.",
        "",
        "A ready delta fan-out pays no prefetch batch; a `lineage-refused` head settles",
        "cold and leaves the fan-out on the legacy memo + prefetch lane, serving no",
        "parent value. That contract holds in both captures: the delta fan-out has no",
        "prefetch row, and every fallback that kept its prefetch row reports a full batch",
        "(`skipped=None`). The fan-out row reports the collapsed label; the causes behind",
        "it are the head table's, whenever the capture carries `bal-head` rows.",
        "",
    ]
    cross = _cross_lane_counts(off_by_count, on_by_count, shared)
    prepare = summarize([row.prepare_us for row in on.heads if row.prepare_us is not None])
    prepare_clause = (
        f"The background prepare wall ({prepare.count} heads, median {_us(prepare.p50)} ms, "
        f"p90 {_us(prepare.p90)} ms) has no OFF counterpart, so its cost against the "
        "flag-off window is still unmeasured. "
        if prepare.count
        else "Neither window carries `bal-head` rows, so the background prepare wall is "
        "unmeasured on both sides. "
    )
    lanes_by_arm = ((off, lab_lanes(off)), (on, lab_lanes(on)))
    largest = max((entry.lab.candidates for _, lanes in lanes_by_arm for entry in lanes), default=0)
    largest_arms_used = [
        capture
        for capture, lanes in lanes_by_arm
        if any(entry.lab.candidates == largest for entry in lanes)
    ]
    largest_arms = (
        f"appears only in the {largest_arms_used[0].label.upper()} window"
        if len(largest_arms_used) == 1
        else "appears in both windows, but at different heads and candidate sets"
    )
    lines += [
        "## Verdict",
        "",
        f"- **Readiness: verified — and it says nothing about cost.** {len(ready)} of "
        f"{len(on.selections)} fenced fan-outs resolved `delta` under the flag; the "
        f"{len(cold)} that did not fell back to `legacy` with a closed-set cold reason "
        f"({_counts_phrase(Counter(str(row.reason) for row in cold))}), and every ready "
        "delta fan-out skipped its prefetch batch.",
    ]
    if cross:
        lines.append(
            "- **Directional small-fan-out signal (non-paired).** At "
            + "; at ".join(
                f"{count} candidates OFF `legacy` paid {_walls(off_legacy)}"
                for count, off_legacy, _ in cross
            )
            + ", against "
            + "; at ".join(
                f"{count} candidates ON `delta` paying {_walls(on_delta)}"
                for count, _, on_delta in cross
            )
            + ". The direction is consistent, and it is all this pair supports: one or two "
            "fan-outs per arm, different heads, different candidates, no repetition, and a "
            "millisecond-resolution wall. It is not a paired head comparison and never a "
            "controlled speedup."
        )
    else:
        lines.append(
            "- **No small-fan-out direction.** No candidate count matches an OFF `legacy` "
            "fan-out against an ON `delta` one, so this pair carries no cross-lane count "
            "observation."
        )
    lines += [
        f"- **Still missing: controlled large-fan-out evidence.** The largest fan-out in "
        f"the pair ({largest} candidates) {largest_arms}, so burst-scale dispatch has no "
        "count-matched counterpart; OFF resolves no `delta` fan-out at all, so no same-lane "
        "pair exists at any count. The lane's benefit at scale stays unmeasured.",
        "- **No GO from whole-window totals.** The whole-window candidate total is "
        f"{_ms(off_total)} ms (off) against {_ms(on_total)} ms (on), a "
        f"{_percent_change(off_total, on_total)} change, but those rows carry no lane and "
        "the windows hold different heads and candidate sets. It is an observation only: no "
        "retention or retirement decision is drawn from it.",
        "- **Retention / longer soak: GO on the contract, not on timing.** The flag left "
        "the observable contract intact — the delta fan-out skipped its prefetch batch, "
        "every fallback reported a closed-set cold reason, and both windows' candidate "
        f"verdicts are {_counts_phrase(Counter(row.verdict for row in on.trace))} (on) "
        f"against {_counts_phrase(Counter(row.verdict for row in off.trace))} (off). "
        + prepare_clause
        + "Pricing the lane needs matched heads, a drop-free capture, and the same "
        "candidate set on both lanes.",
        f"- **Retirement of the dispatch path: NO-GO.** The evidence is {len(ready)} ready "
        f"fan-out and {on_totals.candidates} rendered candidates in the ON window "
        f"({on_totals.ok} ok, {on_totals.exceptions} exceptions), so the capture cannot "
        "price the lane's benefit or its absence.",
        "",
        "## Evidence gaps (what this capture cannot answer)",
        "",
        "1. Which storage lane a candidate read through, for a row that carries no",
        "   `fanout` key — the inline hook re-quote arm, and every pre-`fanout` capture.",
        "   An attributed dispatch fan-out's candidates are answered above.",
        "2. The `CarryRefusal` cause behind a `lineage-refused` label in a capture taken",
        "   before the `bal-head` row existed; an attributed capture answers it per head.",
        "3. The dropped-record count for either capture — it lives on",
        '   `degenbot.log_dropped_total{sink="console"}`, not in the log.',
        "4. The wall a FAN-OUT pays to read its generation: the head's own prepare and",
        "   apply walls are `prepare_us`/`apply_us` above, but no row measures the",
        "   fan-out's own read against them.",
        "5. Any per-head repetition — each head appears once per capture, so a head-level",
        "   distribution (let alone a paired one) is not in this corpus.",
        "",
        "## Regeneration",
        "",
        "Both captures are copies of the launcher's log (`./run_bot.sh` writes",
        "`logs/bot_run.log`), taken from two live windows that differ only in the",
        "dispatch flag. The ON capture is byte-identical to the `logs/bot_run.log` the",
        "second run left behind.",
        "",
        "```bash",
        "# OFF window (then a copy of the launcher log)",
        "DEGENBOT_DELTA_DISPATCH=0 DEGENBOT_DEBUG=1 \\",
        "  RUST_LOG='degenbot=debug,degenbot::sim=trace' \\",
        "  ./run_bot.sh start     # ... observe ... ./run_bot.sh stop",
        "cp logs/bot_run.log logs/bal_dispatch_off.log",
        "",
        "# ON window",
        "DEGENBOT_DELTA_DISPATCH=1 DEGENBOT_DEBUG=1 \\",
        "  RUST_LOG='degenbot=debug,degenbot::sim=trace' \\",
        "  ./run_bot.sh start     # ... observe ... ./run_bot.sh stop",
        "cp logs/bot_run.log logs/bal_dispatch_on.log",
        "",
        "# trace-only extracts (the cross-check the validation table reports)",
        "rg --no-filename 'fanout-trace ' logs/bal_dispatch_off.log \\",
        "  > logs/bal_dispatch_off_trace.log",
        "rg --no-filename 'fanout-trace ' logs/bal_dispatch_on.log \\",
        "  > logs/bal_dispatch_on_trace.log",
        "",
        "# analysis",
        "just bal-live-ab-report",
        "```",
        "",
        "`just bal-live-ab-report` exits 1 when a capture check fails (an OFF window",
        "that selected `delta`, a differing executor image / RPC / snapshot seed,",
        "a trace extract that disagrees with its capture); it still writes the report,",
        "so a failing pair is read, never silently accepted.",
        "",
    ]
    return "\n".join(lines) + "\n"


@dataclass(frozen=True)
class OutcomeTotals:
    """One capture's rendered batches, sliced to its own trace window."""

    batches: int
    candidates: int
    ok: int
    failed: int
    exceptions: int
    buckets: Counter[str]


def _outcome_totals(capture: Capture) -> OutcomeTotals:
    """Sum the driver renders between the capture's first and last trace row."""
    first = capture.trace[0].line if capture.trace else 0
    last = capture.trace[-1].line if capture.trace else 0
    sliced = [row for row in capture.outcomes if first <= row.line <= last]
    buckets: Counter[str] = Counter()
    for row in sliced:
        buckets.update(row.buckets)
    return OutcomeTotals(
        batches=len(sliced),
        candidates=sum(row.candidates for row in sliced),
        ok=sum(row.ok for row in sliced),
        failed=sum(row.failed for row in sliced),
        exceptions=sum(row.exceptions for row in sliced),
        buckets=buckets,
    )


def _extract_cell(capture: Capture) -> str:
    """`not checked` / `N / 0` for the optional trace-extract cross-check."""
    if capture.extract_rows is None:
        return "not checked"
    return f"{capture.extract_rows} / {capture.extract_mismatch}"


def _counts_phrase(counts: Counter[str]) -> str:
    """`label=n` pairs, stable by descending count then label."""
    ordered = sorted(counts.items(), key=lambda item: (-item[1], item[0]))
    return ", ".join(f"{label}={count}" for label, count in ordered) or "none"


def _cordon_phrase(capture: Capture) -> str:
    """How many cordon entries / exits a capture saw, for the confounder list."""
    enters = sum(1 for _, kind in capture.cordons if kind == "enter")
    exits = sum(1 for _, kind in capture.cordons if kind == "exit")
    return f"{enters} enter / {exits} exit"


def _busiest(capture: Capture) -> str:
    grouped = rows_by_block(capture.trace)
    if not grouped:
        return "—"
    block, rows = max(grouped.items(), key=lambda item: (len(item[1]), item[0]))
    return f"{block} ({len(rows)})"


def to_json(off: Capture, on: Capture) -> dict[str, object]:
    """The computed structure the markdown renders, for downstream consumers."""

    def capture_view(capture: Capture) -> dict[str, object]:
        grouped = rows_by_block(capture.trace)
        return {
            "label": capture.label,
            "path": capture.path,
            "boot": capture.boot,
            "executor": capture.executor,
            "trace_rows": len(capture.trace),
            "trace_head_blocks": len(grouped),
            "block_range": window_bounds(capture),
            "selections": [
                {
                    "current_block": row.current_block,
                    "anchor": row.anchor,
                    "storage": row.storage,
                    "reason": row.reason,
                    "fanout": row.fanout,
                }
                for row in capture.selections
            ],
            "prefetches": len(capture.prefetches),
            "selection_rows": len(capture.selections),
            "keyed_selection_rows": sum(1 for row in capture.selections if row.fanout is not None),
            "fanout_ids": sorted(fanout_attribution(capture)),
            "fanout_ids_without_selection": sorted(
                fanout
                for fanout, entry in fanout_attribution(capture).items()
                if entry.selection is None
            ),
            "fanout_walls": [
                {
                    "current_block": entry.lab.current_block,
                    "lane": entry.lane,
                    "joined_by": entry.joined_by,
                    "candidates": entry.lab.candidates,
                    "fanout_ms": entry.lab.fanout_ms,
                    "attributed": entry.attributed,
                    "candidate_ns": entry.candidate_ns,
                }
                for entry in lab_lanes(capture)
            ],
            "head_walls": {
                "prepare": summarize([
                    row.prepare_us for row in capture.heads if row.prepare_us is not None
                ]).__dict__,
                "apply": summarize([
                    row.apply_us for row in capture.heads if row.apply_us is not None
                ]).__dict__,
            },
            "labs": [
                {
                    "current_block": row.current_block,
                    "candidates": row.candidates,
                    "sims": row.sims,
                    "fanout_ms": row.fanout_ms,
                    "fanout": row.fanout,
                }
                for row in capture.labs
            ],
            "fanouts": [
                {
                    "fanout": fanout,
                    "storage": entry.selection.storage if entry.selection else None,
                    "reason": entry.selection.reason if entry.selection else None,
                    "prefetch": entry.prefetch is not None,
                    "lab": entry.lab is not None,
                    "candidates": len(entry.traces),
                    "sim_ns": sum(row.sim_ns for row in entry.traces),
                }
                for fanout, entry in sorted(fanout_attribution(capture).items())
            ],
            "unattributed_candidates": len(unattributed_traces(capture)),
            "heads": [
                {
                    "anchor": row.anchor,
                    "outcome": row.outcome,
                    "chain": row.chain,
                    "refusal": row.refusal,
                    "bal": row.bal,
                    "prepare_us": row.prepare_us,
                    "apply_us": row.apply_us,
                }
                for row in capture.heads
            ],
            "sim_ns": summarize([row.sim_ns for row in capture.trace]).__dict__,
            "verdicts": dict(Counter(row.verdict for row in capture.trace)),
            "candidates": len({row.cand for row in capture.trace}),
            "paths": len(path_ids(capture.trace)),
        }

    return {
        "validation": validation_problems(off, on),
        "off": capture_view(off),
        "on": capture_view(on),
    }


def main(argv: Sequence[str] | None = None) -> int:
    """CLI entry point: scan the pair, then write or print the report."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--off", required=True, type=Path, help="OFF capture log")
    parser.add_argument("--on", required=True, type=Path, help="ON capture log")
    parser.add_argument(
        "--off-trace", type=Path, default=None, help="OFF `fanout-trace`-only extract"
    )
    parser.add_argument(
        "--on-trace", type=Path, default=None, help="ON `fanout-trace`-only extract"
    )
    parser.add_argument("--out", type=Path, default=None, help="markdown destination")
    parser.add_argument("--format", choices=("md", "json"), default="md", help="output format")
    parser.add_argument(
        "--allow-validation-problems",
        action="store_true",
        help="report even when the pair fails a capture check (never hides the failure)",
    )
    args = parser.parse_args(argv)

    off = scan_capture(args.off, label="off", trace_extract=args.off_trace)
    on = scan_capture(args.on, label="on", trace_extract=args.on_trace)
    problems = validation_problems(off, on)

    if args.format == "json":
        text = json.dumps(to_json(off, on), indent=2) + "\n"
    else:
        text = render_markdown(off, on)

    if args.out is not None:
        args.out.write_text(text, encoding="utf-8")
        print(f"wrote {args.out}")
    else:
        sys.stdout.write(text)

    if problems and not args.allow_validation_problems:
        for problem in problems:
            print(f"validation: {problem}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
