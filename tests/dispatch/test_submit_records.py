"""Typed submit records crossing the dispatch_and_submit seam.

The Rust submit leaf returns per-candidate records as dicts with a ``kind``
discriminator (the FFI wire). ``degenbot.dispatch`` owns the single home for
decoding those dicts into typed records (``SubmittedRecord`` /
``SkippedRecord`` with a ``SubmitSkipReason`` member); an unrecognized kind
or reason, or missing payload keys, is wire drift and must raise — never
silently drop a submission event.
"""

from __future__ import annotations

from typing import Any

import pytest

from degenbot.dispatch import (
    SkippedRecord,
    SubmitCandidate,
    SubmitContext,
    SubmitSkipReason,
    SubmittedRecord,
    dispatch_and_submit,
    typed_submit_record,
)
from tests.helpers.seam import CandidateId
from degenbot.exceptions.base import DegenbotValueError


class TestTypedSubmitRecord:
    """The dict → typed-record decoder at the dispatch seam."""

    def test_submitted_record_converts(self) -> None:
        raw = {
            "kind": "submitted",
            "candidate": {"path_id": 7, "deployment_id": 1},
            "tx_hash": "0xabc",
            "nonce": 5,
        }
        record = typed_submit_record(raw)
        assert isinstance(record, SubmittedRecord)
        assert record.candidate == CandidateId(path_id=7, deployment_id=1)
        assert record.path_id == 7
        assert record.tx_hash == "0xabc"
        assert record.nonce == 5

    @pytest.mark.parametrize(
        ("raw_reason", "member", "detail"),
        [
            ("pools_claimed", SubmitSkipReason.POOLS_CLAIMED, None),
            ("dry_run", SubmitSkipReason.DRY_RUN, None),
            ("inject_code", SubmitSkipReason.INJECT_CODE, None),
            ("broadcast_failed", SubmitSkipReason.BROADCAST_FAILED, "rpc down"),
            ("nonce_contended", SubmitSkipReason.NONCE_CONTENDED, None),
        ],
    )
    def test_skipped_reasons_convert(
        self,
        raw_reason: str,
        member: SubmitSkipReason,
        detail: str | None,
    ) -> None:
        raw: dict[str, Any] = {
            "kind": "skipped",
            "candidate": {"path_id": 3, "deployment_id": 1},
            "reason": raw_reason,
        }
        if detail is not None:
            raw["detail"] = detail
        record = typed_submit_record(raw)
        assert isinstance(record, SkippedRecord)
        assert record.candidate == CandidateId(path_id=3, deployment_id=1)
        assert record.reason is member
        assert record.detail == detail

    def test_unknown_kind_raises(self) -> None:
        with pytest.raises(DegenbotValueError, match="Unrecognized submit record"):
            typed_submit_record({
                "kind": "teleported",
                "candidate": {"path_id": 1, "deployment_id": 1},
            })

    def test_unknown_reason_raises(self) -> None:
        raw = {
            "kind": "skipped",
            "candidate": {"path_id": 1, "deployment_id": 1},
            "reason": "gravity",
        }
        with pytest.raises(DegenbotValueError, match="Unrecognized submit record"):
            typed_submit_record(raw)

    @pytest.mark.parametrize(
        "raw",
        [
            {"kind": "submitted"},  # no candidate/tx_hash/nonce
            {"kind": "skipped", "candidate": {"path_id": 1, "deployment_id": 1}},  # no reason
            {"kind": "submitted", "candidate": {"path_id": 1}},  # half an identity
            # The retired wire shape: a bare path id names no deployment, so a
            # record carrying one cannot be joined back to a deployment's lane.
            {"kind": "submitted", "path_id": 1, "tx_hash": "0xh", "nonce": 2},
            {},  # nothing at all
        ],
    )
    def test_missing_keys_raise(self, raw: dict[str, Any]) -> None:
        with pytest.raises(
            DegenbotValueError,
            match="Unrecognized submit record|Candidate identity is missing",
        ):
            typed_submit_record(raw)


