"""Cross-driver parity for the crawl unit's outcome table.

The Rust crawl unit (`degenbot_bot::arb_engine::crawl::unit`) is the Python
crawl's `_registration_unit` as ONE engine call. Both drivers answer the same
contract, and the contract is a FILE: `unit/parity_table.json` beside the Rust
unit. Every row names one scenario's unit kind, the breakdown tag its outcome
records (``None`` = the outcome records no breakdown entry), and the stage that
answered it (`build_paths.UNIT_STAGES`).

The Rust unit asserts those rows in `crawl/unit/tests.rs`; this module drives
the Python unit over a fixture of the same shape and asserts the SAME rows. A
row that only one driver can satisfy is a parity bug, not a fixture detail.

Scope: the rows are the unit's TERMINAL answers. The loud-abort contract
(`VerificationMismatchError` / `VerificationRpcError` /
`DirectionResolutionError`) and the identity-divergence row are exercised by
the Rust unit's own tests: the first three propagate unchanged out of the Python
unit (a receipt double re-raises them, which is a driver-level test), and the
identity divergence is the one DELIBERATE divergence — the Rust unit aborts the
crawl on a family-level identity mismatch (ADR-055 D4) where this driver's
`except Exception` net classifies it transiently.
"""

from __future__ import annotations

import json
from pathlib import Path

from degenbot.exceptions import (
    DynamicFeePoolRejectedError,
    PathRegistryFullError,
    PathRejectedError,
)
from degenbot.pathfinding import PoolKind
from degenbot.runner._registration_ledger import RegistrationOutcome
from degenbot.runner.build_paths import UNIT_STAGES
from tests.arbitrage.test_registration_pipeline import (
    _closed_v3_cycle_steps,
    _ConstrBot,
    _OpaqueStep,
    _PathPredicate,
    _pipeline_over_registry,
    _pipeline_with_bot,
    _RecordingRegistry,
)

#: The table both drivers assert — the Rust unit's own fixture file.
TABLE_PATH = (
    Path(__file__).parents[2]
    / "rust/crates/engine/degenbot-bot/src/arb_engine/crawl/unit/parity_table.json"
)


def _rows() -> dict[str, dict]:
    """The parity table, keyed by case name."""
    table = json.loads(TABLE_PATH.read_text(encoding="utf-8"))
    return {row["case"]: row for row in table["rows"]}


def _breakdown_tag(outcome: object) -> str | None:
    """The tag the summary breakdown records for one unit outcome.

    Mirrors the core fold (`PipelineReport::absorb`) and the Rust receipt's
    `refusal_tag`: a skip records its label, the cap stop records `path-cap`, a
    register failure records `register-fail`, and a rejection or a completed
    registration records no entry of its own (a duplicate enters the breakdown
    through the fold, not through the outcome's tag).
    """
    kind = outcome.kind  # type: ignore[attr-defined]
    if kind == "skip":
        return outcome.tag  # type: ignore[attr-defined]
    if kind == "cap":
        return RegistrationOutcome.PATH_CAP.value
    if kind == "register-fail":
        return RegistrationOutcome.REGISTER_FAIL.value
    return None


def _assert_parity(case: str, outcome: object) -> None:
    """Assert one Python unit outcome against its table row."""
    row = _rows()[case]
    assert outcome.kind == row["kind"], f"{case}: unit kind"  # type: ignore[attr-defined]
    assert _breakdown_tag(outcome) == row["tag"], f"{case}: breakdown tag"
    stages_run = len(outcome.stage_ns)  # type: ignore[attr-defined]
    assert stages_run >= 1, f"{case}: a unit always runs its first stage"
    assert UNIT_STAGES[stages_run - 1] == row["stage"], f"{case}: answering stage"


EXERCISED = frozenset({
    "registered_cycle",
    "duplicate_cycle",
    "memoized_build_refusal",
    "v4_admission_refusal",
    "v4_admission_refusal_memoized",
    "v4_hop_without_pool_id",
    "pinned_direction_mismatch",
    "policy_denied",
    "policy_denied_memoized",
    "cap_stop",
    "register_fail",
})


def test_every_parity_row_is_exercised() -> None:
    """The table and this driver must name the same cases."""
    assert set(_rows()) == EXERCISED


def test_registered_cycle_then_duplicate_cycle(tmp_path: Path) -> None:
    """A fresh cycle registers; the re-sighting is the memoized dup fast-path."""
    registry = _RecordingRegistry()
    pipeline = _pipeline_over_registry(tmp_path / "unused.db", registry)
    steps = _closed_v3_cycle_steps()

    first = pipeline._registration_unit(steps)
    _assert_parity("registered_cycle", first)
    assert first.created is True
    assert registry.registrations == [[(101, True), (202, True)]]

    second = pipeline._registration_unit(steps)
    _assert_parity("duplicate_cycle", second)
    assert second.created is False
    assert len(registry.registrations) == 1, "the dup never reached the engine"


def test_a_memoized_build_refusal_answers_before_any_build(tmp_path: Path) -> None:
    """A pool fact answers the path before the build stage runs."""
    registry = _RecordingRegistry()
    pipeline = _pipeline_over_registry(tmp_path / "unused.db", registry)
    steps = _closed_v3_cycle_steps()
    key = pipeline._ledger.pool_memo_key(steps[0], PoolKind.V3)
    pipeline._ledger.memoize_unregistrable(
        key, RegistrationOutcome.BUILD_V3_REFUSED, counts_as_skip=True
    )

    outcome = pipeline._registration_unit(steps)
    _assert_parity("memoized_build_refusal", outcome)
    assert outcome.tag == RegistrationOutcome.BUILD_V3_REFUSED.value
    assert len(outcome.stage_ns) == 1, "the memo stage answered"


