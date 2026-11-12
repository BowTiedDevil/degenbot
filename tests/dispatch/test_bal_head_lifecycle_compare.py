"""The BAL head-lifecycle summary comparator (`scripts/bal_head_lifecycle_compare.py`).

The comparator is the executable form of the lifecycle's regression rule: three
before summaries, three after summaries, the median of each metric per side, and
a fail above 10% growth. Every pin here states one clause of that contract:

* the rule reads MEDIANS (an outlier on one side of a trio must not move the
  verdict), and the 10% bound is inclusive — exactly 10% passes, anything beyond
  it fails;
* the comparison FAILS CLOSED (exit 2) on every way a summary can be unreadable
  — missing file, unparsable JSON, a foreign schema identity, a missing metric,
  a non-integer or negative value, a tail below its own median, a leg that
  measured nothing, a zero before median, and a trio of the wrong size.

`EMITTED_SUMMARY` is the probe's own emitted shape, copied from a run of
`just bal-head-lifecycle-probe` with `BAL_HEAD_SUMMARY_OUT` set: the fields and
their nesting are the wire contract, so the expected parse below is the format's
truth rather than a restatement of the implementation.
"""

from __future__ import annotations

import json
from importlib.util import module_from_spec, spec_from_file_location
from pathlib import Path
from typing import TYPE_CHECKING

import pytest

if TYPE_CHECKING:
    from types import ModuleType

SCRIPT_PATH = Path(__file__).resolve().parents[2] / "scripts" / "bal_head_lifecycle_compare.py"

#: The probe's emitted summary, verbatim in shape (one real run's numbers).
EMITTED_SUMMARY = """{
  "historical": {
    "apply_p50_ns": 519000,
    "apply_p95_ns": 1420000,
    "heads": 21
  },
  "invocation": {
    "first": 26375033,
    "fresh": 0,
    "last": 26375053,
    "wait_samples": 21
  },
  "schema": "bal-head-lifecycle/1",
  "waiter": {
    "resolve_p50_ns": 48000,
    "resolve_p95_ns": 61000,
    "samples": 21
  }
}
"""


@pytest.fixture(scope="module")
def comparator() -> ModuleType:
    """The comparator script, loaded by path (it is not an importable package)."""
    spec = spec_from_file_location("bal_head_lifecycle_compare", SCRIPT_PATH)
    if spec is None or spec.loader is None:
        message = f"cannot load comparator script: {SCRIPT_PATH}"
        raise RuntimeError(message)
    module = module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def summary(
    *,
    apply_p50_ns: int = 1_000,
    resolve_p50_ns: int = 1_000,
    heads: int = 21,
    samples: int = 21,
) -> dict:
    """One valid summary; each p95 is twice its p50 so the pair is one series."""
    return {
        "schema": "bal-head-lifecycle/1",
        "invocation": {"first": 1, "last": 21, "fresh": 0, "wait_samples": samples},
        "historical": {
            "heads": heads,
            "apply_p50_ns": apply_p50_ns,
            "apply_p95_ns": apply_p50_ns * 2,
        },
        "waiter": {
            "samples": samples,
            "resolve_p50_ns": resolve_p50_ns,
            "resolve_p95_ns": resolve_p50_ns * 2,
        },
    }


def write(directory: Path, name: str, document: object) -> str:
    """Write `document` as one summary file and answer its path."""
    path = directory / name
    path.write_text(json.dumps(document), encoding="utf-8")
    return str(path)


def trio(directory: Path, side: str, documents: list[dict]) -> list[str]:
    """Three summaries of one side, written side by side."""
    return [write(directory, f"{side}-{index}.json", doc) for index, doc in enumerate(documents)]


def run(comparator: ModuleType, before: list[str], after: list[str]) -> int:
    """The comparator's exit code for one before/after pair of trios."""
    return comparator.main(["--before", *before, "--after", *after])


