"""Typed decode of the submit-leaf records crossing dispatch_and_submit.

The Rust SubmitRecord enum (degenbot-submission::submit) is the source of
truth: Submitted { path_id, tx_hash, nonce } or Skipped { path_id, reason }.
Its pyfunction wire flattens those to dicts keyed by a "kind" string plus a
"reason" string for skips. This module is the single home for decoding that
wire into typed records: an unrecognized kind/reason or missing payload keys
is Rust/Python wire drift and RAISES — the driver can no longer silently drop
a submission event inside a string "if" chain.
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import StrEnum
from typing import TYPE_CHECKING, Any

from degenbot.exceptions.base import DegenbotValueError

if TYPE_CHECKING:
    from collections.abc import Mapping


class SubmitSkipReason(StrEnum):
    """The typed SkipReason vocabulary of the Rust submit leaf."""

    POOLS_CLAIMED = "pools_claimed"
    DRY_RUN = "dry_run"
    INJECT_CODE = "inject_code"
    BROADCAST_FAILED = "broadcast_failed"


@dataclass(frozen=True, slots=True)
class SubmittedRecord:
    """The tx was broadcast — the resulting tx_hash + claimed nonce."""

    path_id: int
    tx_hash: str
    nonce: int


@dataclass(frozen=True, slots=True)
class SkippedRecord:
    """The candidate was skipped — detail set for BROADCAST_FAILED."""

    path_id: int
    reason: SubmitSkipReason
    detail: str | None = None


SubmitRecord = SubmittedRecord | SkippedRecord


def typed_submit_record(raw: Mapping[str, Any], /) -> SubmitRecord:
    """Decode one FFI submit-record dict into a typed record.

    Returns:
        The typed record.

    Raises:
        DegenbotValueError: On an unrecognized or missing "kind", an unknown
            skip "reason", or missing payload keys — wire drift, loud.

    """
    kind = raw.get("kind")
    try:
        path_id = raw["path_id"]
    except KeyError as e:
        raise DegenbotValueError(
            message=f"Unrecognized submit record (missing path_id): {raw!r}",
        ) from e

    if kind == "submitted":
        try:
            return SubmittedRecord(
                path_id=path_id,
                tx_hash=raw["tx_hash"],
                nonce=raw["nonce"],
            )
        except KeyError as e:
            raise DegenbotValueError(
                message=f"Unrecognized submit record (submitted payload): {raw!r}",
            ) from e

    if kind == "skipped":
        try:
            reason = SubmitSkipReason(raw["reason"])
        except KeyError as e:
            raise DegenbotValueError(
                message=f"Unrecognized submit record (skipped payload): {raw!r}",
            ) from e
        except ValueError as e:
            raise DegenbotValueError(
                message=f"Unrecognized submit record reason: {raw.get('reason')!r}",
            ) from e
        return SkippedRecord(path_id=path_id, reason=reason, detail=raw.get("detail"))

    raise DegenbotValueError(message=f"Unrecognized submit record kind: {kind!r}")
