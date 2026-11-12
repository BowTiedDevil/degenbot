"""The BAL head-lifecycle summary comparator.

One probe run reports its own numbers and asserts none of them
(`just bal-head-lifecycle-probe`). This comparator is the executable form of the
lifecycle's REGRESSION RULE: exactly three `before` runs and three `after` runs,
the median of each metric on each side, and a fail above 10% growth.

# The summary contract

A summary is one JSON object the probe writes when `BAL_HEAD_SUMMARY_OUT` names
a path. Exactly these fields are consumed, all integer nanoseconds:

* `schema` — `"bal-head-lifecycle/1"`;
* `historical.heads`, `historical.apply_p50_ns`, `historical.apply_p95_ns`;
* `waiter.samples`, `waiter.resolve_p50_ns`, `waiter.resolve_p95_ns`.

Extra keys are ignored (adding a field is additive); a missing key, a wrong
schema identity, a non-integer, a negative value, a `p95` below its own `p50`, or
a leg that measured nothing is MALFORMED. Exit `2` marks every such refusal: the
rule cannot certify a comparison it could not read, so it fails closed instead of
treating an unreadable sample as fast.

# Why medians, and why three

A single session's p50/p95 move with the node's list cache, the machine's load,
and the process's own warmup, so no single absolute number is a budget. Three
runs per side and the median of the three per metric is the smallest sample that
survives one outlier on either side without inventing a distribution. The rule
compares only the two stages the lifecycle owns — the historical leg's apply and
the waiter leg's resolve — and never compares a whole-run wall clock.

Absolute timings stay local artifacts: nothing here is committed as a baseline,
and the rule reads whatever summaries the caller points it at.

# The comparison

For each metric, `after median <= before median * 1.10` passes; growth strictly
beyond 10% fails (`exit 1`). The boundary is inclusive by construction: the
integer test is `after * 100 <= before * 110`, so exactly 10% passes and 10.0001%
does not. A zero before median makes the ratio undefined, so it is a malformed
comparison (`exit 2`), not a pass.

# Usage

    just bal-head-lifecycle-compare --before b1.json b2.json b3.json \
                                    --after a1.json a2.json a3.json
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from collections.abc import Sequence

#: The one schema identity this comparator reads.
SCHEMA = "bal-head-lifecycle/1"

#: The samples the rule is stated over, per side.
SAMPLES_PER_SIDE = 3

#: The regression bound, in percent: growth beyond this fails the comparison.
MAX_REGRESSION_PERCENT = 10

#: Exit codes: a certified comparison, a regression, and a refused comparison.
EXIT_PASS = 0
EXIT_REGRESSION = 1
EXIT_MALFORMED = 2


class MalformedSummaryError(Exception):
    """A summary the rule cannot read: missing, unparsable, or off-contract."""


#: One compared metric: the label the table prints, the leg, and its two fields.
METRICS: tuple[tuple[str, str, str, str], ...] = (
    ("historical apply p50", "historical", "apply_p50_ns", "heads"),
    ("historical apply p95", "historical", "apply_p95_ns", "heads"),
    ("waiter resolve p50", "waiter", "resolve_p50_ns", "samples"),
    ("waiter resolve p95", "waiter", "resolve_p95_ns", "samples"),
)


def _require_int(value: object, where: str) -> int:
    """`value` as a non-negative integer, or a malformed-summary refusal."""
    if isinstance(value, bool) or not isinstance(value, int):
        message = f"{where}: expected an integer nanosecond count, got {value!r}"
        raise MalformedSummaryError(message)
    if value < 0:
        message = f"{where}: expected a non-negative count, got {value}"
        raise MalformedSummaryError(message)
    return value


def _leg(summary: dict[str, object], name: str, path: Path) -> dict[str, object]:
    """One leg object of `summary`, present and an object."""
    leg = summary.get(name)
    if not isinstance(leg, dict):
        message = f"{path}: missing or non-object `{name}` leg"
        raise MalformedSummaryError(message)
    return leg


def load_summary(path: Path) -> dict[str, object]:
    """Read one summary document, refusing anything off the contract."""
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as error:
        message = f"{path}: cannot read summary: {error}"
        raise MalformedSummaryError(message) from error
    try:
        document = json.loads(text)
    except json.JSONDecodeError as error:
        message = f"{path}: not JSON: {error}"
        raise MalformedSummaryError(message) from error
    if not isinstance(document, dict):
        message = f"{path}: expected a JSON object, got {type(document).__name__}"
        raise MalformedSummaryError(message)
    if document.get("schema") != SCHEMA:
        message = f"{path}: schema {document.get('schema')!r} is not the contract's {SCHEMA!r}"
        raise MalformedSummaryError(message)
    return document


def metric_values(path: Path, summary: dict[str, object]) -> dict[str, int]:
    """Every compared metric of one summary, validated together with its leg.

    A leg whose sample count is zero measured nothing: its percentiles are
    placeholders, not fast numbers, so the summary is malformed.
    """
    values: dict[str, int] = {}
    counted: set[str] = set()
    for label, leg_name, field, count_field in METRICS:
        leg = _leg(summary, leg_name, path)
        if leg_name not in counted:
            count = _require_int(leg.get(count_field), f"{path}: {leg_name}.{count_field}")
            if count < 1:
                message = (
                    f"{path}: {leg_name} leg measured nothing "
                    f"({count_field}={count}); its percentiles are not numbers"
                )
                raise MalformedSummaryError(message)
            counted.add(leg_name)
        values[label] = _require_int(leg.get(field), f"{path}: {leg_name}.{field}")
    # p95 is the tail of the same series p50 heads: a summary whose tail is below
    # its own median did not come from one measurement.
    for tail, head in (
        ("historical apply p95", "historical apply p50"),
        ("waiter resolve p95", "waiter resolve p50"),
    ):
        if values[tail] < values[head]:
            message = (
                f"{path}: {tail} ({values[tail]} ns) is below {head} "
                f"({values[head]} ns); the sample is not one series"
            )
            raise MalformedSummaryError(message)
    return values


def load_side(paths: Sequence[Path], side: str) -> dict[str, list[int]]:
    """The per-metric series of one side, exactly `SAMPLES_PER_SIDE` samples."""
    if len(paths) != SAMPLES_PER_SIDE:
        message = (
            f"{side}: the rule is stated over exactly {SAMPLES_PER_SIDE} samples, got {len(paths)}"
        )
        raise MalformedSummaryError(message)
    series: dict[str, list[int]] = {label: [] for label, *_ in METRICS}
    for path in paths:
        values = metric_values(path, load_summary(path))
        for label, value in values.items():
            series[label].append(value)
    return series


def median(series: Sequence[int]) -> int:
    """The median of an odd-length integer series, kept integral."""
    if not series:
        message = "median of an empty series"
        raise ValueError(message)
    ordered = sorted(series)
    return ordered[len(ordered) // 2]


def growth_percent(before: int, after: int) -> float:
    """How far `after` grew over `before`, in percent (negative is an improvement)."""
    return 100.0 * (after - before) / before


def compare(
    before: dict[str, list[int]],
    after: dict[str, list[int]],
) -> list[tuple[str, int, int, float, bool]]:
    """One row per metric: its medians, the growth, and whether it is within bound."""
    rows = []
    for label, *_ in METRICS:
        before_median = median(before[label])
        after_median = median(after[label])
        if before_median == 0:
            message = (
                f"{label}: the before median is 0 ns, so growth is undefined; "
                "reseat the run instead of reading a ratio against nothing"
            )
            raise MalformedSummaryError(message)
        # Integer arithmetic: exactly MAX_REGRESSION_PERCENT passes, anything
        # beyond it fails, and no float rounding can move the boundary.
        within = after_median * 100 <= before_median * (100 + MAX_REGRESSION_PERCENT)
        rows.append(
            (
                label,
                before_median,
                after_median,
                growth_percent(before_median, after_median),
                within,
            ),
        )
    return rows


def _micro(nanos: int) -> str:
    """Nanoseconds as a microsecond figure with the probe report's precision."""
    return f"{nanos / 1_000:.3f}"