class TestDispatchAndSubmitWrapper:
    """The companion wrapper converts the FFI's dict list to typed records."""

    async def test_wrapper_types_the_ffi_dicts(self) -> None:
        raw_records = [
            {
                "kind": "submitted",
                "candidate": {"path_id": 1, "deployment_id": 1},
                "tx_hash": "0xh",
                "nonce": 2,
            },
            {
                "kind": "skipped",
                "candidate": {"path_id": 3, "deployment_id": 1},
                "reason": "dry_run",
            },
        ]

        def fake_py(**_kwargs: object) -> Any:
            async def _inner() -> list[dict[str, Any]]:  # ruff:ignore[unused-async]  (an awaitable, not a coroutine consumer)
                return list(raw_records)

            return _inner()

        records = await dispatch_and_submit(
            candidates=[],
            dispatcher=None,
            provider=None,
            context=SubmitContext(
                signer=None,  # type: ignore[arg-type]
                operator_nonce=0,
                current_block=0,
                dry_run=True,
                inject_code=False,
            ),
            submit_leaf=fake_py,
        )
        assert len(records) == 2
        assert records[0] == SubmittedRecord(
            candidate=CandidateId(path_id=1, deployment_id=1),
            tx_hash="0xh",
            nonce=2,
        )
        assert isinstance(records[1], SkippedRecord)
        assert records[1].candidate == CandidateId(path_id=3, deployment_id=1)
        assert records[1].reason is SubmitSkipReason.DRY_RUN
        assert records[1].detail is None


class TestSubmitCandidateGetters:
    """Read-only getters on ``SubmitCandidate``.

    The ``[dispatch]`` per-path log reads ``candidate_id``/``gross_profit``/
    ``net_profit``/``gas_used``/``priority_fee`` off each survivor and the
    submit orchestration sorts on the same fields, so the pyclass must expose
    them by name. The identity is the composite one: constructing a candidate
    from a bare path id (the retired input) is a TypeError, loud at the seam.
    """

    def test_a_bare_path_id_is_not_a_candidate_identity(self) -> None:
        with pytest.raises(TypeError):
            SubmitCandidate(
                path_id=42,
                gross_profit=1,
                net_profit=1,
                gas_used=1,
                priority_fee=1,
                base_fee_next=1,
                execute_calldata=b"\xde\xad",
                execution_target="0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )

    def test_money_and_gas_getters_round_trip(self) -> None:
        cand = SubmitCandidate(
            candidate_id=CandidateId(path_id=42, deployment_id=1),
            gross_profit=2_000_000_000_000_000_000,
            net_profit=1_500_000_000_000_000_000,
            gas_used=200_000,
            priority_fee=2_000_000_000,
            base_fee_next=1_000_000_000,
            execute_calldata=b"\xde\xad",
            execution_target="0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        assert cand.candidate_id.path_id == 42
        assert cand.candidate_id.deployment_id == 1
        assert cand.gross_profit == 2_000_000_000_000_000_000
        assert cand.net_profit == 1_500_000_000_000_000_000
        assert cand.gas_used == 200_000
        assert cand.priority_fee == 2_000_000_000


class TestSubmitCandidateRetiredKeywords:
    """The retired constructor keyword fails at the boundary, loudly.

    The target is a deployment fact passed as ``execution_target``;
    ``executor_address`` is not a keyword of the constructor, so a caller still
    passing it gets a ``TypeError`` rather than a candidate whose target is
    ignored.
    """

    def test_the_retired_executor_address_keyword_is_a_type_error(self) -> None:
        with pytest.raises(TypeError, match="executor_address"):
            SubmitCandidate(
                candidate_id=CandidateId(path_id=42, deployment_id=1),
                gross_profit=1,
                net_profit=1,
                gas_used=1,
                priority_fee=1,
                base_fee_next=1,
                execute_calldata=b"\xde\xad",
                execution_target="0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                executor_address="0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )

    def test_an_omitted_value_is_the_builtin_zero(self) -> None:
        cand = SubmitCandidate(
            candidate_id=CandidateId(path_id=42, deployment_id=1),
            gross_profit=1,
            net_profit=1,
            gas_used=1,
            priority_fee=1,
            base_fee_next=1,
            execute_calldata=b"\xde\xad",
            execution_target="0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        assert cand.candidate_id.path_id == 42
        assert cand.candidate_id.deployment_id == 1