def test_a_v4_admission_refusal_is_a_stable_uncounted_skip_and_memoizes(
    tmp_path: Path,
) -> None:
    """A dynamic-fee V4 pool is a stable pool fact with its own counter class."""
    build_calls: list[object] = []

    def _build_managed_pool(address: object, request: object) -> None:
        build_calls.append(getattr(request, "pool_id", None))
        raise DynamicFeePoolRejectedError

    bot = _ConstrBot(build_managed_pool=_build_managed_pool, build_pool=lambda *a, **k: None)
    pipeline, _ = _pipeline_with_bot(tmp_path / "unused.db", bot)
    steps = [
        _OpaqueStep(type=PoolKind.V4, address=None, hash=bytes([0xDE]) * 32),
        _OpaqueStep(type=PoolKind.V4, address=None, hash=bytes([0xBE]) * 32),
    ]

    first = pipeline._registration_unit(steps)
    _assert_parity("v4_admission_refusal", first)
    assert first.tag == RegistrationOutcome.V4_DYNAMIC_FEE_REJECTED.value
    assert first.counts_as_skip is False

    second = pipeline._registration_unit(steps)
    _assert_parity("v4_admission_refusal_memoized", second)
    assert len(second.stage_ns) == 1, "the memo stage answered the re-sighting"
    assert build_calls == [bytes([0xDE]) * 32], "the refusal is a pool fact"


def test_a_v4_hop_without_a_pool_id_is_a_bounded_skip(tmp_path: Path) -> None:
    """A malformed V4 edge (no hash) cannot be built at all."""
    pipeline, _ = _pipeline_with_bot(
        tmp_path / "unused.db",
        _ConstrBot(build_managed_pool=lambda *a, **k: None, build_pool=lambda *a, **k: None),
    )
    steps = [_OpaqueStep(type=PoolKind.V4, address=None, hash=None)]
    outcome = pipeline._registration_unit(steps)
    _assert_parity("v4_hop_without_pool_id", outcome)
    assert outcome.tag == RegistrationOutcome.V4_NO_HASH.value


def test_a_pinned_direction_mismatch_skips_before_verify(tmp_path: Path) -> None:
    """A pin whose hop count disagrees with the path is a skip, not a build."""
    registry = _RecordingRegistry()
    pipeline = _pipeline_over_registry(tmp_path / "unused.db", registry)
    outcome = pipeline._registration_unit(_closed_v3_cycle_steps(), [True])
    _assert_parity("pinned_direction_mismatch", outcome)
    assert outcome.tag == RegistrationOutcome.DIRECTION_MISMATCH.value
    assert registry.verifies == [], "the gate answered before verify"
    assert registry.registrations == []


def test_a_policy_denial_is_a_counted_reject_and_memoizes_the_path(
    tmp_path: Path,
) -> None:
    """The policy deny precedes verify and is deterministic per hop signature."""

    def _refuse(_pools_and_zfos: object) -> None:
        raise PathRejectedError

    registry = _RecordingRegistry()
    registry.path_predicate = _PathPredicate(evaluate=_refuse)
    pipeline = _pipeline_over_registry(tmp_path / "unused.db", registry)
    steps = _closed_v3_cycle_steps()

    first = pipeline._registration_unit(steps)
    _assert_parity("policy_denied", first)
    assert first.detail is not None, "the refusal text rides the log detail"
    assert registry.verifies == []
    assert registry.registrations == []

    second = pipeline._registration_unit(steps)
    _assert_parity("policy_denied_memoized", second)
    assert second.detail is None, "the memo holds the fact, not the refusal text"
    assert registry.verifies == []


def test_the_cap_stop_answers_at_the_register_stage(tmp_path: Path) -> None:
    """The benign registered-path cap refusal is the `cap` outcome."""

    class _CappedRegistry(_RecordingRegistry):
        def register_crawl_path(self, engine_hops: list) -> tuple[int, bool]:
            raise PathRegistryFullError

    registry = _CappedRegistry()
    pipeline = _pipeline_over_registry(tmp_path / "unused.db", registry)
    outcome = pipeline._registration_unit(_closed_v3_cycle_steps())
    _assert_parity("cap_stop", outcome)
    assert outcome.tag == RegistrationOutcome.PATH_CAP.value


def test_an_unroutable_hop_list_is_a_transient_register_failure(tmp_path: Path) -> None:
    """A structural register refusal is a transient failure, never memoized."""

    class _RefusingRegistry(_RecordingRegistry):
        def register_crawl_path(self, engine_hops: list) -> tuple[int, bool]:
            msg = "register_path: path has 1 hops (need >= 2)"
            raise ValueError(msg)

    registry = _RefusingRegistry()
    pipeline = _pipeline_over_registry(tmp_path / "unused.db", registry)
    outcome = pipeline._registration_unit(_closed_v3_cycle_steps())
    _assert_parity("register_fail", outcome)
    assert outcome.tag == RegistrationOutcome.REGISTER_FAIL.value
    # A transient failure is not a pool fact: the ledger never memoized it.
    core_gate = pipeline._ledger.path_rejected
    assert core_gate(((101, True), (202, True))) is False
