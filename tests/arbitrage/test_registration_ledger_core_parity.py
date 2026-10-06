"""The registration outcome vocabulary is the CORE's, and the Python
ledger is a thin adapter over it.

`RegistrationOutcome` / `BuildRefusal` / the four memos live in
`degenbot_bot::bot_core::registration_ledger`; the pure-Rust driver and the
Python registration pipeline read the same implementation. These tests pin the
parity from the Python side:

* the closed tag set is exactly the core's, spelled out here as a literal so a
  rename must be a deliberate edit in both places (the Rust pin lives in
  `bot_core::registration_ledger::tests::outcome_tag_set_is_closed_and_unique`);
* the Python label enum is BUILT from the core's list, so it cannot drift;
* the memos and the classification are the core's — the adapter holds no
  Python-side collection or taxonomy, and an unknown input is a loud
  `ValueError` rather than a guessed tag;
* the unit-outcome kinds and the counter fold are the core's too — the
  adapter builds its kind labels from the core's list and folds through
  `PipelineReport::absorb`, never re-implementing the arithmetic (the Rust
  pins live in `registration_ledger::tests::unit_kind_set_is_closed_and_unique`
  and `fold_lands_every_outcome_in_its_counter_bucket`).
"""

from __future__ import annotations

import pytest

from degenbot._ffi import RegistrationLedger as CoreRegistrationLedger
from degenbot._ffi import registration_outcome_tags, registration_unit_kinds
from degenbot.exceptions import (
    DynamicFeePoolRejectedError,
    HighFeePoolRejectedError,
    HookedPoolRejectedError,
)
from degenbot.exceptions.base import DegenbotValueError
from degenbot.pathfinding import PoolKind
from degenbot.runner._registration_ledger import (
    RegistrationLedger,
    RegistrationOutcome,
    RegistrationUnitKind,
)
from degenbot.runner._registration_ledger import fold_registration_unit


class _Step:
    """A minimal discovery-edge hop: the fields ``pool_memo_key`` reads."""

    type = PoolKind.V4
    address = None
    hash = None

#: The closed tag set, pinned. Any change here is a change to the bounded
#: metric vocabulary in the core, not a Python-side edit.
EXPECTED_TAGS = frozenset(
    {
        "registered",
        "dup",
        "path-cap",
        "direction-mismatch",
        "unknown-pool-type",
        "v4-no-hash",
        "v4-hook-rejected",
        "v4-dynamic-fee-rejected",
        "path-rejected-memo",
        "build-v2-refused",
        "build-v3-refused",
        "build-v4-refused",
        "register-fail",
    }
)


def test_the_core_tag_list_is_the_closed_set() -> None:
    assert set(registration_outcome_tags()) == EXPECTED_TAGS


def test_the_python_label_enum_is_built_from_the_core_list() -> None:
    """The enum is minted from the core's tags, not re-declared in Python."""
    assert {member.value for member in RegistrationOutcome} == EXPECTED_TAGS
    assert len(list(RegistrationOutcome)) == len(EXPECTED_TAGS), "no duplicate tags"


#: The closed unit-kind set, pinned for the same reason as the tag set: a
#: rename is a deliberate cross-language edit, never an accident. The Rust
#: pin: `registration_ledger::tests::unit_kind_set_is_closed_and_unique`.
EXPECTED_UNIT_KINDS = frozenset({"skip", "reject", "cap", "register-fail", "registered"})


def test_the_core_unit_kind_list_is_the_closed_set() -> None:
    assert set(registration_unit_kinds()) == EXPECTED_UNIT_KINDS


def test_the_python_unit_kind_enum_is_built_from_the_core_list() -> None:
    """The kind labels are minted from the core's list, not re-declared."""
    assert {member.value for member in RegistrationUnitKind} == EXPECTED_UNIT_KINDS
    assert len(list(RegistrationUnitKind)) == len(EXPECTED_UNIT_KINDS), "no duplicate kinds"


def test_the_ledger_holds_no_python_side_memo_state() -> None:
    """The adapter owns exactly one thing: the core ledger it forwards to."""
    ledger = RegistrationLedger()

    assert isinstance(ledger._core, CoreRegistrationLedger)
    python_state = {
        name
        for name, value in vars(ledger).items()
        if name != "_core" and isinstance(value, (set, dict, list))
    }
    assert python_state == set(), f"the adapter regrew memo state: {python_state}"