def test_three_before_and_three_after_within_bound_pass(
    comparator: ModuleType, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    before = trio(tmp_path, "before", [summary() for _ in range(3)])
    after = trio(tmp_path, "after", [summary(apply_p50_ns=1_050) for _ in range(3)])

    assert run(comparator, before, after) == 0

    printed = capsys.readouterr().out
    assert "historical apply p50" in printed
    assert "before median" in printed
    assert "after median" in printed
    assert "PASS" in printed


def test_exactly_ten_percent_growth_passes(comparator: ModuleType, tmp_path: Path) -> None:
    """The bound is inclusive: 1000 ns -> 1100 ns is exactly 10%."""
    before = trio(tmp_path, "before", [summary(apply_p50_ns=1_000) for _ in range(3)])
    after = trio(tmp_path, "after", [summary(apply_p50_ns=1_100) for _ in range(3)])

    assert run(comparator, before, after) == 0


def test_growth_beyond_ten_percent_on_one_metric_fails(
    comparator: ModuleType, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    before = trio(tmp_path, "before", [summary(resolve_p50_ns=1_000) for _ in range(3)])
    after = trio(
        tmp_path,
        "after",
        [
            summary(resolve_p50_ns=1_000),
            summary(resolve_p50_ns=1_101),
            summary(resolve_p50_ns=1_101),
        ],
    )

    assert run(comparator, before, after) == 1

    printed = capsys.readouterr()
    assert "REGRESSION" in printed.out
    assert "waiter resolve p50" in printed.err


def test_the_rule_reads_medians_not_means(comparator: ModuleType, tmp_path: Path) -> None:
    """One slow before run must not become the side's comparison number."""
    before = trio(
        tmp_path,
        "before",
        [summary(apply_p50_ns=1_000), summary(apply_p50_ns=1_000), summary(apply_p50_ns=9_000)],
    )
    after = trio(tmp_path, "after", [summary(apply_p50_ns=1_100) for _ in range(3)])

    # The median is 1000 ns on both sides of the median, so 1100 ns is +10%; the
    # mean of the before trio (3666 ns) would have called the same run faster.
    assert run(comparator, before, after) == 0


def test_a_tail_below_its_own_median_fails_closed(
    comparator: ModuleType, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    before = trio(tmp_path, "before", [summary() for _ in range(3)])
    broken = summary()
    broken["historical"]["apply_p95_ns"] = 10
    after = trio(tmp_path, "after", [broken, summary(), summary()])

    assert run(comparator, before, after) == 2
    assert "below" in capsys.readouterr().err


def test_a_missing_summary_file_fails_closed(
    comparator: ModuleType, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    before = trio(tmp_path, "before", [summary() for _ in range(3)])
    after = trio(tmp_path, "after", [summary() for _ in range(3)])
    after[1] = str(tmp_path / "never-written.json")

    assert run(comparator, before, after) == 2
    assert "cannot read summary" in capsys.readouterr().err


def test_an_unparsable_summary_fails_closed(
    comparator: ModuleType, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    before = trio(tmp_path, "before", [summary() for _ in range(3)])
    after = trio(tmp_path, "after", [summary() for _ in range(3)])
    Path(after[0]).write_text("{ not json", encoding="utf-8")

    assert run(comparator, before, after) == 2
    assert "not JSON" in capsys.readouterr().err


def test_an_off_contract_schema_fails_closed(
    comparator: ModuleType, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    before = trio(tmp_path, "before", [summary() for _ in range(3)])
    foreign = summary()
    foreign["schema"] = "bal-head-lifecycle/2"
    after = trio(tmp_path, "after", [foreign, summary(), summary()])

    assert run(comparator, before, after) == 2
    assert "schema" in capsys.readouterr().err


def test_a_missing_metric_field_fails_closed(
    comparator: ModuleType, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    before = trio(tmp_path, "before", [summary() for _ in range(3)])
    incomplete = summary()
    del incomplete["waiter"]["resolve_p95_ns"]
    after = trio(tmp_path, "after", [summary(), incomplete, summary()])

    assert run(comparator, before, after) == 2
    assert "resolve_p95_ns" in capsys.readouterr().err


@pytest.mark.parametrize("offending", [-1, "1200", 12.5, True])
def test_a_non_integer_or_negative_metric_fails_closed(
    comparator: ModuleType, tmp_path: Path, offending: object
) -> None:
    before = trio(tmp_path, "before", [summary() for _ in range(3)])
    invalid = summary()
    invalid["historical"]["apply_p50_ns"] = offending
    after = trio(tmp_path, "after", [summary(), invalid, summary()])

    assert run(comparator, before, after) == 2


@pytest.mark.parametrize("count", [2, 4])
def test_a_trio_of_the_wrong_size_fails_closed(
    comparator: ModuleType, tmp_path: Path, count: int, capsys: pytest.CaptureFixture[str]
) -> None:
    before = trio(tmp_path, "before", [summary() for _ in range(3)])
    after = trio(tmp_path, "after", [summary() for _ in range(count)])

    assert run(comparator, before, after) == 2
    assert "exactly 3 samples" in capsys.readouterr().err


@pytest.mark.parametrize(("leg", "count_field"), [("historical", "heads"), ("waiter", "samples")])
def test_a_leg_that_measured_nothing_fails_closed(
    comparator: ModuleType, tmp_path: Path, leg: str, count_field: str
) -> None:
    """A zero-sample leg reports placeholder percentiles, never fast numbers."""
    before = trio(tmp_path, "before", [summary() for _ in range(3)])
    measured_nothing = summary()
    measured_nothing[leg][count_field] = 0
    measured_nothing[leg]["apply_p50_ns" if leg == "historical" else "resolve_p50_ns"] = 0
    measured_nothing[leg]["apply_p95_ns" if leg == "historical" else "resolve_p95_ns"] = 0
    after = trio(tmp_path, "after", [summary(), measured_nothing, summary()])

    assert run(comparator, before, after) == 2


def test_a_zero_before_median_fails_closed(
    comparator: ModuleType, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    """Growth against 0 ns is undefined, so it is refused, not rounded to a pass."""
    before = trio(tmp_path, "before", [summary(apply_p50_ns=0) for _ in range(3)])
    after = trio(tmp_path, "after", [summary(apply_p50_ns=5) for _ in range(3)])

    assert run(comparator, before, after) == 2
    assert "undefined" in capsys.readouterr().err


def test_an_additive_field_is_ignored(comparator: ModuleType, tmp_path: Path) -> None:
    """Adding a key is additive: an older reader must not refuse a newer summary."""
    before = trio(tmp_path, "before", [summary() for _ in range(3)])
    extended = [summary() for _ in range(3)]
    for document in extended:
        document["prepare_p50_ns"] = 123
        document["invocation"]["planted"] = False
    after = trio(tmp_path, "after", extended)

    assert run(comparator, before, after) == 0


def test_the_emitted_summary_shape_parses(comparator: ModuleType, tmp_path: Path) -> None:
    """The probe's own emitted fields are the contract the comparator reads."""
    path = tmp_path / "emitted.json"
    path.write_text(EMITTED_SUMMARY, encoding="utf-8")

    values = comparator.metric_values(path, comparator.load_summary(path))

    assert values == {
        "historical apply p50": 519_000,
        "historical apply p95": 1_420_000,
        "waiter resolve p50": 48_000,
        "waiter resolve p95": 61_000,
    }
