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
    SubmitContext,
    SubmitSkipReason,
    SubmittedRecord,
    dispatch_and_submit,
    typed_submit_record,
)
from degenbot.exceptions.base import DegenbotValueError


class TestTypedSubmitRecord:
    """The dict → typed-record decoder at the dispatch seam."""

    def test_submitted_record_converts(self) -> None:
        raw = {"kind": "submitted", "path_id": 7, "tx_hash": "0xabc", "nonce": 5}
        record = typed_submit_record(raw)
        assert isinstance(record, SubmittedRecord)
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
        ],
    )
    def test_skipped_reasons_convert(
        self,
        raw_reason: str,
        member: SubmitSkipReason,
        detail: str | None,
    ) -> None:
        raw: dict[str, Any] = {"kind": "skipped", "path_id": 3, "reason": raw_reason}
        if detail is not None:
            raw["detail"] = detail
        record = typed_submit_record(raw)
        assert isinstance(record, SkippedRecord)
        assert record.path_id == 3
        assert record.reason is member
        assert record.detail == detail

    def test_unknown_kind_raises(self) -> None:
        with pytest.raises(DegenbotValueError, match="Unrecognized submit record"):
            typed_submit_record({"kind": "teleported", "path_id": 1})

    def test_unknown_reason_raises(self) -> None:
        raw = {"kind": "skipped", "path_id": 1, "reason": "gravity"}
        with pytest.raises(DegenbotValueError, match="Unrecognized submit record"):
            typed_submit_record(raw)

    @pytest.mark.parametrize(
        "raw",
        [
            {"kind": "submitted"},  # no path_id/tx_hash/nonce
            {"kind": "skipped", "path_id": 1},  # no reason
            {},  # nothing at all
        ],
    )
    def test_missing_keys_raise(self, raw: dict[str, Any]) -> None:
        with pytest.raises(DegenbotValueError, match="Unrecognized submit record"):
            typed_submit_record(raw)


class TestDispatchAndSubmitWrapper:
    """The companion wrapper converts the FFI's dict list to typed records."""

    async def test_wrapper_types_the_ffi_dicts(
        self,
        monkeypatch: pytest.MonkeyPatch,
    ) -> None:
        import degenbot.dispatch as dispatch_mod

        raw_records = [
            {"kind": "submitted", "path_id": 1, "tx_hash": "0xh", "nonce": 2},
            {"kind": "skipped", "path_id": 3, "reason": "dry_run"},
        ]

        def fake_py(**_kwargs: object) -> Any:
            async def _inner() -> list[dict[str, Any]]:  # ruff:ignore[unused-async]  (an awaitable, not a coroutine consumer)
                return list(raw_records)

            return _inner()

        monkeypatch.setattr(dispatch_mod, "_dispatch_and_submit_py", fake_py)
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
        )
        assert len(records) == 2
        assert records[0] == SubmittedRecord(path_id=1, tx_hash="0xh", nonce=2)
        assert isinstance(records[1], SkippedRecord)
        assert records[1].path_id == 3
        assert records[1].reason is SubmitSkipReason.DRY_RUN
        assert records[1].detail is None