def test_memos_round_trip_through_the_core_ledger() -> None:
    ledger = RegistrationLedger()
    hop_sig = ((1, True), (2, False))

    assert ledger.path_registered(hop_sig) is False
    ledger.memoize_registered_path(hop_sig)
    assert ledger.path_registered(hop_sig) is True
    assert ledger.path_registered(((1, False), (2, False))) is False, (
        "orientation is part of the signature"
    )

    assert ledger.pool_verified("v3:0x1") is False
    ledger.memoize_verified_pool("v3:0x1")
    assert ledger.pool_verified("v3:0x1") is True

    assert ledger.path_rejected(hop_sig) is False
    ledger.memoize_rejected_path(hop_sig)
    assert ledger.path_rejected(hop_sig) is True

    ledger.memoize_unregistrable(
        "p:0x1", RegistrationOutcome.BUILD_V3_REFUSED, counts_as_skip=True
    )
    record = ledger.unregistrable_record("p:0x1")
    assert record is not None
    assert record.outcome == RegistrationOutcome.BUILD_V3_REFUSED.value
    assert record.counts_as_skip is True
    assert ledger.unregistrable_record(None) is None


def test_the_first_stable_refusal_wins() -> None:
    ledger = RegistrationLedger()
    ledger.memoize_unregistrable(
        "p:0x1", RegistrationOutcome.BUILD_V3_REFUSED, counts_as_skip=True
    )
    ledger.memoize_unregistrable(
        "p:0x1", RegistrationOutcome.V4_HOOK_REJECTED, counts_as_skip=False
    )

    record = ledger.unregistrable_record("p:0x1")
    assert record is not None
    assert record.outcome == RegistrationOutcome.BUILD_V3_REFUSED.value, (
        "a later answer must not overwrite the pool fact"
    )


@pytest.mark.parametrize(
    ("exc", "pool_kind", "expected_tag", "stable", "counts_as_skip"),
    [
        (
            HookedPoolRejectedError(),
            PoolKind.V4,
            RegistrationOutcome.V4_HOOK_REJECTED,
            True,
            False,
        ),
        (
            DynamicFeePoolRejectedError(),
            PoolKind.V4,
            RegistrationOutcome.V4_DYNAMIC_FEE_REJECTED,
            True,
            False,
        ),
        (
            HighFeePoolRejectedError(),
            PoolKind.V3,
            RegistrationOutcome.BUILD_V3_REFUSED,
            True,
            True,
        ),
        (
            HighFeePoolRejectedError(),
            PoolKind.V2,
            RegistrationOutcome.BUILD_V2_REFUSED,
            True,
            True,
        ),
        (
            RuntimeError("rpc blip"),
            PoolKind.V3,
            RegistrationOutcome.BUILD_V3_REFUSED,
            False,
            True,
        ),
    ],
)
def test_build_refusal_classification_is_the_cores_taxonomy(
    exc: BaseException,
    pool_kind: PoolKind,
    expected_tag: RegistrationOutcome,
    stable: bool,
    counts_as_skip: bool,
) -> None:
    """Python names the TYPED failure; the core decides the taxonomy."""
    refusal = RegistrationLedger.classify_build_refusal(exc, pool_kind=pool_kind)

    assert refusal.outcome == expected_tag.value
    assert refusal.stable is stable
    assert refusal.counts_as_skip is counts_as_skip
    assert refusal.detail is not None and type(exc).__name__ in refusal.detail, (
        "the detail carries the exception text for logs, never the label"
    )


def test_a_class_name_that_impersonates_a_stable_refusal_stays_transient() -> None:
    """Classification is by TYPE, never by name — the impostor stays retriable."""

    class HighFeePoolRejectedErrorImpostor(Exception):
        pass

    impostor = HighFeePoolRejectedErrorImpostor("looks stable")
    impostor.__class__.__name__ = "HighFeePoolRejectedError"
    refusal = RegistrationLedger.classify_build_refusal(impostor, pool_kind=PoolKind.V3)
    assert refusal.stable is False, (
        "a matching class NAME is not the refusal TYPE the core classifies on"
    )


def test_an_unknown_outcome_tag_is_a_loud_refusal() -> None:
    """A tag outside the closed set is a ValueError, never a silent default."""
    ledger = RegistrationLedger()
    with pytest.raises(ValueError, match="unknown registration outcome"):
        ledger.memoize_unregistrable("p:0x1", "not-a-real-outcome", counts_as_skip=True)