def format_table(rows: Sequence[tuple[str, int, int, float, bool]]) -> str:
    """The review table: each metric's medians, growth, and verdict."""
    lines = [
        f"{'metric':<22}{'before median':>15}{'after median':>15}"
        f"{'growth':>10}{'bound':>7}  verdict",
        f"{'(microseconds)':<22}{'':>15}{'':>15}{'(+/- %)':>10}{'':>7}",
    ]
    for label, before_median, after_median, growth, within in rows:
        lines.append(
            f"{label:<22}{_micro(before_median):>15}{_micro(after_median):>15}"
            f"{growth:>+9.2f}%{MAX_REGRESSION_PERCENT:>6}%  "
            f"{'ok' if within else 'REGRESSION'}",
        )
    return "\n".join(line.rstrip() for line in lines)


def _parse_args(argv: Sequence[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        prog="bal_head_lifecycle_compare",
        description=(
            "Compare three before and three after bal-head-lifecycle summaries by "
            "median, failing above a 10% regression."
        ),
    )
    parser.add_argument(
        "--before",
        nargs="+",
        required=True,
        metavar="SUMMARY",
        help=f"exactly {SAMPLES_PER_SIDE} pre-change summary paths",
    )
    parser.add_argument(
        "--after",
        nargs="+",
        required=True,
        metavar="SUMMARY",
        help=f"exactly {SAMPLES_PER_SIDE} post-change summary paths",
    )
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    """Run the comparison: print the table, answer an exit code."""
    args = _parse_args(argv)
    try:
        before_paths = [Path(path) for path in args.before]
        after_paths = [Path(path) for path in args.after]
        before = load_side(before_paths, "before")
        after = load_side(after_paths, "after")
        rows = compare(before, after)
    except MalformedSummaryError as error:
        print(f"REFUSED — {error}", file=sys.stderr)
        return EXIT_MALFORMED

    for side, paths, series in (
        ("before", before_paths, before),
        ("after", after_paths, after),
    ):
        print(f"{side} (microseconds):")
        for label, *_ in METRICS:
            samples = ", ".join(_micro(value) for value in series[label])
            print(f"  {label:<22} {samples}")
    print()
    print(format_table(rows))
    print()
    regressions = [row for row in rows if not row[4]]
    if regressions:
        for label, before_median, after_median, growth, _ in regressions:
            print(
                f"REGRESSION — {label}: median {_micro(before_median)} us -> "
                f"{_micro(after_median)} us ({growth:+.2f}%), beyond "
                f"{MAX_REGRESSION_PERCENT}%",
                file=sys.stderr,
            )
        return EXIT_REGRESSION
    print(
        f"PASS — {SAMPLES_PER_SIDE} before + {SAMPLES_PER_SIDE} after summaries, "
        f"every metric's median within {MAX_REGRESSION_PERCENT}%"
    )
    return EXIT_PASS


if __name__ == "__main__":
    raise SystemExit(main())