def test_an_unrecognized_family_is_not_the_no_identity_answer() -> None:
    """The two ``None``-shaped facts are different facts.

    A hop with no identity answers ``None`` (the docstring contract); an
    unrecognized pool family is Rust/Python wire drift and must never join
    that answer — it raises, naming the raw value and the known set.
    """
    step = _Step()

    # The legitimate no-identity answer: a V4 hop carrying no pool id.
    assert RegistrationLedger.pool_memo_key(step, PoolKind.V4) is None

    # The unrecognized family: NOT the same answer.
    with pytest.raises(DegenbotValueError, match=r"SUSHISWAP_V5.*V2, V3, V4"):
        RegistrationLedger.pool_memo_key(step, "SUSHISWAP_V5")


def test_an_unrecognized_family_never_classifies_as_another_family() -> None:
    """``classify_build_refusal`` on an unrecognized family raises naming the
    raw value and the known set — never a silently misclassified tag."""
    with pytest.raises(DegenbotValueError, match=r"SUSHISWAP_V5.*V2, V3, V4"):
        RegistrationLedger.classify_build_refusal(
            RuntimeError("rpc blip"),
            pool_kind="SUSHISWAP_V5",
        )


def test_the_fold_arithmetic_is_the_cores() -> None:
    """The counter fold runs in the core; the delta answers what the core's
    own fold test pins (`fold_lands_every_outcome_in_its_counter_bucket`).

    The Python driver keeps its own counter storage (the cockpit reads the
    pipeline's fields) — what it must NOT do is re-implement WHICH buckets
    move. One fold per outcome shape, asserted against the core's answer:
    """
    skip = fold_registration_unit(
        kind=RegistrationUnitKind.SKIP,
        tag=RegistrationOutcome.V4_HOOK_REJECTED.value,
        counts_as_skip=False,
        created=False,
        v4_hops=0,
        detail=None,
    )
    assert skip.v4_hook_rejected == 1
    assert skip.skip_count == 0, "V4 admission refusals are not skips"
    assert skip.uncounted_skip_count == 1
    assert skip.skip_reasons == [("v4-hook-rejected", 1)]

    reject = fold_registration_unit(
        kind=RegistrationUnitKind.REJECT,
        tag=None,
        counts_as_skip=True,
        created=False,
        v4_hops=0,
        detail=None,
    )
    assert reject.engine_reject_count == 1
    assert reject.other_exc_count == 1
    assert reject.skip_reasons == [], "a rejection is not a skip"

    registered = fold_registration_unit(
        kind=RegistrationUnitKind.REGISTERED,
        tag=None,
        counts_as_skip=True,
        created=True,
        v4_hops=2,
        detail=None,
    )
    assert registered.path_count == 1
    assert registered.v4_pool_count == 2
    assert registered.units_folded == 1

    duplicate = fold_registration_unit(
        kind=RegistrationUnitKind.REGISTERED,
        tag=None,
        counts_as_skip=True,
        created=False,
        v4_hops=3,
        detail=None,
    )
    assert duplicate.dup_count == 1
    assert duplicate.v4_pool_count == 3, "the witness counts duplicates too"
    assert duplicate.skip_reasons == [("dup", 1)]

    cap = fold_registration_unit(
        kind=RegistrationUnitKind.CAP,
        tag=RegistrationOutcome.PATH_CAP.value,
        counts_as_skip=True,
        created=False,
        v4_hops=0,
        detail=None,
    )
    assert cap.capped is True
    assert cap.cap_skip_count == 1
    assert cap.skip_count == 1

    fail = fold_registration_unit(
        kind=RegistrationUnitKind.REGISTER_FAIL,
        tag=RegistrationOutcome.REGISTER_FAIL.value,
        counts_as_skip=True,
        created=False,
        v4_hops=0,
        detail="boom",
    )
    assert fail.register_fail_count == 1
    assert fail.skip_reasons == [("register-fail", 1)]


def test_a_free_form_skip_label_is_recorded_verbatim() -> None:
    """A driver's exception-derived tag rides the fold verbatim; no family
    counter responds to it."""
    delta = fold_registration_unit(
        kind=RegistrationUnitKind.SKIP,
        tag="build-v3:ConnectionError",
        counts_as_skip=True,
        created=False,
        v4_hops=0,
        detail=None,
    )
    assert delta.skip_reasons == [("build-v3:ConnectionError", 1)]
    assert delta.v4_hook_rejected == 0


def test_an_unknown_unit_kind_is_a_loud_refusal() -> None:
    """A kind outside the closed set is a ValueError, never a guessed fold."""
    with pytest.raises(ValueError, match="unknown registration unit kind"):
        fold_registration_unit(
            kind="dedup",
            tag=None,
            counts_as_skip=True,
            created=False,
            v4_hops=0,
            detail=None,
        )
